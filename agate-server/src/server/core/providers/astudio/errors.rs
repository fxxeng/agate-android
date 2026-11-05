//! AStudio 的错误判定：四档分类 + 「输入过长」必须单独抬出来。
//!
//! ── 上游怎么表达失败 ────────────────────────────────────────
//! ```text
//!   账号面（agent.xfyun.cn）{flag: false, code: 80000, desc: "请重新登录"}
//!                             → 会话失效，必须让用户重新登录
//!   模型面（model-gateway）  401/403 凭据被拒
//!                            429     积分用完 / 限流
//!                            400     "context length exceeded" 一类
//!   流内                     `error` / `response.failed` 事件带 message
//! ```
//!
//! ── 为什么「输入过长」必须单列（与 kiro 同一条纪律）──────────
//! 它的文案里带 `limit` / `quota` 一类词，按「额度」归类会被**罚账号**
//! 并换号重试 —— 而换谁发同一份超长 body 都会被拒，等于白扔另一个账号的
//! 额度。所以它在三个入口（HTTP 响应、流内事件、账号面业务码）统一成 400，
//! 编排层据此**不换号**。
//!
//! ── 判据只此一份 ────────────────────────────────────────────
//! `responses::status_for`（流内事件 → 状态码）与 `classify`（适配器四档分类）
//! 共用本文件的两条词表。各写一份必然分叉，症状是「HTTP 路径判成 429、流内
//! 判成 400」—— 同一个超长请求有时换号、有时不换，最难复盘的那类现场。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::Value;

use crate::server::core::providers::adapter::UpstreamErrorClass;

/// 「这段文案在说输入过长」的词表
///
/// 刻意**不收** `max_tokens` 这个裸词：它既可能是「max_tokens 超出模型上限」
/// （确实是超长），也可能是「你今天的 max_tokens 配额用完了」（那是额度）。
/// 收进来会把后者判成 400，于是额度耗尽的账号不会被冷却、每次请求都白打一遍。
/// 「超出上限」那种说法由 `too large` / `maximum length` 兜住。
const TOO_LONG_WORDS: [&str; 7] = [
    "context length",
    "context_length",
    "context window",
    "maximum length",
    "too long",
    "too large",
    "too many tokens",
];

/// 「这段文案在说额度 / 限流」的词表
const QUOTA_WORDS: [&str; 12] = [
    "rate limit",
    "too many requests",
    "quota",
    "balance",
    "credit",
    "insufficient",
    "积分",
    "额度",
    "余额",
    "限流",
    "频率",
    "配额",
];

/// 这段文案是不是在说「对话超出模型窗口」
pub fn too_long_said(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    if TOO_LONG_WORDS.iter().any(|word| lower.contains(word)) {
        return true;
    }
    // 中文文案不一定带上面那些英文词（上游会对中文请求回中文错误）
    ["输入过长", "上下文过长", "超出上下文", "内容过长", "超过最大长度"]
        .iter()
        .any(|word| message.contains(word))
}

/// 这段文案是不是在说「额度 / 限流」
pub fn quota_said(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    QUOTA_WORDS.iter().any(|word| lower.contains(word))
}

/// 常见状态码的标准文案（上游回空体时用）
pub fn status_text(status: u16) -> String {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
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

/// 从错误体里取出上游文案。
///
/// 三种形态都出现过：OpenAI 风格的 `{error:{message}}`、账号面的
/// `{code, desc}`、以及一句裸文本（网关前置层直接回的）。取不到就返回空串，
/// 由调用方用 [`status_text`] 兜底 —— 这里不编造文案。
pub fn message_of(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(text) = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| value.get("message").and_then(Value::as_str))
            .or_else(|| value.get("msg").and_then(Value::as_str))
            .or_else(|| value.get("desc").and_then(Value::as_str))
            .or_else(|| value.get("error").and_then(Value::as_str))
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            return text.to_string();
        }
    }
    // 不是 JSON（或 JSON 里没有文案）就截断原文：一整页 HTML 错误页塞进
    // 客户端只会让它显示成一片乱码
    trimmed.chars().take(500).collect()
}

/// HTTP 状态 + 正文 → 「(对外状态码, 文案)」。
///
/// 两处抬档：输入过长 → 400（透传、不罚号）、凭据被拒 → 401（客户端据此
/// 提示重新粘贴）。其余原样：429 必须保持 429，编排层才会换号。
pub fn failure(status: u16, body: &str) -> (u16, String) {
    let message = message_of(body);
    let message = if message.is_empty() {
        status_text(status)
    } else {
        message
    };
    if too_long_said(&message) {
        return (400, message);
    }
    if status == 401 || status == 403 {
        return (401, message);
    }
    (status, message)
}

/// 四档分类（编排层按它决定「换号 / 续期重试 / 原样透传」）。
///
/// 401 / 403 归 `TokenExpired` 而不是 `Fatal`：本家**没有续期接口**，但编排层
/// 收到 `TokenExpired` 会先调 `refresh_access_token`（适配器如实返回失败），
/// 然后按普通失败处理 —— 与 `Fatal` 的差别只是多一次注定失败的刷新。反过来
/// 归 `Fatal` 的话，将来上游真给了续期接口，这里会静默地永远不续期。
pub fn classify(status: u16, body: &str) -> UpstreamErrorClass {
    let message = message_of(body);
    let message = if message.is_empty() {
        status_text(status)
    } else {
        message
    };
    // 输入过长优先于一切（见模块头）
    if too_long_said(&message) {
        return UpstreamErrorClass::Fatal {
            status: 400,
            message,
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

/// 流内错误文案 → 状态码（`502` = 上游没说清该算哪一档）。
///
/// 流已经开始，HTTP 状态码改不了了；这个数字只用于**给客户端的错误帧**
/// （`rate_limit_error` / `api_error`）与编排层的额度记账。
pub fn in_stream_status(message: &str) -> u16 {
    if too_long_said(message) {
        return 400;
    }
    if quota_said(message) {
        return 429;
    }
    502
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 「输入过长」既不能算额度，也不能被换号抵掉
    #[test]
    fn too_long_is_never_treated_as_quota() {
        let body = r#"{"error":{"message":"This model's maximum context length is 262144 tokens"}}"#;
        let (status, message) = failure(429, body);
        assert_eq!(status, 400, "超长 → 400，不换号");
        assert!(message.contains("maximum context length"), "{message}");
        assert!(matches!(
            classify(429, body),
            UpstreamErrorClass::Fatal { status: 400, .. }
        ));
        assert_eq!(in_stream_status(&message), 400);
    }

    /// 中文文案同样要认（上游对中文请求回中文错误）
    #[test]
    fn chinese_wording_counts_too() {
        assert!(too_long_said("输入过长，请压缩上下文"));
        assert!(too_long_said("上下文过长"));
        assert!(!quota_said("输入过长")); // 「过长」不含额度词
        assert_eq!(in_stream_status("上下文过长"), 400);
    }

    /// 积分用完 / 限流 → 429（换号 + 冷却）
    #[test]
    fn quota_stays_quota() {
        for body in [
            r#"{"message":"rate limit exceeded"}"#,
            r#"{"error":{"message":"积分不足，请充值"}}"#,
            r#"{"desc":"每日额度已用完"}"#,
        ] {
            let (status, _) = failure(429, body);
            assert_eq!(status, 429, "{body}");
            assert!(matches!(
                classify(429, body),
                UpstreamErrorClass::QuotaLimited { .. }
            ));
        }
    }

    /// 凭据被拒：对外 401、对内走「刷新后重试」那一档
    #[test]
    fn a_rejected_credential_is_a_token_expiry() {
        let (status, _) = failure(403, r#"{"message":"invalid token"}"#);
        assert_eq!(status, 401);
        assert!(matches!(
            classify(403, r#"{"message":"invalid token"}"#),
            UpstreamErrorClass::TokenExpired { .. }
        ));
    }

    /// 文案从三种形态里取（OpenAI 风格 / 账号面 / 裸文本），取不到给状态文案
    #[test]
    fn the_upstream_message_is_found_or_replaced() {
        assert_eq!(
            message_of(r#"{"error":{"message":"boom"}}"#),
            "boom".to_string()
        );
        assert_eq!(message_of(r#"{"code":80000,"desc":"请重新登录"}"#), "请重新登录");
        assert_eq!(message_of("Bad gateway"), "Bad gateway");
        assert_eq!(message_of("   "), "");
        // 空体 → 标准文案（不编造）
        assert_eq!(failure(503, ""), (503, status_text(503)));
        // HTML 错误页截断到 500 字符（不把整页塞给客户端）
        let long = "x".repeat(2000);
        assert_eq!(message_of(&long).chars().count(), 500);
    }

    /// 普通 500 / 400 原样透传（不改档）
    #[test]
    fn other_failures_pass_through() {
        let (status, _) = failure(500, r#"{"message":"internal error"}"#);
        assert_eq!(status, 500);
        assert!(matches!(
            classify(500, ""),
            UpstreamErrorClass::Fatal { status: 500, .. }
        ));
        assert_eq!(in_stream_status("something odd"), 502);
    }
}
