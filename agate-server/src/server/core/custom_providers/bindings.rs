//! 自定义提供商的对外绑定；原始 ID 和别名各自控制可见性及路由。

use serde_json::{json, Value};

use crate::server::core::capability;
use crate::server::core::model_meta;
use crate::server::core::models::model_id;

pub fn resolve(provider: &Value, requested: &str) -> Option<(String, Option<String>)> {
    if !provider.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let models = provider.get("models").and_then(Value::as_array)?;
    let mappings = provider.get("mappings").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    let level = |entry: &Value| entry.get("reasoning").and_then(Value::as_str)
        .map(str::trim).filter(|text| !text.is_empty()).map(str::to_string);
    if let Some(model) = models.iter().find(|model| model_id(model).eq_ignore_ascii_case(requested)) {
        let default = mappings.iter().find(|mapping| {
            mapping.get("alias").and_then(Value::as_str).is_some_and(|alias| alias.eq_ignore_ascii_case(requested))
                && mapping.get("target").and_then(Value::as_str).is_some_and(|target| target.eq_ignore_ascii_case(requested))
        });
        if model.get("enabled").and_then(Value::as_bool).unwrap_or(true)
            && default.map_or(true, |mapping| mapping.get("enabled").and_then(Value::as_bool).unwrap_or(true))
        {
            return Some((model_id(model), default.and_then(level).or_else(|| level(model))));
        }
    }
    mappings.iter().find_map(|mapping| {
        let alias = mapping.get("alias")?.as_str()?;
        let target = mapping.get("target")?.as_str()?;
        if !alias.eq_ignore_ascii_case(requested) || alias.eq_ignore_ascii_case(target)
            || !mapping.get("enabled").and_then(Value::as_bool).unwrap_or(true)
        {
            return None;
        }
        // ── 关掉的模型不该因为有了别名而重新出现（2026-11-12）──────────
        // 别名是**这一行的另一个名字**，不是绕过启停的后门。此前这里只找
        // target 条目、不看它的 `enabled`，于是自定义家里「关掉模型」之后，
        // 它的别名照旧解析得通：清单里还在（`public_models` 以 `resolve`
        // 为门）、请求还能派给这家 —— 与内置家同一个漏洞（见
        // `catalog::routing::builtin_target`）。`renamed_away_targets` 的
        // 注释早就写着「关掉的模型不该因为有了别名而从清单里重新出现」，
        // 那条判据本来就靠这里，只是代码没跟上。
        let model = models
            .iter()
            .find(|model| model_id(model).eq_ignore_ascii_case(target))
            .filter(|model| model.get("enabled").and_then(Value::as_bool).unwrap_or(true))?;
        Some((model_id(model), level(mapping)))
    })
}

/// 该家对外的模型条目（`{id, …能力位}`）—— 清单出口与 `/v1/models` 共用。
///
/// 条目的 id 是**对外名**（原始 ID 或某个存活的别名），能力位则取自
/// **target 的登记条目**：别名的清单条目是这里现造的、没有自己的记录，
/// 而下游按对外名看到的能力必须与按原始 ID 看到的完全一致 —— 否则同一个
/// 上游模型会有两套互相打架的声明。
///
/// 能力位**平铺**在条目顶层（与各内置家清单同形，`list_item` 直接读它们）；
/// 存储里那层 `capabilities` 对象由 `capability::apply_overrides` 展开 ——
/// 归一也走它，手改数据里的坏值到不了出口。
///
/// 登记里**没写**的键由 models.dev 补（`core::model_meta::apply`）——那条路径的
/// 优先级与内置家同一条：用户登记的值（上面的 `capabilities`）> models.dev >
/// 未声明。自定义家的条目只服务对外清单与管理页的「这家有哪些模型」，
/// 它不出现在内置家的管理视图里（那套读的是 `providers::catalog::manage_view`）。
///
/// ── 改了名的原生 id 不再单列（2026-11-03，与内置家同一口径）──────
/// 内置家那条规则在 `catalog::advertised_names`（判据 `renamed_away_pairs`），
/// 自定义家的清单出口是本函数 —— 两处都是「这家对外有哪些名字」的**唯一入口**，
/// 不在这里做，自定义家就会继续把 `原id` 和 `别名` 一起报给客户端，
/// 与内置家出现两套行为。判据同样是两条：别名指向的 target 确实是这家登记的
/// 模型、且别名**解析得回**这个 target；任一条不成立就不隐藏（宁可多列一个
/// 名字，也不让模型从清单里消失）。
pub fn public_models(provider: &Value) -> Vec<Value> {
    let models = provider.get("models").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    let renamed = renamed_away_targets(provider);
    let mut names = Vec::new();
    names.extend(
        models.iter()
            .map(model_id)
            .filter(|id| !renamed.iter().any(|hidden| hidden.eq_ignore_ascii_case(id))),
    );
    if let Some(mappings) = provider.get("mappings").and_then(Value::as_array) {
        names.extend(mappings.iter().filter_map(|mapping| mapping.get("alias").and_then(Value::as_str).map(str::to_string)));
    }
    let mut result = Vec::new();
    for name in names {
        if name.is_empty()
            || result.iter().any(|model| model_id(model).eq_ignore_ascii_case(&name))
        {
            continue;
        }
        let Some((target, _)) = resolve(provider, &name) else { continue };
        let mut item = json!({ "id": name });
        let capabilities = capability::normalize_object(
            models
                .iter()
                .find(|model| model_id(model).eq_ignore_ascii_case(&target))
                .and_then(|model| model.get("capabilities")),
        );
        capability::apply_overrides(&mut item, &capabilities);
        model_meta::apply(&mut item);
        result.push(item);
    }
    result
}

/// 这家记录里与 `name` **等价**的一组对外名（含自己）。
///
/// 消费方是清单出口的白名单过滤（`providers::custom::append_models_response`）：
/// 清单按别名收敛之后，勾了原名的 Key 也得认得出这个模型 —— 见那里与
/// `catalog::synonymous_names` 的说明（内置家那一份读 `modelRules`，
/// 自定义家的映射存在**自己的记录**里，是两处不同的存储）。
pub fn synonymous(provider: &Value, name: &str) -> Vec<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = vec![trimmed.to_lowercase()];
    let Some(mappings) = provider.get("mappings").and_then(Value::as_array) else {
        return out;
    };
    for mapping in mappings {
        if !mapping.get("enabled").and_then(Value::as_bool).unwrap_or(true) {
            continue;
        }
        let Some(alias) = mapping.get("alias").and_then(Value::as_str) else { continue };
        let Some(target) = mapping.get("target").and_then(Value::as_str) else { continue };
        let other = if alias.eq_ignore_ascii_case(trimmed) {
            target
        } else if target.eq_ignore_ascii_case(trimmed) {
            alias
        } else {
            continue;
        };
        let other = other.to_lowercase();
        if !other.is_empty() && !out.contains(&other) {
            out.push(other);
        }
    }
    out
}

/// 这家「已被生效别名取代」的原生 id（对外清单里让位给别名）。
///
/// 与内置家的 `catalog::routing::renamed_away_pairs` 同判据：别名必须**解析回
/// 同一个 target**，且不能与这家另一个登记模型撞名 —— 否则隐去原名会让那个
/// 名字无路可走。别名本身是这家登记过的模型 id 时也不算改名（那是给已存在的
/// 名字加了一条同义映射）。
fn renamed_away_targets(provider: &Value) -> Vec<String> {
    let models = provider.get("models").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    let Some(mappings) = provider.get("mappings").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for mapping in mappings {
        let Some(alias) = mapping.get("alias").and_then(Value::as_str) else { continue };
        let Some(target) = mapping.get("target").and_then(Value::as_str) else { continue };
        if alias.is_empty() || target.is_empty() {
            continue;
        }
        if alias.eq_ignore_ascii_case(target) {
            continue;
        }
        if !mapping.get("enabled").and_then(Value::as_bool).unwrap_or(true) {
            continue;
        }
        // target 必须是这家登记的模型；别名撞上另一个登记 id 时不改名
        if !models.iter().any(|model| model_id(model).eq_ignore_ascii_case(target)) {
            continue;
        }
        if models.iter().any(|model| model_id(model).eq_ignore_ascii_case(alias)) {
            continue;
        }
        // 别名必须真的解析回这个 target（`resolve` 已经把「target 已被关掉」
        // 这类情形排除掉了 —— 关掉的模型不该因为有了别名而从清单里重新出现）
        match resolve(provider, alias) {
            Some((resolved, _)) if resolved.eq_ignore_ascii_case(target) => {
                if !out.iter().any(|hidden: &String| hidden.eq_ignore_ascii_case(target)) {
                    out.push(target.to_string());
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 一家挂着一条映射的自定义提供商（`enabled` 必填 —— `resolve` 要求它）
    fn provider(mappings: Value) -> Value {
        json!({
            "id": "custom-x", "enabled": true,
            "models": [{ "id": "sn-glm-5-3" }, { "id": "keep-me" }],
            "mappings": mappings,
        })
    }

    fn ids(provider: &Value) -> Vec<String> {
        public_models(provider).iter().map(model_id).collect()
    }

    /// 生效别名 ⇒ 原名不再单列，只出别名（与内置家 `advertised_names` 同口径）
    #[test]
    fn renamed_original_is_not_listed() {
        let p = provider(json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "enabled": true }
        ]));
        let ids = ids(&p);
        assert!(ids.iter().any(|id| id == "glm-5.3"), "别名必须在: {ids:?}");
        assert!(!ids.iter().any(|id| id == "sn-glm-5-3"), "原名不该再列出: {ids:?}");
        assert!(ids.iter().any(|id| id == "keep-me"), "别的模型不受影响");
    }

    /// 关掉映射 ⇒ 原名回来（「关掉才显示」）
    #[test]
    fn disabled_mapping_keeps_the_original() {
        let p = provider(json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "enabled": false }
        ]));
        let ids = ids(&p);
        assert!(ids.iter().any(|id| id == "sn-glm-5-3"));
        assert!(!ids.iter().any(|id| id == "glm-5.3"));
    }

    /// 别名与另一个登记 id 撞名 ⇒ 不改名（否则那个名字无路可走）
    #[test]
    fn alias_colliding_with_a_registered_id_keeps_the_original() {
        let p = provider(json!([
            { "alias": "keep-me", "target": "sn-glm-5-3", "enabled": true }
        ]));
        let ids = ids(&p);
        assert!(ids.iter().any(|id| id == "sn-glm-5-3"));
    }

    /// 悬空别名（target 没登记）⇒ 不改名，也不广告
    #[test]
    fn dangling_mapping_changes_nothing() {
        let p = provider(json!([
            { "alias": "gone", "target": "not-registered", "enabled": true }
        ]));
        let ids = ids(&p);
        assert_eq!(ids, vec!["sn-glm-5-3".to_string(), "keep-me".to_string()]);
    }

    /// **关掉模型 ⇒ 它的别名一起失效**（与内置家同一判据）
    ///
    /// 反例的症状：模型页把这一行关掉之后，别名还在清单里、请求还能派进来，
    /// 上游回 408 / 400 —— 用户看到的是「这家没启用的模型还在被用」。
    #[test]
    fn alias_of_a_disabled_model_does_not_resolve() {
        let p = json!({
            "id": "custom-x", "enabled": true,
            "models": [{ "id": "sn-kimi-k8", "enabled": false }, { "id": "keep-me" }],
            "mappings": [{ "alias": "kimi-k8", "target": "sn-kimi-k8", "enabled": true }],
        });
        assert!(resolve(&p, "kimi-k8").is_none(), "模型已关，别名不该还能解析");
        assert!(resolve(&p, "sn-kimi-k8").is_none(), "原名同样不可用");
        let ids = ids(&p);
        assert!(!ids.iter().any(|id| id == "kimi-k8"), "别名不该进清单: {ids:?}");
        assert!(
            !ids.iter().any(|id| id == "sn-kimi-k8"),
            "关掉的模型不该因为有了别名而回到清单: {ids:?}"
        );
        assert!(ids.iter().any(|id| id == "keep-me"));
    }

    /// 同义名双向展开（Key 白名单用）
    #[test]
    fn synonyms_expand_both_ways() {
        let p = provider(json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "enabled": true }
        ]));
        assert!(synonymous(&p, "sn-glm-5-3").contains(&"glm-5.3".to_string()));
        assert!(synonymous(&p, "glm-5.3").contains(&"sn-glm-5-3".to_string()));
        // 关掉的映射不算同义
        let off = provider(json!([
            { "alias": "glm-5.3", "target": "sn-glm-5-3", "enabled": false }
        ]));
        assert_eq!(synonymous(&off, "sn-glm-5-3"), vec!["sn-glm-5-3".to_string()]);
    }
}
