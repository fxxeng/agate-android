//! 对外模型名到上游模型的解析。路由与发送侧共用本解析，避免选中一家后再换一套规则。

use serde_json::Value;

use crate::server::core::models::model_id;
use crate::server::core::providers::{kind_from_id, kind_id, ProviderKind};
use crate::server::core::{custom_providers, model_rules};

use super::{adapter_for, all_kinds, manifest_for};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WireTarget {
    pub model: String,
    pub reasoning: Option<String>,
}

pub(super) fn entry_id_in_manifest(manifest: &[Value], requested: &str) -> Option<String> {
    let requested = requested.trim();
    if requested.is_empty() {
        return None;
    }
    // 保留展示名兼容，但必须先匹配原始 ID；返回清单里的实际大小写。
    manifest
        .iter()
        .find(|entry| model_id(entry).eq_ignore_ascii_case(requested))
        .or_else(|| {
            manifest.iter().find(|entry| {
                entry
                    .get("name")
                    .map(crate::server::core::models::shape_value_text)
                    .is_some_and(|name| name.eq_ignore_ascii_case(requested))
            })
        })
        .map(model_id)
}

pub(super) fn builtin_target(
    rules: &model_rules::ModelRules,
    kind: ProviderKind,
    manifest: &[Value],
    requested: &str,
) -> Option<WireTarget> {
    let provider = kind_id(kind);
    if let Some(id) = entry_id_in_manifest(manifest, requested) {
        if rules.default_enabled(provider, &id) {
            return Some(WireTarget {
                reasoning: rules
                    .binding(provider, &id, &id)
                    .and_then(|m| m.reasoning.clone()),
                model: id,
            });
        }
    }
    let candidates: Vec<_> = rules
        .mappings
        .iter()
        .filter(|mapping| mapping.alias.eq_ignore_ascii_case(requested))
        .filter(|mapping| !mapping.alias.eq_ignore_ascii_case(&mapping.target))
        .filter(|mapping| {
            mapping
                .provider
                .as_deref()
                .map_or(true, |id| id == provider)
        })
        .filter(|mapping| {
            rules
                .binding(provider, &mapping.alias, &mapping.target)
                .is_some_and(|effective| effective.enabled)
        })
        .filter_map(|mapping| {
            // ── 目标在这家被关掉 → 别名跟着失效 ────────────────────
            // 别名是「这一行的另一个名字」，不是绕过启停的后门。判据与上面
            // 按原生 id 命中那条同源（default_enabled 同时覆盖 disabled
            // 与恒等映射的开关），否则模型页关掉的模型仍能经别名被广告/派单。
            entry_id_in_manifest(manifest, &mapping.target)
                .filter(|target| rules.default_enabled(provider, target))
                .map(|target| (mapping, target))
        })
        .collect();
    let prefix = super::super::cline::models::Pool::from_provider_id(provider)
        .map(super::super::cline::models::Pool::target_prefix);
    let selected = candidates
        .iter()
        .filter(|(mapping, _)| mapping.provider.is_some())
        .find(|(_, target)| prefix.is_some_and(|prefix| target.starts_with(prefix)))
        .or_else(|| {
            candidates
                .iter()
                .find(|(mapping, _)| mapping.provider.is_some())
        })
        .or_else(|| candidates.first());
    selected.map(|(mapping, target)| WireTarget {
        model: target.clone(),
        reasoning: rules
            .binding(provider, &mapping.alias, &mapping.target)
            .and_then(|effective| effective.reasoning.clone()),
    })
}

/// 这一家里「已被生效别名取代」的 `(原生 id, 别名)` 对。
///
/// 解析侧映射是「追加一个可用名」：原名照旧能调（见 [`builtin_target`] 第一段）。
/// 但在清单里两个都列并不是用户要的 —— 建映射的动机是「我这边只叫这个名字」。
/// 返回的是「该隐去哪条原名」，调用方**只从清单里删掉它**：老客户端 / 老 Key
/// 白名单里写着的原名照旧打得通。
///
/// ── 三条判据（与自定义家 `bindings::renamed_away_targets` 同源）─────
///   1. 有一条 **enabled** 映射，`target` 是这家的原生 id 且 `alias != target`；
///   2. `target` 在这家**没被关掉**（关掉的模型本来就不该出现在清单里）；
///   3. 那条别名在这家**至少能解析出某个上游模型**（[`builtin_target`] 命中）。
///
/// 第 3 条**不要求解析回 target 本身**：别名撞上这家另一个原生 id 时（映射更早
/// 种下、上游后来新增同名模型），解析落到那条原生条目上，此时那个名字仍在清单
/// 里，隐去原名不会让模型消失；真正要防的只有「别名解析不出任何东西」（target
/// 被换名 / 下架）—— 那时隐去原名会让模型从清单里彻底消失。
///
/// 返回成对而不是只给原名集合：调用方还要回答「这条别名替谁顶了名」。
pub(super) fn renamed_away_pairs(
    rules: &model_rules::ModelRules,
    kind: ProviderKind,
    manifest: &[Value],
) -> Vec<(String, String)> {
    let provider = kind_id(kind);
    let mut out: Vec<(String, String)> = Vec::new();
    for mapping in rules.mappings.iter() {
        if !mapping.enabled || mapping.alias.eq_ignore_ascii_case(&mapping.target) {
            continue;
        }
        if !mapping
            .provider
            .as_deref()
            .map_or(true, |owner| owner.eq_ignore_ascii_case(provider))
        {
            continue;
        }
        // `target` 必须是这家的原生 id（不是别家的、也不是另一个别名）
        if !manifest
            .iter()
            .any(|item| model_id(item).eq_ignore_ascii_case(&mapping.target))
        {
            continue;
        }
        // target 自己也被关掉就不必谈「改名」：它本来就不该在清单里
        if !rules.default_enabled(provider, &mapping.target) {
            continue;
        }
        // 别名必须至少能解析出某个上游模型 —— 不要求是 target 本身（撞名时
        // 那个名字仍是清单里的一条，隐去原名不会让模型消失）。
        if builtin_target(rules, kind, manifest, &mapping.alias).is_none() {
            continue;
        }
        out.push((mapping.target.clone(), mapping.alias.clone()));
    }
    out
}

/// 原始能力查询不应用开关，供管理 API 展开旧版全局规则。
pub fn providers_for_model(model: &str) -> Vec<String> {
    let model = model.trim();
    if model.is_empty() {
        return Vec::new();
    }
    let manifests: Vec<_> = all_kinds()
        .into_iter()
        .map(|kind| (kind, manifest_for(kind)))
        .collect();
    let exact = manifests.iter().any(|(_, items)| {
        items
            .iter()
            .any(|item| model_id(item).eq_ignore_ascii_case(model))
    });
    let mut result: Vec<_> = manifests
        .into_iter()
        .filter(|(_, items)| {
            if exact {
                items
                    .iter()
                    .any(|item| model_id(item).eq_ignore_ascii_case(model))
            } else {
                entry_id_in_manifest(items, model).is_some()
            }
        })
        .map(|(kind, _)| kind_id(kind).to_string())
        .collect();
    result.extend(custom_providers::list().into_iter().filter_map(|provider| {
        let models = provider.get("models")?.as_array()?;
        models
            .iter()
            .any(|entry| model_id(entry).eq_ignore_ascii_case(model))
            .then(|| {
                provider
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            })
    }));
    result
}

/// 只返回有开启绑定的提供商，不把关闭的 ID 回落给默认提供商。
pub fn forwarding_providers(model: &str) -> Vec<String> {
    let rules = model_rules::current();
    let mut result: Vec<_> = all_kinds()
        .into_iter()
        .filter(|kind| builtin_target(&rules, *kind, &manifest_for(*kind), model).is_some())
        .map(|kind| kind_id(kind).to_string())
        .collect();
    result.extend(custom_providers::carriers_of_model(model));
    result
}

pub fn model_blocked_everywhere(model: &str) -> bool {
    if !forwarding_providers(model).is_empty() {
        return false;
    }
    !providers_for_model(model).is_empty()
        || model_rules::current().has_alias(model)
        || custom_providers::list().iter().any(|provider| {
            provider
                .get("mappings")
                .and_then(Value::as_array)
                .is_some_and(|mappings| {
                    mappings.iter().any(|mapping| {
                        mapping
                            .get("alias")
                            .and_then(Value::as_str)
                            .is_some_and(|alias| alias.eq_ignore_ascii_case(model))
                    })
                })
        })
}

/// 把对外带前缀名拆成 `(provider_id, 裸名)`。
///
/// 只认**已登记的 provider id**：`deepseek/deepseek-chat` 这类真实模型名不会命中
/// （head 不是 provider id），所以可以放心在转发入口先用它试一次。
///
/// 这是清单里「`provider/model` 锁定这一家」那条语义的**请求侧**实现 ——
/// 没有它，候选家只能拿完整名字去目录里查，一个也查不到，于是回落默认家：
/// 名字对了、家却可能错（多提供商有同名模型时尤其危险）。
pub fn split_provider_prefix(model: &str) -> Option<(String, String)> {
    let text = model.trim();
    let (head, rest) = text.split_once('/')?;
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    if kind_from_id(head).is_none() {
        return None;
    }
    Some((head.to_string(), rest.to_string()))
}

pub fn wire_target_for_provider(
    model: &str,
    provider_id: &str,
    _account: Option<&Value>,
) -> WireTarget {
    let requested = model.trim();
    // 对外带前缀名（`provider/model`）：前缀是清单里的「锁家」写法，**上游不认识**
    // —— 原样发出去会被上游拒。实测：`workbuddy/glm-5.3-flash` →
    // 上游 400 `model [workbuddy/glm-5.3-flash] service info not found`。
    //
    // 只在 head **与目标提供商一致** 时剥：真实模型名里也可能带 `/`
    // （OpenRouter 的 `deepseek/deepseek-chat`），见斜杠就切会把它们切坏。
    let requested = match requested.split_once('/') {
        Some((head, rest)) if !rest.is_empty() && head.eq_ignore_ascii_case(provider_id) => rest,
        _ => requested,
    };
    // 自定义提供商在自己的转发模块解析一次，避免把别名提前改写后丢失其等级绑定。
    let resolved = kind_from_id(provider_id).and_then(|kind| {
        builtin_target(
            &model_rules::current(),
            kind,
            &manifest_for(kind),
            requested,
        )
    });
    resolved.unwrap_or_else(|| WireTarget {
        model: requested.to_string(),
        reasoning: None,
    })
}

pub fn default_model_catalog() -> Vec<Value> {
    let rules = model_rules::current();
    let mut result = Vec::new();
    for kind in all_kinds()
        .into_iter()
        .filter(|kind| adapter_for(*kind).supports_default_model())
    {
        for entry in manifest_for(kind) {
            let id = model_id(&entry);
            if !id.is_empty()
                && rules.default_enabled(kind_id(kind), &id)
                && !result
                    .iter()
                    .any(|item| model_id(item).eq_ignore_ascii_case(&id))
            {
                result.push(entry);
            }
        }
    }
    result
}

pub fn default_model_usable(model: &str) -> bool {
    entry_id_in_manifest(&default_model_catalog(), model).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::core::model_rules::ModelRules;

    /// 构造一份只含 mappings 的规则（不碰全局 config）
    fn rules_with(mappings: Value) -> ModelRules {
        let raw = serde_json::json!({ "modelRules": { "mappings": mappings } })
            .as_object()
            .cloned()
            .unwrap_or_default();
        ModelRules::from_raw(&raw)
    }

    /// 带 disabled 段的规则（钉住「关掉的模型别名也失效」）
    fn rules_with_disabled(mappings: Value, disabled: Value) -> ModelRules {
        let raw = serde_json::json!({
            "modelRules": { "mappings": mappings, "disabled": disabled }
        })
        .as_object()
        .cloned()
        .unwrap_or_default();
        ModelRules::from_raw(&raw)
    }

    /// 上一轮修复：关掉的模型不该经别名被广告 / 派单（两条路同一判据）
    #[test]
    fn alias_of_a_disabled_target_does_not_resolve() {
        let rules = rules_with_disabled(
            serde_json::json!([
                { "alias": "kimi-k8", "target": "sn-kimi-k8", "provider": "raccoon",
                  "enabled": true },
                { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon",
                  "enabled": true }
            ]),
            serde_json::json!([{ "provider": "raccoon", "id": "sn-kimi-k8" }]),
        );
        let manifest = vec![
            serde_json::json!({ "id": "sn-kimi-k8" }),
            serde_json::json!({ "id": "sn-glm-5-3" }),
        ];
        assert!(
            builtin_target(&rules, ProviderKind::Raccoon, &manifest, "kimi-k8").is_none(),
            "模型已关，别名不该还能解析出上游 id"
        );
        assert!(
            builtin_target(&rules, ProviderKind::Raccoon, &manifest, "sn-kimi-k8").is_none(),
            "原名同样不可用：两条路必须同一判据"
        );
        assert_eq!(
            builtin_target(&rules, ProviderKind::Raccoon, &manifest, "glm-5.3")
                .map(|target| target.model),
            Some("sn-glm-5-3".to_string()),
            "没关的那条不受影响"
        );
    }

    /// 生效别名 → 原名进「让位」集合（清单出口据它把原名从 /v1/models 拿掉）
    #[test]
    fn enabled_alias_marks_the_original_as_renamed_away() {
        let rules = rules_with(serde_json::json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon",
              "enabled": true }
        ]));
        let manifest = vec![serde_json::json!({ "id": "sn-glm-5-3" })];
        let pairs = renamed_away_pairs(&rules, ProviderKind::Raccoon, &manifest);
        assert_eq!(
            pairs,
            vec![("sn-glm-5-3".to_string(), "glm-5.3".to_string())]
        );
    }

    /// 关掉的映射不让位（「关掉映射才显示原名」）
    #[test]
    fn disabled_alias_does_not_rename_away_the_original() {
        let rules = rules_with(serde_json::json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon",
              "enabled": false }
        ]));
        let manifest = vec![serde_json::json!({ "id": "sn-glm-5-3" })];
        assert!(renamed_away_pairs(&rules, ProviderKind::Raccoon, &manifest).is_empty());
    }

    /// 别家的映射不给这一家改名（provider 归属）
    #[test]
    fn other_providers_alias_does_not_rename_away_this_manifest() {
        let rules = rules_with(serde_json::json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "workbuddy",
              "enabled": true }
        ]));
        let manifest = vec![serde_json::json!({ "id": "sn-glm-5-3" })];
        assert!(renamed_away_pairs(&rules, ProviderKind::Raccoon, &manifest).is_empty());
    }

    /// 别名撞上本家另一个原生 id 时仍然让位（不再两条并列）
    #[test]
    fn alias_colliding_with_a_native_id_still_renames_the_original() {
        let rules = rules_with(serde_json::json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon",
              "enabled": true }
        ]));
        let manifest = vec![
            serde_json::json!({ "id": "glm-5.3", "name": "GLM-5.3" }),
            serde_json::json!({ "id": "sn-glm-5-3", "name": "GLM-5.3 (Sensenova)" }),
        ];
        assert_eq!(
            renamed_away_pairs(&rules, ProviderKind::Raccoon, &manifest),
            vec![("sn-glm-5-3".to_string(), "glm-5.3".to_string())],
            "别名与原生 id 撞名时也该收敛成一条"
        );
    }

    /// target 被关掉时不必「改名」：它本来就不该出现在清单里
    #[test]
    fn disabled_target_is_not_renamed_away() {
        let rules = rules_with_disabled(
            serde_json::json!([
                { "alias": "glm-5.3", "target": "sn-glm-5-3", "provider": "raccoon",
                  "enabled": true }
            ]),
            serde_json::json!([{ "provider": "raccoon", "id": "sn-glm-5-3" }]),
        );
        let manifest = vec![serde_json::json!({ "id": "sn-glm-5-3", "name": "GLM-5.3" })];
        assert!(
            renamed_away_pairs(&rules, ProviderKind::Raccoon, &manifest).is_empty(),
            "关掉的 target 不该进让位集合"
        );
    }
}
