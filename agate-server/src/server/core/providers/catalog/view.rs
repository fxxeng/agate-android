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
fn advertised_names(
    rules: &model_rules::ModelRules,
    provider: &str,
    manifest: &[Value],
) -> Vec<String> {
    let mut names: Vec<String> = manifest.iter().map(model_id).collect();
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
    let pinned = |provider: &str| {
        pins.iter().any(|pin| pin.eq_ignore_ascii_case(provider))
    };
    for (kind, manifest) in active {
        let provider = kind_id(*kind);
        let names = advertised_names(&rules, provider, manifest);
        // 这一家要不要额外带前缀：`provider` 模式全带；`hybrid` 只带被 pin 的；
        // `model` 模式不带（同名只留一条，用哪家由路由队列决定）。
        let prefixed = match mode {
            ListMode::Provider => true,
            ListMode::Hybrid => pinned(provider),
            ListMode::Model => false,
        };
        for name in names {
            let Some(wire) = builtin_target(&rules, *kind, manifest, &name) else { continue };
            // 未去重：所有模式都按「对外名」去重 —— 同名模型只该出现一次，
            // 除非这一家要求带前缀（前缀形态天然不与裸名冲突，所以两种都出）。
            if !claimed.insert(name.to_lowercase()) && !prefixed {
                continue;
            }
            let Some(upstream) = manifest.iter().find(|item| model_id(item) == wire.model) else { continue };
            let mut item = list_item(upstream, provider);
            let wire_name = if prefixed {
                format!("{provider}/{name}")
            } else {
                name.clone()
            };
            if let Some(object) = item.as_object_mut() {
                object.insert("id".to_string(), Value::String(wire_name.clone()));
                if !wire_name.eq_ignore_ascii_case(&wire.model) {
                    object.insert("name".to_string(), Value::String(wire_name));
                    object.insert("is_default".to_string(), Value::Bool(false));
                }
            }
            result.push((provider.to_string(), item));
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
    let mut data: Vec<_> = public_builtin_items(&active, mode, pins).into_iter()
        .map(|(_, item)| item)
        .filter(|item| key_scope::allows_model(scope, &model_id(item)))
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
        let mut names: Vec<_> = public_builtin_items(&[(kind, manifest)]).into_iter()
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
                "source": source, "enabled": enabled, "aliases": aliases,
                "capabilities": capability::effective(&item),
                "capOverrides": cap_overrides,
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
}
