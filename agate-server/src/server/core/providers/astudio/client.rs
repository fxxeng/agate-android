//! AStudio 的三套出网：Responses（流式）、模型目录（Bearer）、账号面（Cookie）。
//!
//! ── 账号面的信封 ────────────────────────────────────────────
//! `{flag: true, code: 0, data: …}` 才算成功。`flag: false`、非 0 的 `code`、
//! HTTP 401/403、`code: 80000`、文案里含「重新登录」都表示**网页会话失效**。
//! 这类错误必须透出去（账号行要显示「需要重新登录」），不能被折叠成一句
//! 「读取失败」—— 两者的处置完全不同：一个要用户重新粘贴，另一个只需重试。
//!
//! ── 三个入口，三种鉴权 ──────────────────────────────────────
//! ```text
//!   POST {模型域}/model-gateway/v1/responses      Authorization: Bearer <modelBearerToken>
//!   GET  {模型域}/api/v1/model-manager/models     Authorization: Bearer <modelBearerToken>
//!   GET  {账号域}points/balance 等                Cookie: ssoSessionId=…; account_id=…; token=…
//! ```
//! 前两个用**模型凭据**、第三个用**网页会话** —— 这是本家最容易被混起来的地方
//! （见 `credentials` 的模块头）。三个都收在本文件，调用方不再各自拼头。
//!
//! ── `studioVersion` 是账号面共用头（上游 3.4.2 修正）──────────
//! 旧结论是「账号面不发 clientType / studioVersion」，但上游 3.4.2 的
//! `client.rs::decorate` 给**所有账号面请求**都加了 `studioVersion: 3.5.1`：
//! 登录、余额、会员、每日登录共用同一份桌面端身份。`clientType` 仍只在
//! Windows / macOS 宿主发（21 / 22），Android 按上游同一分支不发。
//! 签到要靠 `POST init-app` 触发，与余额查询同属账号面，头必须一致。
//!
//! ── 超时 ────────────────────────────────────────────────────
//! 控制面（目录 / 账号面）用单请求 `.timeout(20s)`；`egress` 的客户端本身
//! **没有总超时**（总超时会掐断长回答，是踩过的坑），所以超时只能挂在这里。
//! 流式那条不设总超时，靠 `stall::idle_guard` 的空闲判定兜底。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::server::core::egress;
use crate::server::core::proxies::ResolvedProxy;
use crate::server::errors::GatewayError;

use super::credentials::Credentials;
use super::endpoints;
use super::errors;

/// 控制面（目录 / 账号面）单请求超时
pub const CONTROL_TIMEOUT_MS: u64 = 20_000;

/// 出站客户端（与全项目同一个出口缓存：代理 / 超时设置一改就换新的）
pub fn client(proxy: Option<&ResolvedProxy>) -> Arc<reqwest::Client> {
    egress::client_for(proxy)
}

/// reqwest 错误的人话描述（与其它家同一口径）
pub fn describe(error: &reqwest::Error) -> String {
    egress::describe_error_detail(error)
}

/// 账号面失败的两种形态（见模块头）
#[derive(Debug)]
pub enum StudioError {
    /// 网页会话失效（要用户重新登录 / 重贴）
    Expired(String),
    /// 其他失败（网络 / 5xx / 业务拒绝）
    Failed(String),
}

impl StudioError {
    pub fn message(&self) -> &str {
        match self {
            Self::Expired(message) | Self::Failed(message) => message,
        }
    }

    pub fn is_expired(&self) -> bool {
        matches!(self, Self::Expired(_))
    }
}

/// 账号面响应 → 数据段（**纯函数**，可离线测）。
///
/// 判据顺序是刻意的：先按 HTTP 状态判会话失效，再解信封。反过来（先解信封）
/// 遇到 401 带 HTML 错误页时会报「返回的不是 JSON」，把「该重新登录」说成
/// 「上游格式异常」—— 用户照着后者排查一辈子也修不好。
pub fn judge(status: u16, text: &str, what: &str) -> Result<Value, StudioError> {
    if status == 401 || status == 403 {
        return Err(StudioError::Expired(format!(
            "{what}失败：AStudio 拒绝了这份网页会话（HTTP {status}），请重新登录或重新粘贴凭据"
        )));
    }
    let payload = serde_json::from_str::<Value>(text).map_err(|_| {
        StudioError::Failed(format!("{what}失败：返回的不是 JSON（HTTP {status}）"))
    })?;
    let envelope = payload.as_object().cloned().unwrap_or_default();
    // `code` 可能是数字也可能是字符串（账号面两种都出现过）
    let code = envelope.get("code").and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
    });
    let flag_false = envelope.get("flag").and_then(Value::as_bool) == Some(false);
    let failed = !(200..300).contains(&status) || flag_false || code.is_some_and(|code| code != 0);
    if !failed {
        return Ok(envelope.get("data").cloned().unwrap_or(Value::Null));
    }
    let desc = envelope
        .get("desc")
        .or_else(|| envelope.get("message"))
        .or_else(|| envelope.get("msg"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    // 会话失效的三种说法：固定业务码 80000、文案里说「重新登录」、说「未登录」
    if code == Some(80_000)
        || desc.contains("重新登录")
        || desc.contains("登录已过期")
        || desc.contains("未登录")
    {
        return Err(StudioError::Expired(if desc.is_empty() {
            format!("{what}失败：AStudio 会话已失效，请重新登录")
        } else {
            format!("{what}失败：{desc}（请重新登录 AStudio）")
        }));
    }
    Err(StudioError::Failed(if desc.is_empty() {
        format!("{what}失败（HTTP {status}）")
    } else {
        format!("{what}失败（HTTP {status}：{desc}）")
    }))
}

/// 账号面请求共用的桌面端身份头（见模块头；`clientType` 在 Android 上不发）。
fn decorate(builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    builder.header("studioVersion", endpoints::STUDIO_VERSION)
}

/// 调一次账号面接口（网页会话 Cookie）。GET / POST 共用同一套头、超时与信封判定。
async fn studio_request(
    credentials: &Credentials,
    method: reqwest::Method,
    path: &str,
    what: &str,
    proxy: Option<&ResolvedProxy>,
) -> Result<Value, StudioError> {
    let url = format!(
        "{}{}",
        endpoints::STUDIO_API_BASE,
        path.trim_start_matches('/')
    );
    let builder = client(proxy)
        .request(method, &url)
        .header("Accept", "application/json")
        .header("Cookie", credentials.cookie_header())
        .timeout(Duration::from_millis(CONTROL_TIMEOUT_MS));
    let response = decorate(builder)
        .send()
        .await
        .map_err(|error| {
            StudioError::Failed(format!("{what}网络失败：{}", describe(&error)))
        })?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    judge(status, &text, what)
}

/// 读一次账号面接口（GET）。
pub async fn studio_json(
    credentials: &Credentials,
    path: &str,
    what: &str,
    proxy: Option<&ResolvedProxy>,
) -> Result<Value, StudioError> {
    studio_request(credentials, reqwest::Method::GET, path, what, proxy).await
}

/// 复用桌面端启动时的每日登录动作：带 Cookie 的 POST，**不附带请求体**。
///
/// 上游 3.4.2 实测「只读查询不发奖」，这个 POST 才触发当日登录奖励。
/// `banned` 是账号状态：`false` 才算动作执行成功；封禁与缺字段都不能
/// 被折叠成「余额没涨」—— 两者的处置完全不同。
pub async fn init_app(
    credentials: &Credentials,
    proxy: Option<&ResolvedProxy>,
) -> Result<(), StudioError> {
    let value = studio_request(
        credentials,
        reqwest::Method::POST,
        endpoints::INIT_APP,
        "每日登录",
        proxy,
    )
    .await?;
    match value.get("banned").and_then(Value::as_bool) {
        Some(false) => Ok(()),
        Some(true) => Err(StudioError::Failed("AStudio 账号已被封禁，无法签到".into())),
        None => Err(StudioError::Failed("每日登录返回的账号状态不完整".into())),
    }
}

/// 拉一次模型目录（模型凭据 Bearer）。
pub async fn catalog(
    key: &str,
    proxy: Option<&ResolvedProxy>,
) -> Result<Value, StudioError> {
    let what = "拉取 AStudio 模型目录";
    let response = client(proxy)
        .get(endpoints::MODEL_MANAGER_URL)
        .header("Authorization", format!("Bearer {key}"))
        .header("Accept", "application/json")
        .timeout(Duration::from_millis(CONTROL_TIMEOUT_MS))
        .send()
        .await
        .map_err(|error| StudioError::Failed(format!("{what}失败：{}", describe(&error))))?;
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return Err(StudioError::Expired(format!(
            "{what}失败：AStudio 拒绝了这份 modelBearerToken（HTTP {status}），请重新登录或重新粘贴"
        )));
    }
    let text = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        let (_, message) = errors::failure(status, &text);
        return Err(StudioError::Failed(format!("{what}失败：{message}")));
    }
    serde_json::from_str::<Value>(&text)
        .map_err(|_| StudioError::Failed(format!("{what}失败：返回的不是有效 JSON")))
}

/// 发一次 Responses 请求（流式）。
///
/// 非 2xx 直接折成网关错误：状态码按 [`errors::failure`] 归一（超长 → 400、
/// 凭据被拒 → 401、额度 → 429），编排层据此决定换不换号。
pub async fn responses(
    credentials: &Credentials,
    body: &Value,
    proxy: Option<&ResolvedProxy>,
) -> Result<reqwest::Response, GatewayError> {
    let response = client(proxy)
        .post(endpoints::RESPONSES_URL)
        .header("Authorization", format!("Bearer {}", credentials.key))
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .json(body)
        .send()
        .await
        .map_err(|error| {
            GatewayError::with_status(502, format!("请求 AStudio 失败：{}", describe(&error)))
        })?;
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(response);
    }
    let text = response.text().await.unwrap_or_default();
    let (code, message) = errors::failure(status, &text);
    Err(GatewayError::with_status(i32::from(code), message))
}

/// 读 Responses 的字节流（SSE），已接上空闲兜底
pub fn byte_stream(
    response: reqwest::Response,
    idle: Duration,
) -> std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send>,
> {
    use futures::StreamExt;
    let source = response
        .bytes_stream()
        .map(move |item| item.map_err(|error| std::io::Error::other(describe(&error))));
    crate::server::core::upstream::stall::idle_guard(Box::pin(source), idle)
}

/// 流内 / 兜底错误体（网关按 OpenAI chat 错误解析）
pub fn error_body(status: u16, message: &str) -> Value {
    json!({
        "error": {
            "type": if status == 429 { "rate_limit_error" } else { "api_error" },
            "message": message,
        }
    })
}

/// 一次性 id（`chatcmpl-` 后缀用）
pub fn message_id() -> String {
    super::super::atomcode::ids::uuid_v4().replace('-', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 成功信封 → 取 `data` 段（没有 data 时给 null，不是整个信封）
    #[test]
    fn a_good_envelope_yields_its_data() {
        let text = r#"{"flag":true,"code":0,"data":{"totalBalance":12.5}}"#;
        let data = judge(200, text, "积分余额").expect("应成功");
        assert_eq!(data.pointer("/totalBalance").and_then(Value::as_f64), Some(12.5));
        // 只有 null 数据不算失败
        let empty = judge(200, r#"{"flag":true,"code":0}"#, "会员信息").expect("应成功");
        assert!(empty.is_null());
    }

    /// `code` 是字符串 `"0"` 也要认（账号面两种都出现过）
    #[test]
    fn a_string_zero_code_is_success() {
        assert!(judge(200, r#"{"flag":true,"code":"0","data":{}}"#, "积分汇总").is_ok());
        let error = judge(200, r#"{"flag":true,"code":"80000","desc":"请重新登录"}"#, "会员信息")
            .expect_err("应失败");
        assert!(error.is_expired());
    }

    /// 会话失效的四种说法都归到 `Expired`（要用户重新登录，不是重试）
    #[test]
    fn session_expiry_is_recognised_in_every_shape() {
        let cases = [
            (401, r#"{"desc":"anything"}"#),
            (403, r#"{"desc":"anything"}"#),
            (200, r#"{"flag":false,"code":80000,"desc":"x"}"#),
            (200, r#"{"flag":false,"desc":"会话已过期，请重新登录"}"#),
            (200, r#"{"flag":false,"desc":"用户未登录"}"#),
        ];
        for (status, text) in cases {
            let error = judge(status, text, "积分余额").expect_err(text);
            assert!(error.is_expired(), "{status} {text} 应判成会话失效");
        }
    }

    /// 业务失败但不是会话问题 → `Failed`，并把上游文案带出来
    #[test]
    fn a_business_failure_keeps_the_upstream_wording() {
        let error = judge(200, r#"{"flag":false,"code":500,"desc":"服务繁忙"}"#, "积分余额")
            .expect_err("应失败");
        assert!(!error.is_expired());
        assert!(error.message().contains("服务繁忙"), "{}", error.message());
        // 非 JSON（网关前置层的 HTML 错误页）不能报成「格式异常」之外的东西
        let broken = judge(502, "<html>bad gateway</html>", "会员信息").expect_err("应失败");
        assert!(broken.message().contains("不是 JSON"), "{}", broken.message());
    }

    /// 错误体是 OpenAI chat 的形状（前端 / 客户端按它识别失败）
    #[test]
    fn the_error_body_looks_like_openai() {
        let body = error_body(429, "额度用完");
        assert_eq!(body.pointer("/error/type").and_then(Value::as_str), Some("rate_limit_error"));
        let body = error_body(502, "上游断了");
        assert_eq!(body.pointer("/error/type").and_then(Value::as_str), Some("api_error"));
        assert_eq!(body.pointer("/error/message").and_then(Value::as_str), Some("上游断了"));
    }

    /// 一次性 id 是 32 位 hex（chatcmpl 的后缀形态）
    #[test]
    fn message_ids_are_unique_hex() {
        let first = message_id();
        assert_eq!(first.len(), 32, "{first}");
        assert!(first.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_ne!(first, message_id());
    }
}
