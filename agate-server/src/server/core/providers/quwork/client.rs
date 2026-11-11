//! QuWork 两类出站请求的薄封装：账号服务（Bearer accessToken）与公开模型配置。
//!
//! 复用本仓 `core::auth_http::send_raw`（直连 + 20 秒总超时），与 modelscope
//! 的 `client.rs` 同口径：账号级辅助请求不带转发账号的代理，用户看到的「直连」
//! 就是真的直连。上游插件用的是 `PluginCtx` 里的网关级 client + `control_client`，
//! 本仓静态编译没有那个概念。
//!
//! ── 响应信封 ────────────────────────────────────────────────
//! QuWork 账号服务统一回 `{success, data, message}`；**业务位为假**或 HTTP 4xx
//! 都算失败（`Reply::data` 一处收口）。模型网关的 chat/completions 错误体走
//! `chat::classify`，不经这里。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::auth_http::send_raw;
use crate::server::errors::GatewayError;

use super::endpoints;

/// 辅助请求的总超时（与 modelscope 同档；egress 默认 600s 是给 SSE 的）
const TIMEOUT_MS: u64 = 20_000;

/// 一次响应的原始形态（状态 + 已解析/兜底的 JSON）
#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
}

/// 发一次账号服务请求（路径会自动拼到 `ACCOUNT_ORIGIN`）。
///
/// `method` 只有 "POST" 会真的发 POST，其余一律 GET（与 `send_raw` 的契约）。
/// 请求头带 `Authorization: Bearer {token}` + 两个 `X-QurWork-*` 身份头。
pub async fn request(
    method: &str,
    path: &str,
    token: &str,
    body: Option<&Value>,
) -> Result<Reply, GatewayError> {
    let url = format!("{}{path}", endpoints::ACCOUNT_ORIGIN);
    // `send_raw` 会自动补 Accept；这里只加身份头与账号面鉴权
    let mut headers = vec![("Cache-Control".to_string(), "no-cache".to_string())];
    if !token.is_empty() {
        headers.push(("Authorization".to_string(), format!("Bearer {token}")));
    }
    headers.extend(endpoints::headers());
    let response = send_raw(method, &url, body, &headers, None, Some(TIMEOUT_MS))
        .await
        .map_err(network_error)?;
    Ok(read_reply(response.status, response.payload, &[token]))
}

/// 拉一次公开模型配置（无鉴权、无身份头）。
pub async fn public_config() -> Result<Reply, GatewayError> {
    let headers = vec![("Cache-Control".to_string(), "no-cache".to_string())];
    let response = send_raw(
        "GET",
        endpoints::CATALOG_URL,
        None,
        &headers,
        None,
        Some(TIMEOUT_MS),
    )
    .await
    .map_err(network_error)?;
    Ok(read_reply(response.status, response.payload, &[]))
}

/// 把 `ApiResponse` 变成 `Reply`，并把响应文案里的令牌抹掉、截断到 300 字。
///
/// 非 2xx 且响应体不是 JSON 时给一个可读的兜底信封（上游 `read_reply` 同款）。
fn read_reply(status: u16, payload: Option<Value>, secrets: &[&str]) -> Reply {
    let mut body = match payload {
        Some(body) => body,
        None if !(200..300).contains(&status) => {
            json!({ "success": false, "message": format!("上游返回 HTTP {status}") })
        }
        None => json!({ "success": false, "message": "QuWork 未返回有效的 JSON 数据" }),
    };
    for path in ["/message", "/msg", "/error/message"] {
        if let Some(value) = body.pointer_mut(path) {
            if let Some(text) = value.as_str() {
                let mut text = text.to_string();
                for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
                    text = text.replace(secret, "[已隐藏]");
                }
                *value = json!(text.chars().take(300).collect::<String>());
            }
        }
    }
    Reply { status, body }
}

/// 上游文案（三个常见位置都认，截断 300 字）
pub fn message(body: &Value) -> String {
    body.pointer("/error/message")
        .or_else(|| body.get("message"))
        .or_else(|| body.get("msg"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .take(300)
        .collect()
}

/// 业务成功位（bool / 非零数字 / "true"|"1" 字符串）
pub fn success(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|value| value != 0.0),
        Some(Value::String(text)) => {
            matches!(text.trim().to_ascii_lowercase().as_str(), "true" | "1")
        }
        _ => false,
    }
}

/// 非负数字（数字或数字字符串；负数 / 非法一律 None）
pub fn number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|text| text.trim().parse::<f64>().ok()))
        })
        .filter(|value| value.is_finite() && *value >= 0.0)
}

/// 时间戳归一：数字（秒 → 毫秒）或 RFC3339 字符串 → 毫秒
pub fn timestamp(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(number) = number(Some(value)).filter(|value| *value > 0.0) {
        let number = number as i64;
        return Some(if number < 100_000_000_000 {
            number.saturating_mul(1000)
        } else {
            number
        });
    }
    chrono::DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|date| date.timestamp_millis())
}

impl Reply {
    /// 2xx 且业务成功
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status) && success(self.body.get("success"))
    }

    /// 401，或「未成功且文案是未登录」
    pub fn unauthorized(&self) -> bool {
        self.status == 401
            || (!self.ok() && matches!(message(&self.body).as_str(), "未登录" | "用户未登录"))
    }

    /// 取业务 `data`；失败时翻译成可读错误（401 原样透出供刷新重试）
    pub fn data(&self, operation: &str) -> Result<Value, GatewayError> {
        if !self.ok() {
            let detail = message(&self.body);
            let status = if self.unauthorized() {
                401
            } else if self.status >= 400 {
                i32::from(self.status)
            } else {
                502
            };
            return Err(GatewayError::with_status(
                status,
                if detail.is_empty() {
                    format!("QuWork {operation}未成功")
                } else {
                    format!("QuWork {operation}未成功：{detail}")
                },
            ));
        }
        Ok(self.body.get("data").cloned().unwrap_or(Value::Null))
    }
}

fn network_error(error: reqwest::Error) -> GatewayError {
    GatewayError::with_status(
        if error.is_timeout() { 504 } else { 502 },
        if error.is_timeout() {
            "QuWork 请求超时".to_string()
        } else {
            "无法连接 QuWork，请检查网络".to_string()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_accepts_bool_number_and_string() {
        assert!(success(Some(&json!(true))));
        assert!(success(Some(&json!(1))));
        assert!(success(Some(&json!("true"))));
        assert!(!success(Some(&json!(false))));
        assert!(!success(Some(&json!(0))));
        assert!(!success(None));
    }

    #[test]
    fn number_filters_negative_and_accepts_string() {
        assert_eq!(number(Some(&json!(12.5))), Some(12.5));
        assert_eq!(number(Some(&json!("7"))), Some(7.0));
        assert_eq!(number(Some(&json!(-1))), None);
        assert_eq!(number(None), None);
    }

    #[test]
    fn timestamp_scales_seconds_and_parses_rfc3339() {
        assert_eq!(timestamp(Some(&json!(1_700_000_000))), Some(1_700_000_000_000));
        assert_eq!(timestamp(Some(&json!(1_700_000_000_000_i64))), Some(1_700_000_000_000));
        assert!(timestamp(Some(&json!("2026-11-04T00:00:00Z"))).is_some());
        assert_eq!(timestamp(Some(&json!(0))), None);
    }
}
