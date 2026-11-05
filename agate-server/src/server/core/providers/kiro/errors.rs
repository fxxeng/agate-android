//! Kiro 的错误分类：四档映射 + 「输入过长」必须原样透传。
//!
//! ── 上游怎么表达失败 ────────────────────────────────────────
//! ```text
//!   400  ValidationException "Input is too long."（reason CONTENT_LENGTH_EXCEEDS_THRESHOLD）
//!   403  token 被拒（刷新后用同一账号重试一次）
//!   429  月额度用完 / 请求数超限（USAGE_LIMIT / MONTHLY_REQUEST_COUNT）
//!   503  INSUFFICIENT_MODEL_CAPACITY（模型没容量）
//!   流内  event stream 的 exception 帧：throttlingException / validationException / …
//! ```
//!
//! ── 为什么「输入过长」必须单独抬出来 ────────────────────────
//! 它的文案里带 `limit` / `too long` 一类词，若按「额度」分类会被**罚账号**
//! 并换号重试 —— 但换谁发同一份超长 body 都会被拒，等于把另一个账号的额度
//! 白扔一次。所以它在两处入口（HTTP 响应、流内异常帧）都统一成
//! 400 + 固定前缀 [`TOO_LONG`]，客户端据此提示「压缩上下文」，
//! 编排层据此**不换号**。
//!
//! ── 与别家的差异 ────────────────────────────────────────────
//! 本家没有 `ContentBlocked` 这一档：上游不做内容审核拦截（实测），
//! 把 400 里带敏感词的回复硬塞进降级状态机只会误伤。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use crate::server::core::providers::adapter::UpstreamErrorClass;

/// 「输入过长」的统一前缀（客户端按它判「该压缩上下文」）。
pub const TOO_LONG: &str = "input is too long for the model's context: ";

/// 文案里是否在说「对话超出模型窗口」
pub fn too_long_said(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("content_length_exceeds_threshold")
        || lower.contains("model_context_window_exceeded")
        || lower.contains("input is too long")
        || message.contains("输入过长")
}

/// 文案里是否在说「额度 / 限流」
pub fn quota_said(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "quota",
        "balance",
        "credit",
        "billing",
        "usage_limit",
        "usage limit",
        "monthly_request_count",
        "rate limit",
        "rate limited",
        "too many requests",
        "throttl",
        "limit reached",
        "overloaded",
        "额度",
        "限流",
        "频率",
    ]
    .iter()
    .any(|word| lower.contains(word))
}

/// 常见状态码的标准文案（上游回空体时用）
pub fn status_text(status: u16) -> String {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Request Entity Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown Error",
    }
    .to_string()
}

/// 从响应体里取出上游文案（`{"message","reason"}` 是 AWS 的固定形态）
fn message_of(body: &str) -> String {
    let mut message = body.trim().to_string();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(text) = value
            .get("message")
            .or_else(|| value.get("Message"))
            .and_then(|value| value.as_str())
        {
            message = text.trim().to_string();
        }
        if let Some(reason) = value.get("reason").and_then(|value| value.as_str()) {
            if !reason.trim().is_empty() {
                message = format!("{message} ({reason})");
            }
        }
    }
    message
}

/// HTTP 状态 + 正文 → 「(对外状态码, 文案)」。
///
/// 三处抬档：输入过长 → 400（透传、不罚号）、模型容量不足 → 503、
/// 额度类 → 429（换号 + 冷却）。
pub fn failure(status: u16, body: &str) -> (u16, String) {
    let mut message = message_of(body);
    if message.is_empty() {
        message = status_text(status);
    }
    if too_long_said(&message) {
        return (400, format!("{TOO_LONG}{message}"));
    }
    if message.contains("INSUFFICIENT_MODEL_CAPACITY") {
        return (503, message);
    }
    if message.contains("MONTHLY_REQUEST_COUNT") || message.contains("USAGE_LIMIT") {
        return (429, format!("usage limit reached: {message}"));
    }
    (status, message)
}

/// 四档分类（编排层按它决定「换号 / 续期重试 / 原样透传」）。
pub fn classify(status: u16, body: &str) -> UpstreamErrorClass {
    let message = message_of(body);
    let message = if message.is_empty() {
        status_text(status)
    } else {
        message
    };
    // 「输入过长」优先于一切：它的文案里也有 limit 字样，必须先判掉，
    // 否则会被归进 QuotaLimited 白罚一个账号（见模块头）
    if too_long_said(&message) {
        return UpstreamErrorClass::Fatal {
            status: 400,
            message: format!("{TOO_LONG}{message}"),
            upstream_code: None,
        };
    }
    if status == 401 || status == 403 {
        return UpstreamErrorClass::TokenExpired { message };
    }
    if status == 429 || quota_said(&message) {
        return UpstreamErrorClass::QuotaLimited {
            reset_at: None,
            message,
            upstream_code: None,
            status: if status == 0 { 429 } else { status },
        };
    }
    UpstreamErrorClass::Fatal {
        status,
        message,
        upstream_code: None,
    }
}

/// 流内异常帧 → 文案（AWS 的 exception 帧体也是 `{message, reason}`）
pub fn exception_text(kind: &str, payload: &str) -> String {
    let mut message = message_of(payload);
    if message.is_empty() {
        message = "Kiro 返回了一个空的错误".to_string();
    }
    if kind.to_ascii_lowercase().contains("throttl") {
        return format!("rate limited: {message}");
    }
    if kind.is_empty() {
        message
    } else {
        format!("{kind}: {message}")
    }
}

/// 流内错误该按额度处理吗（换号 + 冷却）
pub fn stream_error_is_quota(text: &str) -> bool {
    quota_said(text) && !too_long_said(text)
}

/// 流内错误是「输入过长」吗（透传、不罚号）
pub fn stream_error_is_too_long(text: &str) -> bool {
    too_long_said(text)
}

/// 流内错误该用什么状态码收尾（`None` = 502，上游没说清）
pub fn stream_error_status(text: &str) -> Option<u16> {
    if stream_error_is_too_long(text) {
        Some(400)
    } else if stream_error_is_quota(text) {
        Some(429)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 「输入过长」既不能算额度，也不能被换号抵掉
    #[test]
    fn too_long_is_never_treated_as_quota() {
        let body = r#"{"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#;
        let (status, message) = failure(400, body);
        assert_eq!(status, 400);
        assert!(message.starts_with(TOO_LONG), "{message}");
        // 分类同样是 Fatal（不透传成 QuotaLimited）
        assert!(matches!(
            classify(429, body),
            UpstreamErrorClass::Fatal { status: 400, .. }
        ));
        // 流内判据也一致
        assert!(stream_error_is_too_long(&message));
        assert!(!stream_error_is_quota(&message), "文案里有 limit 也不能算额度");
        assert_eq!(stream_error_status(&message), Some(400));
    }

    /// 月额度 / 请求数超限 → 429（换号 + 冷却）
    #[test]
    fn the_monthly_limit_becomes_a_quota_error() {
        for body in [
            r#"{"message":"You have reached the monthly limit.","reason":"MONTHLY_REQUEST_COUNT"}"#,
            r#"{"message":"x","reason":"USAGE_LIMIT"}"#,
        ] {
            let (status, message) = failure(400, body);
            assert_eq!(status, 429, "{body}");
            assert!(message.starts_with("usage limit reached: "), "{message}");
        }
        assert!(matches!(
            classify(429, "{}"),
            UpstreamErrorClass::QuotaLimited { status: 429, .. }
        ));
    }

    /// 模型没容量 → 503（不是额度，也不是鉴权）
    #[test]
    fn model_capacity_is_a_503() {
        let (status, _) = failure(503, r#"{"message":"INSUFFICIENT_MODEL_CAPACITY"}"#);
        assert_eq!(status, 503);
        assert!(matches!(
            classify(503, r#"{"message":"INSUFFICIENT_MODEL_CAPACITY"}"#),
            UpstreamErrorClass::Fatal { status: 503, .. }
        ));
    }

    /// 401 / 403 → TokenExpired（同一账号续期后重试一次）
    #[test]
    fn auth_failures_ask_for_a_refresh() {
        for status in [401u16, 403] {
            assert!(matches!(
                classify(status, r#"{"message":"token expired"}"#),
                UpstreamErrorClass::TokenExpired { .. }
            ));
        }
    }

    /// 空体 / 非 JSON 体不能变成空文案
    #[test]
    fn empty_bodies_fall_back_to_the_status_text() {
        let (status, message) = failure(502, "");
        assert_eq!(status, 502);
        assert_eq!(message, "Bad Gateway");
        let (status, message) = failure(500, "<html>oops</html>");
        assert_eq!(status, 500);
        assert_eq!(message, "<html>oops</html>");
    }

    /// 异常帧文案：throttling 加 rate limited 前缀，其余带类型名
    #[test]
    fn exception_frames_read_like_the_upstream_ones() {
        let text = exception_text("ThrottlingException", r#"{"message":"slow down"}"#);
        assert_eq!(text, "rate limited: slow down");
        assert!(stream_error_is_quota(&text));

        let text = exception_text("ValidationException", r#"{"message":"bad input"}"#);
        assert_eq!(text, "ValidationException: bad input");
        assert!(!stream_error_is_quota(&text));
        assert_eq!(stream_error_status(&text), None);

        // 空体也要给一句话，而不是空白错误
        assert_eq!(exception_text("", ""), "Kiro 返回了一个空的错误");
    }
}
