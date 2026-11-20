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
//!   - `Gemini`：图在 **SSE 帧**里（`candidates[].content.parts[].inlineData`），
//!     要逐帧解析、累积、再等 `finishReason` 才敢说收齐；
//!   - 后续会接的豆包工作（对话流里取图、再换无水印原图）同理。这些都属于
//!     「按上游协议读」，与 provider 凭证无关，放在这里是唯一不漏的地方。
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
#[derive(Debug)]
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
    // ── 流还是整包：由**上游实际回的 content-type** 决定，不由 kind 决定 ──
    // Gemini 用 `:streamGenerateContent?alt=sse`，但上游在某些情形下（例如
    // 网关层改写了端点）会回一整包 JSON。按 kind 硬走 SSE 会把一个完全正常的
    // JSON 响应按行切开、逐行 `serde_json` 解析 —— 症状是「上游出图了，网关说
    // 响应不是有效 JSON」。所以判据放在 content-type 上，两条路后面汇合。
    let sse = kind == ImageResponseKind::Gemini
        && response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"));

    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    let mut lines = super::super::protocol::SseLineBuffer::new();
    let mut images: Vec<Value> = Vec::new();
    let mut usage: Option<Value> = None;
    // `finished`：上游明确说过「这一轮结束了」（Gemini 的 `finishReason=STOP`）。
    // 语义只在 SSE 分支里有意义，普通 JSON 分支不看它。
    let mut finished = false;
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
        if !sse {
            bytes.extend_from_slice(&chunk);
            continue;
        }
        for payload in lines.push(&chunk) {
            let Some(payload) = payload else {
                // `[DONE]`：与 `finishReason=STOP` 同义，但两者未必都出现。
                finished = true;
                continue;
            };
            let frame: Value = serde_json::from_str(&payload)
                .map_err(|_| ImageFailure::invalid("上游生图 SSE 含不完整的 JSON 帧"))?;
            gemini_frame(&frame, &mut images, &mut usage, &mut finished)?;
        }
    }
    if sse {
        for payload in lines.finish() {
            let Some(payload) = payload else {
                finished = true;
                continue;
            };
            let frame: Value = serde_json::from_str(&payload)
                .map_err(|_| ImageFailure::invalid("上游生图 SSE 被截断"))?;
            gemini_frame(&frame, &mut images, &mut usage, &mut finished)?;
        }
        // ── 没有收尾信号就不算成功 ─────────────────────────────────
        // 流断了但没有 `STOP` / `[DONE]`：最后一张图可能只到一半。截断的 PNG
        // 解码出来是半张图，用户看到的是「生成了一张奇怪的东西」—— 比一个明确
        // 的失败难判断得多，而且这次额度已经扣了。宁可报错让他重试。
        if !finished {
            return Err(ImageFailure::invalid(
                "上游生图流没有正常收尾，未将可能截断的图片当作成功：请重试",
            ));
        }
        return finish_images(images, usage, telemetry);
    }

    let payload: Value = serde_json::from_slice(&bytes)
        .map_err(|_| ImageFailure::invalid("上游生图响应不是有效 JSON"))?;
    match kind {
        // 首包只是「任务已创建」：真正的图在 `/v1/tasks/{id}` 上，交给轮询。
        ImageResponseKind::ModelscopeAsync => {
            super::image_tasks::collect(&payload, transport, telemetry).await
        }
        ImageResponseKind::Workbuddy => workbuddy_body(&payload, status),
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
        // 非流式的 Gemini 响应：整包就是一帧（`candidates` 直接在顶层）。
        ImageResponseKind::Gemini => {
            gemini_frame(&payload, &mut images, &mut usage, &mut finished)?;
            finish_images(images, usage, telemetry)
        }
        // 豆包工作：**对话流里取图**。它的流不是 `data:` 帧的标准 SSE 形状吗？——
        // 是，但它还要「按对象键合并两种块」与「换无水印原图」两步，所以走
        // 单独一条路（`read_doubao`）。这里传的是**已读到的全部字节**，
        // 不是单帧 —— 它自己按 `\n\n` 分块。
        ImageResponseKind::Doubao => doubao_body(&bytes, transport, telemetry).await,
    }
}

/// Gemini 系（`Gemini`）的收尾：把累积到的图折成标准形态。
///
/// 「一张都没拿到」单独给一条文案：Gemini 的纯文字回复也是 200 + `STOP`，
/// 把它当成功，客户端会显示「生成完成但空白」。
fn finish_images(
    images: Vec<Value>,
    usage: Option<Value>,
    telemetry: &RequestTelemetry,
) -> Result<Value, ImageFailure> {
    if images.is_empty() {
        return Err(ImageFailure::invalid(
            "上游没有返回生成图片（可能被内容策略拦截），未将文字回复当作生图成功",
        ));
    }
    if let Some(usage) = &usage {
        telemetry.report_usage(usage);
    }
    let mut result = json!({
        "created": crate::server::logging::now_ms() / 1000,
        "data": images,
    });
    if let Some(usage) = usage {
        result["usage"] = usage;
    }
    Ok(result)
}

/// 读一帧 Gemini 响应（SSE 的一帧，或整包 JSON）。
///
/// ── 三个容易漏的点 ──────────────────────────────────────────
/// 1. **信封可能多一层 `response`**：Antigravity 的 v1internal 把 Gemini 的
///    原生响应包在 `{"response": {...}}` 里，而直连 Gemini 的响应没有这层壳。
///    所以先取 `response`，取不到就用整帧 —— 两边都能读。
/// 2. **`thought: true` 的 part 是草稿图**：Gemini 3 的思考过程也会带
///    `inlineData`，不跳过就会把思考阶段的草图当成结果交出去。
/// 3. **失败也走 200**：内容策略拦截在流里表现为 `finishReason=SAFETY`
///    或 `promptFeedback.blockReason`，HTTP 状态是正常的。不在这里转成错误，
///    用户会拿到一个空的 `data: []`。
fn gemini_frame(
    frame: &Value,
    images: &mut Vec<Value>,
    usage: &mut Option<Value>,
    finished: &mut bool,
) -> Result<(), ImageFailure> {
    let response = frame.get("response").unwrap_or(frame);
    if response.get("error").is_some_and(|error| !error.is_null()) {
        return Err(failure_from(response, 200));
    }
    if frame.get("error").is_some_and(|error| !error.is_null()) {
        return Err(failure_from(frame, 200));
    }
    if let Some(metadata) = response
        .get("usageMetadata")
        .or_else(|| response.get("usage_metadata"))
    {
        let input = metadata
            .get("promptTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output = metadata
            .get("candidatesTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let thought = metadata
            .get("thoughtsTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        *usage = Some(json!({
            "input_tokens": input,
            "output_tokens": output.saturating_add(thought),
            "total_tokens": metadata
                .get("totalTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(input.saturating_add(output).saturating_add(thought)),
        }));
    }
    let Some(candidates) = response.get("candidates").and_then(Value::as_array) else {
        // 没有 candidates 又有 blockReason ⇒ 提示词被拦了（这时还没出图，
        // 也就没有候选可说）。
        if response.pointer("/promptFeedback/blockReason").is_some() {
            return Err(ImageFailure {
                status: 400,
                body: json!({
                    "code": "content_policy_violation",
                    "message": "上游内容策略拦截了生图提示词",
                }),
            });
        }
        return Ok(());
    };
    for candidate in candidates {
        let reason = candidate.get("finishReason").and_then(Value::as_str);
        if reason == Some("STOP") {
            *finished = true;
        }
        if let Some(parts) = candidate
            .pointer("/content/parts")
            .and_then(Value::as_array)
        {
            for part in parts {
                if part.get("thought").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                let Some(inline) = part.get("inlineData").or_else(|| part.get("inline_data"))
                else {
                    continue;
                };
                let Some(data) = inline
                    .get("data")
                    .and_then(Value::as_str)
                    .filter(|data| !data.is_empty())
                else {
                    continue;
                };
                let mime = inline
                    .get("mimeType")
                    .or_else(|| inline.get("mime_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !mime.starts_with("image/") {
                    continue;
                }
                // 同一张图可能被重复下发（重连 / 多候选），按内容去重。
                if images
                    .iter()
                    .any(|image| image.get("b64_json").and_then(Value::as_str) == Some(data))
                {
                    continue;
                }
                images.push(json!({ "b64_json": data, "mime_type": mime }));
            }
        }
        if reason.is_some_and(|reason| {
            matches!(reason, "SAFETY" | "IMAGE_SAFETY" | "PROHIBITED_CONTENT")
        }) {
            return Err(ImageFailure {
                status: 400,
                body: json!({
                    "code": "content_policy_violation",
                    "message": "上游内容策略拦截了图片输出",
                }),
            });
        }
    }
    Ok(())
}

/// WorkBuddy 的生图响应：多一层信封，**成败看 `code` 而不是 HTTP 状态**。
///
/// ```text
/// { "code": 0, "msg": "ok", "request_id": "…",
///   "data": { "data": [ {"url"|"b64_json": …} ], "usage": { "credit": 3 } } }
/// ```
///
/// 为什么不能当 OpenAI 形态读：顶层没有 `data` 数组（`data` 是个对象），
/// 硬读会落到「上游没给图」那条分支 —— 明明出图了却报失败，而且日志里看到
/// 的是一个完全正常的 200 响应。
///
/// 积分（`usage.credit`）随响应原样带出，不单独上报：理由见模块头。
fn workbuddy_body(payload: &Value, http_status: u16) -> Result<Value, ImageFailure> {
    let code = payload.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code != 0 {
        return Err(failure_from(payload, http_status));
    }
    let envelope = payload.get("data").filter(|data| data.is_object());
    let data = envelope
        .and_then(|data| data.get("data"))
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
        .ok_or_else(|| failure_from(payload, http_status))?;
    let mut result = json!({
        "created": crate::server::logging::now_ms() / 1000,
        "data": data,
    });
    if let Some(usage) = envelope.and_then(|data| data.get("usage")) {
        result["usage"] = usage.clone();
    }
    Ok(result)
}

/// 豆包工作的生图响应：从**对话流**里取图，再换无水印原图。
///
/// ── 两种块都要收（这条决定了能不能拿到原图）──────────────────
/// `10010`（`gen_image_block`，交付结果）给的是**带水印的预览**；
/// `10084`（`rich_media_layout_block`，画布渲染）给的是**无水印原图**
/// （`image_ori_raw`）与对象键。只认前者就只能拿到带「AI 生成」角标那张；
/// 只认后者则在画布块缺失（旧版本 / 某些账号）时什么都没有。所以两边都收，
/// 按**对象键**合并 —— 两种块报的是同一张图。
///
/// ── 为什么要按对象键合并而不是按 url ──────────────────────────
/// 同一条 `10010` 与 `10084` 给的并不是同一个字符串（一个是带 tplv 水印模板的
/// 预览地址，一个是原图地址），但它们指向同一个对象。对象键是**去掉 host 与
/// 模板后缀**的那一段（见 [`doubao_object_key`]），两张块唯一的公共身份。
async fn doubao_body(
    bytes: &[u8],
    transport: &super::request::TransportRequest,
    // 不读用量：豆包这条流里没有 token / 积分字段（对话链路的 `usage` 也不在这
    // 条路的信封上），硬编一个 0 会让报表里出现「用了 0 个 token」的假数据。
    _telemetry: &RequestTelemetry,
) -> Result<Value, ImageFailure> {
    let text = String::from_utf8_lossy(bytes);
    let found = doubao_images(&text);
    if found.is_empty() {
        // 上游可能是直接报错（风控 / 参数）：把它的错误摘出来，别报成
        // 「没有生成图片」。两种形态都看 —— 流内 `data:` 帧，以及整段就是一个
        // JSON 对象（有些失败分支不走 SSE）。
        let failure = doubao_stream_failure(&text).or_else(|| {
            serde_json::from_str::<Value>(text.trim())
                .ok()
                .and_then(|value| doubao_value_failure(&value))
        });
        if let Some(failure) = failure {
            return Err(failure);
        }
        return Err(ImageFailure::invalid(
            "豆包没有返回图片（可能被内容策略拦截，或该账号的图像生成不可用），\
             未把文字回复当作生图成功",
        ));
    }
    // 流里通常已经带了无水印原图；只有缺了才补一次额外请求（能省就省）。
    let missing: Vec<DoubaoImage> = found
        .iter()
        .filter(|image| image.raw.is_none())
        .cloned()
        .collect();
    let cleaned = if missing.is_empty() {
        Vec::new()
    } else {
        doubao_without_watermark(transport, &missing).await
    };
    let data: Vec<Value> = found
        .iter()
        .map(|image| {
            let url = image
                .raw
                .clone()
                .or_else(|| {
                    cleaned
                        .iter()
                        .find(|(key, _)| key == &image.key)
                        .map(|(_, url)| url.clone())
                })
                // 最后退回预览：有图总比报错好（只在两种来源都缺时发生）。
                .unwrap_or_else(|| image.preview.clone());
            json!({ "url": url })
        })
        .collect();
    Ok(json!({
        "created": crate::server::logging::now_ms() / 1000,
        "data": data,
    }))
}

/// 流里收到的一张图（两种块报的是同一张，按对象键合并）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct DoubaoImage {
    /// 对象键（`tos-cn-i-…/rc_gen_image/x.jpeg`）——换原图那个接口收的就是它。
    key: String,
    /// 无水印原图（`10084` 的 `image_ori_raw`）。
    ///
    /// 实测（2026-11-03）同一张图：流里这份 `image_raw` 与官方
    /// `/creativity/resource/get_without_watermark` 返回的 `cgen_wm0` **字节
    /// 完全相同**（4624288 字节、同一 md5）。所以正常情况下一次请求就够，
    /// [`doubao_without_watermark`] 只服务「流里没给」的兜底情形。
    raw: Option<String>,
    /// 带水印的预览（`10010` 的 `uri` / `10084` 的 `image_ori`），最后的兜底。
    preview: String,
}

/// 按对象键把一张图并进结果（先到的补预览、后到的补原图，两种顺序都成立）。
fn doubao_merge(
    found: &mut Vec<DoubaoImage>,
    key: String,
    raw: Option<String>,
    preview: Option<String>,
) {
    if let Some(known) = found.iter_mut().find(|known| known.key == key) {
        if known.raw.is_none() {
            known.raw = raw;
        }
        if known.preview.is_empty() {
            known.preview = preview.unwrap_or_default();
        }
        return;
    }
    found.push(DoubaoImage {
        key,
        raw,
        preview: preview.unwrap_or_default(),
    });
}

/// 从豆包 SSE 文本里取图片（按对象键去重、保持先后）。
fn doubao_images(text: &str) -> Vec<DoubaoImage> {
    let mut found: Vec<DoubaoImage> = Vec::new();
    for data in doubao_data_frames(text) {
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        let mut blocks: Vec<&Value> = Vec::new();
        // 增量帧：`patch_op[].patch_value.content_block`（`patch_object == 1`）
        for patch in value
            .get("patch_op")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if patch.get("patch_object").and_then(Value::as_u64) != Some(1) {
                continue;
            }
            blocks.extend(
                patch
                    .pointer("/patch_value/content_block")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten(),
            );
        }
        // 全量帧：两种挂法都见过
        for holder in [&value["content"]["content_block"], &value["message"]["content_block"]] {
            blocks.extend(holder.as_array().into_iter().flatten());
        }
        for block in blocks {
            match block.get("block_type").and_then(Value::as_u64) {
                Some(10010) => {
                    for image in block
                        .pointer("/content/gen_image_block/images")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let Some(url) = ["uri", "thumb_url"].iter().find_map(|key| {
                            image[*key].as_str().filter(|value| !value.trim().is_empty())
                        }) else {
                            continue;
                        };
                        let Some(key) = doubao_object_key(url) else { continue };
                        doubao_merge(&mut found, key, None, Some(url.to_string()));
                    }
                }
                Some(10084) => {
                    for media in block
                        .pointer("/content/rich_media_layout_block/media")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let image = &media["creation"]["image"];
                        let key = image["key"]
                            .as_str()
                            .filter(|value| !value.trim().is_empty())
                            .map(str::to_string)
                            .or_else(|| {
                                ["image_ori", "image_preview", "image_thumb"].iter().find_map(
                                    |field| doubao_object_key(image[*field]["url"].as_str()?),
                                )
                            });
                        let Some(key) = key else { continue };
                        let raw = image["image_ori_raw"]["url"]
                            .as_str()
                            .filter(|value| !value.trim().is_empty())
                            .map(str::to_string);
                        let preview = ["image_ori", "image_preview", "image_thumb"]
                            .iter()
                            .find_map(|field| {
                                image[*field]["url"]
                                    .as_str()
                                    .filter(|value| !value.trim().is_empty())
                            })
                            .map(str::to_string);
                        doubao_merge(&mut found, key, raw, preview);
                    }
                }
                _ => {}
            }
        }
    }
    found
}

/// 图片地址 → 对象键。
///
/// 地址形如 `https://host/tos-cn-i-…/rc_gen_image/x.jpeg~tplv-…?…`：对象键是
/// `~` 之前、去掉协议与 host 的那一段。换原图那个接口收的就是它。
fn doubao_object_key(url: &str) -> Option<String> {
    let without_query = url.split('?').next().unwrap_or(url);
    let path = without_query.split('~').next().unwrap_or(without_query);
    let key = path
        .strip_prefix("https://")
        .or_else(|| path.strip_prefix("http://"))
        .map(|rest| rest.split_once('/').map(|(_, path)| path).unwrap_or(""))
        .unwrap_or(path);
    (!key.is_empty()).then(|| key.to_string())
}

/// 从 SSE 文本里取 `data:` 载荷（不看 `event:` 名字 —— 这里只看内容）。
fn doubao_data_frames(text: &str) -> Vec<String> {
    let mut frames = Vec::new();
    for block in text.split("\n\n") {
        let mut data: Vec<&str> = Vec::new();
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                data.push(rest.strip_prefix(' ').unwrap_or(rest));
            }
        }
        if !data.is_empty() {
            frames.push(data.join("\n"));
        }
    }
    frames
}

/// 流里没有图片时，多半是上游直接报了错：逐帧取错误，让用户看到原因。
fn doubao_stream_failure(text: &str) -> Option<ImageFailure> {
    doubao_data_frames(text)
        .iter()
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .find_map(|value| doubao_value_failure(&value))
}

/// 一个豆包失败体 → 归一化失败。
///
/// ── 只认豆包自己的错误键（`error_code` / `error_msg` / `error_message` /
/// `detail`）────────────────────────────────────────────────
/// 不去认通用的 `message` / `msg`：**正常流里也有 `message` 这个对象字段**，
/// 认宽了会把一次正常但没出图的答复报成上游故障。这是实测踩出来的。
fn doubao_value_failure(value: &Value) -> Option<ImageFailure> {
    let code = value.get("error_code").and_then(Value::as_u64);
    let message = ["error_msg", "error_message", "detail"]
        .iter()
        .find_map(|key| value[*key].as_str().filter(|text| !text.trim().is_empty()));
    if code.is_none() && message.is_none() {
        return None;
    }
    let message = message.unwrap_or("上游返回了无法解析的错误");
    if code == Some(710_022_004) {
        // 人机验证：这一条必须给出**能照做**的下一步，否则用户只会反复重试。
        return Some(ImageFailure {
            status: 429,
            body: json!({
                "message": "豆包工作生图触发了上游人机验证：请在该账号的豆包网页端发一条消息完成\
                            验证后重试；也可以等待约 30 分钟或更换出口网络再试",
                "error_code": 710_022_004,
            }),
        });
    }
    Some(ImageFailure {
        status: 502,
        body: json!({ "message": format!("豆包工作生图失败：{message}"), "error_code": code }),
    })
}

/// 拿对象键换无水印原图地址（对象键 → 地址）。
///
/// 失败一律返回空表（调用方退回预览地址）：这一步只是「锦上添花」，
/// 不该因为它把已经生成好的图整条丢掉。
async fn doubao_without_watermark(
    transport: &super::request::TransportRequest,
    images: &[DoubaoImage],
) -> Vec<(String, String)> {
    let Ok(base) = url::Url::parse(&transport.url) else {
        return Vec::new();
    };
    let Ok(mut target) = base.join("/creativity/resource/get_without_watermark") else {
        return Vec::new();
    };
    // 查询串要照搬：那几个 client 参数（device_id / pc_version / fp …）是接口
    // 认「这个账号与这台设备」的依据，缺了就会被当成未受信客户端。
    target.set_query(base.query());
    let keys: Vec<&str> = images.iter().map(|image| image.key.as_str()).collect();
    let client = crate::server::core::egress::client_without_redirects(transport.proxy.as_ref());
    let mut request = client
        .post(target)
        .timeout(Duration::from_secs(60))
        .json(&json!({ "uri": keys }));
    for (name, value) in &transport.headers {
        // `content-length` / `content-type` 交给 reqwest 按 `.json()` 自己写：
        // 照搬会多出第二条同名字段（父请求的长度还是那份 SSE 的）。
        if name.eq_ignore_ascii_case("content-length") || name.eq_ignore_ascii_case("content-type") {
            continue;
        }
        request = request.header(name, value);
    }
    let response = match request
        .header("Accept", "application/json, text/plain, */*")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            crate::server::logging::verbose(
                "[Upstream]",
                &format!("豆包换无水印原图失败，改用预览地址：{error}"),
            );
            return Vec::new();
        }
    };
    let status = response.status().as_u16();
    let Ok(value) = response.json::<Value>().await else {
        crate::server::logging::verbose("[Upstream]", "豆包换无水印原图的响应不是 JSON，改用预览地址");
        return Vec::new();
    };
    if !(200..300).contains(&status) || value.get("code").and_then(Value::as_i64).unwrap_or(0) != 0 {
        crate::server::logging::verbose(
            "[Upstream]",
            &format!("豆包换无水印原图被拒（HTTP {status}），改用预览地址"),
        );
        return Vec::new();
    }
    let data = value.get("data").unwrap_or(&Value::Null);
    if data.get("without_watermark").and_then(Value::as_bool) != Some(true) {
        crate::server::logging::verbose("[Upstream]", "该账号没有无水印权益，改用预览地址");
        return Vec::new();
    }
    let mut found = Vec::new();
    for key in ["download_image", "preview_image"] {
        for (uri, entry) in data
            .get(key)
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let Some(url) = entry.get("url").and_then(Value::as_str).filter(|url| !url.is_empty())
            else {
                continue;
            };
            found.push((uri.clone(), url.to_string()));
        }
        if !found.is_empty() {
            break;
        }
    }
    found
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


#[cfg(test)]
mod tests {
    use super::*;

    /// 信封形态的成败看 `code` 而不是 HTTP 状态：上游把失败也包在 200 里。
    ///
    /// 这条不能靠「顶层有没有 data 数组」判 —— 信封的 `data` 是个对象，
    /// 按 OpenAI 形态读会一路走到「上游没给图」那条分支，日志里是一个完全
    /// 正常的 200，用户拿到的却是一句失败。
    #[test]
    fn workbuddy_envelope_is_unwrapped_not_read_as_openai() {
        let payload = json!({
            "code": 0,
            "msg": "ok",
            "request_id": "r-1",
            "data": {
                "data": [{ "url": "https://example.com/a.png" }],
                "usage": { "credit": 3 }
            }
        });
        let body = workbuddy_body(&payload, 200).expect("应当读出图");
        assert_eq!(body["data"][0]["url"], json!("https://example.com/a.png"));
        // 积分随响应带出（本仓不单独上报，见模块头）
        assert_eq!(body["usage"]["credit"], json!(3));
    }

    #[test]
    fn workbuddy_error_code_wins_even_on_http_200() {
        let payload = json!({ "code": 6004, "msg": "使用量已超出频率限制" });
        let failure = workbuddy_body(&payload, 200).expect_err("code != 0 就是失败");
        assert!(failure.body["message"].as_str().unwrap_or("").contains("频率限制"));
    }

    /// 信封里没有图（只有文字回复 / 被策略拦截）时不能当成功：客户端会拿到
    /// 一个空的 `data: []` 并显示「生成完成但空白」。
    #[test]
    fn workbuddy_empty_envelope_is_a_failure() {
        let payload = json!({ "code": 0, "data": { "data": [] } });
        assert!(workbuddy_body(&payload, 200).is_err());
        let payload = json!({ "code": 0, "data": {} });
        assert!(workbuddy_body(&payload, 200).is_err());
    }

    /// Gemini 帧：图在 `inlineData`，`thought` part 是草稿图不算结果。
    ///
    /// 这条挡的是「把思考阶段的草图当成交付图」——Gemini 3 的思考过程也带
    /// `inlineData`，不跳过的话用户会拿到一张没画完的图，而且没有任何提示。
    #[test]
    fn gemini_frame_reads_inline_images_and_skips_thoughts() {
        let frame = json!({
            "response": {
                "candidates": [{
                    "content": { "parts": [
                        { "thought": true, "inlineData": { "mimeType": "image/png", "data": "DRAFT" } },
                        { "inlineData": { "mimeType": "image/png", "data": "REAL" } },
                        { "text": "已经画好了" }
                    ]},
                    "finishReason": "STOP"
                }],
                "usageMetadata": {
                    "promptTokenCount": 12,
                    "candidatesTokenCount": 1120,
                    "thoughtsTokenCount": 80,
                    "totalTokenCount": 1212
                }
            }
        });
        let mut images = Vec::new();
        let mut usage = None;
        let mut finished = false;
        gemini_frame(&frame, &mut images, &mut usage, &mut finished).unwrap();
        assert!(finished, "STOP 是正常收尾信号");
        assert_eq!(images.len(), 1, "草稿图不算");
        assert_eq!(images[0]["b64_json"], json!("REAL"));
        assert_eq!(images[0]["mime_type"], json!("image/png"));
        // 思考 token 计入 output（与对话链路的口径一致）
        assert_eq!(usage.as_ref().unwrap()["output_tokens"], json!(1200));
    }

    /// 直连 Gemini 的响应没有 `response` 壳；同一段代码两条路都要读得出来。
    #[test]
    fn gemini_frame_accepts_both_envelope_shapes() {
        let bare = json!({
            "candidates": [{ "content": { "parts": [
                { "inline_data": { "mime_type": "image/jpeg", "data": "IMG" } }
            ]}}]
        });
        let mut images = Vec::new();
        let mut usage = None;
        let mut finished = false;
        gemini_frame(&bare, &mut images, &mut usage, &mut finished).unwrap();
        assert_eq!(images.len(), 1);
        // snake_case 的 mime 也要认出来（Gemini 两种命名都出现过）
        assert_eq!(images[0]["mime_type"], json!("image/jpeg"));
    }

    /// 内容策略拦截也走 200：不在这里转成错误，用户拿到的是空的 `data: []`。
    /// 状态码给 400 —— 换号重发同一段提示词没有意义（而且这次额度已经扣了）。
    #[test]
    fn gemini_policy_blocks_become_client_errors() {
        let prompt_block = json!({ "promptFeedback": { "blockReason": "SAFETY" } });
        let mut images = Vec::new();
        let mut usage = None;
        let mut finished = false;
        let failure =
            gemini_frame(&prompt_block, &mut images, &mut usage, &mut finished).unwrap_err();
        assert_eq!(failure.status, 400);
        assert_eq!(failure.body["code"], json!("content_policy_violation"));
        assert!(failure.body["message"]
            .as_str()
            .unwrap_or("")
            .contains("提示词"));

        let output_block = json!({
            "candidates": [{ "finishReason": "IMAGE_SAFETY" }]
        });
        let failure =
            gemini_frame(&output_block, &mut images, &mut usage, &mut finished).unwrap_err();
        assert_eq!(failure.status, 400);
        assert!(failure.body["message"]
            .as_str()
            .unwrap_or("")
            .contains("输出"));
    }

    /// 一张都没拿到不能算成功：Gemini 的纯文字回复也是 200 + STOP。
    #[test]
    fn no_images_is_a_failure_even_after_a_clean_stop() {
        let telemetry = RequestTelemetry::new();
        let failure = finish_images(Vec::new(), None, &telemetry).unwrap_err();
        assert!(failure.body["message"]
            .as_str()
            .unwrap_or("")
            .contains("没有返回生成图片"));
    }

    /// 一张图会在流里出现**两次**（`10010` 带水印预览、`10084` 无水印原图），
    /// 必须按对象键合并成一条 —— 不合并的话客户端会拿到两张一样的图。
    #[test]
    fn doubao_merges_two_block_kinds_by_object_key() {
        let key = "tos-cn-i-abc/rc_gen_image/x.jpeg";
        let frames = [
            format!(
                r#"{{"content":{{"content_block":[{{"block_type":10010,"content":{{"gen_image_block":{{"images":[{{"uri":"https://cdn.example.com/{key}~tplv-downsize_watermark?x=1"}}]}}}}}}]}}}}"#
            ),
            format!(
                r#"{{"content":{{"content_block":[{{"block_type":10084,"content":{{"rich_media_layout_block":{{"media":[{{"creation":{{"image":{{"key":"{key}","image_ori":{{"url":"https://cdn.example.com/{key}~tplv-preview?x=1"}},"image_ori_raw":{{"url":"https://cdn.example.com/{key}~tplv-image_raw.png"}}}}}}}}]}}}}}}]}}}}"#
            ),
        ];
        let text = frames.iter().map(|frame| format!("data: {frame}\n\n")).collect::<String>();
        let found = doubao_images(&text);
        assert_eq!(found.len(), 1, "同一张图只应当有一条");
        assert_eq!(found[0].key, key);
        // 原图优先，预览作为兜底
        assert_eq!(
            found[0].raw.as_deref(),
            Some(format!("https://cdn.example.com/{key}~tplv-image_raw.png").as_str())
        );
        assert!(found[0].preview.contains("downsize_watermark") || found[0].preview.contains("preview"));
    }

    /// 两种块**顺序无关**：先到哪一张都要能合并出来（上游的下发顺序不保证）。
    #[test]
    fn doubao_merge_is_order_independent() {
        let key = "tos-cn-i-abc/rc_gen_image/y.jpeg";
        let raw = format!("https://cdn.example.com/{key}~tplv-image_raw.png");
        let preview = format!("https://cdn.example.com/{key}~tplv-downsize_watermark");
        // 先原图、再预览
        let mut found = Vec::new();
        doubao_merge(&mut found, key.to_string(), Some(raw.clone()), Some(preview.clone()));
        doubao_merge(&mut found, key.to_string(), None, Some(preview.clone()));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].raw.as_deref(), Some(raw.as_str()));
        // 反过来：先预览、再原图
        let mut found = Vec::new();
        doubao_merge(&mut found, key.to_string(), None, Some(preview.clone()));
        doubao_merge(&mut found, key.to_string(), Some(raw.clone()), None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].raw.as_deref(), Some(raw.as_str()));
    }

    /// 对象键要剥掉协议、host、查询串与 `~` 之后的模板后缀。
    #[test]
    fn doubao_object_key_strips_host_and_template() {
        assert_eq!(
            doubao_object_key("https://host.example/tos-cn-i-abc/rc_gen_image/x.jpeg~tplv-a?y=1").as_deref(),
            Some("tos-cn-i-abc/rc_gen_image/x.jpeg")
        );
        // 没有模板后缀也认
        assert_eq!(
            doubao_object_key("https://host/tos/a.png").as_deref(),
            Some("tos/a.png")
        );
        assert_eq!(doubao_object_key(""), None);
    }

    /// 增量帧（`patch_op`）里的块也要读：客户端收到的是逐块 patch，不是整包。
    #[test]
    fn doubao_reads_incremental_patch_frames() {
        let frame = r#"{"patch_op":[{"patch_object":1,"patch_value":{"content_block":[{"block_type":10010,"content":{"gen_image_block":{"images":[{"uri":"https://h/tos/a.png~tplv-w"}]}}}]}}]}"#;
        let text = format!("data: {frame}\n\n");
        let found = doubao_images(&text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "tos/a.png");
    }

    /// 风控那条业务码要单独给一句能照做的文案，而不是「上游故障，请重试」。
    #[test]
    fn doubao_risk_control_asks_for_a_human_verification() {
        let failure = doubao_value_failure(&json!({
            "error_code": 710_022_004,
            "error_msg": "rate limited"
        }))
        .expect("应当识别成失败");
        assert_eq!(failure.status, 429);
        let message = failure.body["message"].as_str().unwrap_or("");
        assert!(message.contains("人机验证"), "{message}");
        assert!(message.contains("网页端"), "{message}");
    }

    /// ⚠️ 正常流里也有 `message` 这个**对象**字段：按通用键判失败会把一次
    /// 正常的答复误报成上游故障（这是实测踩出来的）。
    #[test]
    fn doubao_does_not_mistake_a_normal_message_object_for_an_error() {
        let normal = json!({ "message": { "content_block": [] }, "conversation_id": "c1" });
        assert!(doubao_value_failure(&normal).is_none());
        // 真正的错误才认
        let real = json!({ "error_code": 500, "error_msg": "服务异常" });
        assert!(doubao_value_failure(&real).is_some());
    }

    /// 流里只有文字（agent 只回了一段说明）不能当生图成功：
    /// 客户端会显示「生成完成但空白」。
    #[test]
    fn doubao_text_only_stream_is_not_a_success() {
        let frame = r#"{"content":{"content_block":[{"block_type":10000,"content":{"text_block":{"text":"我画好了"}}}]}}"#;
        let text = format!("data: {frame}\n\n");
        assert!(doubao_images(&text).is_empty());
        // 没有错误体 ⇒ 调用方会给「没有返回图片」那条，而不是当成成功
        assert!(doubao_stream_failure(&text).is_none());
    }
}
