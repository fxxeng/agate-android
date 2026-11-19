//! QuWork 对话请求构造与上游错误分类。
//!
//! ── 请求体改写（照上游 `chat.rs::build`）───────────────────────
//! `POST {MODEL_ORIGIN}/chat/completions`，`Authorization: Bearer {llmToken}` +
//! `X-QurWork-Env: prod` / `X-QurWork-Version: 1.6.0`。按模型目录里的 `compat`
//! 处理：`store` 移除、`max_completion_tokens → max_tokens`、`developer → system`、
//! assistant 消息补空 `reasoning_content`、deepseek 的 `thinking` 档位。
//! 流式注入 `stream_options.include_usage=true`（本仓恒以流式请求上游）。
//!
//! ── 错误分类 ─────────────────────────────────────────────────
//! 四档对齐本仓 `UpstreamErrorClass`（不是上游 proto 枚举）：content_filter →
//! ContentBlocked、401/token_expired → TokenExpired、余额关键词 / 429/402 →
//! QuotaLimited（429 非余额时 reset_at = now+60s）、其余 Fatal。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::model_rules;
use crate::server::core::providers::adapter::{ReasoningPatch, UpstreamErrorClass};

use super::{client, endpoints, models};

/// 构造发给模型网关的请求体（纯函数，方便单测）。
///
/// 模型名在编排层已按目录改写成上游真名（`send_body`），这里读 `body["model"]`
/// 查 `compat`；查不到就按「无特殊兼容」处理（不因自定义模型名报错）。
pub fn request_body(body: &Value) -> Value {
    let mut payload = body.clone();
    let Some(object) = payload.as_object_mut() else {
        return payload;
    };
    let wire_model = object
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let stream = object
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    object.insert("stream".to_string(), Value::Bool(stream));
    if stream {
        let options = object
            .entry("stream_options")
            .or_insert_with(|| json!({}));
        if let Some(options) = options.as_object_mut() {
            options.insert("include_usage".to_string(), Value::Bool(true));
        }
    } else {
        object.remove("stream_options");
    }

    let catalog = models::current();
    let compat = catalog
        .model(&wire_model)
        .and_then(|row| row.get("compat"))
        .cloned()
        .unwrap_or(Value::Null);
    let efforts = catalog
        .model(&wire_model)
        .and_then(|row| row.get("reasoningEfforts"))
        .cloned()
        .unwrap_or(Value::Null);

    if compat.get("supportsStore").and_then(Value::as_bool) == Some(false) {
        object.remove("store");
    }
    if compat.get("maxTokensField").and_then(Value::as_str) == Some("max_tokens") {
        if let Some(limit) = object.remove("max_completion_tokens") {
            object.entry("max_tokens").or_insert(limit);
        }
    }
    // deepseek 思考档位：客户端 / 映射已给出档位时，翻成上游的 thinking 信封。
    // `reasoning_patch`（适配器）先把绑定等级注入 `reasoning_effort`，这里读到的
    // 就是最终随行档位（与上游 `chat.rs::build` 同一条取值链）。
    if compat.get("thinkingFormat").and_then(Value::as_str) == Some("deepseek") {
        let explicit = model_rules::read_client_level(&payload);
        let level = explicit.as_deref().unwrap_or("");
        if !level.is_empty() && payload.get("thinking").is_none_or(Value::is_null) {
            let normalized = if level == "none" { "off" } else { level };
            if let Some(wire) = efforts.get(normalized).and_then(Value::as_str) {
                if normalized == "off" {
                    payload["thinking"] = json!({ "type": "disabled" });
                    if let Some(object) = payload.as_object_mut() {
                        object.remove("reasoning_effort");
                    }
                } else {
                    payload["thinking"] = json!({ "type": "enabled" });
                    payload["reasoning_effort"] = json!(wire);
                }
            }
        }
    }
    if let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            if compat.get("supportsDeveloperRole").and_then(Value::as_bool) == Some(false)
                && message.get("role").and_then(Value::as_str) == Some("developer")
            {
                message["role"] = json!("system");
            }
            if compat
                .get("requiresReasoningContentOnAssistantMessages")
                .and_then(Value::as_bool)
                == Some(true)
                && message.get("role").and_then(Value::as_str) == Some("assistant")
                && message.get("reasoning_content").is_none_or(Value::is_null)
            {
                message["reasoning_content"] = message
                    .get("reasoning")
                    .filter(|value| value.is_string())
                    .cloned()
                    .unwrap_or(json!(""));
            }
        }
    }
    payload
}

/// 请求头（鉴权 + 两个身份头 + Content-Type / Accept）
///
/// `send_chat_with_client` 只透传 `plan.headers`（不会自动补 Content-Type），
/// 上游 chat.rs 也显式带了 Content-Type，这里照搬。
pub fn headers(token: &str) -> Vec<(String, String)> {
    let mut headers = endpoints::headers();
    headers.push(("Authorization".to_string(), format!("Bearer {token}")));
    headers.push(("Content-Type".to_string(), "application/json".to_string()));
    headers.push(("Accept".to_string(), "text/event-stream".to_string()));
    headers
}

/// 思考等级绑定 → 上游 `reasoning_effort`（照上游 `reasoning_patch`）。
///
/// 客户端已指定档位、或 body 里已经有 `thinking` 时让位；只认模型目录声明的
/// `reasoningEfforts`，自定义档位一律 Skip（塞未知值 upstream 会 400）。
pub fn reasoning_patch(level: &str, model: &str, body: &Value) -> ReasoningPatch {
    let skip = || ReasoningPatch::Skip {
        reason: "QuWork 按模型目录和客户端设置处理思考档位",
    };
    if model_rules::read_client_level(body).is_some()
        || body.get("thinking").is_some_and(|value| !value.is_null())
    {
        return skip();
    }
    let normalized = if level == "none" { "off" } else { level };
    let catalog = models::current();
    let Some(wire) = catalog
        .model(model)
        .and_then(|row| row.get("reasoningEfforts"))
        .and_then(|levels| levels.get(normalized))
        .and_then(Value::as_str)
    else {
        return skip();
    };
    ReasoningPatch::Set {
        field: "reasoning_effort",
        value: json!(wire),
    }
}

/// 上游错误分类（本仓四档，见模块头）
pub fn classify(status: u16, error_body: &Value) -> UpstreamErrorClass {
    let raw = client::message(error_body);
    let lower = raw.to_ascii_lowercase();
    let upstream_code = error_body
        .pointer("/error/code")
        .or_else(|| error_body.get("code"))
        .and_then(Value::as_i64);
    let kind = error_body
        .pointer("/error/code")
        .or_else(|| error_body.pointer("/error/type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let message = format!("上游返回 {status}: {raw}");

    if kind == "content_filter"
        || kind == "content_policy_violation"
        || lower.contains("content policy")
        || raw.contains("内容安全")
        || raw.contains("内容审核")
    {
        return UpstreamErrorClass::ContentBlocked {
            status,
            message,
            upstream_code,
        };
    }
    if status == 401 || matches!(kind, "invalid_api_key" | "token_expired" | "invalid_token") {
        return UpstreamErrorClass::TokenExpired { message };
    }
    let balance = matches!(kind, "insufficient_quota" | "insufficient_balance")
        || [
            "余额不足",
            "积分不足",
            "额度不足",
            "额度已用完",
            "insufficient quota",
            "insufficient balance",
            "quota exceeded",
        ]
        .iter()
        .any(|marker| lower.contains(marker));
    if balance || status == 429 || status == 402 {
        return UpstreamErrorClass::QuotaLimited {
            reset_at: if status == 429 && !balance {
                Some(crate::server::logging::now_ms() + 60_000)
            } else {
                None
            },
            message,
            upstream_code,
            status,
        };
    }
    UpstreamErrorClass::Fatal {
        status,
        message,
        upstream_code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_body_injects_include_usage_and_keeps_client_options() {
        let out = request_body(&json!({
            "model": "deepseek-flash",
            "stream": true,
            "stream_options": { "foo": 1 }
        }));
        assert_eq!(
            out.pointer("/stream_options/include_usage").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(out.pointer("/stream_options/foo").and_then(Value::as_i64), Some(1));
    }

    #[test]
    fn deepseek_compat_rewrites_fields() {
        let body = json!({
            "model": "deepseek-flash",
            "stream": true,
            "store": true,
            "max_completion_tokens": 100,
            "messages": [
                { "role": "developer", "content": "x" },
                { "role": "assistant", "content": "y" }
            ]
        });
        let out = request_body(&body);
        assert!(out.get("store").is_none(), "supportsStore=false 要移除 store");
        assert_eq!(out.get("max_tokens").and_then(Value::as_i64), Some(100));
        assert!(out.get("max_completion_tokens").is_none());
        let messages = out.get("messages").and_then(Value::as_array);
        assert_eq!(
            messages.and_then(|m| m.first()).and_then(|m| m.get("role")).and_then(Value::as_str),
            Some("system")
        );
        assert_eq!(
            messages.and_then(|m| m.get(1)).and_then(|m| m.get("reasoning_content")).and_then(Value::as_str),
            Some("")
        );
    }

    #[test]
    fn deepseek_thinking_maps_a_bound_level() {
        let body = json!({
            "model": "deepseek-flash",
            "stream": true,
            "reasoning_effort": "high"
        });
        let out = request_body(&body);
        assert_eq!(out.pointer("/thinking/type").and_then(Value::as_str), Some("enabled"));
        assert_eq!(out.get("reasoning_effort").and_then(Value::as_str), Some("high"));
    }

    #[test]
    fn classify_maps_401_to_token_expired() {
        let out = classify(401, &json!({ "message": "invalid api key", "error": { "code": "invalid_api_key" } }));
        assert!(matches!(out, UpstreamErrorClass::TokenExpired { .. }));
    }

    #[test]
    fn classify_maps_429_to_quota_limited_with_reset() {
        let out = classify(429, &json!({ "message": "too many requests" }));
        let reset = match out {
            UpstreamErrorClass::QuotaLimited { reset_at, status, .. } => {
                assert_eq!(status, 429);
                reset_at
            }
            _ => None,
        };
        assert!(reset.is_some(), "429 非余额时给 now+60s");
    }

    #[test]
    fn classify_maps_balance_keywords_without_reset() {
        let out = classify(400, &json!({ "message": "积分不足" }));
        assert!(matches!(
            out,
            UpstreamErrorClass::QuotaLimited { reset_at: None, .. }
        ));
    }

    #[test]
    fn classify_maps_content_filter_to_content_blocked() {
        let out = classify(400, &json!({ "error": { "code": "content_filter" }, "message": "blocked" }));
        assert!(matches!(out, UpstreamErrorClass::ContentBlocked { .. }));
    }
}
