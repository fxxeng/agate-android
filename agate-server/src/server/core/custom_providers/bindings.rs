//! 自定义提供商的对外绑定；原始 ID 和别名各自控制可见性及路由。

use serde_json::{json, Value};

use crate::server::core::capability;
use crate::server::core::model_meta;
use crate::server::core::models::model_id;

pub fn resolve(provider: &Value, requested: &str) -> Option<(String, Option<String>)> {
    if !provider
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let models = provider.get("models").and_then(Value::as_array)?;
    let mappings = provider
        .get("mappings")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let level = |entry: &Value| {
        entry
            .get("reasoning")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    if let Some(model) = models
        .iter()
        .find(|model| model_id(model).eq_ignore_ascii_case(requested))
    {
        let default = mappings.iter().find(|mapping| {
            mapping
                .get("alias")
                .and_then(Value::as_str)
                .is_some_and(|alias| alias.eq_ignore_ascii_case(requested))
                && mapping
                    .get("target")
                    .and_then(Value::as_str)
                    .is_some_and(|target| target.eq_ignore_ascii_case(requested))
        });
        if model
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true)
            && default.map_or(true, |mapping| {
                mapping
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
            })
        {
            return Some((
                model_id(model),
                default.and_then(level).or_else(|| level(model)),
            ));
        }
    }
    mappings.iter().find_map(|mapping| {
        let alias = mapping.get("alias")?.as_str()?;
        let target = mapping.get("target")?.as_str()?;
        if !alias.eq_ignore_ascii_case(requested)
            || alias.eq_ignore_ascii_case(target)
            || !mapping
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        {
            return None;
        }
        // 关掉的模型不该因为有了别名而重新出现：别名是这一行的另一个名字，
        // 不是绕过启停的后门。与内置家 catalog::routing::builtin_target 同一判据。
        let model = models
            .iter()
            .find(|model| model_id(model).eq_ignore_ascii_case(target))
            .filter(|model| {
                model
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
            })?;
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
pub fn public_models(provider: &Value) -> Vec<Value> {
    let models = provider
        .get("models")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut names = Vec::new();
    names.extend(models.iter().map(model_id));
    if let Some(mappings) = provider.get("mappings").and_then(Value::as_array) {
        names.extend(mappings.iter().filter_map(|mapping| {
            mapping
                .get("alias")
                .and_then(Value::as_str)
                .map(str::to_string)
        }));
    }
    let mut result = Vec::new();
    for name in names {
        if name.is_empty()
            || result
                .iter()
                .any(|model| model_id(model).eq_ignore_ascii_case(&name))
        {
            continue;
        }
        let Some((target, _)) = resolve(provider, &name) else {
            continue;
        };
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

#[cfg(test)]
mod tests {
    use super::resolve;
    use serde_json::json;

    /// 关掉的模型不该因为有了别名而重新解析得通（与内置家同一判据）：
    /// 别名是这一行的另一个名字，不是绕过启停的后门。
    #[test]
    fn alias_of_a_disabled_model_does_not_resolve() {
        let p = json!({
            "id": "custom-x", "enabled": true,
            "models": [{ "id": "sn-kimi-k8", "enabled": false }, { "id": "keep-me" }],
            "mappings": [{ "alias": "kimi-k8", "target": "sn-kimi-k8", "enabled": true }],
        });
        assert!(
            resolve(&p, "kimi-k8").is_none(),
            "模型已关，别名不该还能解析"
        );
        assert!(resolve(&p, "sn-kimi-k8").is_none(), "原名同样不可用");
        assert!(resolve(&p, "keep-me").is_some(), "未关闭的模型仍可用");
    }
}
