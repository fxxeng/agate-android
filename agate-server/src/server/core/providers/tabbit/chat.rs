//! Tabbit 对话转发：建房间 → 订阅事件流 → 投递消息 → 翻译成 OpenAI chunk。
//!
//! ── 上游是**四步时序**（见 `mod.rs` 模块头）──────────────────────
//! ```text
//!   1. POST /panel/session                  房间 id（30 分钟复用，见 `http.rs`）
//!   2. POST /api/v3/chat/rooms/{id}/join    订阅 SSE（**唯一带签名的对话请求**）
//!   3. POST /api/v3/chat/rooms/{id}/runs    投递消息（不带签名）
//!   4. 读 2 的流，逐帧翻译
//! ```
//! 2 必须在 3 之前：join 是订阅、runs 是投递；顺序反了会漏掉开头的事件。
//!
//! ── 为什么先窥一帧再决定「算不算成功」──────────────────────
//! 上游常用 **HTTP 200 + `run_failed` / `error` 事件**表达权益不足（492）或
//! 排队拒绝。只看 HTTP 状态会把它记成成功、账号也就不会轮换（用户看到的是一条
//! 空回答）。所以这里在返回 `ForwardOutcome::Stream` **之前**先读到「第一条正文
//! 或错误」：读到错误就把整次请求作为错误上抛（编排层据此换账号），读到正文
//! 才开始逐帧转发。窥探用掉的那几帧一并带进流里，不丢。
//!
//! ── 409「会话正在处理中」的处置 ─────────────────────────────
//! 扔房间 → 新建 → 重试一次。它不是鉴权失败也不是限流，**不罚账号额度**
//! （见 `adapter::classify_error`：只有 429/492 才进冷却）。
//!
//! ── 不做工具调用模拟 ────────────────────────────────────────
//! 网页 completion 没有原生 `tool_calls`，`prompt.rs` 已明确拒绝 `tools`，
//! 因此这里没有工具标记扫描器。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic；持锁不跨 await。

use std::sync::Arc;

use bytes::Bytes;
use futures::Stream;
use serde_json::{json, Map, Value};
use tokio_stream::wrappers::ReceiverStream;

use crate::server::core::account_store::AccountStore;
use crate::server::core::proxies::ResolvedProxy;
use crate::server::core::upstream::usage::RequestTelemetry;
use crate::server::core::upstream::ForwardOutcome;
use crate::server::errors::GatewayError;

use super::credentials::Credentials;
use super::sign;
use super::{endpoints, http, models, prompt, Realm};

/// 上游事件流的空闲兜底（项目编排层另有统一 idle_guard；这里是兜底值）
const STREAM_IDLE_MS: u64 = 120_000;
/// 窥探阶段最多容忍多少条控制事件（超过就当作「上游只发心跳不发正文」放行）
const PEEK_FRAME_LIMIT: usize = 64;

/// 事件流的类型别名（窥探与续读共用同一条流）
type Source = std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>;

/// 转发入口（`forward_conversation` 转调）。
pub async fn run_chat(
    store: &AccountStore,
    realm: Realm,
    account_id: &str,
    body: &Value,
    proxy: Option<ResolvedProxy>,
    stream: bool,
    telemetry: &Arc<RequestTelemetry>,
) -> Result<ForwardOutcome, GatewayError> {
    let record = store.tabbit_account_record(realm, account_id).ok_or_else(|| {
        GatewayError::with_status(
            404,
            format!(
                "还没有 {} 账号：请先在「添加账号」里粘贴网页 Cookie",
                realm.label()
            ),
        )
    })?;
    let credentials = Credentials::from_record(Some(&record))?;
    let client = http::client(proxy.as_ref());

    let requested = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let content = prompt::build(body)?;
    // 目录可能还没拉过：`list` 会先给兜底（一条 default），于是这里至少能发出去
    let catalog = models::list(realm);
    let upstream_model = models::select(&catalog, &requested)?;
    let display_model = if requested.is_empty() {
        upstream_model.clone()
    } else {
        requested
    };

    let (response, run_id, run_url) =
        connect(&credentials, realm, &client, &content, &upstream_model).await?;

    if let Some(capture) = telemetry.capture().as_deref() {
        capture.reset_request(&run_url, realm.id(), &[], body);
        capture.attach_response(response.status().as_u16(), response.headers());
    }

    if !stream {
        return aggregate(response, &display_model).await;
    }

    let source: Source = Box::pin(http::byte_stream(
        response,
        std::time::Duration::from_millis(STREAM_IDLE_MS),
    ));
    let mut forwarder = Forwarder::new(&run_id, &display_model, source);
    let ready = forwarder.peek().await?;
    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);
    if ready.done {
        // 上游一帧就收尾了（例如极短的答复）：写完就可以关流，不必起后台任务
        for frame in ready.frames {
            let _ = sender.send(Ok(Bytes::from(frame.wire()))).await;
        }
        let _ = sender.send(Ok(Bytes::from(Frame::Done.wire()))).await;
        drop(sender);
        return Ok(ForwardOutcome::Stream {
            status: 200,
            stream: Box::new(ReceiverStream::new(receiver)),
        });
    }
    crate::spawn_task(async move {
        if let Err(error) = forwarder.drive(ready, sender).await {
            crate::server::logging::verbose(
                "[Tabbit]",
                &format!("对话流处理中断：{}", error.message),
            );
        }
    });
    Ok(ForwardOutcome::Stream {
        status: 200,
        stream: Box::new(ReceiverStream::new(receiver)),
    })
}

/// 建房间 → join → runs，返回（已打开的事件流，run_id，投递 URL）。
///
/// 409（房间正在处理）时扔房间重来一次：那是「这个房间还占着」的正常语义，
/// 不是错误。
async fn connect(
    credentials: &Credentials,
    realm: Realm,
    client: &reqwest::Client,
    content: &str,
    upstream_model: &str,
) -> Result<(reqwest::Response, String, String), GatewayError> {
    let mut last_error: Option<GatewayError> = None;
    for _attempt in 0..2 {
        let room = http::session_id(credentials, realm, client).await?;
        let join_body = http::build_join_payload(&format!("{}|{}", credentials.uid, room));
        let response = match http::open_join(credentials, realm, &room, &join_body, client).await {
            Ok(response) => response,
            Err(error) if error.status_code == 409 => {
                http::drop_session(credentials, realm);
                last_error = Some(error);
                continue;
            }
            Err(error) => return Err(error),
        };
        let run_body = http::build_run_payload(content, upstream_model);
        match http::post_run(credentials, realm, &room, &run_body, client).await {
            Ok(run_id) => {
                let url = format!("{}{}", realm.base(), endpoints::run_path(&room));
                return Ok((response, run_id, url));
            }
            Err(error) if error.status_code == 409 => {
                drop(response);
                http::drop_session(credentials, realm);
                last_error = Some(error);
                continue;
            }
            Err(error) => {
                drop(response);
                return Err(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        GatewayError::with_status(502, "Tabbit 会话反复冲突，请稍后重试")
    }))
}

// ─── 事件 → 动作 ────────────────────────────────────────────

/// 一条 SSE 事件
#[derive(Clone, Debug, Default)]
pub(super) struct Event {
    pub name: String,
    pub data: String,
}

/// 翻译结果
pub(super) enum Action {
    Ignore,
    Chunks(Vec<Frame>),
    Error(String, u16),
    Done(Vec<Frame>),
}

/// 一条出站帧
#[derive(Clone, Debug)]
pub(super) enum Frame {
    Chunk(Value),
    Done,
}

impl Frame {
    /// 落到线上的字节（含 `\n\n` 帧尾）
    pub(super) fn wire(&self) -> String {
        match self {
            Frame::Chunk(chunk) => format!("data: {chunk}\n\n"),
            Frame::Done => "data: [DONE]\n\n".to_string(),
        }
    }
}

/// 翻译状态（响应 id、是否已发 role、是否已收尾、用量）
pub(super) struct State {
    id: String,
    created: i64,
    model: String,
    started: bool,
    finished: bool,
    usage: Option<Value>,
}

impl State {
    fn new(model: &str) -> Self {
        Self {
            id: format!("chatcmpl-{}", sign::random_uuid().replace('-', "")),
            created: crate::server::logging::now_ms() / 1000,
            model: model.to_string(),
            started: false,
            finished: false,
            usage: None,
        }
    }

    fn chunk(&self, delta: Value, finish: Option<&str>) -> Value {
        json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
        })
    }

    fn ensure_role(&mut self, out: &mut Vec<Frame>) {
        if self.started {
            return;
        }
        self.started = true;
        out.push(Frame::Chunk(
            self.chunk(json!({ "role": "assistant", "content": "" }), None),
        ));
    }

    fn content(&mut self, text: &str, out: &mut Vec<Frame>) {
        if text.is_empty() {
            return;
        }
        self.ensure_role(out);
        out.push(Frame::Chunk(self.chunk(json!({ "content": text }), None)));
    }

    fn reasoning(&mut self, text: &str, out: &mut Vec<Frame>) {
        if text.is_empty() {
            return;
        }
        self.ensure_role(out);
        out.push(Frame::Chunk(
            self.chunk(json!({ "reasoning_content": text }), None),
        ));
    }

    /// 收尾：role（若还没发）→ finish 帧 → 用量帧
    fn finish(&mut self, reason: &str) -> Vec<Frame> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut out = Vec::new();
        self.ensure_role(&mut out);
        let reason = if reason.is_empty() { "stop" } else { reason };
        out.push(Frame::Chunk(self.chunk(json!({}), Some(reason))));
        if let Some(usage) = self.usage.clone() {
            out.push(Frame::Chunk(json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": self.created,
                "model": self.model,
                "choices": [],
                "usage": usage,
            })));
        }
        out
    }
}

fn text_of(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|text| !text.is_empty())
        .unwrap_or("")
        .to_string()
}

/// 用量可能在顶层 / `payload` / `chunk_payload` 任一层（v3 实测位置不稳定）
fn usage_of(value: &Value) -> Option<Value> {
    let source = value
        .get("token_usage")
        .filter(|nested| nested.is_object())
        .unwrap_or(value);
    let prompt = ["input_tokens", "prompt_tokens"]
        .iter()
        .find_map(|key| http::as_i64(source.get(*key).unwrap_or(&Value::Null)));
    let completion = ["output_tokens", "completion_tokens"]
        .iter()
        .find_map(|key| http::as_i64(source.get(*key).unwrap_or(&Value::Null)));
    if prompt.is_none() && completion.is_none() {
        return None;
    }
    let prompt = prompt.unwrap_or(0);
    let completion = completion.unwrap_or(0);
    Some(json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
    }))
}

/// 流内错误 → 对外状态码。
///
/// 上游用 200 + 事件表达错误，所以这里必须自己判档：判成 401 会去刷一个
/// **刷不了**的凭证；判成 502 则不会轮换账号 —— 两样都不对。
fn error_status(raw: &str, message: &str) -> u16 {
    let all = format!(
        "{}\n{}",
        raw.to_ascii_lowercase(),
        message.to_ascii_lowercase()
    );
    if all.contains("insufficient")
        || all.contains("no_quota")
        || all.contains("quota")
        || all.contains("积分不足")
        || all.contains("余额不足")
        || all.contains("额度")
        || all.contains("premium")
        || all.contains("upgrade")
        || all.contains("492")
    {
        return 429;
    }
    if all.contains("rate limit") || all.contains("too many") || all.contains("429") {
        return 429;
    }
    if all.contains("unauthorized") || all.contains("401") || all.contains("token") {
        return 401;
    }
    if all.contains("session is processing")
        || all.contains("正在处理")
        || all.contains("处理中")
        || all.contains("409")
    {
        return 409;
    }
    if all.contains("serviceavailable") && all.contains("false") {
        return 503;
    }
    502
}

/// 一条事件 → 动作（纯函数，便于逐帧单元测试）
pub(super) fn translate(event: &Event, run_id: &str, state: &mut State) -> Action {
    let raw = event.data.trim();
    if raw.is_empty() || raw == "[DONE]" {
        return Action::Ignore;
    }
    let payload: Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(_) => return Action::Ignore,
    };
    let mut name = event.name.trim().to_string();
    if name.is_empty() {
        name = text_of(&payload, &["type", "event", "event_type"]);
    }
    let inner = payload.get("payload").filter(|value| value.is_object());
    // 用量可能挂在顶层、payload 或 chunk_payload 任一层
    if let Some(usage) = usage_of(&payload) {
        state.usage = Some(usage);
    }
    if let Some(inner) = inner {
        if let Some(usage) = usage_of(inner) {
            state.usage = Some(usage);
        }
        if let Some(chunk) = inner.get("chunk_payload") {
            if let Some(usage) = usage_of(chunk) {
                state.usage = Some(usage);
            }
        }
        // 别的 run 的事件一律忽略（同一房间可能有并发的 run）
        if let Some(run) = inner.get("run_id").and_then(Value::as_str) {
            if !run.is_empty() && !run_id.is_empty() && run != run_id {
                return Action::Ignore;
            }
        }
        match name.as_str() {
            "event_message_chunk" => {
                let kind = text_of(inner, &["chunk_type"]);
                let chunk = inner.get("chunk_payload").unwrap_or(&Value::Null);
                return match kind.as_str() {
                    "thinking" => {
                        let mut out = Vec::new();
                        state.reasoning(
                            &text_of(chunk, &["reasoning_content", "content"]),
                            &mut out,
                        );
                        Action::Chunks(out)
                    }
                    "thinking_finished" => Action::Ignore,
                    "error" => {
                        let message = text_of(chunk, &["message", "error_message", "detail"]);
                        Action::Error(message, error_status(raw, &text_of(inner, &["message"])))
                    }
                    _ => {
                        let mut out = Vec::new();
                        state.content(&text_of(chunk, &["content"]), &mut out);
                        Action::Chunks(out)
                    }
                };
            }
            "assistant_message_delta" => {
                let mut text = text_of(inner, &["delta"]);
                if text.is_empty() {
                    if let Some(chunk) = inner.get("chunk_payload") {
                        text = text_of(chunk, &["content"]);
                    }
                }
                let mut out = Vec::new();
                state.content(&text, &mut out);
                return Action::Chunks(out);
            }
            "assistant_message_finished" => return Action::Ignore,
            "run_completed" => {
                if let Some(usage) = usage_of(inner) {
                    state.usage = Some(usage);
                }
                return Action::Done(state.finish("stop"));
            }
            "run_failed" | "run_cancelled" => {
                let message = {
                    let text = text_of(inner, &["message", "error_message", "reason", "detail"]);
                    if text.is_empty() {
                        text_of(inner, &["status"])
                    } else {
                        text
                    }
                };
                let message = if message.is_empty() {
                    "Tabbit 上游拒绝了本次请求".to_string()
                } else {
                    message
                };
                return Action::Error(message.clone(), error_status(raw, &message));
            }
            "run_queued" => {
                // 排队帧里带 `serviceAvailable:false` = 该模型此刻不可服务
                let flat = raw
                    .to_ascii_lowercase()
                    .replace([' ', '\t', '\r', '\n'], "");
                if flat.contains("\"serviceavailable\":false") {
                    return Action::Error(
                        "Tabbit 上游该模型此刻不可服务（排队拒绝），请稍后重试".to_string(),
                        503,
                    );
                }
                return Action::Ignore;
            }
            "snapshot_begin" | "snapshot_end" | "message_history" | "active_runs_snapshot"
            | "run_started" | "user_message_created" | "assistant_message_started"
            | "session_title_updated" | "room_reset_required" => {
                return Action::Ignore;
            }
            _ => {}
        }
    }
    // 非 v3 的扁平事件（兼容旧房间或心跳）
    match name.as_str() {
        "thinking" => {
            let mut out = Vec::new();
            state.reasoning(
                &text_of(&payload, &["reasoning_content", "content", "text"]),
                &mut out,
            );
            Action::Chunks(out)
        }
        "message_chunk" | "message" | "delta" => {
            let mut text = text_of(&payload, &["content", "text", "delta"]);
            if text.is_empty() {
                if let Some(delta) = payload.get("delta") {
                    text = text_of(delta, &["content", "text"]);
                }
            }
            let mut out = Vec::new();
            state.content(&text, &mut out);
            Action::Chunks(out)
        }
        "usage" => {
            if let Some(usage) = usage_of(&payload) {
                state.usage = Some(usage);
            }
            Action::Ignore
        }
        "error" => {
            let message = {
                let text = text_of(&payload, &["message", "msg", "error_message", "displayTips"]);
                if text.is_empty() {
                    text_of(payload.get("error").unwrap_or(&Value::Null), &["message"])
                } else {
                    text
                }
            };
            Action::Error(message, error_status(raw, ""))
        }
        "message_finish" | "finish" | "close" | "cancel" => {
            let reason = text_of(&payload, &["finish_reason", "stop_reason"]);
            Action::Done(state.finish(&reason))
        }
        _ => Action::Ignore,
    }
}

// ─── SSE 分帧 ──────────────────────────────────────────────

/// 取出一个完整帧（以空行结尾）；不足一帧返回 None。
fn take_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let mut line_start = 0usize;
    let mut index = 0usize;
    while index < buffer.len() {
        if buffer[index] == b'\n' {
            let mut end = index;
            if end > line_start && buffer[end - 1] == b'\r' {
                end -= 1;
            }
            if end == line_start {
                // 帧内容**不含**最后一行的换行符：那是分隔符的一部分，不是内容。
                // 带上它不影响解析（`parse_frame` 逐行 strip），但会让「一帧的
                // 文本」在日志 / 断言里与 `data:` 拼出来的结果差一个换行 —— 调试
                // 时要多看一眼才能确定没丢内容。
                let mut frame_end = line_start;
                if frame_end > 0 && buffer[frame_end - 1] == b'\n' {
                    frame_end -= 1;
                    if frame_end > 0 && buffer[frame_end - 1] == b'\r' {
                        frame_end -= 1;
                    }
                }
                let frame = buffer[..frame_end].to_vec();
                buffer.drain(..=index);
                return Some(frame);
            }
            line_start = index + 1;
        }
        index += 1;
    }
    None
}

/// 一帧 → 事件（`event:` + 多行 `data:`）
fn parse_frame(frame: &[u8]) -> Option<Event> {
    if frame.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(frame);
    let mut name = String::new();
    let mut data = Vec::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("event:") {
            name = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value).to_string());
        }
    }
    if name.is_empty() && data.is_empty() {
        return None;
    }
    Some(Event {
        name,
        data: data.join("\n"),
    })
}

// ─── 驱动 ──────────────────────────────────────────────────

/// 窥探阶段的结果
pub(super) struct Ready {
    frames: Vec<Frame>,
    done: bool,
}

/// 转发器：**持有上游流**（窥探与续读共用同一条流，不丢中间的事件）
pub(super) struct Forwarder {
    run_id: String,
    state: State,
    buffer: Vec<u8>,
    eof: bool,
    source: Source,
}

impl Forwarder {
    fn new(run_id: &str, model: &str, source: Source) -> Self {
        Self {
            run_id: run_id.to_string(),
            state: State::new(model),
            buffer: Vec::new(),
            eof: false,
            source,
        }
    }

    /// 从流里取下一帧（跨 chunk 的半帧留在 buffer 里）
    async fn next_event(&mut self) -> Option<Event> {
        loop {
            if let Some(frame) = take_frame(&mut self.buffer) {
                return parse_frame(&frame);
            }
            if self.eof {
                if self.buffer.is_empty() {
                    return None;
                }
                let frame = std::mem::take(&mut self.buffer);
                return parse_frame(&frame);
            }
            match futures::StreamExt::next(&mut self.source).await {
                Some(Ok(chunk)) => self.buffer.extend_from_slice(&chunk),
                Some(Err(_)) | None => self.eof = true,
            }
        }
    }

    /// 窥探：读到第一条正文 / 错误 / 结束（最多容忍 [`PEEK_FRAME_LIMIT`] 条控制事件）。
    ///
    /// 错误在这里**返回 Err**（而不是写进流里）—— 编排层只有拿到错误才会换账号。
    async fn peek(&mut self) -> Result<Ready, GatewayError> {
        let mut frames: Vec<Frame> = Vec::new();
        let mut seen = 0usize;
        loop {
            let Some(event) = self.next_event().await else {
                break;
            };
            seen += 1;
            match translate(&event, &self.run_id, &mut self.state) {
                Action::Ignore => {
                    if seen >= PEEK_FRAME_LIMIT {
                        break;
                    }
                }
                Action::Chunks(chunks) => {
                    frames.extend(chunks);
                    return Ok(Ready {
                        frames,
                        done: false,
                    });
                }
                Action::Done(chunks) => {
                    frames.extend(chunks);
                    return Ok(Ready { frames, done: true });
                }
                Action::Error(message, status) => {
                    let message = if message.trim().is_empty() {
                        "Tabbit 上游拒绝了本次请求".to_string()
                    } else {
                        message
                    };
                    return Err(GatewayError::with_status(i32::from(status), message));
                }
            }
        }
        Ok(Ready {
            frames,
            done: false,
        })
    }

    /// 继续读剩余事件并下发；EOF 时补收尾帧
    async fn drive(
        mut self,
        ready: Ready,
        sender: tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    ) -> Result<(), GatewayError> {
        for frame in ready.frames {
            send_frame(&sender, &frame).await?;
        }
        if ready.done {
            return send_frame(&sender, &Frame::Done).await;
        }
        loop {
            let Some(event) = self.next_event().await else {
                break;
            };
            match translate(&event, &self.run_id, &mut self.state) {
                Action::Ignore => {}
                Action::Chunks(chunks) => {
                    for frame in chunks {
                        send_frame(&sender, &frame).await?;
                    }
                }
                Action::Done(chunks) => {
                    for frame in chunks {
                        send_frame(&sender, &frame).await?;
                    }
                    return send_frame(&sender, &Frame::Done).await;
                }
                Action::Error(message, _) => {
                    // 已经发过正文：错误只能写在流里（HTTP 头早发出去了）
                    let text = if message.trim().is_empty() {
                        "Tabbit 上游拒绝了本次请求".to_string()
                    } else {
                        message
                    };
                    crate::server::logging::verbose("[Tabbit]", &format!("流内错误：{text}"));
                    send_frame(
                        &sender,
                        &Frame::Chunk(
                            json!({ "error": { "message": text, "type": "upstream_error" } }),
                        ),
                    )
                    .await?;
                    return send_frame(&sender, &Frame::Done).await;
                }
            }
        }
        for frame in self.state.finish("stop") {
            send_frame(&sender, &frame).await?;
        }
        send_frame(&sender, &Frame::Done).await
    }
}

async fn send_frame(
    sender: &tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    frame: &Frame,
) -> Result<(), GatewayError> {
    sender
        .send(Ok(Bytes::from(frame.wire())))
        .await
        .map_err(|error| GatewayError::with_status(502, format!("Tabbit 对话流下发失败：{error}")))
}

// ─── 非流式聚合 ────────────────────────────────────────────

/// 非流式：读完整条流，聚合成 `chat.completion`
async fn aggregate(
    response: reqwest::Response,
    display_model: &str,
) -> Result<ForwardOutcome, GatewayError> {
    let mut source: Source = Box::pin(http::byte_stream(
        response,
        std::time::Duration::from_millis(STREAM_IDLE_MS),
    ));
    let mut buffer: Vec<u8> = Vec::new();
    let mut eof = false;
    let mut state = State::new(display_model);
    let mut content = String::new();
    let mut reasoning = String::new();
    loop {
        let event = loop {
            if let Some(frame) = take_frame(&mut buffer) {
                break parse_frame(&frame);
            }
            if eof {
                if buffer.is_empty() {
                    break None;
                }
                break parse_frame(&std::mem::take(&mut buffer));
            }
            match futures::StreamExt::next(&mut source).await {
                Some(Ok(chunk)) => buffer.extend_from_slice(&chunk),
                Some(Err(_)) | None => eof = true,
            }
        };
        let Some(event) = event else { break };
        match translate(&event, "", &mut state) {
            Action::Ignore => {}
            Action::Chunks(frames) => collect_text(frames, &mut content, &mut reasoning),
            Action::Done(frames) => {
                collect_text(frames, &mut content, &mut reasoning);
                break;
            }
            Action::Error(message, status) => {
                return Err(GatewayError::with_status(i32::from(status), message));
            }
        }
    }
    let mut message = Map::new();
    message.insert("role".to_string(), json!("assistant"));
    message.insert("content".to_string(), json!(content));
    if !reasoning.is_empty() {
        message.insert("reasoning_content".to_string(), json!(reasoning));
    }
    let mut choice = Map::new();
    choice.insert("index".to_string(), json!(0));
    choice.insert("message".to_string(), Value::Object(message));
    choice.insert("finish_reason".to_string(), json!("stop"));
    let mut body = Map::new();
    body.insert("id".to_string(), json!(state.id));
    body.insert("object".to_string(), json!("chat.completion"));
    body.insert("created".to_string(), json!(state.created));
    body.insert("model".to_string(), json!(state.model));
    body.insert(
        "choices".to_string(),
        Value::Array(vec![Value::Object(choice)]),
    );
    if let Some(usage) = state.usage.clone() {
        body.insert("usage".to_string(), usage);
    }
    Ok(ForwardOutcome::Completion {
        body: Value::Object(body),
    })
}

/// 从帧里抽正文 / 思考（非流式聚合用）
fn collect_text(frames: Vec<Frame>, content: &mut String, reasoning: &mut String) {
    for frame in frames {
        let Frame::Chunk(chunk) = frame else { continue };
        let Some(delta) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
        else {
            continue;
        };
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            content.push_str(text);
        }
        if let Some(text) = delta.get("reasoning_content").and_then(Value::as_str) {
            reasoning.push_str(text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, data: Value) -> Event {
        Event {
            name: name.to_string(),
            data: data.to_string(),
        }
    }

    fn fresh() -> State {
        State::new("gpt-5-5")
    }

    /// 正文增量（v3 的 `assistant_message_delta`）：先一条 role 帧，再正文帧
    #[test]
    fn delta_events_become_content_chunks() {
        let mut state = fresh();
        let action = translate(
            &event(
                "assistant_message_delta",
                json!({ "payload": { "delta": "你好" } }),
            ),
            "run-1",
            &mut state,
        );
        let Action::Chunks(frames) = action else {
            panic!("应产出正文帧");
        };
        assert_eq!(frames.len(), 2, "role 帧 + 正文帧");
        let Frame::Chunk(role) = &frames[0] else {
            panic!("应为 chunk");
        };
        assert_eq!(role["choices"][0]["delta"]["role"], "assistant");
        let Frame::Chunk(chunk) = &frames[1] else {
            panic!("应为 chunk");
        };
        assert_eq!(chunk["choices"][0]["delta"]["content"], "你好");
    }

    /// 思考增量走 `reasoning_content`，不进正文
    #[test]
    fn thinking_chunks_are_reasoning_not_content() {
        let mut state = fresh();
        let action = translate(
            &event(
                "event_message_chunk",
                json!({
                    "payload": {
                        "chunk_type": "thinking",
                        "chunk_payload": { "reasoning_content": "想一想" }
                    }
                }),
            ),
            "run-1",
            &mut state,
        );
        let Action::Chunks(frames) = action else {
            panic!("应产出思考帧");
        };
        let Frame::Chunk(chunk) = frames.last().expect("至少一帧") else {
            panic!("应为 chunk");
        };
        assert_eq!(chunk["choices"][0]["delta"]["reasoning_content"], "想一想");
        assert!(chunk["choices"][0]["delta"].get("content").is_none());
    }

    /// 别的 run 的事件必须忽略（同房间可能并发）
    #[test]
    fn events_from_another_run_are_ignored() {
        let mut state = fresh();
        let action = translate(
            &event(
                "assistant_message_delta",
                json!({ "payload": { "run_id": "run-2", "delta": "不该出现" } }),
            ),
            "run-1",
            &mut state,
        );
        assert!(matches!(action, Action::Ignore));
    }

    /// `run_completed` → finish 帧（带 finish_reason）+ 用量帧
    #[test]
    fn run_completed_finishes_with_usage() {
        let mut state = fresh();
        state.content("正文", &mut Vec::new());
        let action = translate(
            &event(
                "run_completed",
                json!({ "payload": { "token_usage": { "input_tokens": 7, "output_tokens": 3 } } }),
            ),
            "run-1",
            &mut state,
        );
        let Action::Done(frames) = action else {
            panic!("run_completed 应收尾");
        };
        frames
            .iter()
            .find_map(|frame| match frame {
                Frame::Chunk(chunk) if chunk["choices"][0]["finish_reason"] == "stop" => Some(()),
                _ => None,
            })
            .expect("应有 finish 帧");
        let usage = frames
            .iter()
            .find_map(|frame| match frame {
                Frame::Chunk(chunk) if chunk.get("usage").is_some() => Some(chunk),
                _ => None,
            })
            .expect("应有用量帧");
        assert_eq!(usage["usage"]["total_tokens"], 10);
    }

    /// `run_failed` → 错误动作，状态码按文案分档（额度不足必须是 429）
    #[test]
    fn run_failed_classifies_by_message() {
        let mut state = fresh();
        let action = translate(
            &event(
                "run_failed",
                json!({ "payload": { "message": "今日额度已用完（premium 需要升级）" } }),
            ),
            "run-1",
            &mut state,
        );
        match action {
            Action::Error(message, status) => {
                assert!(message.contains("额度"));
                assert_eq!(status, 429, "权益不足必须判成 429 才能冷却换号");
            }
            _ => panic!("run_failed 应是错误"),
        }

        let mut state = fresh();
        let action = translate(
            &event("run_failed", json!({ "payload": { "message": "unauthorized" } })),
            "run-1",
            &mut state,
        );
        assert!(matches!(action, Action::Error(_, 401)));
    }

    /// 排队帧里的 `serviceAvailable:false` → 503（该模型此刻不可服务）
    #[test]
    fn queue_rejection_is_reported() {
        let mut state = fresh();
        let action = translate(
            &event("run_queued", json!({ "payload": { "serviceAvailable": false } })),
            "run-1",
            &mut state,
        );
        assert!(matches!(action, Action::Error(_, 503)));

        let mut state = fresh();
        let action = translate(
            &event("run_queued", json!({ "payload": { "serviceAvailable": true } })),
            "run-1",
            &mut state,
        );
        assert!(matches!(action, Action::Ignore));
    }

    /// 快照类控制事件一律忽略（订阅时上游会先发一串）
    #[test]
    fn snapshot_events_are_ignored() {
        for name in [
            "snapshot_begin",
            "snapshot_end",
            "message_history",
            "active_runs_snapshot",
            "run_started",
            "assistant_message_started",
        ] {
            let mut state = fresh();
            assert!(matches!(
                translate(&event(name, json!({ "payload": {} })), "run-1", &mut state),
                Action::Ignore
            ));
        }
    }

    /// 分帧：一次给两帧、以及跨 chunk 的半帧都要能正确切
    #[test]
    fn frames_are_split_across_chunks() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(b"event: a\ndata: 1\n\nevent: b\ndata: 2\n\n");
        let first = take_frame(&mut buffer).expect("第一帧");
        assert_eq!(String::from_utf8_lossy(&first), "event: a\ndata: 1");
        let second = take_frame(&mut buffer).expect("第二帧");
        assert_eq!(String::from_utf8_lossy(&second), "event: b\ndata: 2");
        assert!(take_frame(&mut buffer).is_none());

        // 半帧留在缓冲里
        let mut buffer = Vec::new();
        buffer.extend_from_slice(b"event: a\ndata: 1\n");
        assert!(take_frame(&mut buffer).is_none());
        buffer.extend_from_slice(b"\n");
        assert!(take_frame(&mut buffer).is_some());
    }

    /// CRLF 行尾也要能切（上游有代理时会改行尾）
    #[test]
    fn crlf_frames_are_handled() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(b"event: a\r\ndata: 1\r\n\r\n");
        let frame = take_frame(&mut buffer).expect("应切出一帧");
        let event = parse_frame(&frame).expect("应解析出事件");
        assert_eq!(event.name, "a");
        assert_eq!(event.data, "1");
    }

    /// 多行 data 用换行拼回（SSE 规范）
    #[test]
    fn multi_line_data_is_joined() {
        let event = parse_frame(b"event: x\ndata: line1\ndata: line2").expect("应解析出事件");
        assert_eq!(event.data, "line1\nline2");
    }

    /// 事件名缺失时从 payload 里取（上游有些帧不带 `event:` 行）
    #[test]
    fn event_name_falls_back_to_payload_type() {
        let mut state = fresh();
        let action = translate(
            &Event {
                name: String::new(),
                data: json!({
                    "event_type": "assistant_message_delta",
                    "payload": { "delta": "hi" }
                })
                .to_string(),
            },
            "run-1",
            &mut state,
        );
        assert!(matches!(action, Action::Chunks(_)));
    }

    /// 非 JSON 的帧一律忽略（心跳 / 空行）
    #[test]
    fn non_json_frames_are_ignored() {
        let mut state = fresh();
        assert!(matches!(
            translate(
                &Event {
                    name: "ping".into(),
                    data: "not json".into()
                },
                "r",
                &mut state
            ),
            Action::Ignore
        ));
        assert!(matches!(
            translate(
                &Event {
                    name: "".into(),
                    data: "[DONE]".into()
                },
                "r",
                &mut state
            ),
            Action::Ignore
        ));
    }

    /// 收尾只发生一次（finish 之后的重复收尾不能再吐帧）
    #[test]
    fn finish_is_idempotent() {
        let mut state = fresh();
        let first = state.finish("stop");
        assert!(!first.is_empty());
        let second = state.finish("stop");
        assert!(second.is_empty(), "重复收尾不应再吐帧");
    }
}
