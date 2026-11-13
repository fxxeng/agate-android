//! 上游错误分类 + 320KB 出站预检。
//!
//! 判据来自方案 L5 / L12 与 `Coding2API`、`dsh-router-wpslingxi` 的语义状态表：
//!   401 凭证死（无续期接口，只能重贴）· 402 额度尽 · 404 短冷却 ·
//!   429 **模型级**（不是账号级，按模型收窄顺延池）· 审核拦截换号无用。
//!
//! ⚠️ 403 在 `Coding2API` 记作瞬时、在 DSH 插件记作 `session_dead`，两边冲突；
//! 本仓先按「瞬时」处理（更保守：不误停一个其实没死的号），真机实测后校正。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic。

use serde_json::Value;

use super::endpoints;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// 凭证失效：不刷新、不重试，标记「需重新登录」。
    DeadCredential,
    /// 额度耗尽。
    Quota,
    /// 模型级限流（429）：按模型收窄顺延池，不整家冷却。
    ModelRateLimited,
    /// 短冷却（404）。
    ShortCooldown,
    /// 内容审核：换号无用，不进轮换。
    ContentFiltered,
    /// 瞬时错误（403 / 5xx）。
    Transient,
    /// 其它透传。
    Fatal,
}

/// 上游文案里的审核信号。**只认结构化关键字，不按中文兜底话术猜**
/// （话术原文未在任何公开资料里给出，见 `stream.rs` 模块头）。
pub fn looks_content_filtered(body: &Value) -> bool {
    let text = body
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .or_else(|| body.get("message").and_then(Value::as_str))
        .unwrap_or("");
    let lowered = text.to_ascii_lowercase();
    lowered.contains("auditfailed")
        || lowered.contains("content_filter")
        || lowered.contains("audit_failed")
        || lowered.contains("sensitive")
}

pub fn classify(status: u16, body: &Value) -> Failure {
    if looks_content_filtered(body) {
        return Failure::ContentFiltered;
    }
    match status {
        401 => Failure::DeadCredential,
        402 => Failure::Quota,
        404 => Failure::ShortCooldown,
        429 => Failure::ModelRateLimited,
        403 | 500..=599 => Failure::Transient,
        _ => Failure::Fatal,
    }
}

/// 出站前按**序列化字节数**预检。序列化失败时不拦（让上游去判），
/// 不在这里猜一个长度。
pub fn oversized(body: &Value) -> bool {
    serde_json::to_vec(body)
        .map(|bytes| bytes.len() > endpoints::MAX_PROMPT_BYTES)
        .unwrap_or(false)
}

/// 本地拒绝文案：上游只回「AI服务异常」不提示太长，所以这条必须我们自己写清。
pub fn oversized_message() -> String {
    format!(
        "请求体超过灵犀上游约 {} KiB 的上限（已在本机拦截，未发往上游）",
        endpoints::MAX_PROMPT_BYTES / 1024
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn credential_death_is_not_treated_as_transient() {
        assert_eq!(classify(401, &json!({ "message": "用户无权限" })), Failure::DeadCredential);
    }

    #[test]
    fn status_map_follows_the_documented_semantics() {
        assert_eq!(classify(402, &json!({})), Failure::Quota);
        assert_eq!(classify(404, &json!({})), Failure::ShortCooldown);
        assert_eq!(classify(429, &json!({})), Failure::ModelRateLimited);
        assert_eq!(classify(403, &json!({})), Failure::Transient);
        assert_eq!(classify(503, &json!({})), Failure::Transient);
        assert_eq!(classify(400, &json!({})), Failure::Fatal);
    }

    #[test]
    fn audit_failure_never_triggers_account_rotation() {
        assert_eq!(
            classify(200, &json!({ "error": { "message": "AuditFailed" } })),
            Failure::ContentFiltered
        );
    }

    #[test]
    fn oversized_body_is_rejected_locally() {
        let big = json!({ "messages": [{ "role": "user", "content": "x".repeat(330_000) }] });
        assert!(oversized(&big));
        assert!(oversized_message().contains("本机拦截"));
    }

    #[test]
    fn normal_body_passes_the_precheck() {
        assert!(!oversized(&json!({ "model": "qwen3.8_flash", "messages": [] })));
    }
}
