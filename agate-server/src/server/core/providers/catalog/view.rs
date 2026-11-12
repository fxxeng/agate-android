//! 模型的管理视图和对外视图。所有对外出口使用相同的开启绑定集合。

use std::collections::HashSet;

use serde_json::{json, Map, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::core::capability;
use crate::server::core::key_scope::{self, KeyScope};
use crate::server::core::model_meta;
use crate::server::core::model_rules;
use crate::server::core::models::{list_item, list_response_from, model_id, suggest_from};
use crate::server::core::providers::{kind_from_id, kind_id, ProviderKind};

use super::routing::{builtin_target, entry_id_in_manifest, split_provider_prefix};
use super::{active_manifests, aggregate_source, manifest_for, refresh_meta};

/// 一家会对外广告哪些名字：清单里原生的上游 id + **已启用**的映射别名。
///
/// ── `enabled` 这一道过滤不能少（曾经的漏洞）──────────────────
/// 漏掉它时，被关掉的别名仍会进候选名，然后在 [`builtin_target`] 里绕开
/// 「映射分支」的启停判定走另一条路：`entry_id_in_manifest` 按 id 找不到会
/// **回落到比对上一条目的展示名（`name`）**，命中后按 `default_enabled` 判定
/// —— 那与别名映射的开关无关。于是「关了映射、`/v1/models` 里却还在」。
/// 实测：`space-bunny-alpha`（与展示名逐字相同）会中，`mimo-v2.6-flash`
/// （展示名是 `Mimo V2.6 Flash`，比不中）不会中 —— 同一个开关两种结果。
/// 模块头那句「所有对外出口使用相同的开启绑定集合」说的就是这里必须过滤。
///
/// ── 被别名的原生 id 改名让位（2026-11-03）──────────────────
/// 建了生效映射 `sn-glm-5-3 → glm-5.3` 之后，这一家在清单里**只报 `glm-5.3`**，
/// 不再同时报原名。此前两个名字都进候选名，于是客户端下拉列表里出现两条看
/// 起来不同、实则同一个上游模型的条目 —— 用户建映射的动机恰恰是「我这边只叫
/// 这个名字」。判据见 [`renamed_away_pairs`]（要求别名在本家**至少解析得出某个
/// 上游模型**，否则宁可不隐藏，避免模型从清单里彻底消失；别名与另一个原生
/// id 撞名时仍然让位 —— 那个名字自己就在清单里，不会让模型无路可走）。
///
/// 关掉那条映射 → 原生 id 立刻回到清单里（判据是实时算的，不是一次性迁移）。
fn advertised_names(
    rules: &model_rules::ModelRules,
    provider: &str,
    manifest: &[Value],
) -> Vec<String> {
    // 家 id 反查不到（调用方传了脏值）就不做改名判定 —— 宁可按旧行为两个名字
    // 都报，也不要凭一个猜不出的 kind 去隐藏名字
    let renamed = match kind_from_id(provider) {
        Some(kind) => super::routing::renamed_away_pairs(rules, kind, manifest),
        None => Vec::new(),
    };
    // ── 同一对外名在这一家只广告一次（2026-11-07 真机反馈）──────────
    // 重复的来源有两类，都在这一处收口：
    //   ① 清单本身同一 id 出现两次（上游目录形态多变；`manage_view` 早有
    //      `claimed` 兜底，说明这不是假想）；
    //   ② `provider` 缺省的**全局映射**对每一家都生效（`map_or(true, ..)`
    //      是它的语义），别名恰好与这家某个原生 id 同名 —— 旧残留
    //      `sn-glm-5-3 → glm-5.3` 撞上 ZCode 原生的 `glm-5.3` 就是这种。
    // 旧写法先 `collect()` 原生清单再 `extend()` 别名，两种重复都会留下。
    // Model 形态被 `public_builtin_items` 的跨家 `claimed` 吃掉，Provider /
    // Hybrid 的带前缀分支却各推一条 —— 界面上就是「同一模型重复显示」
    // （ZCode 11 条清单变 13 条）。两个相同的对外名没有语义差别，且解析时
    // `builtin_target` 本来就先按清单 id 匹配、撞名的别名根本不会生效。
    // 按大小写不敏感去重，保留先出现的原生 id。
    let mut names: Vec<String> = Vec::new();
    let mut claimed: HashSet<String> = HashSet::new();
    for id in manifest.iter()
        .map(model_id)
        .filter(|id| !renamed.iter().any(|(hidden, _)| hidden.eq_ignore_ascii_case(id)))
    {
        if claimed.insert(id.to_lowercase()) {
            names.push(id);
        }
    }
    // ── 别名里不能再把「已让位的原名」加回来（2026-11-12 真机）──────────
    // 上面那个 filter 只是从**清单**里拿掉原名，`claimed` 里因此没有它；而
    // 「默认绑定」给每个模型都存了一条 `alias == target` 的自映射，它的别名
    // 恰好就是刚被拿掉的原名 —— 于是 `sn-glm-5-3` 又从下面这个循环回到清单，
    // 客户端看到 `glm-5.3` 与 `sn-glm-5-3` 各占一行，「按别名聚合」等于没发生
    // （用户会以为跟「默认绑定」有关，实际是这一处漏了判据）。
    // 凡是被改名让位的名字一律不再广告，不管它以别名身份来还是以自映射来。
    let hidden: HashSet<String> = renamed.iter()
        .map(|(target_id, _)| target_id.to_lowercase())
        .collect();
    for mapping in rules.mappings.iter()
        .filter(|mapping| mapping.enabled)
        .filter(|mapping| mapping.provider.as_deref().map_or(true, |owner| owner == provider))
        .filter(|mapping| !hidden.contains(&mapping.alias.to_lowercase()))
    {
        if claimed.insert(mapping.alias.to_lowercase()) {
            names.push(mapping.alias.clone());
        }
    }
    names
}

/// 先在各提供商内判断绑定，再按对外名去重；不能先合并上游 ID 再拼别名，
/// 否则某家关闭的映射会借另一家的同名上游重新出现在列表中。
fn public_builtin_items(
    active: &[(ProviderKind, Vec<Value>)],
    mode: ListMode,
    pins: &[String],
) -> Vec<(String, Value)> {
    let rules = model_rules::current();
    let mut claimed = HashSet::new();
    let mut result = Vec::new();
    // pin 同时认 **provider id** 与**展示名**。
    //
    // 只认 id 时，任何把「界面上的家名」当成 pin 存下来的路径（历史配置、
    // 手改 config.json、别的客户端）都会静默失效：芯片点不亮、`hybrid` 计数
    // 与 `model` 一样、清单里一个带前缀的条目都没有 —— 正是「混合模式没生效」
    // 那个现象。多认一个 label 不会有歧义（展示名在同一时刻是唯一的），
    // 却能把这一类「存了但认不出」的输入救回来。
    let pinned = |provider: &str| {
        let label = crate::server::core::providers::label_of(provider);
        pins.iter().any(|pin| {
            pin.eq_ignore_ascii_case(provider) || pin.eq_ignore_ascii_case(&label)
        })
    };

    // 单条 pin：`provider/对外名`。写法与对外 id 完全一致 —— 用户在清单里
    // 看到的那串字符串可以直接填进来，不需要记另一套语法。
    //
    // 与整家 pin 的关系：**整家优先**。某家在 `pins` 里出现时，这家所有模型都
    // 带前缀，此时单条条目即使还在也只是冗余（不会被重复推送）。
    let entry_pinned = |provider: &str, name: &str| {
        let key = format!("{provider}/{name}");
        pins.iter().any(|pin| pin.eq_ignore_ascii_case(&key))
    };
    for (kind, manifest) in active {
        let provider = kind_id(*kind);
        let names = advertised_names(&rules, provider, manifest);
        // 这一家要不要额外带前缀：`provider` 模式全带；`hybrid` 只带被 pin 的；
        // `model` 模式不带（同名只留一条，用哪家由路由队列决定）。
        // 整家是否被 pin（Hybrid 下「这家全部带前缀」的快捷方式）
        let home_pinned = pinned(provider);
        for name in names {
            let Some(wire) = builtin_target(&rules, *kind, manifest, &name) else { continue };
            let Some(upstream) = manifest.iter().find(|item| model_id(item) == wire.model) else { continue };

            // ── 裸名与带前缀是**两条**，不是二选一（2026-10-29 修）──────
            // 旧写法用 `wire_name = if prefixed { 前缀 } else { 裸名 }` —— 于是
            // 被 pin 的家**只出带前缀那一条**，同名裸名被它顶掉。症状很迷惑：
            // 勾 ZCode 一切正常（它的模型先被别家出了裸名），勾 WorkBuddy 就
            // 「裸名全变成 workbuddy/xxx」，因为裸名正是这家自己出的。
            //
            // 混合模式的语义就是「平时的合并清单 + 勾选那几家再各加一份带前缀」
            // —— 两份都要在。
            //
            // 三种模式各自要出什么：
            //   Model    → 只出裸名（同名跨家去重）
            //   Provider → 只出带前缀（每家的每个模型各一条，同名不合并）
            //   Hybrid   → 出裸名（去重）+ 被 pin 的家另出带前缀（不参与去重）
            let bare_wanted = mode != ListMode::Provider;
            // 逐条决定：整家 pin 或这一条自己被 pin
            let prefix_wanted = match mode {
                ListMode::Provider => true,
                ListMode::Hybrid => home_pinned || entry_pinned(provider, &name),
                ListMode::Model => false,
            };

            if bare_wanted {
                // `insert` 的返回值顺便完成去重：同名只有第一家能出裸名
                if claimed.insert(name.to_lowercase()) {
                    let mut item = list_item(upstream, provider);
                    if let Some(object) = item.as_object_mut() {
                        object.insert("id".to_string(), Value::String(name.clone()));
                        if !name.eq_ignore_ascii_case(&wire.model) {
                            object.insert("name".to_string(), Value::String(name.clone()));
                            object.insert("is_default".to_string(), Value::Bool(false));
                        }
                    }
                    result.push((provider.to_string(), item));
                }
            }

            if prefix_wanted {
                let wire_name = format!("{provider}/{name}");
                let mut item = list_item(upstream, provider);
                if let Some(object) = item.as_object_mut() {
                    object.insert("id".to_string(), Value::String(wire_name.clone()));
                    object.insert("name".to_string(), Value::String(wire_name));
                    object.insert("is_default".to_string(), Value::Bool(false));
                }
                result.push((provider.to_string(), item));
            }
        }
    }
    result
}

/// `/v1/models` 的三种输出形态。
///
/// ── 为什么要区分（这是客户端的真实体验差异）──────────────────
///   · `Model`    —— 同名模型只出现一次、不带前缀。用哪一家由「路由」队列决定，
///                   并可在同款模型之间故障转移。客户端列表最短、最好选（推荐默认）。
///   · `Provider` —— 每家的每个模型都列一遍，写成 `provider/model`。客户端能精确
///                   指定某一家，但同名模型会重复很多次，列表容易上百项。
///   · `Hybrid`   —— 平时按模型合并；只为**指定的几家**额外列出带前缀的条目 ——
///                   用于必须指定某家的场景（自建中转、要单独核算的账号）。
///
/// ── 前缀里的 provider 必须是**对外可见的 id** ────────────────
/// 客户端拿这个前缀去发请求，转发层据此锁定那一家，所以两边必须用同一套
/// id（`kind_id`），大小写也要一致。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ListMode {
    #[default]
    Model,
    Provider,
    Hybrid,
}

impl ListMode {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "provider" => ListMode::Provider,
            "hybrid" => ListMode::Hybrid,
            _ => ListMode::Model,
        }
    }
}

pub fn models_response(store: &AccountStore, scope: Option<&KeyScope>) -> Value {
    // 清单形态从配置读（改完下一个请求生效，与其它低频开关同一模式）。
    // 只接受 model / provider / hybrid 三个值，非法值由 config 侧回落成 model。
    let config = crate::server::config::current();
    let mode = ListMode::parse(config.models_list_mode());
    let pins: Vec<String> = config.models_list_pins().to_vec();
    models_response_with_mode(store, scope, mode, &pins)
}

/// 按指定形态生成清单（`models_response` 的实质；单独拆出来便于测试与
/// 管理页推演复用 —— 管理页要用「还没保存的草稿模式」预览客户端会看到什么）。
pub fn models_response_with_mode(
    store: &AccountStore,
    scope: Option<&KeyScope>,
    mode: ListMode,
    pins: &[String],
) -> Value {
    let active: Vec<_> = active_manifests(store).into_iter()
        .filter(|(kind, _)| key_scope::allows_provider(scope, kind_id(*kind)))
        .collect();
    let (source, refreshed_at) = aggregate_source(&active);
    let items = public_builtin_items(&active, mode, pins);
    // ── Key 白名单：按**同义名**判（2026-11-03）───────────────
    // 清单按别名收敛之后，勾了**原名**的那把 Key 会看不到这条模型 —— 逐字比对
    // 会把 `glm-5.3` 判成「不在白名单」（它勾的是 `sn-glm-5-3`），于是模型从那把
    // Key 的清单里凭空消失。这里把白名单展开成「名字 + 它的同义名」一次，判据
    // 与 [`advertised_names`] 的收敛口径同源（见 [`synonymous_names`]）。
    //
    // 一次展开成集合而不是逐条展开：`model_rules::current()` 每次都重新解析一遍
    // 配置，放进逐条循环就是「几十个白名单项 × 几百行清单」次解析。
    let allowed = scope
        .filter(|scope| scope.restricts_models())
        .map(|scope| {
            let rules = model_rules::current();
            let mut set: HashSet<String> = HashSet::new();
            for name in scope.allowed_model_names() {
                for synonym in synonyms_in(&rules, &name) {
                    set.insert(synonym.to_lowercase());
                }
            }
            set
        });
    let mut data: Vec<_> = items.into_iter()
        .filter(|(_, item)| match &allowed {
            None => true,
            Some(set) => {
                // 白名单里存的可能是裸名，而 Provider / Hybrid 形态下清单里的
                // 条目是 `provider/模型`。只按字面比对，勾了裸名的 Key 一换成
                // 「按提供商」就会看不到任何条目（与自定义家那段同一口径，见
                // `providers::custom::append_models_response`）。
                let id = model_id(item);
                let bare = split_provider_prefix(&id).map(|(_, bare)| bare);
                set.contains(&id.to_lowercase())
                    || bare.is_some_and(|bare| set.contains(&bare.to_lowercase()))
            }
        })
        .map(|(_, item)| item)
        .collect();
    let builtin_count = data.len();
    super::super::custom::append_models_response(store, scope, mode, pins, &mut data);
    // 自定义家贡献了条目时，这份列表就不再是单/多家内置的来源了 ——
    // 与「多家内置」同一个词（aggregate），下游据此知道列表是拼出来的。
    let source = if data.len() > builtin_count { "aggregate" } else { source };
    list_response_from(data, source, refreshed_at)
}

/// 一家可用提供商都没有时（没加账号），入口校验要跳过「广告里有才放行」与
/// Key 白名单两条判定，把「没有可用账号」那句更可操作的错误留给转发层 ——
/// 与改造前的两条例外逐字同源（见 `pipeline::resolve_model` 的说明）。
pub fn has_available_providers(store: &AccountStore) -> bool {
    !active_manifests(store).is_empty()
}

pub fn advertised_model_ids(store: &AccountStore) -> Vec<String> {
    models_response(store, None).get("data").and_then(Value::as_array)
        .map(|items| items.iter().map(model_id).collect()).unwrap_or_default()
}

/// 与 `name` **等价**的一组对外名（含自己）。
///
/// ── 别名不是「另一个名字」，是同一个模型的两个写法 ────────────
/// 名单一旦按别名收敛（见 [`advertised_names`]），原名就不再出现在
/// `/v1/models` 里。而下面两处**仍然按名字**做判定，直接按字面比对就会给出
/// 两个错误结论：
///   1. **入口门禁**（`api::pipeline` 的「列表里没有就拒绝」）：一个已经在用
///      `sn-glm-5-3` 的客户端，在他建了 `glm-5.3` 映射之后忽然 404 —— 而内核
///      明明还能把它解析成上游 id。改名是**给名字加一个更合口味的写法**，
///      不该让老写法失效。
///   2. **Key 的模型白名单**：那把 Key 勾的是 `sn-glm-5-3`，清单现在只广告
///      `glm-5.3` —— 逐字比对会让这个模型从**那把 Key 的清单里消失**，
///      看起来像「我勾的模型不见了」。
/// 两处都要认「同义」：沿 `mappings` 双向展开（`alias → target` 与
/// `target → alias`），一跳到头（别名之间不允许链式指向，见 `Mapping` 的说明）。
///
/// ── 两个存储都要扫 ──────────────────────────────────────
/// 内置家的映射在 `modelRules.mappings`，自定义家的在**自己那条记录**的
/// `mappings[]` 里（`custom_providers::synonymous_models`）。只扫前者的话，
/// 自定义家的改名对白名单/门禁不生效，症状是「内置家改名后能调、自定义家改名后
/// 404」—— 同一个功能两套行为，最难查。
pub fn synonymous_names(name: &str) -> Vec<String> {
    synonyms_expanded(
        &model_rules::current(),
        &crate::server::core::custom_providers::list(),
        name,
    )
}

/// 请求名在入口判定里要按哪些写法去比对。
///
/// `provider/model` 是 `/v1/models` 在 Provider / Hybrid 形态下给出的「锁家」
/// 写法（见 `ListMode`），转发层早已用 `routing::split_provider_prefix` 把它
/// 剥成裸名。但入口的三道判定（是否全关 / 清单里有没有 / Key 白名单）都发生在
/// 转发层之前、按**原样**的请求名比对 —— 于是客户端从清单里复制的名字会被
/// 判成「模型不存在」，错误提示还把同一个名字当「相近的模型」列出来
/// （2026-11-07 真机反馈：`模型不存在: zcode/glm-5.3-flash（目录里相近的
/// 模型: zcode/glm-5.3-flash ...）`）。
///
/// 这里刻意**不读当前清单形态**：无论配置是 Model 还是 Provider，带前缀写法
/// 与裸名都是同一个模型的两种写法，判据应当与形态无关（否则切一次模式，
/// 客户端记下的名字就集体失效）。只认已登记的 provider id 作前缀，真实模型名
/// 里的斜杠不受影响。
fn request_name_variants(name: &str) -> Vec<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut out = vec![trimmed.to_string()];
    if let Some((_, bare)) = split_provider_prefix(trimmed) {
        if !out.iter().any(|item| item.eq_ignore_ascii_case(&bare)) {
            out.push(bare);
        }
    }
    out
}

/// [`synonymous_names`] 的实质（规则快照与自定义家清单由调用方给，便于单测）。
///
/// 对 `request_name_variants` 的每一种写法分别展开同义名：带前缀名除了自己，
/// 还要认裸名以及裸名自己的别名（例如 `zcode/sn-glm-5-3` → `glm-5.3`）。
fn synonyms_expanded(
    rules: &model_rules::ModelRules,
    customs: &[Value],
    name: &str,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for seed in request_name_variants(name) {
        for synonym in synonyms_in(rules, &seed) {
            if !out.iter().any(|n| n.eq_ignore_ascii_case(&synonym)) {
                out.push(synonym);
            }
        }
        for provider in customs {
            for synonym in crate::server::core::custom_providers::synonymous_models(provider, &seed) {
                if !out.iter().any(|n| n.eq_ignore_ascii_case(&synonym)) {
                    out.push(synonym);
                }
            }
        }
    }
    out
}

/// 单条写法的别名双向展开（规则快照由调用方给：清单出口已读过一遍配置）。
fn synonyms_in(rules: &model_rules::ModelRules, name: &str) -> Vec<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = vec![trimmed.to_string()];
    let push = |candidate: &str, out: &mut Vec<String>| {
        if !candidate.is_empty() && !out.iter().any(|n| n.eq_ignore_ascii_case(candidate)) {
            out.push(candidate.to_string());
        }
    };
    for mapping in rules.mappings.iter().filter(|mapping| mapping.enabled) {
        if mapping.alias.eq_ignore_ascii_case(trimmed) {
            push(&mapping.target, &mut out);
        } else if mapping.target.eq_ignore_ascii_case(trimmed) {
            push(&mapping.alias, &mut out);
        }
    }
    out
}

/// 入口门禁口径：这个名字**能不能调**（含已改名让位、不再广告的原名）。
///
/// 与 [`advertised_manifest_contains`] 的差别有两个方向，都是刻意的：
///   · **更宽**：认同义名（[`synonymous_names`]）—— 改名之后老写法仍可调用。
///   · **与清单形态无关**：固定按 `Model` 形态算裸名。`/v1/models` 的三种形态
///     只是**展示写法**（`provider/` 前缀是给人挑的），不是「这条模型存不存在」
///     的判据；跟着当前配置走会让「切到按提供商模式 → 所有裸名调用一起 404」。
///     带前缀的请求名由 `synonymous_names` 归一出裸名后一起比对（2026-11-07），
///     所以客户端照清单复制的 `zcode/glm-5.3-flash` 也能通过这道门禁。
pub fn callable_manifest_contains(store: &AccountStore, model: &str) -> bool {
    let ids: Vec<String> = models_response_with_mode(store, None, ListMode::Model, &[])
        .get("data").and_then(Value::as_array)
        .map(|items| items.iter().map(model_id).collect())
        .unwrap_or_default();
    synonymous_names(model).iter()
        .any(|name| ids.iter().any(|id| id.eq_ignore_ascii_case(name)))
}

pub fn advertised_manifest_contains(store: &AccountStore, model: &str) -> bool {
    advertised_model_ids(store).iter().any(|id| id.eq_ignore_ascii_case(model))
}

pub fn suggest_advertised(store: &AccountStore, model: &str, limit: usize) -> Vec<String> {
    suggest_from(advertised_model_ids(store), model, limit)
}

pub fn session_models(store: &AccountStore) -> Vec<Value> {
    models_response(store, None).get("data").and_then(Value::as_array)
        .cloned().unwrap_or_default().into_iter().map(|mut item| {
            let provider = item.get("owned_by").and_then(Value::as_str).unwrap_or("").to_string();
            let is_default = item.get("is_default").cloned().unwrap_or(Value::Bool(false));
            if let Some(object) = item.as_object_mut() {
                object.insert("providerLabel".to_string(), Value::String(super::super::label_of(&provider)));
                object.insert("provider".to_string(), Value::String(provider));
                object.insert("isDefault".to_string(), is_default);
            }
            item
        }).collect()
}

pub fn models_by_provider(store: &AccountStore) -> Value {
    let mut map = Map::new();
    for (kind, manifest) in active_manifests(store) {
        // 这里只取「这家有哪些对外名」，与清单形态无关 —— 传 Model 是刻意的：
        // 前缀形态只是显示层的写法，模型集合本身不变。
        let mut names: Vec<_> = public_builtin_items(&[(kind, manifest)], ListMode::Model, &[])
            .into_iter()
            .map(|(_, item)| model_id(&item)).collect();
        names.sort_by_key(|name| name.to_lowercase());
        map.insert(kind_id(kind).to_string(), json!(names));
    }
    for (provider, items) in super::super::custom::catalog_providers(store) {
        let mut names: Vec<_> = items.iter().map(model_id).collect();
        names.sort_by_key(|name| name.to_lowercase());
        map.insert(provider, json!(names));
    }
    Value::Object(map)
}

/// 一条映射是不是**用户改的名**：`alias == target` 的自映射存的是模型自身的
/// 状态（启停 + 思考等级），不是一条改名规则。
///
/// 这是出口分类的唯一判据：`mappings[]` 只收 `true` 的那些，模型状态走
/// `models[]`。大小写不敏感 —— 与存储侧（`model_rules` 全按 `eq_ignore_ascii_case`
/// 命中同一条）同一口径，否则 `GLM-5.3 → glm-5.3` 会被当成一条改名规则。
fn is_rename(alias: &str, target: &str) -> bool {
    !alias.eq_ignore_ascii_case(target)
}

/// 这条别名**此刻是否真的生效** —— 不问界面、不问开关，直接问请求侧同名入口。
///
/// ── 为什么只认 `builtin_target` 一个结果 ─────────────────────
/// 「生效」不是「映射开关是开的」这么简单：目标模型被关、目标已从目录消失、
/// 别名撞上这家另一个原生 id、映射自身被关，全都会让这个名字解析不到东西。
/// 这些判据已经在 [`builtin_target`] 里收齐（66b43c5 补的正是「target 被关
/// ⇒ 别名跟着失效」），而 `/v1/models` 的 `public_builtin_items` 也用它逐名
/// 过滤（`let Some(wire) = builtin_target(..) else { continue }`）。
///
/// 界面若自己拼一遍这些条件，就是**第二套判据**：内核一加新条件，模型页就会
/// 把一条已失效的别名画成生效。所以管理视图在这里把真值算好，随每条
/// `mappings[]` 行发下去，客户端只读不算（见 `ModelsPage` 的别名标注）。
///
/// 口径与 `renamed_away_pairs` 的判据 3 一致：只要求**解析得出某个上游模型**，
/// 不要求解析回 `target`。别名撞原生 id 时请求确实走那条原生条目，但清单里
/// 名字仍在、原名仍让位 —— 两处必须同口径，否则界面会把刚藏掉的原名放回来，
/// 重现「按模型模式里原名与别名并列」。
fn alias_effective(
    rules: &model_rules::ModelRules,
    kind: Option<ProviderKind>,
    manifest: &[Value],
    alias: &str,
) -> bool {
    let Some(kind) = kind else { return false };
    builtin_target(rules, kind, manifest, alias).is_some()
}

/// 一家内置提供商的清单 → `(models[], mappings[])` 两类行。
///
/// 从 `manage_view` 里拆出来只为一件事：这里的两个判定就是「默认绑定」改造的
/// 全部语义，而 `manage_view` 需要真实账号才有清单（`active_manifests` 按可用
/// 账号过滤），端到端测不了。判据是：**模型自身的状态挂在模型行上**（`enabled`
/// / `reasoningBinding`），**`mappings[]` 只装用户改的名**。
fn builtin_rows(
    rules: &model_rules::ModelRules,
    provider: &str,
    mut manifest: Vec<Value>,
    remote: bool,
    refreshed_at: i64,
    claimed: &mut HashSet<(String, String)>,
    attached: &mut HashSet<usize>,
) -> (Vec<Value>, Vec<Value>) {
    let mut models = Vec::new();
    let mut mappings = Vec::new();
    // 关掉的沉到底部（与清单、界面的阅读顺序一致）
    manifest.sort_by_key(|item| !rules.default_enabled(provider, &model_id(item)));
    let source = if remote { "remote" } else { "builtin" };
    // `alias_effective` 要拿同一份清单问请求侧入口，所以这里按引用遍历，
    // 不把 manifest move 掉。`provider` 来自 `kind_id(kind)`，必能反查回来。
    let kind = kind_from_id(provider);
    for item in &manifest {
        let id = model_id(item);
        if id.is_empty() { continue; }
        if !claimed.insert((provider.to_string(), id.to_lowercase())) {
            continue; // 同家同 id 的重复条目（见 manage_view 里 claimed 的说明）
        }
        // 模型自身的状态：存量就是一条 `alias == target` 的自映射（`binding`
        // 把「没有这条映射」也归一到同一份默认值）。它不再被写成一条映射行，
        // 只把两个字段挂到下面的模型行上。
        let default = rules.binding(provider, &id, &id);
        let enabled = rules.default_enabled(provider, &id);
        let mut aliases = Vec::new();
        for (index, mapping) in rules.mappings.iter().enumerate() {
            if !mapping.target.eq_ignore_ascii_case(&id)
                || !mapping.provider.as_deref().map_or(true, |owner| owner == provider)
            { continue; }
            // 命中这条模型的规则一律算「已归属」—— 包括下面要跳过的自映射，
            // 否则它会从函数末尾的「悬空规则」那一轮再冒出来。
            attached.insert(index);
            if !is_rename(&mapping.alias, &id)
                || aliases.iter().any(|alias: &String| alias.eq_ignore_ascii_case(&mapping.alias))
            { continue; }
            let Some(effective) = rules.binding(provider, &mapping.alias, &id) else { continue };
            aliases.push(mapping.alias.clone());
            mappings.push(json!({
                "alias": effective.alias, "target": id, "provider": provider,
                "enabled": effective.enabled, "reasoning": effective.reasoning,
                // 真值由请求侧同名入口算（见 alias_effective）：界面不自己拼
                // 「映射开 / 模型开 / 在清单」那套条件，免得两套判据漂移。
                "effective": alias_effective(rules, kind, &manifest, &mapping.alias),
                "dangling": false, "carried": true,
            }));
        }
        let source = if rules.custom.iter().any(|custom| custom.matches(provider, &id)) {
            "manual"
        } else { source };
        // 覆盖键列表从**本次快照**里查（不调 model_rules::current() —— 那会
        // 每行重新解析一遍配置，几百行的清单就是几百次解析）
        let cap_overrides: Vec<String> = rules
            .capabilities
            .iter()
            .find(|entry| entry.matches(provider, &id))
            .map(|entry| capability::overridden_keys(&entry.values))
            .unwrap_or_default();
        models.push(json!({
            "id": id, "name": item.get("name"), "credits": item.get("credits"),
            "isDefault": item.get("isDefault").and_then(Value::as_bool).unwrap_or(false),
            "provider": provider, "providerLabel": super::super::label_of(provider),
            // ── 图像条目的三个键（2026-11-06）────────────────────
            // 生图通道接通之后，管理视图里第一次出现 **`kind=image` 的行**。
            // 不把这三件带出来的话，模型页只能按「工具 ✗ 图片 ✗ 思考 ✗」
            // 画一条三连灰 —— 用户完全看不出它不是对话模型（那三个位为 false
            // 是**如实**的：生图模型确实不做工具调用，也不收图）。
            // 判据与出口同源（`shape::list_item` 读的是同一批键）。
            "kind": item.get("kind"),
            "outputModalities": item.get("output_modalities"),
            "supportsImageGeneration": item.get("supports_image_generation"),
            "supportsImageEditing": item.get("supports_image_editing"),
            "source": source, "enabled": enabled, "aliases": aliases,
            // 用户给这条模型**绑定的思考档位**（`null` = 未绑定，跟随请求默认）。
            // 与下面 `reasoningDefaultLevel`（模型自己声明的默认档）是两件事：
            // 那是上游的事实（它自己默认怎么想），这是用户的偏好（我要它用哪
            // 一档）。模型行那颗「思考」芯片的当前值读的就是本键 —— 此前它只能
            // 从一条合成的自映射行里读（`mappings[]`），于是模型页要先把「映射」
            // 翻一遍才拿得到模型自己的状态。
            "reasoningBinding": default.and_then(|mapping| mapping.reasoning.clone()),
            "capabilities": capability::effective(item),
            "capOverrides": cap_overrides,
            // 在不在**官方客户端下拉（picker）**里：`shown` / `hidden`，
            // 拿不到名单时缺失（null）。这是 `apply_remote` 写进清单条目的
            // 同一个键 —— 在此之前它**没有任何消费方**（管理视图不返回、
            // 前端不读），于是「上游给了 40 条、桌面端只显示 18 条」这件事
            // 在界面上完全看不出来。模型页据它标「桌面端未列」并解释
            // 「为什么我这个客户端看到的名字和桌面端不一样」。
            "visibility": item.get("visibility"),
            // 每个能力键的**来源**（`upstream` / `modelsdev` / `user`，见
            // `core::model_meta` 的模块头）。管理页据它把「上游实测」「社区
            // 库补的」「用户改的」三档分开显示 —— 没有它，六个键看起来一样
            // 可信，而它们的可信度差着量级。缺失（没有这个键）= 那一项没有
            // 任何来源（未声明）。
            "capSources": item.get("capSources"),
            // 模型**自己能配哪些思考档位**（出口键名，由 `core::model_meta`
            // 在清单成型处归一：这家自己声明的 > models.dev 的 effort 列表）。
            // 与 `reasoningBinding` 是两件事：这里回答「这个模型支持哪些档」，
            // 那里回答「用户给它选了哪一档」。缺失（null）= 该模型未声明，
            // 界面据此回落通用档位表。
            "reasoningLevels": item.get("reasoningLevels"),
            "reasoningDefaultLevel": item.get("reasoningDefaultLevel"),
            // 这一家的清单是什么时候拉到的（毫秒；0 = 从未成功拉过）。
            // 缓存恢复的清单与刚拉的清单**都是 `remote`**，时效只能靠这个
            // 时间戳说明（见 `providers::catalog_cache` 的模块头）。
            "refreshedAt": refreshed_at,
        }));
    }
    (models, mappings)
}

/// 模型管理视图的两份清单：`models`（每条上游模型一行，含已禁用）与
/// `mappings`（**只**含用户改的名）。
///
/// ── 自映射（`alias == target`）为什么不在 `mappings[]` 里 ────────────
/// 存储上模型状态确实是一条自映射：`ModelRules` 用同一张表承载启停、思考等级
/// 与改名。但**出口不必照抄存储形态**。照抄的后果是映射页里堆满「自己改成
/// 自己」的条目，用户读成「这里有 N 条改名规则」，而它其实是「这个模型关着 /
/// 用哪一档思考」（2026-11-12 的反馈原话：「默认绑定误以为映射改名」）。
///
/// 所以出口按**是谁的属性**分类：
///   · `models[]` —— 模型自己的状态：`enabled`（在不在清单里）、
///     `reasoningBinding`（用哪一档思考）；
///   · `mappings[]` —— 人对名字做的改动：`alias`（对外名）→ `target`（上游 id）。
///
/// 存储与写接口都没变：`POST /api/models/mappings` 仍然用 (alias, target,
/// provider) 三元组命中同一条，自映射照样能写（模型页的思考芯片就走它）。
/// 变的只是读出来的位置 —— 界面不必再从「映射」里反推模型状态。
///
/// ── 能力位（`capabilities` / `capOverrides`）─────────────────
/// 每行带两个能力字段（管理页的「上下文 / 输出」与「能力」两列读它们）：
///   · `capabilities`：**生效值**（清单原值 + 用户覆盖，六项齐全，
///     `null` = 未声明 —— 与「不支持 / 0」区分开，用户编辑时要靠它分辨
///     「不知道」与「明确不支持」）；
///   · `capOverrides`：被用户覆盖的键（管理页据此给「已改」标记；
///     空数组 = 全部继承清单原值）。
/// 清单原值已由 `manifest_for` 应用过覆盖，所以这里读到的就是生效值。
pub fn manage_view(store: &AccountStore) -> Value {
    let rules = model_rules::current();
    let mut models = Vec::new();
    let mut mappings = Vec::new();
    let mut attached = HashSet::new();
    // `(provider, id 小写)` 的出口去重（修复 #177 的兜底一层）。
    //
    // 各家的目录解析**应**在源头去重（如 Trae 的 `parse_catalog` 按
    // `config_name`），但上游形态随时可能变化、别家未必有同样的去重 —— 管理页
    // 是「清单的可视检查」，同 id 画出两行，用户看到的就是两条一模一样的条目。
    // 这一层独立于源头：不管哪家目录出了脏数据，这里只认第一次出现的那条。
    let mut claimed: HashSet<(String, String)> = HashSet::new();
    for (kind, manifest) in active_manifests(store) {
        // 一次取「远程与否 + 拉取时刻」：两者是同一处状态，分两次取会多读一次锁
        let (remote, refreshed_at) = refresh_meta(kind);
        let (mut model_rows, mut mapping_rows) = builtin_rows(
            &rules,
            kind_id(kind),
            manifest,
            remote,
            refreshed_at,
            &mut claimed,
            &mut attached,
        );
        models.append(&mut model_rows);
        mappings.append(&mut mapping_rows);
    }
    // ── 自定义家（含「预置 API」建出来的那些）────────────────────
    //
    // 管理视图此前只遍历 `active_manifests` —— 那是**纯内置适配器**（`all_kinds`
    // 来自 `PROVIDERS` 表）。自定义家（`custom-xxxx`）没有 `ProviderKind`，
    // 于是**永远进不了这个视图**。
    //
    // 后果是模型页的【全部】筛选行少了一整类家：那个筛选行按「清单里有哪些家」
    // 派生（`allRows` = 会话清单 ∪ 管理视图），而自定义家只可能从会话清单进来
    // —— 那条路要求这家**已有模型**。于是「刚建好、还没登记模型」的自定义家
    // 在模型页彻底不出现：用户建完家去模型页找它，什么都没有，也没有任何
    // 报错能解释它去哪儿了。
    //
    // 这里把自定义家并进来。**它家自己的记录就是事实来源**（不走 `model_rules`
    // —— 那个是内置家的启停与映射存储）：启停状态、别名映射、能力位覆盖都
    // 读自定义家自己的字段。
    //
    // ── 数据源取「登记条目」而不是 `public_models` ────────────────
    // `public_models` 走的是 `bindings::resolve`，而 `resolve` 会把**已禁用**
    // 的模型过滤掉 —— 那是给对外清单用的（禁用的不该广告给客户端）。但管理
    // 视图的职责恰恰相反：**关掉的模型要留在页面上（置灰），用户才能重新
    // 打开它**。用 `public_models` 当数据源，被关掉的自定义模型会从管理视图
    // 里消失，用户再也找不到它、也再也打不开它。
    //
    // 所以这里直接读这家登记的 `models[]`（原始形态，不过滤启停），能力位
    // 按与内置家同一套判定合成（`capability::effective` + `model_meta::apply`），
    // 保证两边看到的能力值同源。
    for (provider, _) in super::super::custom::catalog_providers(store) {
        // 登记条目（原始形态，含用户登记的 enabled / capabilities / reasoning）
        let registered = crate::server::core::custom_providers::get(&provider);
        let Some(registered) = registered else { continue; };
        let registered_models = registered
            .get("models")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let registered_mappings = registered
            .get("mappings")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        for entry in registered_models {
            let id = model_id(entry);
            if id.is_empty() { continue; }
            if !claimed.insert((provider.to_string(), id.to_lowercase())) {
                continue; // 同家同 id 的重复条目（与内置家同一道去重）
            }
            // 能力位生效值：登记的那份 + models.dev 补齐（与内置家同一套判定 ——
            // 用户登记的值 > models.dev > 未声明）。
            // `id` 在下面还要用（映射的 alias/target），所以这里用引用构造，
            // 不把它 move 走。
            let mut item = json!({ "id": id.as_str() });
            if let Some(capabilities) = entry.get("capabilities") {
                let normalized = capability::normalize_object(Some(capabilities));
                capability::apply_overrides(&mut item, &normalized);
            }
            model_meta::apply(&mut item);
            let enabled = entry.get("enabled").and_then(Value::as_bool).unwrap_or(true);
            // 别名：登记在这家、且指向这条模型的映射（对外名 → 原始 id）。
            //
            // ⚠️ 这些映射**不进** `mappings[]`（那是内置家的映射视图，写入口
            // `POST /api/models/mappings` 只认内置家）。自定义家的映射由
            // 「登记模型」弹层管理（它走整表保存接口，与那家的启停 / 能力位
            // 同一处存储）。写进 `mappings[]` 只会让映射页出现**点不动的死行**
            // —— 开关一拨就报「未知的内置提供商」。
            //
            // 别名名单本身仍如实填进模型的 `aliases`（这是模型的事实，不是
            // 配置视图）。
            let mut aliases = Vec::new();
            for mapping in registered_mappings {
                let Some(alias) = mapping.get("alias").and_then(Value::as_str) else { continue; };
                let Some(mapped) = mapping.get("target").and_then(Value::as_str) else { continue; };
                if !mapped.eq_ignore_ascii_case(&id) { continue; }
                if alias.eq_ignore_ascii_case(&id)
                    || aliases.iter().any(|name: &String| name.eq_ignore_ascii_case(alias))
                { continue; }
                aliases.push(alias.to_string());
            }
            // 能力位覆盖：登记条目上用户手填的那几项（`capabilities` 稀疏表）。
            let cap_overrides: Vec<String> = entry
                .get("capabilities")
                .and_then(Value::as_object)
                .map(|map| capability::overridden_keys(map))
                .unwrap_or_default();
            // ⚠️ 这里**没有** `reasoningBinding`：自定义家的思考等级存在它自己
            // 那条记录上（`bindings::resolve` 读 `models[].reasoning` 与自映射的
            // `reasoning`），写入口是「登记模型」的整表保存 —— `POST
            // /api/models/mappings` 只认内置家。给出一个界面改不动的值只会让
            // 模型行那颗芯片点出一句报错，所以本键缺失（= 内核没说）比给个谎好。
            models.push(json!({
                "id": id.as_str(), "name": item.get("name"), "credits": item.get("credits"),
                "isDefault": false,
                "provider": provider, "providerLabel": super::super::label_of(&provider),
                // 自定义家的模型全是**用户登记**的 —— 与内置家的
                // `builtin` / `remote` 不同源，标成 `manual`（映射页据它
                // 区分「用户手工登记的模型」）。
                "source": "manual", "enabled": enabled, "aliases": aliases,
                // 图像三键与内置家同一处置（见上面那段说明）。自定义家目前
                // **接不上生图通道**（没有 `ProviderAdapter` 可问能力位），
                // 所以这三件在它这里通常是缺失/null —— 如实透出，界面据此
                // 显示「未声明」而不是画一条误导性的三连灰。
                "kind": item.get("kind"),
                "outputModalities": item.get("output_modalities"),
                "supportsImageGeneration": item.get("supports_image_generation"),
                "supportsImageEditing": item.get("supports_image_editing"),
                "capabilities": capability::effective(&item),
                "capOverrides": cap_overrides,
                "visibility": Value::Null,
                "capSources": item.get("capSources"),
                "reasoningLevels": item.get("reasoningLevels"),
                "reasoningDefaultLevel": item.get("reasoningDefaultLevel"),
                // 自定义家没有「远程目录」这回事（清单是登记的，不是拉的）——
                // 0 = 从未成功拉过（与内置家同一语义）。
                "refreshedAt": 0,
            }));
        }
    }
    // ── 剩下的是「没有任何模型接管」的规则 ────────────────────────
    //
    // 自映射在这里**一律跳过**：它是模型状态、不是改名规则（见本函数的说明）。
    // 目标模型不在任何清单里时，那条状态无处可编辑（页面上没有那一行），
    // 展示出来只会变成一条读不懂的「A → A」—— 模型回到清单时状态自然还在，
    // 所以这里只是不展示，不是丢弃。
    for (index, mapping) in rules.mappings.iter().enumerate() {
        if attached.contains(&index) { continue; }
        if !is_rename(&mapping.alias, &mapping.target) { continue; }
        let carried = match mapping.provider.as_deref() {
            Some(provider) => kind_from_id(provider).is_some_and(|kind| {
                entry_id_in_manifest(&manifest_for(kind), &mapping.target).is_some()
            }),
            None => !super::providers_for_model(&mapping.target).is_empty(),
        };
        mappings.push(json!({
            "alias": mapping.alias, "target": mapping.target, "provider": mapping.provider,
            "enabled": mapping.enabled, "reasoning": mapping.reasoning,
            // 悬空 = 当前**没有任何 active 清单**接管这条规则，请求侧无从解析。
            // `carried` 说的是另一件事（静态目录里还有没有这名字），两者不矛盾：
            // 目录有、但这家现在没在服务，别名照样调不通。
            "effective": false, "dangling": true, "carried": carried,
        }));
    }
    json!({
        "models": models,
        "mappings": mappings,
        "reasoningLevels": model_rules::REASONING_LEVELS,
        // models.dev 元数据的当前状态（`builtin` / `cache` / `network` + 生成
        // 时刻 + 条数）。它服务的是「这份档位/能力是什么时候的数据」这个问题 ——
        // 界面上那些档位来自哪一份、新不新，只能由这里回答。
        "modelsDev": model_meta::status(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `from_raw` 收的是整份 config（它自己取 `modelRules` 键），所以包一层
    fn rules_with(mappings: Value) -> model_rules::ModelRules {
        let raw = json!({ "modelRules": { "mappings": mappings } });
        model_rules::ModelRules::from_raw(raw.as_object().expect("对象字面量"))
    }

    /// 一条展示名与别名逐字相同的上游条目（`space-bunny-alpha` 是实测命中的那个）
    fn manifest() -> Vec<Value> {
        vec![json!({ "id": "stealth/space-bunny-alpha", "name": "space-bunny-alpha" })]
    }

    /// 自映射（`alias == target`）是**模型自身的状态**，不是一条改名规则。
    ///
    /// 它此前被合成成 `mappings[]` 里一行 `alias == target` 的条目，映射页于是
    /// 看起来堆着「自己改成自己」的规则（用户 2026-11-12 的反馈）。现在出口把
    /// 它拆成两处：`models[].enabled` 与 `models[].reasoningBinding`。
    #[test]
    fn self_mapping_is_model_state_not_a_rename() {
        let rules = rules_with(json!([
            { "alias": "glm-5.3", "target": "glm-5.3", "provider": "zcode",
              "enabled": true, "reasoning": "high" }
        ]));
        let (models, mappings) = builtin_rows(
            &rules,
            "zcode",
            vec![json!({ "id": "glm-5.3" })],
            false,
            0,
            &mut HashSet::new(),
            &mut HashSet::new(),
        );
        assert!(mappings.is_empty(), "自映射不该出现在 mappings[] 里: {mappings:?}");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["enabled"], json!(true));
        assert_eq!(models[0]["reasoningBinding"], json!("high"));
    }

    /// 真改名照旧进 `mappings[]`，并且模型行不再顺带背一个等级。
    ///
    /// 两个出口各管一件事：改名的思考等级属于**那条别名**，模型行上的
    /// `reasoningBinding` 只回答「这条上游模型自己用哪一档」。
    #[test]
    fn rename_stays_in_mappings_and_model_row_carries_its_own_level() {
        let rules = rules_with(json!([
            { "alias": "glm-5-3", "target": "glm-5.3", "provider": "zcode",
              "enabled": true, "reasoning": "low" }
        ]));
        let (models, mappings) = builtin_rows(
            &rules,
            "zcode",
            vec![json!({ "id": "glm-5.3" })],
            false,
            0,
            &mut HashSet::new(),
            &mut HashSet::new(),
        );
        assert_eq!(mappings.len(), 1, "改名规则要留在 mappings[] 里: {mappings:?}");
        assert_eq!(mappings[0]["alias"], json!("glm-5-3"));
        assert_eq!(mappings[0]["reasoning"], json!("low"));
        assert!(
            mappings[0].get("isDefault").is_none(),
            "mappings[] 里不该再有 isDefault 这个键（这份清单只装改名）"
        );
        assert_eq!(models[0]["aliases"], json!(["glm-5-3"]));
        assert_eq!(
            models[0].get("reasoningBinding"),
            Some(&Value::Null),
            "没给这条模型绑等级时本键存在且为 null（界面据此显示「不覆盖」）"
        );
    }

    /// 带 `disabled` 的完整规则（「模型被关 ⇒ 别名失效」要用）。
    fn rules_with_state(mappings: Value, disabled: Value) -> model_rules::ModelRules {
        let raw = json!({ "modelRules": { "mappings": mappings, "disabled": disabled } });
        model_rules::ModelRules::from_raw(raw.as_object().expect("对象字面量"))
    }

    /// 一条映射 + 一条在清单里的目标 ⇒ 别名生效。
    fn rows_for(rules: &model_rules::ModelRules) -> (Vec<Value>, Vec<Value>) {
        builtin_rows(
            rules, "zcode", vec![json!({ "id": "glm-5.3" })], false, 0,
            &mut HashSet::new(), &mut HashSet::new(),
        )
    }

    /// 别名的**真值**跟着请求侧入口走，不是「映射开关是开的」就算数。
    /// 这一组钉住的是 `mappings[].effective` 的四种形状：界面只读这个值，
    /// 所以这里错一个，模型页就会把一条调不通的别名画成生效。
    #[test]
    fn alias_effective_follows_the_request_side_resolver() {
        let live = json!([
            { "alias": "glm-5-3", "target": "glm-5.3", "provider": "zcode", "enabled": true }
        ]);
        let (_, mappings) = rows_for(&rules_with_state(live.clone(), json!([])));
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0]["effective"], json!(true));

        // ① 目标模型被关（#129 的主症状）：映射还开着，别名也调不通
        let (_, mappings) = rows_for(&rules_with_state(
            live.clone(), json!([{ "provider": "zcode", "id": "glm-5.3" }]),
        ));
        assert_eq!(mappings[0]["effective"], json!(false), "模型关了，别名必须跟着失效");

        // ② 映射自己被关
        let off = json!([
            { "alias": "glm-5-3", "target": "glm-5.3", "provider": "zcode", "enabled": false }
        ]);
        let (_, mappings) = rows_for(&rules_with_state(off, json!([])));
        assert_eq!(mappings[0]["effective"], json!(false), "关掉的映射不该显示成生效");

        // ③ 目标已经不在清单里（悬空）
        let dangling = json!([
            { "alias": "glm-5-3", "target": "gone-model", "provider": "zcode", "enabled": true }
        ]);
        let rules = rules_with_state(dangling, json!([]));
        assert!(!alias_effective(
            &rules, kind_from_id("zcode"), &[json!({ "id": "glm-5.3" })], "glm-5-3",
        ));
    }

    /// 别名撞上这家另一个原生 id 时，解析落到那条原生条目上 —— 只要**解析得出
    /// 东西**就算生效。与 `renamed_away_pairs` 判据 3 同口径；如果这里收紧成
    /// 「必须解析回 target」，界面会把刚让位藏起来的原名又放回清单。
    #[test]
    fn alias_resolving_to_another_native_id_is_still_effective() {
        let rules = rules_with(json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "zcode", "enabled": true }
        ]));
        let manifest = vec![json!({ "id": "sn-glm-5-3" }), json!({ "id": "glm-5.3" })];
        assert!(alias_effective(&rules, kind_from_id("zcode"), &manifest, "glm-5.3"));
    }

    /// 自映射的判据大小写不敏感：存储侧全按 `eq_ignore_ascii_case` 命中同一条，
    /// 出口若区分大小写，`GLM-5.3 → glm-5.3` 会被当成一条改名规则漏出来。
    #[test]
    fn self_mapping_is_matched_case_insensitively() {
        assert!(!is_rename("GLM-5.3", "glm-5.3"));
        assert!(!is_rename("glm-5.3", "glm-5.3"));
        assert!(is_rename("glm-5-3", "glm-5.3"));
    }

    /// 被关掉的映射别名不能进广告候选名。漏了这道过滤时，别名会经由
    /// `builtin_target` 的「展示名回落」绕开启停判定 —— 于是「关了映射，
    /// `/v1/models` 里却还在」（2026-10-03 实测复现）。
    #[test]
    fn disabled_mapping_alias_is_not_advertised() {
        let rules = rules_with(json!([
            { "alias": "space-bunny-alpha", "target": "stealth/space-bunny-alpha",
              "provider": "cline-free", "enabled": false }
        ]));
        let names = advertised_names(&rules, "cline-free", &manifest());
        assert!(
            !names.iter().any(|name| name == "space-bunny-alpha"),
            "关掉的别名不该出现在候选名里: {names:?}"
        );
        // 原生 id 不受影响（开关只管别名）
        assert!(names.iter().any(|name| name == "stealth/space-bunny-alpha"));
    }

    /// 开着的映射别名照旧进候选 —— 不能让这次修复把功能一起修没
    #[test]
    fn enabled_mapping_alias_is_advertised() {
        let rules = rules_with(json!([
            { "alias": "space-bunny-alpha", "target": "stealth/space-bunny-alpha",
              "provider": "cline-free", "enabled": true }
        ]));
        let names = advertised_names(&rules, "cline-free", &manifest());
        assert!(names.iter().any(|name| name == "space-bunny-alpha"));
    }

    /// 缺 `enabled` 键 = 开关功能上线前的历史条目 = 默认启用（升级不改行为）
    #[test]
    fn mapping_without_enabled_key_is_advertised() {
        let rules = rules_with(json!([
            { "alias": "space-bunny-alpha", "target": "stealth/space-bunny-alpha",
              "provider": "cline-free" }
        ]));
        let names = advertised_names(&rules, "cline-free", &manifest());
        assert!(names.iter().any(|name| name == "space-bunny-alpha"));
    }

    /// 别家的映射别名不该混进这一家的候选（provider 过滤仍在）
    #[test]
    fn other_providers_alias_is_not_included() {
        let rules = rules_with(json!([
            { "alias": "x-alias", "target": "stealth/space-bunny-alpha",
              "provider": "workbuddy", "enabled": true }
        ]));
        let names = advertised_names(&rules, "cline-free", &manifest());
        assert!(!names.iter().any(|name| name == "x-alias"));
    }

    /// 小浣熊的最小清单：`sn-glm-5-3` 一条
    fn raccoon_manifest() -> Vec<Value> {
        vec![json!({ "id": "sn-glm-5-3" })]
    }

    /// **改名之后，这一家在清单里只报别名**（2026-11-03 用户第三次反馈的口径）。
    ///
    /// 建了 `sn-glm-5-3 → glm-5.3` 之后，`GET /v1/models` 里不该再出现原名 ——
    /// 那是「客户端所见清单」那张卡的唯一数据源，用户看到的是「映射也建了，
    /// 清单里还是原名」。
    #[test]
    fn renamed_original_id_is_not_advertised() {
        let rules = rules_with(json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon", "enabled": true }
        ]));
        let names = advertised_names(&rules, "raccoon", &raccoon_manifest());
        assert!(names.iter().any(|name| name == "glm-5.3"), "别名必须在: {names:?}");
        assert!(
            !names.iter().any(|name| name == "sn-glm-5-3"),
            "改名的原生 id 不该再广告: {names:?}"
        );
    }

    /// **默认绑定（`alias == target` 的自映射）不该把让位的原名加回来**
    ///
    /// 真机现场（2026-11-12）：映射页「默认绑定」给每个模型都存了一条自映射，
    /// 于是 `advertised_names` 的清单一侧把 `sn-glm-5-3` 拿掉之后，别名一侧
    /// 又被这条自映射的 `alias`（正是 `sn-glm-5-3`）加回来 —— 客户端里
    /// `glm-5.3` 与 `sn-glm-5-3` 各占一行，「按别名聚合」完全没有发生。
    #[test]
    fn default_binding_does_not_resurrect_the_hidden_name() {
        let rules = rules_with(json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon", "enabled": true },
            { "alias": "sn-glm-5-3", "target": "sn-glm-5-3", "provider": "raccoon", "enabled": true }
        ]));
        let manifest = vec![json!({ "id": "sn-glm-5-3" }), json!({ "id": "sn-kimi-k8" })];
        let names = advertised_names(&rules, "raccoon", &manifest);
        assert!(
            !names.iter().any(|name| name == "sn-glm-5-3"),
            "自映射把让位的原名加回来了: {names:?}"
        );
        assert_eq!(names, vec!["sn-kimi-k8".to_string(), "glm-5.3".to_string()]);
    }
    /// 关掉那条映射 → 原名立刻回到清单里（「关掉才显示」）
    #[test]
    fn disabling_the_mapping_restores_the_original_id() {
        let rules = rules_with(json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon", "enabled": false }
        ]));
        let names = advertised_names(&rules, "raccoon", &raccoon_manifest());
        assert!(names.iter().any(|name| name == "sn-glm-5-3"));
        assert!(!names.iter().any(|name| name == "glm-5.3"));
    }

    /// 别名指向一个**清单里没有**的 target（悬空映射）→ 不隐藏原名。
    ///
    /// 否则这个模型会从清单里彻底消失，而它其实还能调 —— 消失是故障，
    /// 多列一个名字只是吵。
    #[test]
    fn dangling_mapping_does_not_hide_the_original() {
        let rules = rules_with(json!([
            { "alias": "gone", "target": "sn-not-there", "provider": "raccoon", "enabled": true }
        ]));
        let names = advertised_names(&rules, "raccoon", &raccoon_manifest());
        assert!(names.iter().any(|name| name == "sn-glm-5-3"));
    }

    /// 别名与本家**另一个原生 id** 撞名 → 仍然隐藏原名（2026-11-12 收口）
    ///
    /// 撞名时 `glm-5.3` 自己就在清单里（它就是原生条目），隐去原名不会让模型
    /// 消失；两条并列才是问题 —— 客户端下拉里出现「看起来不同、实则同一个
    /// 上游模型」的两个名字，正是「按模型模式里还是显示原名」那个现象。
    /// 旧判据（不隐藏）是更早的保守选择，用户明确要求按别名聚合。
    #[test]
    fn alias_colliding_with_another_native_id_hides_the_original() {
        let rules = rules_with(json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon", "enabled": true }
        ]));
        let manifest = vec![json!({ "id": "sn-glm-5-3" }), json!({ "id": "glm-5.3" })];
        let names = advertised_names(&rules, "raccoon", &manifest);
        assert_eq!(names, vec!["glm-5.3".to_string()], "撞名时也只该剩一条: {names:?}");
    }

    /// 别家的同名映射不影响这一家的原名广告（映射按 provider 归属）
    #[test]
    fn other_providers_rename_does_not_hide_this_providers_original() {
        let rules = rules_with(json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "workbuddy", "enabled": true }
        ]));
        let names = advertised_names(&rules, "raccoon", &raccoon_manifest());
        assert!(names.iter().any(|name| name == "sn-glm-5-3"));
    }

    /// 全局映射（provider 缺省）的别名与这一家的原生 id 撞名 → 只广告一次。
    ///
    /// 真机（2026-11-07）：ZCode 静态清单 11 条，模型页却显示 13 条，多出来的
    /// 正是 `glm-5.3` / `glm-5.3-flash` 各一条逐字相同的 `zcode/...` 行。
    /// 全局映射对每一家都生效，别名与本家原生 id 同名时旧写法会给出两个
    /// 相同的对外名；Model 形态被 `claimed` 吃掉，Provider / Hybrid 的带前缀
    /// 分支不去重，于是重复原样出现在 `/v1/models` 与 `/api/session`。
    #[test]
    fn global_alias_colliding_with_native_id_is_advertised_once() {
        let rules = rules_with(json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "enabled": true },
            { "alias": "glm-5.3-flash", "target": "sn-glm-5-3-flash", "enabled": true }
        ]));
        let zcode = vec![
            json!({ "id": "glm-5.3" }),
            json!({ "id": "glm-5.3-flash" }),
            json!({ "id": "glm-5.2" }),
        ];
        let names = advertised_names(&rules, "zcode", &zcode);
        assert_eq!(names.len(), 3, "撞名的全局别名不该多出条目: {names:?}");
        assert_eq!(
            names.iter().filter(|name| name.as_str().eq_ignore_ascii_case("glm-5.3")).count(),
            1,
            "{names:?}"
        );
        assert_eq!(
            names.iter().filter(|name| name.as_str().eq_ignore_ascii_case("glm-5.3-flash")).count(),
            1,
            "{names:?}"
        );
    }

    /// 清单本身同一 id 出现两次时，这一家也只广告一次（`manage_view` 早有这层兜底）。
    #[test]
    fn duplicate_manifest_id_is_advertised_once() {
        let rules = rules_with(json!([]));
        let zcode = vec![
            json!({ "id": "glm-5.3" }),
            json!({ "id": "glm-5.3" }),
            json!({ "id": "glm-5.3-flash" }),
        ];
        let names = advertised_names(&rules, "zcode", &zcode);
        assert_eq!(
            names.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["glm-5.3", "glm-5.3-flash"],
            "{names:?}"
        );
    }

    /// 带 `provider/` 前缀的请求名要按裸名一起判；只拆已登记的 provider id。
    #[test]
    fn request_name_variants_only_strip_registered_provider_prefix() {
        let variants = request_name_variants("zcode/glm-5.3-flash");
        assert_eq!(
            variants.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["zcode/glm-5.3-flash", "glm-5.3-flash"],
        );
        // 真实模型名里的斜杠：head 不是已登记的 provider id，不能被切开
        let real_name = request_name_variants("deepseek/deepseek-chat");
        assert_eq!(
            real_name.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["deepseek/deepseek-chat"],
        );
        assert_eq!(
            request_name_variants("glm-5.3").iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["glm-5.3"],
        );
        assert!(request_name_variants("  ").is_empty());
    }

    /// 带前缀名的等价集合要含裸名、以及裸名自己的别名（入口门禁据此放行）。
    #[test]
    fn prefixed_name_expands_to_bare_name_and_its_alias() {
        let rules = rules_with(json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon", "enabled": true }
        ]));
        let names = synonyms_expanded(&rules, &[], "zcode/sn-glm-5-3");
        assert!(names.iter().any(|n| n == "zcode/sn-glm-5-3"), "{names:?}");
        assert!(names.iter().any(|n| n == "sn-glm-5-3"), "裸名必须在: {names:?}");
        assert!(names.iter().any(|n| n == "glm-5.3"), "裸名的别名也要展开: {names:?}");
    }
}
