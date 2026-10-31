//! AtomCode 网关的错误分类，以及**两种静默失败**的识别。
//!
//! ── 网关的两套错误信封（都要认）──────────────────────────────
//! ```text
//!   {"error":{"message":"...","type":"auth_error","code":"403"}}   LiteLLM 信封
//!   {"detail":{"code":"ATOMCODE_SIG_MISSING","message":"..."}}     AtomCode 信封
//! ```
//!
//! ⚠️ 传到 [`classify_gateway_error`] 的 `error_body` 是**传输层归一化之后**的形态
//! （`upstream::request::read_upstream_error`）：它只从**顶层**取 `code`
//! （且要求是数字）与 `message`/`msg`/`error.message`。所以：
//!   - 第一套信封的 `message` 能取到，`code` 取不到（它是字符串 `"403"`）；
//!   - 第二套信封两个都取不到，`message` 退化成**截断后的原文 JSON**
//!     —— 好消息是原文里就含 `ATOMCODE_SIG_MISSING` 这个码，所以按文案匹配
//!     仍然可靠。
//! 这一节是「为什么这里在读字符串」的答案，别改成只读结构字段。
//!
//! ── 两种静默失败（HTTP 200 的假成功）─────────────────────────
//! 网关拒绝请求时**不返回错误状态**，而是回一条「看起来正常的回答」：
//!
//! ```text
//!   模型不在该账号套餐内 / 参数不被接受  -> HTTP 200，正文/首帧 content = 参数错误
//!   模型被列出但后端是坏的              -> HTTP 200，正文 = 三方请求失败: 502 <html>…
//! ```
//!
//! 第二种尤其恶劣：它会把**上游的 HTML 报错页**当作模型回答粘进对话，而客户端
//! 完全看不出异常（`cpa-jethub-plugins/plugins/atomcode/errors.go` 记了实测）。
//! 两种都在这里识别，交给 SSE 出口与错误分类两条路使用。
//!
//! ── ⚠️ 静默失败的三件套目前**没有调用点**（与 lobsterai 同一处公开的缺口）──
//! 本仓的逐帧出口（`upstream::ForwardStream`）只按适配器声明的 `finish_reason`
//! 识别拒答（`ProviderAdapter::stream_refusal_finishes`），没有「按帧内容嗅探」
//! 的挂点，而适配器的 `is_error_response` 又只拿得到 `status + headers`。
//! 所以 [`silent_failure_in_content`] / [`silent_failure_error`] 先作为**纯函数 +
//! 测试**立住判据，等通用层给出挂点再接 —— 在那之前，这类失败会以**正常回答**
//! 的形式到达客户端（如实记下，不假装已覆盖）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::Value;

use crate::server::core::providers::adapter::UpstreamErrorClass;
use crate::server::core::providers::content_block;
use crate::server::errors::GatewayError;

/// 网关在**成功状态**下用这句正文表示「我拒绝了这个请求」。
///
/// 实测 2026-10-01：未知模型名得到 `HTTP 200` +
/// `choices[0].message.content == "参数错误"`，而不是错误状态。
pub const PARAMETER_ERROR_MESSAGE: &str = "参数错误";

/// 网关在**成功状态**下表示「这个模型被列出来了但后端是坏的」。
///
/// 实测 2026-10-01：`Qwen/Qwen3-4B-Instruct-2507` 回 `HTTP 200`，正文整段是
/// `三方请求失败: 502 <html>…`。
pub const THIRD_PARTY_FAILURE_MARKER: &str = "三方请求失败";

/// 需要闭源签名的网关给出的码。
///
/// 它不是凭证问题、也不是额度问题 —— 它意味着请求打到了
/// `llm-api.atomgit.com`（或同类需要签名的主机）而不是无需签名的
/// `api-ai.gitcode.com`。当成普通 403 报出去，用户会去查一个并不存在的登录问题。
pub const SIGNATURE_MISSING_CODE: &str = "ATOMCODE_SIG_MISSING";

/// 一次「静默失败」的识别结果。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SilentFailure {
    /// 网关以成功状态拒绝了模型或参数（正文是 [`PARAMETER_ERROR_MESSAGE`]）
    ParameterRejected,
    /// 模型被列出但后端坏了（正文是上游报错页，不能当回答透传）
    ThirdPartyBroken,
}

/// 判断一段**正文/首帧内容**是不是网关的静默失败。
///
/// ── 判据为什么是「相等」与「前缀」而不是 `contains` ────────────
/// `contains` 会在模型**正常复述**这些词时误判：
/// 用户完全可能问「参数错误是什么」，模型答「参数错误通常指…」——
/// 那一帧被判成失败，整轮就变成了一次凭空的换账号重试。
/// 网关的形态是确定的：正文**整段就是**那四个字（首尾空白可能有），
/// 或**以** `三方请求失败` 开头后跟原始错误。按这两种形态判，误判面最小。
pub fn silent_failure_in_content(content: &str) -> Option<SilentFailure> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == PARAMETER_ERROR_MESSAGE {
        return Some(SilentFailure::ParameterRejected);
    }
    if trimmed.starts_with(THIRD_PARTY_FAILURE_MARKER) {
        return Some(SilentFailure::ThirdPartyBroken);
    }
    None
}

/// 把静默失败渲染成客户端可见的错误。
///
/// 一律是 502 而不是 400：请求本身没问题（HTTP 通、网关也答了 200），
/// 是**这次生成没成**。归成 400 会让客户端以为是自己的请求体写错了。
///
/// `model` 是**客户端看到的那个名字**（不是改写后发给上游的 id）——
/// 报错要报用户能据以行动的那个名字。
pub fn silent_failure_error(failure: SilentFailure, model: &str) -> GatewayError {
    let subject = if model.trim().is_empty() {
        "该请求".to_string()
    } else {
        format!("模型「{}」", model.trim())
    };
    match failure {
        SilentFailure::ParameterRejected => GatewayError::with_status(
            502,
            format!(
                "AtomCode 网关拒绝了{subject}：返回成功状态但正文是「{PARAMETER_ERROR_MESSAGE}」。\
                 通常表示该模型不在这个账号的套餐内，或请求参数不被接受"
            ),
        ),
        SilentFailure::ThirdPartyBroken => GatewayError::with_status(
            502,
            format!(
                "AtomCode 网关返回了上游报错页而不是回答（{subject}）：该模型被列出但后端不可用，\
                 请换一个模型重试"
            ),
        ),
    }
}

/// 从归一化后的错误体里取一行可读文案。
fn message_of(error_body: &Value) -> String {
    error_body
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("上游错误")
        .to_string()
}

/// 从归一化后的错误体里取业务码（**字符串与数字都认**）。
///
/// 归一化层只填数字码，但 `detail.code` 那套信封是字符串码、且它已经被折进
/// `message` 原文里，所以这里把两种来源都覆盖上。
fn code_of(error_body: &Value) -> Option<String> {
    if let Some(text) = error_body.get("code").and_then(Value::as_str) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    if let Some(number) = error_body.get("code").and_then(Value::as_i64) {
        return Some(number.to_string());
    }
    None
}

/// 判断错误体（或其原文）里有没有某个码 —— 用于 `detail.code` 那种
/// 「码被折进 message 原文」的情形。
fn message_mentions(error_body: &Value, needle: &str) -> bool {
    message_of(error_body).contains(needle)
}

/// AtomCode 网关的错误分类。
///
/// 判定顺序（顺序本身是契约，别调）：
///   1. `ATOMCODE_SIG_MISSING` → 配置错误，**先于** 401/403 判（它也是 403）；
///   2. 401 / 403：
///      - 文案含 `codingplan` → `Fatal`（账号还没领免费套餐，令牌是好的，
///        判成 TokenExpired 会让上层反复刷新一个没问题的令牌）；
///      - 其余 → `TokenExpired`（刷新后用同一账号重试一次）；
///   3. 429 → `QuotaLimited`（网关不给结构化恢复时间）；
///   4. 其余 → 交共用内容拦截分类（审核文案 → `ContentBlocked`，否则 `Fatal`）。
pub fn classify_gateway_error(status: u16, error_body: &Value) -> UpstreamErrorClass {
    let raw = message_of(error_body);
    let message = format!("上游返回 {status}: {raw}");
    let upstream_code = code_of(error_body).and_then(|code| code.parse::<i64>().ok());

    // 1) 打到需要签名的主机：这是**配置**问题，不是凭证也不是额度。
    if message_mentions(error_body, SIGNATURE_MISSING_CODE) {
        return UpstreamErrorClass::Fatal {
            status: 502,
            message: format!(
                "AtomCode 网关要求官方版请求签名（{SIGNATURE_MISSING_CODE}）：\
                 当前配的网关地址指向了需要签名的官方主机，请改回无需签名的 api-ai.gitcode.com（{raw}）"
            ),
            upstream_code: None,
        };
    }

    // 2) 鉴权类
    if status == 401 || status == 403 {
        if message_mentions(error_body, "codingplan") {
            return UpstreamErrorClass::Fatal {
                status: 403,
                message: format!("该 AtomGit 账号还没有领取免费套餐，请先在 AtomCode 页面领取（{raw}）"),
                upstream_code,
            };
        }
        return UpstreamErrorClass::TokenExpired { message };
    }

    // 3) 限额（网关不吐 x-ratelimit-*，也没有结构化恢复时间）
    if status == 429 {
        return UpstreamErrorClass::QuotaLimited {
            reset_at: None,
            message,
            upstream_code,
            status,
        };
    }

    // 4) 其余（含 5xx / 400）：交共用内容拦截分类
    content_block::classify_or_fatal(status, error_body, message, upstream_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 判定顺序即契约：签名缺失**先于** 401/403（它本身也是 403）
    #[test]
    fn signature_missing_beats_auth_classification() {
        let class = classify_gateway_error(
            403,
            &json!({"message": "{\"detail\":{\"code\":\"ATOMCODE_SIG_MISSING\"}}"}),
        );
        // 取出来再断言（`matches!` 会消费掉 `class`）
        let (status, message) = match class {
            UpstreamErrorClass::Fatal { status, message, .. } => (status, message),
            other => (0, format!("非 Fatal：{other:?}")),
        };
        assert_eq!(status, 502);
        assert!(message.contains("api-ai.gitcode.com"));
    }

    /// 401 / 403：文案含 codingplan = 没领套餐（Fatal），否则 = 令牌过期
    #[test]
    fn auth_errors_split_on_codingplan() {
        assert!(matches!(
            classify_gateway_error(403, &json!({"message": "no codingplan for this account"})),
            UpstreamErrorClass::Fatal { status: 403, .. }
        ));
        assert!(matches!(
            classify_gateway_error(401, &json!({"message": "401 Unauthorized"})),
            UpstreamErrorClass::TokenExpired { .. }
        ));
        assert!(matches!(
            classify_gateway_error(403, &json!({"message": "forbidden"})),
            UpstreamErrorClass::TokenExpired { .. }
        ));
    }

    /// 429 → QuotaLimited（网关不给恢复时间，reset_at 恒 None）
    #[test]
    fn too_many_requests_is_quota_limited() {
        let class = classify_gateway_error(429, &json!({"message": "rate limited"}));
        assert!(matches!(
            class,
            UpstreamErrorClass::QuotaLimited { reset_at: None, status: 429, .. }
        ));
    }

    /// 其余 4xx 交共用内容拦截分类：审核文案 → ContentBlocked，否则 Fatal
    #[test]
    fn other_errors_delegate_to_content_block() {
        assert!(matches!(
            classify_gateway_error(
                400,
                &json!({"message": "Blocked by security policy: prompt"})
            ),
            UpstreamErrorClass::ContentBlocked { .. }
        ));
        assert!(matches!(
            classify_gateway_error(502, &json!({"message": "boom"})),
            UpstreamErrorClass::Fatal { status: 502, .. }
        ));
    }

    /// ⚠️ 静默失败：整段相等 / 前缀才判（`contains` 会误伤模型的正常复述）
    #[test]
    fn silent_failure_uses_equality_and_prefix() {
        assert_eq!(
            silent_failure_in_content("  参数错误 "),
            Some(SilentFailure::ParameterRejected)
        );
        assert_eq!(
            silent_failure_in_content("三方请求失败: 502 <html>x</html>"),
            Some(SilentFailure::ThirdPartyBroken)
        );
        // 模型正常解释这个词时**不能**判失败
        assert_eq!(silent_failure_in_content("参数错误通常指请求体不合法"), None);
        assert_eq!(silent_failure_in_content(""), None);
    }

    /// 静默失败的错误一律 502（请求本身没问题，是这次生成没成）
    #[test]
    fn silent_failure_errors_are_502() {
        assert_eq!(
            silent_failure_error(SilentFailure::ParameterRejected, "GLM-5.3-Flash").status_code,
            502
        );
        let error = silent_failure_error(SilentFailure::ThirdPartyBroken, "");
        assert_eq!(error.status_code, 502);
        assert!(error.message.contains("该请求"));
    }
}
