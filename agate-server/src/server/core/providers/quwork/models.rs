//! QuWork 模型目录：内置 `models.json` 静态兜底 + 远程刷新 + 进程内缓存。
//!
//! ── 远程配置是安全边界，不是「拉到就用」────────────────────────
//! 上游 `models.rs::validate` 的道理照搬：模型网关的地址写在远程 JSON 里，
//! 一旦被换成别的 host，转发会把 llmToken 发给未确认的地址。因此必须校验
//! `key` / `schemaVersion` / `providers[id=qurwork].api` / `baseURL`，以及模型
//! id 去重、条数上限；**任何一项不通过都不替换原目录**（照上游 `refresh`）。
//!
//! 本仓用 `OnceLock<Mutex<Option<Catalog>>>` 做进程内缓存即可（上游用
//! `settings_get/settings_set` 落盘），TTL 10 分钟与上游一致。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::server::core::providers::adapter::ModelRefreshOutcome;

use super::{client, endpoints};

#[derive(Clone)]
pub struct Catalog {
    pub config: Value,
    pub fetched_at: i64,
}

fn memory() -> &'static Mutex<Option<Catalog>> {
    static CACHE: OnceLock<Mutex<Option<Catalog>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn cached() -> Option<Catalog> {
    memory()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn save(catalog: Catalog) {
    *memory()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(catalog);
}

/// 内置兜底（10 个模型，逐字抄上游 `models.json`）
pub fn fallback() -> Catalog {
    Catalog {
        config: serde_json::from_str(include_str!("models.json")).unwrap_or(Value::Null),
        fetched_at: 0,
    }
}

/// 校验远程模型配置（`models.rs::validate` 的判据，**纯函数**）。
///
/// 失败时调用方保留原目录、绝不向未确认的地址发凭证。
pub fn validate(config: &Value) -> Result<(), String> {
    if config.get("key").and_then(Value::as_str) != Some(endpoints::MODEL_CONFIG_KEY)
        || config.get("schemaVersion").and_then(Value::as_u64) != Some(endpoints::MODEL_SCHEMA_VERSION)
        || config.get("enabled").and_then(Value::as_bool).is_none()
    {
        return Err("QuWork 模型配置格式不兼容".into());
    }
    let providers = config
        .get("providers")
        .and_then(Value::as_array)
        .filter(|items| items.len() <= 8)
        .ok_or("QuWork 模型提供商配置异常")?;
    let mut ids = HashSet::new();
    for provider in providers {
        if provider.get("id").and_then(Value::as_str) != Some(endpoints::MODEL_PROVIDER_ID) {
            continue;
        }
        if provider.get("api").and_then(Value::as_str) != Some(endpoints::MODEL_API) {
            return Err("QuWork 模型接口协议已变化，请更新网关".into());
        }
        let base_url = provider
            .get("baseURL")
            .and_then(Value::as_str)
            .map(|url| url.trim_end_matches('/'))
            .unwrap_or("");
        if !endpoints::MODEL_ORIGINS.contains(&base_url) {
            return Err(format!(
                "QuWork 模型服务地址已变更为 {base_url}（不在已确认的白名单内），\
                 未向未确认的地址发送凭证。已知地址：{}",
                endpoints::MODEL_ORIGINS.join(" / ")
            ));
        }
        let rows = provider
            .get("models")
            .and_then(Value::as_array)
            .filter(|rows| rows.len() <= 128)
            .ok_or("QuWork 模型清单格式异常")?;
        for row in rows {
            let id = row
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
                .ok_or("QuWork 模型名称格式异常")?;
            if !ids.insert(id) {
                return Err("QuWork 模型清单存在重复名称".into());
            }
            if row.get("enabled").and_then(Value::as_bool).is_none() {
                return Err("QuWork 模型启用状态不明确".into());
            }
        }
    }
    if !providers
        .iter()
        .any(|provider| provider.get("id").and_then(Value::as_str) == Some(endpoints::MODEL_PROVIDER_ID))
    {
        return Err("QuWork 模型配置未包含模型提供商".into());
    }
    Ok(())
}

/// 当前目录（进程缓存优先，没有则内置兜底）
pub fn current() -> Catalog {
    cached().unwrap_or_else(fallback)
}

/// 当前生效的模型网关地址（**转发唯一的取址入口**）。
///
/// 取「远程配置里那台已校验过的网关」，而不是常量：官方换域时只要地址落在
/// `MODEL_ORIGINS` 白名单里就照它的走，不必再改代码发一版。拖不到配置 / 配置里
/// 的地址不在白名单（理论上已被 `validate` 挡掉）都回落白名单第一项。
pub fn model_origin() -> String {
    current()
        .config
        .get("providers")
        .and_then(Value::as_array)
        .and_then(|providers| {
            providers.iter().find(|provider| {
                provider.get("id").and_then(Value::as_str) == Some(endpoints::MODEL_PROVIDER_ID)
            })
        })
        .and_then(|provider| provider.get("baseURL").and_then(Value::as_str))
        .map(|url| url.trim_end_matches('/'))
        .filter(|url| endpoints::MODEL_ORIGINS.contains(url))
        .map(str::to_string)
        .unwrap_or_else(|| endpoints::MODEL_ORIGIN.to_string())
}

/// 是否真的从远程拉到过（供 `catalog::refresh_meta` 的「来源」列）
pub fn remote_refreshed() -> bool {
    cached().map(|catalog| catalog.fetched_at > 0).unwrap_or(false)
}

/// 上次远程拉取时刻（没拉过给 0）
pub fn last_refreshed_at() -> i64 {
    cached().map(|catalog| catalog.fetched_at).unwrap_or(0)
}

impl Catalog {
    /// 启用的 qurwork 模型行（按 sort 排序）
    pub fn rows(&self) -> Vec<&Value> {
        if self.config.get("enabled").and_then(Value::as_bool) != Some(true) {
            return Vec::new();
        }
        let mut rows: Vec<&Value> = self
            .config
            .get("providers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|provider| {
                provider.get("id").and_then(Value::as_str) == Some(endpoints::MODEL_PROVIDER_ID)
            })
            .flat_map(|provider| {
                provider
                    .get("models")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter(|model| model.get("enabled").and_then(Value::as_bool) == Some(true))
            .collect();
        rows.sort_by_key(|row| row.get("sort").and_then(Value::as_i64).unwrap_or(i64::MAX));
        rows
    }

    /// 按 id 取原始模型行（含 compat / reasoningEfforts，供 `chat` 用）
    pub fn model(&self, id: &str) -> Option<&Value> {
        self.rows()
            .into_iter()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(id))
    }

    /// 聚合层认的清单条目形状
    pub fn list(&self) -> Vec<Value> {
        let default = self
            .config
            .pointer("/defaultModel/modelId")
            .and_then(Value::as_str)
            .unwrap_or("");
        self.rows()
            .into_iter()
            .map(|row| {
                let id = row.get("id").and_then(Value::as_str).unwrap_or("");
                let efforts = row.get("reasoningEfforts").and_then(Value::as_object);
                let mut model = json!({
                    "id": id,
                    "name": row.get("name").and_then(Value::as_str).unwrap_or(id),
                    "supportsToolCall": true,
                    "supportsImages": row.get("supportsImage").and_then(Value::as_bool).unwrap_or(false),
                    "isDefault": id == default,
                });
                for (target, source) in [
                    ("maxInputTokens", "contextWindow"),
                    ("maxOutputTokens", "maxTokens"),
                ] {
                    if let Some(value) = row
                        .get(source)
                        .and_then(Value::as_u64)
                        .filter(|value| *value > 0)
                    {
                        model[target] = json!(value);
                    }
                }
                if let Some(efforts) = efforts {
                    model["supportsReasoning"] = json!(true);
                    model["reasoningLevels"] = json!(efforts.keys().collect::<Vec<_>>());
                }
                model
            })
            .collect()
    }
}

/// 内置默认清单（注册适配器时用，也是远程刷新失败时的兜底）
pub fn default_models() -> Vec<Value> {
    fallback().list()
}

/// 刷新模型目录。`force = false`（自动路径）TTL 内早退；`force = true` 真打上游。
///
/// 校验失败 / 空清单 / 网络失败一律 `failed(...)` 且**不动原缓存**。
pub async fn refresh(force: bool) -> ModelRefreshOutcome {
    let catalog = current();
    let now = crate::server::logging::now_ms();
    if !force
        && catalog.fetched_at > 0
        && now.saturating_sub(catalog.fetched_at) < endpoints::CATALOG_TTL_MS
    {
        return if catalog.list().is_empty() {
            ModelRefreshOutcome::failed("QuWork 当前未开放可用模型")
        } else {
            ModelRefreshOutcome::unchanged()
        };
    }
    let reply = match client::public_config().await {
        Ok(reply) => reply,
        Err(error) => return ModelRefreshOutcome::failed(error.message),
    };
    if !(200..300).contains(&reply.status) {
        return ModelRefreshOutcome::failed(format!(
            "QuWork 模型目录返回 HTTP {}（保留现有清单）",
            reply.status
        ));
    }
    if let Err(reason) = validate(&reply.body) {
        return ModelRefreshOutcome::failed(format!("{reason}（保留现有清单）"));
    }
    let next = Catalog {
        config: reply.body,
        fetched_at: now,
    };
    let count = next.list().len();
    if count == 0 {
        return ModelRefreshOutcome::failed(
            "QuWork 当前未开放可用模型，未替换原目录".to_string(),
        );
    }
    save(next);
    ModelRefreshOutcome::refreshed(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> Value {
        serde_json::from_str(include_str!("models.json")).unwrap_or(Value::Null)
    }

    #[test]
    fn validate_accepts_the_builtin_catalog() {
        assert!(validate(&valid_config()).is_ok());
    }

    #[test]
    fn validate_accepts_every_whitelisted_origin() {
        // 官方 2026-11 的配置把 baseURL 从 qurwork-llm.rrzu.com/v1 换成了
        // llm.quwork.cn/v1。白名单必须两个都收，否则「提取模型」报「服务地址
        // 已变化」而界面上只是刷不出模型（真机反馈就是这个）。
        for origin in endpoints::MODEL_ORIGINS {
            let mut config = valid_config();
            config["providers"][0]["baseURL"] = json!(origin);
            assert!(validate(&config).is_ok(), "白名单地址被拒：{origin}");
        }
    }

    #[test]
    fn model_origin_always_lands_on_a_whitelisted_gateway() {
        // 转发取址只能落在已确认的域上：内置目录（旧域）与远程配置（新域）都算
        let origin = model_origin();
        assert!(
            endpoints::MODEL_ORIGINS.contains(&origin.as_str()),
            "实际取到：{origin}"
        );
    }

    #[test]
    fn validate_rejects_a_foreign_base_url() {
        let mut config = valid_config();
        config["providers"][0]["baseURL"] = json!("https://evil.example.com/v1");
        let err = validate(&config).err().unwrap_or_default();
        assert!(err.contains("地址"));
    }

    #[test]
    fn validate_rejects_duplicate_model_ids() {
        let mut config = valid_config();
        let first = config["providers"][0]["models"][0].clone();
        if let Some(models) = config["providers"][0]["models"].as_array_mut() {
            models.push(first);
        }
        assert!(validate(&config).is_err());
    }

    #[test]
    fn validate_rejects_bad_schema_and_api() {
        let mut config = valid_config();
        config["schemaVersion"] = json!(2);
        assert!(validate(&config).is_err());
        let mut config = valid_config();
        config["providers"][0]["api"] = json!("responses");
        assert!(validate(&config).is_err());
    }

    #[test]
    fn fallback_exposes_ten_models_with_capability_fields() {
        let list = default_models();
        assert_eq!(list.len(), 10);
        let first = &list[0];
        assert!(first.get("id").is_some());
        assert!(first.get("name").is_some());
        assert_eq!(first.get("supportsToolCall"), Some(&Value::Bool(true)));
        assert!(first.get("maxInputTokens").is_some());
        assert!(first.get("maxOutputTokens").is_some());
        let deepseek = list
            .iter()
            .find(|item| item.get("id").and_then(Value::as_str) == Some("deepseek-flash"));
        assert!(deepseek.is_some());
        assert!(deepseek.and_then(|v| v.get("reasoningLevels")).is_some());
    }
}
