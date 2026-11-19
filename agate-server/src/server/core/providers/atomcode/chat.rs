//! AtomCode 的转发：**同方言透传**，本仓最薄的一层。
//!
//! ── 为什么这一层几乎什么都不做 ────────────────────────────────
//! 网关本身就是 OpenAI Chat Completions，所以客户端请求体几乎逐字节上行、
//! 上游的 SSE 帧逐字节下发。别家那套「按家翻译帧」在这里没有对应物 ——
//! 没有一条字段会在「没有发生的翻译」里丢掉，这正是走同方言上游的全部好处。
//! 剩下的只有格式本身表达不了的三件事：**身份、模型名映射、参数补全**
//! （外加静默失败的识别，那条在 [`super::errors`] 与 SSE 出口）。
//!
//! ── 只改两处，每处都有它非改不可的理由 ──────────────────────────
//!   1. `model`：客户端点的是**展示名**（目录里的 `GLM-5.3-Flash`），网关只认
//!      自己的 id（`glm5.3-flash`）。不做这一步，网关不会报错，它会回
//!      `HTTP 200 + 正文 参数错误`（见 [`super::errors`] 的模块头）——
//!      一个「模型名没映射」的 bug 会表现为「参数不被接受」。
//!      同时剥掉账号前缀（`<accountId>/<model>`，多账号下同名模型的消歧手段）。
//!   2. `stream_options`：官方客户端**无条件**发
//!      `{"include_usage": true}`，网关据此才吐尾部 usage 帧。客户端没指定时补上
//!      （客户端显式指定就让它赢）；非流式路径删掉它（没有流可挂）。
//!
//! 其余（messages / tools / tool_choice / temperature / max_tokens /
//! reasoning_effort / 图片分片）**一律不动**。
//!
//! ── User-Agent 是功能性的（再说一遍，因为它最容易被「清理」掉）────
//! 网关按 UA 决定是否索要闭源签名：自称 `atomcode/<ver>` 会立刻
//! `403 ATOMCODE_SIG_MISSING`。见 [`super::endpoints::ADAPTER_USER_AGENT`] 的实测表。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use axum::http::HeaderMap;
use serde_json::Value;

use crate::server::core::providers::adapter::ChatRequestPlan;
use crate::server::errors::GatewayError;

use super::endpoints::{gateway_chat_url, ADAPTER_USER_AGENT};
use super::models;

/// 账号前缀的分隔符（目录里同名模型的消歧形态：`<accountId>/<model>`）
const ACCOUNT_PREFIX_SEPARATOR: char = '/';

/// 剥掉账号前缀：`<account>/glm5.3-flash` → `glm5.3-flash`。
///
/// ⚠️ **只剥一层且只剥「已知账号 id」那一层**不能在这里做（本函数拿不到账号表），
/// 因此判据是「第一段看起来像账号 id」：目录里的模型名本身**不含** `/`
/// （网关的 id 形如 `glm5.3-flash` / `deepseek-flash`），所以出现 `/` 就说明
/// 那是我们自己的前缀。这条判据的代价写在这里：将来若网关开始下发带 `/` 的
/// 模型 id（如 `moonshotai/Kimi-K2-Instruct` 这种**含斜杠**的上游 id），
/// 本函数会误剥。届时改为「按调用方给的账号 id 精确剥」。
/// 现在不那样写是因为 `build_chat_request` 的 `account` 参数里没有账号 id
/// 的独立字段。
pub fn strip_account_prefix(model: &str) -> &str {
    let trimmed = model.trim();
    match trimmed.split_once(ACCOUNT_PREFIX_SEPARATOR) {
        Some((_prefix, rest)) if !rest.trim().is_empty() => rest.trim(),
        _ => trimmed,
    }
}

/// 准备一次上游调用。
pub fn build_chat_request(
    account: &Value,
    body: &Value,
    _client_headers: &HeaderMap,
) -> Result<ChatRequestPlan, GatewayError> {
    // 令牌读取口径与其余各家一致（会话形态的 `auth.accessToken`；顶层也认，
    // 手工构造的 account 形态没有 auth 包裹）
    let access_token = account
        .get("auth")
        .and_then(|auth| auth.get("accessToken"))
        .or_else(|| account.get("accessToken"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if access_token.is_empty() {
        return Err(GatewayError::with_status(
            401,
            "AtomCode 账号缺少访问令牌，无法转发（请重新登录）",
        ));
    }

    let requested = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if requested.is_empty() {
        return Err(GatewayError::with_status(400, "请求缺少 model 字段"));
    }
    // 客户端点的是展示名 / 带前缀名；网关只认自己的 id。
    let published = strip_account_prefix(requested);
    let upstream_model =
        models::upstream_id_for(published).unwrap_or_else(|| published.to_string());

    let mut payload = body.clone();
    let Some(object) = payload.as_object_mut() else {
        return Err(GatewayError::with_status(400, "请求体不是 JSON 对象"));
    };
    object.insert("model".to_string(), Value::String(upstream_model));
    let streaming = object
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if streaming {
        // 官方客户端无条件带它，网关据此才吐尾部 usage 帧；
        // 客户端显式指定过就不覆盖。
        if !object.contains_key("stream_options") {
            object.insert(
                "stream_options".to_string(),
                serde_json::json!({ "include_usage": true }),
            );
        }
    } else {
        // 非流式路径没有流可挂 usage，留着只会让上游多回一个字段
        object.remove("stream_options");
    }

    let headers: Vec<(String, String)> = vec![
        (
            "authorization".to_string(),
            format!("Bearer {access_token}"),
        ),
        ("accept".to_string(), "application/json".to_string()),
        // ⚠️ `Content-Type` 必须显式发：本仓的转发层用 `.body(bytes)`，
        // reqwest 的 `.body()` 不会自动补这个头（只有 `.json()` 会）。
        ("content-type".to_string(), "application/json".to_string()),
        // 功能性常量：自称官方客户端会触发 ATOMCODE_SIG_MISSING
        ("user-agent".to_string(), ADAPTER_USER_AGENT.to_string()),
    ];

    Ok(ChatRequestPlan::chat(gateway_chat_url(), headers, payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn account(token: &str) -> Value {
        json!({"auth": {"accessToken": token}})
    }

    /// 账号前缀只剥一层；没有前缀、或前缀后面是空的都原样返回
    #[test]
    fn account_prefix_is_stripped_once() {
        assert_eq!(strip_account_prefix("atomcode-user-1/glm5.3-flash"), "glm5.3-flash");
        assert_eq!(strip_account_prefix("glm5.3-flash"), "glm5.3-flash");
        // 「前缀后面是空的」不是前缀形态，原样保留（否则会把一个空模型名发出去）
        assert_eq!(strip_account_prefix("only-prefix/  "), "only-prefix/");
        assert_eq!(strip_account_prefix("  glm5.3-flash  "), "glm5.3-flash");
    }

    /// ⚠️ `stream_options` 的两条路：流式补、非流式删；客户端显式给了就不覆盖
    #[test]
    fn stream_options_follow_stream_flag() {
        let streaming = build_chat_request(
            &account("tok"),
            &json!({"model": "glm5.3-flash", "stream": true, "messages": []}),
            &HeaderMap::new(),
        )
        .ok()
        .map(|plan| plan.body)
        .unwrap_or(Value::Null);
        assert_eq!(
            streaming
                .get("stream_options")
                .and_then(|value| value.get("include_usage"))
                .and_then(Value::as_bool),
            Some(true)
        );
        // 客户端自己指定的值要保留
        let explicit = build_chat_request(
            &account("tok"),
            &json!({"model": "glm5.3-flash", "stream": true,
                    "stream_options": {"include_usage": false}}),
            &HeaderMap::new(),
        )
        .ok()
        .map(|plan| plan.body)
        .unwrap_or(Value::Null);
        assert_eq!(
            explicit
                .get("stream_options")
                .and_then(|value| value.get("include_usage"))
                .and_then(Value::as_bool),
            Some(false)
        );
        // 非流式：删掉
        let plain = build_chat_request(
            &account("tok"),
            &json!({"model": "glm5.3-flash", "stream": false,
                    "stream_options": {"include_usage": true}}),
            &HeaderMap::new(),
        )
        .ok()
        .map(|plan| plan.body)
        .unwrap_or(Value::Null);
        assert!(plain.get("stream_options").is_none());
    }

    /// 认不出的模型**原样发**（不猜：猜一个更接近的名字会把「名字没映射」
    /// 变成一次静默的错误重试）；其余字段不动
    #[test]
    fn unknown_model_is_sent_as_is_and_other_fields_untouched() {
        let plan = build_chat_request(
            &account("tok"),
            &json!({"model": "no-such-model-xyz", "temperature": 0.3,
                    "messages": [{"role": "user", "content": "hi"}]}),
            &HeaderMap::new(),
        )
        .ok()
        .map(|plan| plan.body)
        .unwrap_or(Value::Null);
        assert_eq!(
            plan.get("model").and_then(Value::as_str),
            Some("no-such-model-xyz")
        );
        assert_eq!(plan.get("temperature").and_then(Value::as_f64), Some(0.3));
        assert_eq!(
            plan.get("messages")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
    }

    /// 缺令牌 401、缺 model 400（都发生在网络请求之前）
    #[test]
    fn missing_token_or_model_is_rejected_early() {
        assert_eq!(
            build_chat_request(&json!({}), &json!({"model": "x"}), &HeaderMap::new())
                .err()
                .map(|error| error.status_code),
            Some(401)
        );
        assert_eq!(
            build_chat_request(&account("t"), &json!({}), &HeaderMap::new())
                .err()
                .map(|error| error.status_code),
            Some(400)
        );
    }
}
