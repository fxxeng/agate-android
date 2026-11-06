//! 生图响应的**读取层**：把一家上游返回的东西读成统一的 `{created, data:[…]}`。
//!
//! ── 为什么不经 Chat 翻译器 ────────────────────────────────────
//! 对话链路的翻译器做的是「把上游对谈形态归一成 OpenAI 对话形态」，而图片
//! 响应根本没有对话结构（就是一组图 + 可选 usage）。硬塞进去的结果是每个
//! 形态都要在翻译器里开一条旁路 —— 所以这里独立一份，只回答一个问题：
//! 「这次响应里有几张图」。
//!
//! ── 形态从哪来、为什么由宿主侧认领 ────────────────────────────
//! provider 侧只交出「往哪发、发什么、响应该按哪种形态读」
//! （`providers::images::ImageRequestPlan`）；**读响应**这一半全在这里。
//! 理由是有些家的「读」不是读一包 JSON：
//!   - `ModelscopeAsync`：首包只有 `task_id`，要转 [`super::image_tasks`]
//!     轮询 `/v1/tasks/{id}`；
//!   - 后续会接的 Gemini（SSE 帧里的 inlineData）与豆包工作（对话流里取图、
//!     再换无水印原图）同理。这些都属于「按上游协议读」，与 provider 凭证
//!     无关，放在这里是唯一不漏的地方。
//!
//! ── 本模块**不**报销积分（与上游的差异，别照搬）──────────────
//! 上游在这里会 `report_usage({"credit": points_consumed})` 把消耗的积分
//! 记进报表。本仓的用量表只有 token 维度（`usage::extract_usage` 认的键里
//! 没有 credit），那行调用在这里是**静默无效的死代码**，还会让人以为积分已经
//! 记过账。所以本仓不写它，改为**把上游给的 `usage` / `points_consumed`
//! 原样随响应返回**（见 `api::image_output` 的保留键清单）—— 客户端与
//! `curl` 都能看到这次消耗了多少，报表侧则如实只有 token。

use std::time::Duration;

use futures::StreamExt;
use serde_json::{json, Value};

use crate::server::core::providers::images::ImageResponseKind;

use super::usage::RequestTelemetry;

/// 一次生图响应的失败：带建议 HTTP 状态 + 给用户看的 JSON 体。
///
/// 与 `GatewayError` 的区别：这里还不知道「该不该换号重试」（那是适配器按
/// 这家语义分类的事），只表达「这次没拿到图，原因是什么」。
pub(super) struct ImageFailure {
    pub status: u16,
    pub body: Value,
}

impl ImageFailure {
    pub(super) fn invalid(message: impl Into<String>) -> Self {
        Self {
            status: 502,
            body: json!({ "message": message.into() }),
        }
    }
}

/// 读取结果：要么还没读完（继续沿用底层响应），要么已经聚合好。
///
/// `Live` 只在流式形态（Gemini SSE）下被外部继续读；本仓已接的两种形态都是
/// 「读完就完」，所以这里保留形状是为了接第三种形态时不必改调用方。
pub(super) enum ImageResponse {
    Live(reqwest::Response),
    Complete(Value),
}

impl ImageResponse {
    pub fn status(&self) -> reqwest::StatusCode {
        match self {
            Self::Live(response) => response.status(),
            Self::Complete(_) => reqwest::StatusCode::OK,
        }
    }
}

/// 读一次成功响应（HTTP 2xx 才调到这里）。
///
/// 外面套了两层保护，缺一层都会变成「请求一直挂着」或「手动终止后还在读」：
///   - **总时长**受 `timeout_settings().body_ms()` 约束（与对话链路同一份配置）：
///     图片比文本大得多，没有上限时一个卡住的上游能把网关连接占满；
///   - **手动终止**：用户点了终止就直接收尾，不等上游把图吐完。
pub(super) async fn collect(
    response: reqwest::Response,
    kind: ImageResponseKind,
    transport: &super::request::TransportRequest,
    telemetry: &RequestTelemetry,
) -> Result<Value, ImageFailure> {
    let budget = Duration::from_millis(crate::server::config::timeout_settings().body_ms());
    let reading = collect_body(response, kind, transport, telemetry);
    let timed = async {
        tokio::time::timeout(budget, reading)
            .await
            .unwrap_or_else(|_| {
                Err(ImageFailure::invalid(format!(
                    "生图响应超时（{}秒），上游可能已消耗额度",
                    budget.as_secs()
                )))
            })
    };
    if let Some(token) = telemetry.cancel_token() {
        tokio::select! {
            result = timed => result,
            _ = token.cancelled() => Err(ImageFailure {
                status: super::cancellation::MANUAL_TERMINATED_STATUS as u16,
                body: json!({ "message": super::cancellation::MANUAL_TERMINATED }),
            }),
        }
    } else {
        timed.await
    }
}

async fn collect_body(
    response: reqwest::Response,
    kind: ImageResponseKind,
    transport: &super::request::TransportRequest,
    telemetry: &RequestTelemetry,
) -> Result<Value, ImageFailure> {
    /// 96 MiB：一张 4096² 的 PNG 约 30 MiB，`n=3` 的内联 b64 也在同一量级。
    /// 上限存在的意义是「坏掉的上游别把内存吃穿」，不是限制正常出图。
    const MAX_RESPONSE_BYTES: usize = 96 * 1024 * 1024;

    let status = response.status().as_u16();
    let capture = telemetry.capture();
    if let Some(capture) = capture.as_deref() {
        capture.attach_response(status, response.headers());
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    let mut total = 0usize;
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|_| ImageFailure::invalid("上游生图响应传输中断，上游可能已消耗额度"))?;
        total = total.saturating_add(chunk.len());
        if total > MAX_RESPONSE_BYTES {
            return Err(ImageFailure::invalid("上游生图响应超过 96 MiB 上限"));
        }
        if !chunk.is_empty() {
            telemetry.note_first_frame();
        }
        if let Some(capture) = capture.as_deref() {
            capture.push(&chunk);
        }
        bytes.extend_from_slice(&chunk);
    }
    let payload: Value = serde_json::from_slice(&bytes)
        .map_err(|_| ImageFailure::invalid("上游生图响应不是有效 JSON"))?;
    match kind {
        // 首包只是「任务已创建」：真正的图在 `/v1/tasks/{id}` 上，交给轮询。
        ImageResponseKind::ModelscopeAsync => {
            super::image_tasks::collect(&payload, transport, telemetry).await
        }
        ImageResponseKind::Openai => {
            if payload.get("error").is_some_and(|error| !error.is_null()) {
                return Err(failure_from(&payload, status));
            }
            let data = payload.get("data").and_then(Value::as_array);
            if data.is_some_and(|data| !data.is_empty()) {
                if let Some(usage) = payload.get("usage") {
                    telemetry.report_usage(usage);
                }
                return Ok(payload);
            }
            Err(failure_from(&payload, status))
        }
    }
}

/// 上游没给图（或给了错误）：把它的错误体折成一条能看懂的失败。
///
/// 状态码的取舍：HTTP 状态优先（那是真实发生的），否则按上游自报的
/// `status` 字符串映射（Gemini 系会这么给），再否则认 `code` 里的 4xx/5xx，
/// 最后一律 502 —— 「上游没给图」而不是「客户端请求错」。
fn failure_from(payload: &Value, http_status: u16) -> ImageFailure {
    let error = payload.get("error").unwrap_or(payload);
    let code = error.get("code").cloned().unwrap_or(Value::Null);
    let message = error
        .get("message")
        .or_else(|| error.get("msg"))
        .and_then(Value::as_str)
        .unwrap_or("上游没有返回有效的图片数据");
    let upstream_status = error.get("status").and_then(Value::as_str).unwrap_or("");
    let status = if http_status >= 400 {
        http_status
    } else {
        match upstream_status {
            "UNAUTHENTICATED" => 401,
            "PERMISSION_DENIED" => 403,
            "RESOURCE_EXHAUSTED" => 429,
            "INVALID_ARGUMENT" => 400,
            _ => code
                .as_u64()
                .filter(|code| (400..600).contains(code))
                .map(|code| code as u16)
                .unwrap_or(502),
        }
    };
    ImageFailure {
        status,
        body: json!({ "code": code, "message": format!("{upstream_status} {message}").trim() }),
    }
}
