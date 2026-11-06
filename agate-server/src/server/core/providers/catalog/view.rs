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

use super::routing::{builtin_target, entry_id_in_manifest};
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
/// 这个名字」。判据见 [`renamed_away_pairs`]（要求别名在本家**解析得回同一个
/// 上游 id**，否则宁可不隐藏，避免模型从清单里彻底消失）。
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
    let mut names: Vec<String> = manifest.iter()
        .map(model_id)
        .filter(|id| {
            !renamed.iter().any(|(hidden, _)| hidden.eq_ignore_ascii_case(id))
        })
        .collect();
    names.extend(rules.mappings.iter()
        .filter(|mapping| mapping.enabled)
        .filter(|mapping| mapping.provider.as_deref().map_or(true, |owner| owner == provider))
        .map(|mapping| mapping.alias.clone()));
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
            Some(set) => set.contains(&model_id(item).to_lowercase()),
        })
        .map(|(_, item)| item)
        .collect();
    let builtin_count = data.len();
    super::super::custom::append_models_response(store, scope, &mut data);
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
    let mut out = synonyms_in(&model_rules::current(), name);
    for provider in crate::server::core::custom_providers::list() {
        for synonym in crate::server::core::custom_providers::synonymous_models(&provider, name) {
            if !out.iter().any(|n| n.eq_ignore_ascii_case(&synonym)) {
                out.push(synonym);
            }
        }
    }
    out
}

/// [`synonymous_names`] 的实质（规则快照由调用方给：清单出口已经读过一遍配置）。
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

/// 每条原始 ID 都返回一个默认绑定；历史的同名映射合并进默认绑定，避免两个开关
/// 控制同一个请求名。默认绑定不可删除，关闭不影响这一行的其他别名。
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
    // 是「清单的可视检查」，同 id 画出两行会连带下面那条默认映射也重复一条。
    // 这一层独立于源头：不管哪家目录出了脏数据，这里只认第一次出现的那条。
    let mut claimed: HashSet<(String, String)> = HashSet::new();
    for (kind, mut manifest) in active_manifests(store) {
        let provider = kind_id(kind);
        manifest.sort_by_key(|item| !rules.default_enabled(provider, &model_id(item)));
        // 一次取「远程与否 + 拉取时刻」：两者是同一处状态，分两次取会多读一次锁
        let (remote, refreshed_at) = refresh_meta(kind);
        let source = if remote { "remote" } else { "builtin" };
        for item in manifest {
            let id = model_id(&item);
            if id.is_empty() { continue; }
            if !claimed.insert((provider.to_string(), id.to_lowercase())) {
                continue; // 同家同 id 的重复条目（见 claimed 的说明）
            }
            let default = rules.binding(provider, &id, &id);
            let enabled = rules.default_enabled(provider, &id);
            mappings.push(json!({
                "alias": id, "target": id, "provider": provider, "enabled": enabled,
                "reasoning": default.and_then(|mapping| mapping.reasoning.clone()),
                "isDefault": true, "dangling": false, "carried": true,
            }));
            let mut aliases = Vec::new();
            for (index, mapping) in rules.mappings.iter().enumerate() {
                if !mapping.target.eq_ignore_ascii_case(&id)
                    || !mapping.provider.as_deref().map_or(true, |owner| owner == provider)
                { continue; }
                attached.insert(index);
                if mapping.alias.eq_ignore_ascii_case(&id)
                    || aliases.iter().any(|alias: &String| alias.eq_ignore_ascii_case(&mapping.alias))
                { continue; }
                let Some(effective) = rules.binding(provider, &mapping.alias, &id) else { continue };
                aliases.push(mapping.alias.clone());
                mappings.push(json!({
                    "alias": effective.alias, "target": id, "provider": provider,
                    "enabled": effective.enabled, "reasoning": effective.reasoning,
                    "isDefault": false, "dangling": false, "carried": true,
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
                "capabilities": capability::effective(&item),
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
                // 与上面 `mappings[].reasoning`（用户给某条映射**指定**的档位）
                // 是两件事：这里回答「这个模型支持哪些档」，那里回答「这条映射
                // 用哪一档」。缺失（null）= 该模型未声明，界面据此回落通用档位表。
                "reasoningLevels": item.get("reasoningLevels"),
                "reasoningDefaultLevel": item.get("reasoningDefaultLevel"),
                // 这一家的清单是什么时候拉到的（毫秒；0 = 从未成功拉过）。
                // 缓存恢复的清单与刚拉的清单**都是 `remote`**，时效只能靠这个
                // 时间戳说明（见 `providers::catalog_cache` 的模块头）。
                "refreshedAt": refreshed_at,
            }));
        }
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
    for (index, mapping) in rules.mappings.iter().enumerate() {
        if attached.contains(&index) { continue; }
        let carried = match mapping.provider.as_deref() {
            Some(provider) => kind_from_id(provider).is_some_and(|kind| {
                entry_id_in_manifest(&manifest_for(kind), &mapping.target).is_some()
            }),
            None => !super::providers_for_model(&mapping.target).is_empty(),
        };
        mappings.push(json!({
            "alias": mapping.alias, "target": mapping.target, "provider": mapping.provider,
            "enabled": mapping.enabled, "reasoning": mapping.reasoning,
            "isDefault": mapping.alias.eq_ignore_ascii_case(&mapping.target),
            "dangling": true, "carried": carried,
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

    /// 别名与本家**另一个原生 id** 撞名 → 不隐藏（否则那个名字无路可走）
    #[test]
    fn alias_colliding_with_another_native_id_does_not_hide_the_original() {
        let rules = rules_with(json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon", "enabled": true }
        ]));
        let manifest = vec![json!({ "id": "sn-glm-5-3" }), json!({ "id": "glm-5.3" })];
        let names = advertised_names(&rules, "raccoon", &manifest);
        assert!(names.iter().any(|name| name == "glm-5.3"));
        assert!(
            names.iter().any(|name| name == "sn-glm-5-3"),
            "别名被原生 id 占用时不能隐藏原名: {names:?}"
        );
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
}
