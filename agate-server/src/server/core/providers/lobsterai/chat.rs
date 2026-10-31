//! LobsterAI 的转发：**同方言透传 + 四处上游强制的改写**。
//!
//! ── 为什么这一层也很薄 ──────────────────────────────────────
//! 对话端点是 OpenAI 兼容的原生协议（`POST /api/proxy/v1/chat/completions`），
//! 网关本身就是 OpenAI Chat Completions，所以 body 几乎逐字节上行、上游的 SSE
//! 帧逐字节下发。剩下的只有四件格式本身表达不了的事。
//!
//! ── 只改四处，每处都有它非改不可的理由（全部实测）────────────────
//!   1. **`role: "developer"` → `"system"`**：上游不认 OpenAI 新版角色，带它的
//!      请求被网关以 **HTTP 502「角色信息不正确」**拒绝（system / user 均 200）。
//!      两者在 OpenAI 规范里语义相同，按 `system` 下发。
//!      ⚠️ 本仓的通用转发层在「开发者角色兼容」开关打开时也会做同一件事 ——
//!      这里再写一遍不是冗余：开关关掉时它就是唯一的一道，且对已归一的体是**幂等**的。
//!   2. **`tool_choice` 在 `""` / `"none"` / `null` 时删掉**：客户端可能原样转发
//!      一个上游拒绝的取值。
//!   3. **绝不下发 `prompt_cache_key`**：那是腾讯后端的机制，本家不知道认不认；
//!      存在就删。
//!   4. **`model` 走一次名字解析**（展示名 / 规范化别名 → 线上 id）：认不出时
//!      **原样发**，不猜。通用转发层已按目录改写过一次，所以这一步通常幂等；
//!      留着它是为了「客户端直接点名一个规范化别名」那条路。
//!
//! ── 两处**刻意不做**的改写（都是实测结论）──────────────────────
//!   · **不动 `stream`**：本家 `stream:false` 上游直接回 HTTP 500，而本网关的
//!     转发入口「无论客户端要不要流式，上游恒以 `stream:true` 请求」——
//!     这条要求天然满足，**碰它反而会把它弄坏**。
//!   · **不转换图片**：上游只收 `{type:"image_url",image_url:{url}}`，收到
//!     `{type:"image"}` 会回 HTTP 500。我们**不做**形状修补 —— 让上游把它的真实
//!     错误如实报给客户端，好过我们替它猜一种可能不对的形状。
//!
//! ── 客户端版本头为什么要一个进程级快照 ──────────────────────────
//! `X-LobsterAI-Client-Version` 是功能性头（缺它 / 给空串会被按「不认识的客户端」
//! 对待），而版本号要**异步**从上游更新接口取（`version::resolve_client_version()`）。
//! 但 `ProviderAdapter::build_chat_request` 是**同步**的（trait 契约，不能改成
//! async），所以这里用一份进程级快照：异步侧（[`prime_client_version`]，由目录刷新 /
//! 签到 / 余额这些本来就要取版本的异步路径调用）写，同步侧（[`client_version_header`]）
//! 读；快照还没填时退回已知可用的兜底常量，**绝不发空版本头**。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::sync::{Mutex, OnceLock};

use axum::http::HeaderMap;
use serde_json::Value;

use crate::server::core::providers::adapter::ChatRequestPlan;
use crate::server::errors::GatewayError;

use super::endpoints::{chat_url, CLIENT_CAPABILITIES, USER_AGENT};
use super::{models, version};

/// 进程级版本快照（由 [`prime_client_version`] 写、[`client_version_header`] 读）
static CLIENT_VERSION: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn version_slot() -> &'static Mutex<Option<String>> {
    CLIENT_VERSION.get_or_init(|| Mutex::new(None))
}

/// 解析一次客户端版本并写进快照，返回解析结果。
///
/// **异步入口**：目录刷新 / 签到 / 余额查询这些异步路径都调它 —— 一次调用既拿到
/// 自己要用的版本，也顺手把 [`client_version_header`] 的快照填上（幂等，底层
/// `version::resolve_client_version` 自带 12 小时缓存）。
pub async fn prime_client_version() -> String {
    let resolved = version::resolve_client_version().await;
    store_client_version(&resolved);
    resolved
}

/// 写入快照（空串不覆盖：宁可留着上一次的好值，也不要为一次瞬时失败清空）
fn store_client_version(value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    let mut guard = match version_slot().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = Some(trimmed.to_string());
}

/// 同步读取 `X-LobsterAI-Client-Version` 的值。
///
/// 快照没填过（进程刚起、还没走过任何异步取版本的路径）时退回兜底常量 ——
/// **绝不发空版本头**（空串会被服务端按「不认识的客户端」对待）。
pub fn client_version_header() -> String {
    let cached = match version_slot().lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    match cached {
        Some(value) if !value.trim().is_empty() => value,
        _ => version::fallback_version().to_string(),
    }
}

/// 上游不认的 OpenAI 角色名（带它会被网关以 HTTP 502「角色信息不正确」拒绝）
const DEVELOPER_ROLE: &str = "developer";
/// 归一后的角色名（两者在 OpenAI 规范里语义相同）
const DEMOTED_ROLE: &str = "system";

/// 把 `role: "developer"` 的消息改成 `"system"`（原地改写，非对象条目原样放过）。
fn demote_developer_messages(object: &mut serde_json::Map<String, Value>) {
    let Some(messages) = object.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for message in messages.iter_mut() {
        let Some(entry) = message.as_object_mut() else {
            continue;
        };
        let is_developer = entry
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|role| role == DEVELOPER_ROLE);
        if is_developer {
            entry.insert("role".to_string(), Value::String(DEMOTED_ROLE.to_string()));
        }
    }
}

/// `tool_choice` 是否属于「上游会拒绝、必须删掉」的取值（`""` / `"none"` / `null`）
fn tool_choice_must_go(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty() || text.trim().eq_ignore_ascii_case("none"),
        _ => false,
    }
}

/// 准备一次上游调用。
pub fn build_chat_request(
    account: &Value,
    body: &Value,
    _client_headers: &HeaderMap,
) -> Result<ChatRequestPlan, GatewayError> {
    // 令牌读取口径与其余各家一致（会话形态的 `auth.accessToken`）
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
            "LobsterAI 账号缺少访问令牌，无法转发（请重新登录）",
        ));
    }

    let requested = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if requested.is_empty() {
        return Err(GatewayError::with_status(400, "请求缺少 model 字段"));
    }

    let mut payload = body.clone();
    let Some(object) = payload.as_object_mut() else {
        return Err(GatewayError::with_status(400, "请求体不是 JSON 对象"));
    };

    // ① developer → system（上游 502「角色信息不正确」的唯一成因）
    demote_developer_messages(object);
    // ② tool_choice：`""` / `"none"` / `null` 一律删掉
    if object.get("tool_choice").is_some_and(tool_choice_must_go) {
        object.remove("tool_choice");
    }
    // ③ 绝不下发 prompt_cache_key（腾讯后端机制，本家不认）
    object.remove("prompt_cache_key");
    // ④ 模型名解析：展示名 / 规范化别名 → 线上 id；认不出原样发
    let wire_model = models::upstream_id_for(&requested).unwrap_or_else(|| requested.clone());
    object.insert("model".to_string(), Value::String(wire_model));

    // `stream` **不动**：本网关的转发入口恒以 `stream:true` 请求上游，本家
    // `stream:false` 会回 HTTP 500 —— 碰它反而会把它弄坏。

    let headers: Vec<(String, String)> = vec![
        (
            "authorization".to_string(),
            format!("Bearer {access_token}"),
        ),
        // 只收 SSE：Accept 两个都报（照参考实现）
        (
            "accept".to_string(),
            "text/event-stream, application/json".to_string(),
        ),
        // ⚠️ `Content-Type` **必须显式发**：本仓的转发层用 `client.post(url).body(bytes)`，
        // reqwest 的 `.body()` **不会**自动补这个头 —— 只有 `.json()` 才会。剥掉它之后
        // 上游回的是**鉴权之后的** `{"code":500,"message":"服务器内部错误"}`（HTTP 500，
        // 约 46ms 即返），跨模型稳定复现，看上去像「网关坏了」而不是「少了个头」。
        (
            "content-type".to_string(),
            "application/json".to_string(),
        ),
        // 两个客户端头都是功能性的（能力位过滤模型、版本决定被当成哪个客户端）
        (
            "x-lobsterai-client-capabilities".to_string(),
            CLIENT_CAPABILITIES.to_string(),
        ),
        (
            "x-lobsterai-client-version".to_string(),
            client_version_header(),
        ),
        // UA 固定不跟随真实版本（照抄参考实现，不是装饰）
        ("user-agent".to_string(), USER_AGENT.to_string()),
    ];

    Ok(ChatRequestPlan::chat(chat_url(), headers, payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn account(token: &str) -> Value {
        json!({"auth": {"accessToken": token}})
    }

    /// developer → system（上游硬要求：带 developer 会被 502）
    #[test]
    fn developer_role_is_demoted() {
        let mut payload = json!({
            "model": "glm-5",
            "messages": [
                {"role": "developer", "content": "x"},
                {"role": "user", "content": "y"}
            ]
        });
        if let Some(object) = payload.as_object_mut() {
            demote_developer_messages(object);
        }
        let roles: Vec<&str> = payload
            .get("messages")
            .and_then(Value::as_array)
            .map(|messages| {
                messages
                    .iter()
                    .filter_map(|message| message.get("role").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(roles, vec!["system", "user"]);
    }

    /// tool_choice 的三种「必须删」取值 + 两种「保留」
    #[test]
    fn tool_choice_removal_rules() {
        assert!(tool_choice_must_go(&json!(null)));
        assert!(tool_choice_must_go(&json!("")));
        assert!(tool_choice_must_go(&json!("NONE")));
        assert!(!tool_choice_must_go(&json!("auto")));
        assert!(!tool_choice_must_go(&json!({"type": "function"})));
    }

    /// 请求体改写：developer 归一 + 删 tool_choice(null) + 删 prompt_cache_key
    #[test]
    fn request_body_rewrites_are_applied() {
        let body = json!({
            "model": "glm-5",
            "messages": [{"role": "developer", "content": "x"}],
            "tool_choice": null,
            "prompt_cache_key": "abc",
            "stream": false
        });
        let plan = build_chat_request(&account("tok"), &body, &HeaderMap::new());
        let plan = plan.map(|plan| plan.body).unwrap_or(Value::Null);
        assert_eq!(
            plan.get("messages")
                .and_then(Value::as_array)
                .and_then(|messages| messages.first())
                .and_then(|message| message.get("role"))
                .and_then(Value::as_str),
            Some("system")
        );
        assert!(plan.get("tool_choice").is_none());
        assert!(plan.get("prompt_cache_key").is_none());
        // `stream` 刻意不动（上游 stream:false 会 500，但入口恒 true，碰它反而坏）
        assert_eq!(plan.get("stream").and_then(Value::as_bool), Some(false));
    }

    /// 缺令牌 / 缺 model 各自报 401 / 400
    #[test]
    fn missing_token_or_model_errors() {
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

    /// 快照没填时版本头退兜底常量，**绝不发空串**
    #[test]
    fn version_header_never_empty() {
        store_client_version("2026.9.4");
        assert_eq!(client_version_header(), "2026.9.4");
        // 空串不覆盖好值
        store_client_version("");
        assert_eq!(client_version_header(), "2026.9.4");
    }
}