//! AStudio 模型目录：按账号实时拉取（读取不计费），进程内缓存 + 静态兜底。
//!
//! ── 上游形状 ────────────────────────────────────────────────
//! ```json
//! {"code":0,"data":{"models":[{"slug":"spark-x2.6","display_name":"Spark-X2.6",
//!   "context_window":262144,"max_output_tokens":32768,
//!   "input_modalities":["text","image"],
//!   "supported_reasoning_levels":[{"effort":"none"},{"effort":"high"}],
//!   "badge":{"text":{"en":"Free"}},"visibility":"public","supported_in_api":true}]}}
//! ```
//!
//! ── 不落盘，只做进程内缓存 ──────────────────────────────────
//! 与 kiro 同一条理由：清单**随账号等级变**（免费账号看得到 `-4b` 那条免费
//! 模型，付费账号看得到全部），落一份"全局事实"会让低等级账号看到它无权使用
//! 的模型 —— 用户选了报错，而且错在「模型不可用」，看不出真正原因。
//! 清单「少了」刷新一次就回来，「多了」回不来。所以只在内存里按账号存
//! 10 分钟，进程重启就没了。
//!
//! ── `list_any`（并集）为什么必要 ────────────────────────────
//! `ProviderAdapter::list_models()` **没有账号参数**，而这里的缓存是按账号的。
//! 与 kiro 同一取舍：并集的「多了」是一条明确的 400，「少了」是用户以为不能
//! 用。没刷过任何账号时给兜底清单（管理页不至于空白）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic；持锁不跨 await。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::server::core::proxies::ResolvedProxy;
use crate::server::core::providers::adapter::ModelRefreshOutcome;

use super::client::{self, StudioError};
use super::credentials::Credentials;

/// 缓存有效期（与上游参考实现一致）
const TTL_MS: i64 = 10 * 60 * 1000;

/// 上游没指定模型时的默认值（本家的路由模型）
pub const DEFAULT_MODEL: &str = "astronclaw-auto";

struct Catalog {
    models: Vec<Value>,
    fetched_at: i64,
}

fn cache() -> &'static Mutex<HashMap<String, Catalog>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Catalog>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 取锁执行（毒锁按内部值继续：一次 panic 不该让目录永久不可用）
fn with_cache<T>(action: impl FnOnce(&mut HashMap<String, Catalog>) -> T) -> T {
    match cache().lock() {
        Ok(mut table) => action(&mut table),
        Err(poisoned) => action(&mut poisoned.into_inner()),
    }
}

fn now_ms() -> i64 {
    crate::server::logging::now_ms()
}

/// 静态兜底（2026-11-01 的实测快照）。
///
/// 只在「还没刷过任何账号」时出场：它的作用是让管理页与模型选择器不是空白，
/// 不是假装自己知道最新清单。里面的 `maxInputTokens` 也是实测值，不是猜的。
pub fn fallback() -> Vec<Value> {
    let rows: [(&str, &str, i64, i64, bool); 7] = [
        ("astronclaw-auto", "Auto", 262_144, 32_768, false),
        ("spark-x2.6", "Spark-X2.6", 262_144, 32_768, false),
        ("spark-x2.6-4b", "Spark-X2.6-Lite", 1_048_576, 110_000, true),
        ("spark-x2.5", "Spark-X2.5", 262_144, 32_768, false),
        ("xopglm52", "GLM-5.2", 1_048_576, 32_768, false),
        (
            "xopdeepseekv4pro0813",
            "DeepSeek-V4-Pro",
            1_048_576,
            32_768,
            false,
        ),
        (
            "xopdsv4flash0731in",
            "DeepSeek-V4-Flash",
            1_048_576,
            32_768,
            false,
        ),
    ];
    rows.iter()
        .map(|(id, name, context, output, free)| {
            let mut record = json!({
                "id": id,
                "name": name,
                "maxInputTokens": context,
                "maxOutputTokens": output,
                "supportsToolCall": true,
                "supportsReasoning": true,
                "isDefault": *id == DEFAULT_MODEL,
            });
            if *free {
                record["free"] = json!(true);
            }
            record
        })
        .collect()
}

/// 上游的一条模型记录 → 网关的模型行（`None` = 这条不该出现在清单里）
fn row_of(entry: &Value) -> Option<Value> {
    let id = entry
        .get("slug")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    // 隐藏的 / 明确不给 API 用的都不列：列出来用户选了只会得到一条拒绝
    if entry.get("visibility").and_then(Value::as_str) == Some("hidden") {
        return None;
    }
    if entry.get("supported_in_api").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    let name = entry
        .get("display_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(id);
    let context = entry
        .get("context_window")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .unwrap_or(0);
    // 上游没给输出上限时用 32768（本家所有模型的公共值）：给 0 会让客户端的
    // 「最大输出」滑杆变成 0，反而更像故障
    let output = entry
        .get("max_output_tokens")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .unwrap_or(32_768);
    let images = entry
        .get("input_modalities")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.as_str().is_some_and(|text| text == "image"))
        });
    // 只要有一个非 `none` 的档位就算支持思考（全 `none` 表示这模型不思考）
    let reasoning = entry
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .is_some_and(|levels| {
            levels.iter().any(|level| {
                level
                    .get("effort")
                    .and_then(Value::as_str)
                    .is_some_and(|effort| !effort.is_empty() && effort != "none")
            })
        });
    let free = entry
        .pointer("/badge/text/en")
        .and_then(Value::as_str)
        .is_some_and(|text| text == "Free");
    let mut record = json!({
        "id": id,
        "name": name,
        "maxInputTokens": context,
        "maxOutputTokens": output,
        "supportsToolCall": true,
        "supportsReasoning": reasoning,
        "isDefault": id == DEFAULT_MODEL,
    });
    if images {
        record["supportsImages"] = json!(true);
    }
    if free {
        record["free"] = json!(true);
    }
    Some(record)
}

/// 目录响应 → 模型行（**纯函数**）
///
/// ── 为什么「一条都没有」算失败而不是成功 ────────────────────
/// 目录接口正常时至少有 `astronclaw-auto`。返回空只可能是「形状变了」或
/// 「这个账号被禁了」，此时**保留旧清单 / 用兜底**比把模型列表清空更接近真相
/// —— 清空会让管理页一条都没有，用户以为账号坏了。
pub fn parse(payload: &Value) -> Result<Vec<Value>, String> {
    let list = payload
        .pointer("/data/models")
        .and_then(Value::as_array)
        .ok_or_else(|| "AStudio 模型目录格式异常（找不到 data.models）".to_string())?;
    let mut models: Vec<Value> = list.iter().filter_map(row_of).collect();
    if models.is_empty() {
        return Err("AStudio 模型目录为空（账号可能被限制）".to_string());
    }
    // 默认模型排第一：客户端的模型选择器按顺序渲染，默认值在第一个是它的
    // 既有习惯（与 kiro 的 `auto` 同一处处置）
    models.sort_by_key(|model| {
        if model.get("isDefault").and_then(Value::as_bool) == Some(true) {
            0
        } else {
            1
        }
    });
    Ok(models)
}

/// 某个账号的清单（缓存优先，没有就给兜底）
pub fn list(account: &str) -> Vec<Value> {
    with_cache(|table| {
        table
            .get(account)
            .filter(|catalog| now_ms() - catalog.fetched_at < TTL_MS)
            .map(|catalog| catalog.models.clone())
    })
    .unwrap_or_else(fallback)
}

/// 本进程见过的所有账号清单的**并集**（去重按 `id`，保持首次出现的顺序）
///
/// 见模块头的取舍；没刷过任何账号时给兜底。
pub fn list_any() -> Vec<Value> {
    with_cache(|table| {
        let mut seen: Vec<String> = Vec::new();
        let mut models: Vec<Value> = Vec::new();
        for catalog in table.values() {
            for model in &catalog.models {
                let Some(id) = model.get("id").and_then(Value::as_str) else {
                    continue;
                };
                if seen.iter().any(|item| item == id) {
                    continue;
                }
                seen.push(id.to_string());
                models.push(model.clone());
            }
        }
        if models.is_empty() {
            fallback()
        } else {
            models
        }
    })
}

/// 有没有远程清单（任一账号刷过）
pub fn remote_refreshed() -> bool {
    with_cache(|table| !table.is_empty())
}

/// 最近一次成功刷新的时刻（0 = 从没刷过）
pub fn last_refreshed_at() -> i64 {
    with_cache(|table| {
        table
            .values()
            .map(|catalog| catalog.fetched_at)
            .max()
            .unwrap_or(0)
    })
}

/// 拉一次目录并写缓存（`force` 绕过 TTL；手动刷新走它）。
pub async fn refresh(
    credentials: &Credentials,
    account_id: &str,
    proxy: Option<&ResolvedProxy>,
    force: bool,
) -> ModelRefreshOutcome {
    if !force && with_cache(|table| {
        table
            .get(account_id)
            .is_some_and(|catalog| now_ms() - catalog.fetched_at < TTL_MS)
    }) {
        return ModelRefreshOutcome::unchanged();
    }
    let payload = match client::catalog(&credentials.key, proxy).await {
        Ok(payload) => payload,
        Err(error) => {
            // 会话失效与普通失败在这里都是「这次没刷成」，但文案要能指路：
            // 前者让用户重贴凭据，后者可能只是网络抖了一下
            let hint = if matches!(error, StudioError::Expired(_)) {
                "凭据被拒"
            } else {
                "拉取失败"
            };
            crate::server::logging::verbose(
                "[AStudio]",
                &format!("模型目录{hint}：{}", error.message()),
            );
            return ModelRefreshOutcome::failed(error.message().to_string());
        }
    };
    let models = match parse(&payload) {
        Ok(models) => models,
        Err(message) => {
            crate::server::logging::verbose("[AStudio]", &message);
            return ModelRefreshOutcome::failed(message);
        }
    };
    let count = models.len();
    with_cache(|table| {
        table.insert(
            account_id.to_string(),
            Catalog {
                models,
                fetched_at: now_ms(),
            },
        );
    });
    ModelRefreshOutcome::refreshed(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 完整目录 → 行字段齐全（含图片 / 思考 / 免费标记）
    #[test]
    fn a_full_catalog_entry_becomes_a_complete_row() {
        let payload = json!({
            "code": 0,
            "data": { "models": [{
                "slug": "spark-x2.6",
                "display_name": "Spark-X2.6",
                "context_window": 262144,
                "max_output_tokens": 8192,
                "input_modalities": ["text", "image"],
                "supported_reasoning_levels": [{"effort": "none"}, {"effort": "high"}],
                "badge": { "text": { "en": "Free" } },
                "visibility": "public",
                "supported_in_api": true,
            }]}
        });
        let models = parse(&payload).expect("应解析成功");
        assert_eq!(models.len(), 1);
        let row = &models[0];
        assert_eq!(row["id"], "spark-x2.6");
        assert_eq!(row["name"], "Spark-X2.6");
        assert_eq!(row["maxInputTokens"], 262144);
        assert_eq!(row["maxOutputTokens"], 8192);
        assert_eq!(row["supportsImages"], true);
        assert_eq!(row["supportsReasoning"], true);
        assert_eq!(row["free"], true);
        assert_eq!(row["supportsToolCall"], true);
        assert_eq!(row["isDefault"], false);
    }

    /// 隐藏的 / 不给 API 用的 / 没有 slug 的都不列
    #[test]
    fn entries_that_cannot_be_used_are_dropped() {
        let payload = json!({ "data": { "models": [
            { "slug": "ok", "supported_in_api": true },
            { "slug": "hidden", "visibility": "hidden" },
            { "slug": "no-api", "supported_in_api": false },
            { "display_name": "没有 slug" },
        ]}});
        let models = parse(&payload).expect("应解析成功");
        let ids: Vec<&str> = models
            .iter()
            .filter_map(|row| row.get("id").and_then(Value::as_str))
            .collect();
        assert_eq!(ids, vec!["ok"]);
    }

    /// 只有 `none` 档位 = 不支持思考；缺输出上限时给 32768
    #[test]
    fn reasoning_needs_a_real_level_and_output_has_a_default() {
        let payload = json!({ "data": { "models": [{
            "slug": "plain",
            "supported_reasoning_levels": [{"effort": "none"}],
        }]}});
        let models = parse(&payload).expect("应解析成功");
        assert_eq!(models[0]["supportsReasoning"], false);
        assert_eq!(models[0]["maxOutputTokens"], 32768);
        assert_eq!(models[0]["maxInputTokens"], 0);
    }

    /// 空清单 / 形状不对都算失败（保留旧清单比清空更接近真相）
    #[test]
    fn an_empty_or_misshapen_catalog_is_a_failure() {
        assert!(parse(&json!({ "data": { "models": [] } })).is_err());
        assert!(parse(&json!({ "data": {} })).is_err());
        assert!(parse(&json!({})).is_err());
        // 全部被过滤掉也算空
        assert!(parse(&json!({ "data": { "models": [{ "slug": "x", "visibility": "hidden" }] } })).is_err());
    }

    /// 默认模型排第一
    #[test]
    fn the_default_model_comes_first() {
        let payload = json!({ "data": { "models": [
            { "slug": "spark-x2.6" },
            { "slug": "astronclaw-auto" },
        ]}});
        let models = parse(&payload).expect("应解析成功");
        assert_eq!(models[0]["id"], "astronclaw-auto");
        assert_eq!(models[0]["isDefault"], true);
    }

    /// 没刷过任何账号时，清单与并集都给兜底（管理页不能是空白）
    #[test]
    fn the_fallback_is_never_empty() {
        let models = fallback();
        assert!(models.len() >= 7);
        let default = models
            .iter()
            .find(|row| row.get("isDefault").and_then(Value::as_bool) == Some(true))
            .expect("有默认模型");
        assert_eq!(default["id"], DEFAULT_MODEL);
        // 兜底的行与远程行同形（缺字段会让前端两种来源显示成两种样子）
        for row in &models {
            for key in ["id", "name", "maxInputTokens", "maxOutputTokens", "supportsToolCall"] {
                assert!(row.get(key).is_some(), "兜底行缺 {key}");
            }
        }
    }
}
