//! Kiro 的出站 HTTP：控制面的两种调用形态、统一错误、字节流。
//!
//! ── 控制面有**两条**完全不同的形态，由凭证类型决定 ──────────
//! ```text
//!   API key   POST {management}/?origin=KIRO_CLI
//!             X-Amz-Target: <操作名>   tokentype: API_KEY
//!             Content-Type: application/x-amz-json-1.0
//!   OAuth     GET  {management}/<路径>?origin=KIRO_CLI&…[&profileArn=…]
//!             Authorization: Bearer …  [tokentype: EXTERNAL_IDP]
//! ```
//! 这两条不是「同一个接口的两种写法」，而是两套 API：API key 走的是 AWS
//! JSON 1.0（目标操作放 header、参数放 body），OAuth 走的是普通的 REST 查询
//! 参数。所以分派收在本模块的 [`Operation`] + [`control`] 里，目录与余额两处
//! 调用写的是同一个函数 —— 不然「某天给余额加个参数、只加在一条路上」这类
//! 分叉会立刻发生。
//!
//! ── 为什么复用 `egress::client_for` ──────────────────────────
//! 上游参考实现自建 client 是因为它带 20 秒**总超时**；本项目的 egress 客户端
//! 本来就没有总超时（总超时会把长回答掐断，是踩过的坑）。控制面要的那 20 秒
//! 在单请求上用 `.timeout()` 给，语义更准：只约束这一次控制面调用。

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::server::core::egress;
use crate::server::core::proxies::ResolvedProxy;
use crate::server::errors::GatewayError;

use super::credentials::KiroCredentials;
use super::endpoints;
use super::errors;
use super::eventstream::Source;
use super::CONTROL_TIMEOUT_MS;

/// 一次控制面操作。两种凭证形态的差异全部在这里登记。
pub struct Operation {
    /// OAuth 路线的路径段（`GET {management}/{path}`）
    pub path: &'static str,
    /// OAuth 路线的附加 query（含前导 `&`，空串表示没有）
    pub query: &'static str,
    /// API key 路线的 `X-Amz-Target`
    pub target: &'static str,
    /// 出错时的中文动作名（只用于文案）
    pub what: &'static str,
}

/// 可用模型清单（`models` 用）
pub const OP_MODELS: Operation = Operation {
    path: "List-Available-Models",
    query: "",
    target: endpoints::TARGET_KEY_MODELS,
    what: "拉取 Kiro 模型目录",
};

/// 余额（`usage` 用）
pub const OP_USAGE: Operation = Operation {
    path: "Get-Usage-Limits",
    query: "&resourceType=CREDIT&isEmailRequired=true",
    target: endpoints::TARGET_USAGE_LIMITS,
    what: "查询 Kiro 额度",
};

/// 出站客户端（与全项目同一个出口缓存：代理 / 超时设置一改就换新的）
pub fn client(proxy: Option<&ResolvedProxy>) -> Arc<reqwest::Client> {
    egress::client_for(proxy)
}

/// reqwest 错误的人话描述（与其它家同一口径）
pub fn describe(error: &reqwest::Error) -> String {
    egress::describe_error_detail(error)
}

/// RFC 4122 v4 形态的随机串（`amz-sdk-invocation-id` 用）
pub fn uuid_v4() -> String {
    super::super::atomcode::ids::uuid_v4()
}

/// 32 位十六进制的一次性 id（AWS SDK 的 `md/appVersion-…` 段与 chatcmpl id）
pub fn message_id() -> String {
    uuid_v4().replace('-', "")
}

/// 调一次控制面（`extra` 是 API key 路线要额外合进 body 的字段）。
pub async fn control(
    credentials: &KiroCredentials,
    op: Operation,
    extra: &Value,
    proxy: Option<&ResolvedProxy>,
) -> Result<Value, GatewayError> {
    let region = credentials.region();
    let client = client(proxy);
    let (status, text) = if credentials.is_api_key() {
        let url = format!("{}/?origin=KIRO_CLI", endpoints::management(&region));
        let mut body = json!({ "origin": "KIRO_CLI" });
        if let (Some(map), Some(patch)) = (body.as_object_mut(), extra.as_object()) {
            merge(map, patch);
        }
        let request = client
            .post(&url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-amz-json-1.0")
            .header("X-Amz-Target", op.target)
            .header("x-amzn-codewhisperer-optout", "false")
            .header("Authorization", format!("Bearer {}", credentials.access))
            .header("tokentype", "API_KEY")
            .timeout(Duration::from_millis(CONTROL_TIMEOUT_MS))
            .json(&body);
        read(request.send().await, op.what).await?
    } else {
        let mut url = format!("{}/{}?origin=KIRO_CLI", endpoints::management(&region), op.path);
        url.push_str(op.query);
        // profile ARN 是「这次查哪个订阅」的钥匙：缺了它 OAuth 账号会拿到
        // 一个空清单（上游实测），而带上它就必须整段编码（见 `query_escape`）
        if !credentials.profile.is_empty() {
            url.push_str("&profileArn=");
            url.push_str(&endpoints::query_escape(&credentials.profile));
        }
        let mut request = client
            .get(&url)
            .header("Accept", "application/json")
            .header("Authorization", format!("Bearer {}", credentials.access))
            .timeout(Duration::from_millis(CONTROL_TIMEOUT_MS));
        if let Some(kind) = credentials.token_type() {
            request = request.header("tokentype", kind);
        }
        read(request.send().await, op.what).await?
    };
    if !(200..300).contains(&status) {
        let (code, message) = errors::failure(status, &text);
        return Err(GatewayError::with_status(
            i32::from(code),
            format!("{}失败（HTTP {status}）：{message}", op.what),
        ));
    }
    serde_json::from_str(&text).map_err(|_| {
        GatewayError::with_status(502, format!("{}失败：返回的不是有效 JSON", op.what))
    })
}

/// 发出去 + 收正文（网络错误统一成 502）
async fn read(
    response: Result<reqwest::Response, reqwest::Error>,
    what: &str,
) -> Result<(u16, String), GatewayError> {
    let response = response.map_err(|error| {
        GatewayError::with_status(502, format!("{what}失败：{}", describe(&error)))
    })?;
    let status = response.status().as_u16();
    Ok((status, response.text().await.unwrap_or_default()))
}

/// 浅合并（同名覆盖；不做深合并 —— 这两个操作的 body 只有一层）
fn merge(target: &mut Map<String, Value>, patch: &Map<String, Value>) {
    for (key, value) in patch {
        target.insert(key.clone(), value.clone());
    }
}

/// 推理接口的一次请求：`POST {runtime}/generateAssistantResponse`。
///
/// 单独一条而不是并进 [`control`]：它返回的是**二进制事件流**、头也不一样
/// （`Accept: application/vnd.amazon.eventstream` + `x-amzn-kiro-agent-mode`），
/// 并进去只会让 `control` 多一堆分支。
pub async fn ask(
    credentials: &KiroCredentials,
    body: &Value,
    proxy: Option<&ResolvedProxy>,
) -> Result<reqwest::Response, GatewayError> {
    let region = credentials.region();
    let url = format!("{}{}", endpoints::runtime(&region), endpoints::CHAT_PATH);
    let user_agent = sdk_user_agent();
    let client = client(proxy);
    let mut request = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", credentials.access))
        .header("x-amzn-codewhisperer-optout", "true")
        .header("x-amzn-kiro-agent-mode", "vibe")
        .header("Content-Type", "application/json")
        .header("Accept", "application/vnd.amazon.eventstream")
        .header("amz-sdk-invocation-id", uuid_v4())
        .header("amz-sdk-request", "attempt=1; max=1")
        .header("x-amz-user-agent", &user_agent)
        .header("User-Agent", &user_agent)
        .json(body);
    if let Some(kind) = credentials.token_type() {
        request = request.header("tokentype", kind);
    }
    let response = request.send().await.map_err(|error| {
        GatewayError::with_status(502, format!("请求 Kiro 失败：{}", describe(&error)))
    })?;
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(response);
    }
    let text = response.text().await.unwrap_or_default();
    let (code, message) = errors::failure(status, &text);
    Err(GatewayError::with_status(i32::from(code), message))
}

/// AWS SDK 形态的 UA。
///
/// 尾部的 `md/appVersion-…-<id>` 是随请求变的：控制面按「这串像不像 SDK 自己
/// 发的」判来源，固定不变的尾段比版本段更容易被认出来。id 只要求每次不同，
/// 不承担别的语义。
pub fn sdk_user_agent() -> String {
    format!(
        "aws-sdk-rust/1.0.0 ua/2.1 os/other lang/rust \
         api/codewhispererstreaming#1.28.3 m/E app/AmazonQ-For-CLI md/appVersion-1.28.3-{}",
        message_id()
    )
}

/// 读推理接口的字节流（AWS event stream），已接上空闲兜底
pub fn byte_stream(response: reqwest::Response, idle: Duration) -> Source {
    use futures::StreamExt;
    let source = response.bytes_stream().map(move |item| {
        item.map_err(|error| std::io::Error::other(describe(&error)))
    });
    crate::server::core::upstream::stall::idle_guard(Box::pin(source), idle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两个操作的三处登记必须自洽（path / query / target 少一个就是死路）
    #[test]
    fn every_control_operation_is_fully_described() {
        for op in [OP_MODELS, OP_USAGE] {
            assert!(!op.path.is_empty(), "{} 缺 OAuth 路径", op.what);
            assert!(!op.target.is_empty(), "{} 缺 x-amz-target", op.what);
            assert!(op.target.contains('.'), "target 是 AWS JSON 1.0 的操作名");
            assert!(!op.what.is_empty());
            // 附加 query 要么空，要么以 `&` 开头（要拼在 `?origin=KIRO_CLI` 后面）
            assert!(op.query.is_empty() || op.query.starts_with('&'));
        }
    }

    /// 两条路线的 path 不同：API key 走的是「裸根路径 + 目标头」，
    /// OAuth 走的是 REST 路径 —— 混用会得到一个 400
    #[test]
    fn the_two_credential_styles_use_different_paths() {
        assert_eq!(OP_MODELS.path, "List-Available-Models");
        assert_eq!(OP_USAGE.path, "Get-Usage-Limits");
        assert_ne!(OP_MODELS.target, OP_USAGE.target);
        // 额度查询要带这两条 query（缺 resourceType 会回一堆无关的窗口）
        assert!(OP_USAGE.query.contains("resourceType=CREDIT"));
        assert!(OP_USAGE.query.contains("isEmailRequired=true"));
    }

    /// API key 的 body 是「origin + 调用方给的字段」浅合并
    #[test]
    fn the_api_key_body_is_merged_over_the_origin() {
        let mut body = json!({ "origin": "KIRO_CLI" });
        let patch = json!({ "nextToken": "abc", "origin": "IGNORED" });
        if let (Some(map), Some(extra)) = (body.as_object_mut(), patch.as_object()) {
            merge(map, extra);
        }
        assert_eq!(body.pointer("/origin").and_then(Value::as_str), Some("IGNORED"));
        assert_eq!(body.pointer("/nextToken").and_then(Value::as_str), Some("abc"));
    }

    /// 一次性 id 的形态：uuid 带连字符、message id 是 32 位 hex
    #[test]
    fn one_shot_ids_have_the_right_shapes() {
        let uuid = uuid_v4();
        assert_eq!(uuid.len(), 36, "{uuid}");
        assert_eq!(uuid.matches('-').count(), 4);
        assert_eq!(&uuid[14..15], "4", "v4 的版本位");
        let id = message_id();
        assert_eq!(id.len(), 32, "{id}");
        assert!(id.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_ne!(message_id(), message_id());
    }

    /// UA 的段落顺序与官方 SDK 一致（上游按它判来源），且尾部每次不同
    #[test]
    fn the_sdk_user_agent_matches_the_official_shape() {
        let agent = sdk_user_agent();
        assert!(agent.starts_with("aws-sdk-rust/1.0.0 ua/2.1 "), "{agent}");
        assert!(agent.contains("api/codewhispererstreaming#"));
        assert!(agent.contains("app/AmazonQ-For-CLI"));
        assert!(agent.contains("md/appVersion-1.28.3-"));
        assert_ne!(agent, sdk_user_agent(), "尾部的一次性 id 必须每次都变");
    }
}
