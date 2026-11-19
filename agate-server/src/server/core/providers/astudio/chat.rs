//! AStudio 的会话式转发：Responses SSE 事件 → 标准 chat 帧 / 完整响应。
//!
//! ── 为什么必须是会话式（`is_stateful() == true`）──────────────
//! 上游响应是 OpenAI **Responses** 的事件流（`response.output_text.delta` 等），
//! 网关的 `UpstreamKind` 是**封闭枚举**，没有这一套。由本模块自己读事件、翻成
//! chat 帧之后，账号轮换 / 限额冷却 / 用量统计仍然由编排层负责 —— 与 kiro 的
//! AWS 二进制帧同一条理由，也是唯一不扩协议的路。
//!
//! ── 先读一步再决定「算不算成功」────────────────────────────
//! 上游经常先回 200，再把错误塞进流里（`error` / `response.failed`）。直接推
//! 200 的话，网关与客户端都会当成一次成功回答（用户看到一条空消息），而且这个
//! 账号不会被轮换。所以**预读第一个有意义的事件**：是失败就整次上抛。
//!
//! ── 事件解读为什么不在本文件 ─────────────────────────────────
//! 「哪种事件算什么」是纯逻辑（含「同一份内容给两次」的去重），收在
//! `responses::StreamState` 里离线可测。本文件只做三件事：读字节 → 叫状态机
//! 拿一个 `Step` → 把 `Step` 写成帧。`Step` 是**已经消费过事件**的结论，
//! 所以这里的 `frames` 不会再碰状态机（碰了就会把 ToolStart 的序号加两次）。
//!
//! ── 停止原因：有工具调用就报 `tool_calls` ─────────────────────
//! 上游的 `response.completed` 只说「完成」，不区分「说完」与「要去调工具」。
//! 客户端拿到 `stop` 会以为这是最终答案、不去执行工具 —— 于是用户看到一段
//! 「我要查一下天气」然后卡住。与 kiro 同一处置：发过工具调用的流，
//! `finish_reason` 报 `tool_calls`。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic；持锁不跨 await。

use std::collections::VecDeque;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use tokio_stream::wrappers::ReceiverStream;

use crate::server::core::proxies::ResolvedProxy;
use crate::server::core::upstream::usage::RequestTelemetry;
use crate::server::core::upstream::ForwardOutcome;
use crate::server::errors::GatewayError;

use super::credentials::Credentials;
use super::responses::{self, Step, StreamState, Usage};
use super::{client, endpoints, models};

/// 事件流的空闲兜底（编排层另有统一的 idle_guard；这里是兜底值）
const STREAM_IDLE_MS: u64 = 120_000;

/// 转发入口（`forward_conversation` 转调）。
///
/// 本家**没有续期接口**（见 `mod.rs` 的模块头），所以这里既不做临期刷新、
/// 也没有「401 刷新后重发」那一段：凭据被拒就是被拒，如实上报让用户重贴。
pub async fn run_chat(
    credentials: &Credentials,
    account_id: &str,
    body: &Value,
    proxy: Option<ResolvedProxy>,
    stream: bool,
    telemetry: &RequestTelemetry,
) -> Result<ForwardOutcome, GatewayError> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(models::DEFAULT_MODEL)
        .to_string();
    // 上游只吃 Responses 协议：`stream = true` 是它的固定形态（非流式由我们
    // 侧聚合），`build_request` 会按这个形态写 `stream` 字段
    let request = responses::build_request(body, &model, true);
    let response = client::responses(credentials, &request, proxy.as_ref()).await?;
    if let Some(capture) = telemetry.capture().as_deref() {
        capture.reset_request(endpoints::RESPONSES_URL, super::ID, &[], body);
        capture.attach_response(response.status().as_u16(), response.headers());
    }
    crate::server::logging::verbose(
        "[AStudio]",
        &format!(
            "会话式转发 model={model} stream={stream} account={} key={}",
            if account_id.is_empty() {
                "-"
            } else {
                account_id
            },
            credentials.masked()
        ),
    );

    let source = client::byte_stream(response, Duration::from_millis(STREAM_IDLE_MS));
    let mut forwarder = Forwarder::new(EventReader::new(source), model);
    if !stream {
        return forwarder.aggregate().await;
    }
    // 预读一步：失败必须在发 head 之前上抛（见模块头）
    let first = match forwarder.read_step().await {
        Ok(step) => step,
        Err(message) => return Err(GatewayError::with_status(502, message)),
    };
    match first {
        None => {
            return Err(GatewayError::with_status(
                502,
                "AStudio 在开始回答前就结束了响应",
            ))
        }
        Some(Step::Failed { status, message }) => {
            return Err(GatewayError::with_status(i32::from(status), message))
        }
        Some(step) => {
            let (sender, receiver) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);
            crate::spawn_task(async move {
                if let Err(error) = forwarder.drive(step, sender).await {
                    crate::server::logging::verbose(
                        "[AStudio]",
                        &format!("对话流处理中断：{}", error.message),
                    );
                }
            });
            Ok(ForwardOutcome::Stream {
                status: 200,
                stream: Box::new(ReceiverStream::new(receiver)),
            })
        }
    }
}

// ─── 线协议 ─────────────────────────────────────────────────

/// 上游字节流（与 `UpstreamResponse` 的流同型）
type Source = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<Bytes, std::io::Error>> + Send>,
>;

/// 按**字节**缓冲的行拆分器。
///
/// 逐片 `String::from_utf8_lossy` 会把跨片切开的多字节汉字变成替换字符；
/// 只在完整行上解码就没有这个问题。非 `data:` 行（`event:` / 注释 / 心跳）
/// 直接忽略 —— 本家上游只用 `data:` 承载 JSON。
#[derive(Default)]
struct LineBuffer {
    tail: Vec<u8>,
}

impl LineBuffer {
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.tail.extend_from_slice(chunk);
        let mut out: Vec<String> = Vec::new();
        let mut start = 0usize;
        while let Some(offset) = self.tail[start..].iter().position(|byte| *byte == b'\n') {
            let end = start + offset;
            let line = String::from_utf8_lossy(&self.tail[start..end]).to_string();
            let line = line.strip_suffix('\r').unwrap_or(&line).to_string();
            if let Some(data) = line.strip_prefix("data:") {
                out.push(data.trim().to_string());
            }
            start = end + 1;
        }
        self.tail.drain(..start);
        out
    }

    /// 流结束：吐出尾行（上游没以换行收尾时的最后一条）
    fn finish(&mut self) -> Vec<String> {
        let tail = std::mem::take(&mut self.tail);
        let line = String::from_utf8_lossy(&tail).to_string();
        let line = line.strip_suffix('\r').unwrap_or(&line).to_string();
        match line.strip_prefix("data:") {
            Some(data) if !data.trim().is_empty() => vec![data.trim().to_string()],
            _ => Vec::new(),
        }
    }
}

/// Responses 的事件读取器：字节流 → 事件（`Value`）
struct EventReader {
    stream: Source,
    lines: LineBuffer,
    pending: VecDeque<Value>,
    ended: bool,
}

impl EventReader {
    fn new(stream: Source) -> Self {
        Self {
            stream,
            lines: LineBuffer::default(),
            pending: VecDeque::new(),
            ended: false,
        }
    }

    /// 下一个事件（`None` = 流正常结束）
    async fn next(&mut self) -> Result<Option<Value>, String> {
        use futures::StreamExt;
        loop {
            if let Some(value) = self.pending.pop_front() {
                return Ok(Some(value));
            }
            if self.ended {
                return Ok(None);
            }
            match self.stream.next().await {
                Some(Ok(bytes)) => {
                    for line in self.lines.push(&bytes) {
                        self.accept(&line);
                    }
                }
                Some(Err(error)) => {
                    return Err(format!("读取 AStudio 的响应失败：{error}"));
                }
                None => {
                    self.ended = true;
                    for line in self.lines.finish() {
                        self.accept(&line);
                    }
                }
            }
        }
    }

    fn accept(&mut self, line: &str) {
        if line.is_empty() {
            return;
        }
        if line == "[DONE]" {
            self.ended = true;
            return;
        }
        // 解不出来的行直接丢：它不是内容（可能是上游的调试输出）。
        // 真出问题会在后面的 `error` / `response.failed` 事件里体现。
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            self.pending.push_back(value);
        }
    }
}

// ─── 帧与状态 ───────────────────────────────────────────────

/// 响应 id 与收尾状态
struct State {
    id: String,
    created: i64,
    model: String,
    started: bool,
    finished: bool,
}

impl State {
    fn new(model: &str) -> Self {
        Self {
            id: format!("chatcmpl-{}", client::message_id()),
            created: crate::server::logging::now_ms() / 1000,
            model: model.to_string(),
            started: false,
            finished: false,
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

    fn ensure_role(&mut self, out: &mut Vec<Value>) {
        if self.started {
            return;
        }
        self.started = true;
        out.push(self.chunk(json!({ "role": "assistant", "content": "" }), None));
    }

    /// 收尾帧（`finish_reason` + `usage`），幂等
    fn finish(&mut self, reason: &str, usage: Usage, out: &mut Vec<Value>) {
        if self.finished {
            return;
        }
        self.finished = true;
        let mut chunk = self.chunk(json!({}), Some(reason));
        chunk["usage"] = usage.to_json();
        out.push(chunk);
    }
}

/// SSE 帧
enum Frame {
    Chunk(Value),
    Done,
}

impl Frame {
    fn wire(&self) -> String {
        match self {
            Frame::Chunk(chunk) => format!("data: {chunk}\n\n"),
            Frame::Done => "data: [DONE]\n\n".to_string(),
        }
    }
}

/// 事件 → 帧
struct Forwarder {
    reader: EventReader,
    /// 事件解读状态（「同一份内容给两次」的去重就靠它，见 `responses`）
    stream_state: StreamState,
    state: State,
    /// 这条流上出现过工具调用（决定 `finish_reason`）
    tools_seen: bool,
}

impl Forwarder {
    fn new(reader: EventReader, model: String) -> Self {
        Self {
            reader,
            stream_state: StreamState::new(),
            state: State::new(&model),
            tools_seen: false,
        }
    }

    /// 读到下一个**有意义**的 `Step`（元事件 / 重复内容都被状态机吃掉）。
    ///
    /// `Err` 是读流失败（与「上游报错」不同：那是一个 `Step::Failed`）
    async fn read_step(&mut self) -> Result<Option<Step>, String> {
        loop {
            let Some(event) = self.reader.next().await? else {
                return Ok(None);
            };
            match self.state_step(&event) {
                Step::Skip => continue,
                step => return Ok(Some(step)),
            }
        }
    }

    /// 状态机的一步（单独包一层：这里才是事件被消费的唯一入口，见模块头）
    fn state_step(&mut self, event: &Value) -> Step {
        let step = self.stream_state.step(event);
        if matches!(step, Step::ToolStart { .. }) {
            self.tools_seen = true;
        }
        step
    }

    /// 一步 → 若干帧
    fn frames(&mut self, step: Step) -> Vec<Frame> {
        let mut chunks: Vec<Value> = Vec::new();
        let mut done = false;
        match step {
            Step::Skip => {}
            Step::Text(text) => {
                self.state.ensure_role(&mut chunks);
                chunks.push(self.state.chunk(json!({ "content": text }), None));
            }
            Step::Think(text) => {
                self.state.ensure_role(&mut chunks);
                chunks.push(self.state.chunk(json!({ "reasoning_content": text }), None));
            }
            Step::ToolStart { index, id, name } => {
                self.state.ensure_role(&mut chunks);
                chunks.push(self.state.chunk(
                    json!({
                        "tool_calls": [{
                            "index": index,
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": "" },
                        }]
                    }),
                    None,
                ));
            }
            Step::ToolArgs { index, delta } => {
                chunks.push(self.state.chunk(
                    json!({
                        "tool_calls": [{
                            "index": index,
                            "function": { "arguments": delta },
                        }]
                    }),
                    None,
                ));
            }
            Step::Done { reason, usage } => {
                self.state.ensure_role(&mut chunks);
                let reason = if reason == "stop" && self.tools_seen {
                    "tool_calls"
                } else {
                    reason
                };
                self.state.finish(reason, usage, &mut chunks);
                done = true;
            }
            Step::Failed { status, message } => {
                crate::server::logging::verbose(
                    "[AStudio]",
                    &format!("流内错误（{status}）：{message}"),
                );
                // 流已经开始，状态码改不了了：按 OpenAI 的流内错误帧下发，
                // 客户端据此识别失败（而不是把半截回答当成完整回答）
                chunks.push(client::error_body(status, &message));
                done = true;
            }
        }
        let mut out: Vec<Frame> = chunks.into_iter().map(Frame::Chunk).collect();
        if done {
            out.push(Frame::Done);
        }
        out
    }

    /// 继续读流并下发（预读用掉的那一步一并带上）
    async fn drive(
        &mut self,
        first: Step,
        sender: tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    ) -> Result<(), GatewayError> {
        let mut pending = Some(first);
        loop {
            let step = match pending.take() {
                Some(step) => Some(step),
                None => match self.read_step().await {
                    Ok(step) => step,
                    Err(message) => {
                        // 读流失败：补一个错误帧 + 收尾，别让客户端一直等
                        let mut chunks: Vec<Value> = vec![client::error_body(502, &message)];
                        chunks.push(self.state.chunk(json!({}), Some("stop")));
                        let mut out: Vec<Frame> =
                            chunks.into_iter().map(Frame::Chunk).collect();
                        out.push(Frame::Done);
                        return send_frames(out, &sender).await;
                    }
                },
            };
            let Some(step) = step else {
                // 流结束但没收到收尾事件：补一个 stop（上游偶发不发 completed）
                let mut chunks: Vec<Value> = Vec::new();
                self.state.ensure_role(&mut chunks);
                self.state.finish("stop", Usage::default(), &mut chunks);
                let mut out: Vec<Frame> = chunks.into_iter().map(Frame::Chunk).collect();
                out.push(Frame::Done);
                return send_frames(out, &sender).await;
            };
            let frames = self.frames(step);
            let done = matches!(frames.last(), Some(Frame::Done));
            send_frames(frames, &sender).await?;
            if done {
                return Ok(());
            }
        }
    }

    /// 非流式：把事件流聚合成一份 `chat.completion`
    async fn aggregate(&mut self) -> Result<ForwardOutcome, GatewayError> {
        let mut acc = Acc::default();
        loop {
            match self.read_step().await {
                Ok(Some(Step::Failed { status, message })) => {
                    return Err(GatewayError::with_status(i32::from(status), message))
                }
                Ok(Some(step)) => acc.push(step),
                Ok(None) => break,
                Err(message) => return Err(GatewayError::with_status(502, message)),
            }
        }
        acc.finish_reason(self.tools_seen);
        Ok(ForwardOutcome::Completion {
            body: acc.completion(&self.state),
        })
    }
}

/// 下发一批帧。
///
/// 写成**自由函数**而不是 `&self` 的方法：`&Forwarder` 需要 `Send` 才能跨
/// `.await`，而 `Forwarder` 里的 `EventReader` 装着 `dyn Stream + Send`
/// （不含 `Sync`），所以 `&Forwarder` 不是 `Send` —— 方法版会让后台任务的
/// future 不 Send，直接编译不过。这里只需要帧本身。
async fn send_frames(
    frames: Vec<Frame>,
    sender: &tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
) -> Result<(), GatewayError> {
    for frame in frames {
        if sender.send(Ok(Bytes::from(frame.wire()))).await.is_err() {
            // 客户端断开：静默收工（这不是错误）
            return Ok(());
        }
    }
    Ok(())
}

// ─── 非流式聚合 ─────────────────────────────────────────────

/// 聚合成 chat.completion 的那堆碎片
#[derive(Default)]
struct Acc {
    content: String,
    reasoning: String,
    tool_calls: Vec<Value>,
    usage: Usage,
    reason: &'static str,
}

impl Acc {
    fn push(&mut self, step: Step) {
        match step {
            Step::Text(text) => self.content.push_str(&text),
            Step::Think(text) => self.reasoning.push_str(&text),
            Step::ToolStart { id, name, .. } => self.tool_calls.push(json!({
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": "" },
            })),
            Step::ToolArgs { index, delta } => {
                // `index` 是上游给的流内序号；聚合是按出现顺序 push 的，
                // 两者在单流内一致（上游偶尔从 1 开始数，所以夹到范围内而不是
                // 直接取下标）。没有工具时这段参数是孤儿，丢掉。
                if !self.tool_calls.is_empty() {
                    let slot =
                        (index.max(0) as usize).min(self.tool_calls.len().saturating_sub(1));
                    if let Some(call) = self.tool_calls.get_mut(slot) {
                        let merged = format!(
                            "{}{}",
                            call.pointer("/function/arguments")
                                .and_then(Value::as_str)
                                .unwrap_or(""),
                            delta
                        );
                        call["function"]["arguments"] = json!(merged);
                    }
                }
            }
            Step::Done { reason, usage } => {
                self.reason = reason;
                self.usage = usage;
            }
            // `Skip` 不该走到这里（`read_step` 已滤掉）；`Failed` 在调用方处理
            Step::Skip | Step::Failed { .. } => {}
        }
    }

    /// 有工具调用时 `stop` 要改报 `tool_calls`（见模块头）
    fn finish_reason(&mut self, tools_seen: bool) {
        if self.reason.is_empty() {
            self.reason = "stop";
        }
        if tools_seen && self.reason == "stop" {
            self.reason = "tool_calls";
        }
    }

    fn completion(&self, state: &State) -> Value {
        let mut message = json!({ "role": "assistant", "content": self.content });
        if !self.reasoning.is_empty() {
            message["reasoning_content"] = json!(self.reasoning);
        }
        if !self.tool_calls.is_empty() {
            message["tool_calls"] = json!(self.tool_calls);
        }
        json!({
            "id": state.id,
            "object": "chat.completion",
            "created": state.created,
            "model": state.model,
            "choices": [{ "index": 0, "message": message, "finish_reason": self.reason }],
            "usage": self.usage.to_json(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forwarder() -> Forwarder {
        Forwarder::new(
            EventReader::new(Box::pin(futures::stream::empty())),
            "spark-x2.6".to_string(),
        )
    }

    fn chunks(frames: &[Frame]) -> Vec<&Value> {
        frames
            .iter()
            .filter_map(|frame| match frame {
                Frame::Chunk(chunk) => Some(chunk),
                Frame::Done => None,
            })
            .collect()
    }

    fn delta_of(frames: &[Frame]) -> Vec<Value> {
        chunks(frames)
            .into_iter()
            .filter_map(|chunk| chunk.pointer("/choices/0/delta").cloned())
            .collect()
    }

    /// 正文前先发 role（客户端按它开始渲染）
    #[test]
    fn the_first_content_frame_is_preceded_by_the_role() {
        let mut forwarder = forwarder();
        let frames = forwarder.frames(Step::Text("你好".to_string()));
        let deltas = delta_of(&frames);
        assert_eq!(deltas[0]["role"], "assistant");
        assert_eq!(deltas[1]["content"], "你好");
        // 第二批不再发 role
        let frames = forwarder.frames(Step::Text("再见".to_string()));
        assert_eq!(delta_of(&frames).len(), 1);
    }

    /// 思考走 reasoning_content，不混进 content
    #[test]
    fn thinking_becomes_reasoning_content() {
        let mut forwarder = forwarder();
        let frames = forwarder.frames(Step::Think("想一想".to_string()));
        let delta = delta_of(&frames)
            .into_iter()
            .find(|delta| delta.get("reasoning_content").is_some())
            .expect("有 reasoning_content");
        assert_eq!(delta["reasoning_content"], "想一想");
        assert!(delta.get("content").is_none());
    }

    /// 工具调用：首帧带 id/name，参数增量只带 arguments，收尾报 tool_calls
    ///
    /// 走 `state_step`（事件 → Step 的唯一入口）而不是直接构造 `Step`：
    /// 「这条流有过工具调用」也是状态机那一步记下的，绕过它测不到真实链路。
    #[test]
    fn tool_calls_switch_the_finish_reason() {
        let mut forwarder = forwarder();
        let start = forwarder.state_step(&json!({
            "type": "response.output_item.added",
            "item": { "type": "function_call", "call_id": "call_1", "name": "get_weather" }
        }));
        let frames = forwarder.frames(start);
        let open = delta_of(&frames)
            .into_iter()
            .find(|delta| delta.get("tool_calls").is_some())
            .expect("有 tool_calls");
        assert_eq!(open.pointer("/tool_calls/0/id").and_then(Value::as_str), Some("call_1"));
        assert_eq!(
            open.pointer("/tool_calls/0/function/name").and_then(Value::as_str),
            Some("get_weather")
        );
        let args = forwarder.state_step(&json!({
            "type": "response.function_call_arguments.delta",
            "delta": "{\"a\":1}"
        }));
        let frames = forwarder.frames(args);
        assert_eq!(
            delta_of(&frames)[0].pointer("/tool_calls/0/function/arguments").and_then(Value::as_str),
            Some("{\"a\":1}")
        );
        // 收尾：上游说 stop，但我们发过工具 → 报 tool_calls
        let done = forwarder.state_step(&json!({
            "type": "response.completed",
            "response": { "usage": { "input_tokens": 100, "output_tokens": 20,
                                     "input_tokens_details": { "cached_tokens": 50 } } }
        }));
        let frames = forwarder.frames(done);
        let last = chunks(&frames).into_iter().last().cloned().expect("有收尾帧");
        assert_eq!(last.pointer("/choices/0/finish_reason"), Some(&json!("tool_calls")));
        assert_eq!(last.pointer("/usage/prompt_tokens"), Some(&json!(100)));
        assert_eq!(
            last.pointer("/usage/prompt_tokens_details/cached_tokens"),
            Some(&json!(50))
        );
        assert!(matches!(frames.last(), Some(Frame::Done)));
    }

    /// 截断（`incomplete`）即使有工具也要保持 `length`：那是真被截了
    #[test]
    fn a_truncated_reply_stays_length() {
        let mut forwarder = forwarder();
        let _ = forwarder.frames(Step::ToolStart {
            index: 0,
            id: "call_1".to_string(),
            name: "x".to_string(),
        });
        let frames = forwarder.frames(Step::Done {
            reason: "length",
            usage: Usage::default(),
        });
        let last = chunks(&frames).into_iter().last().cloned().expect("有收尾帧");
        assert_eq!(last.pointer("/choices/0/finish_reason"), Some(&json!("length")));
    }

    /// 流内错误 → 错误帧 + [DONE]（客户端据此识别失败）
    #[test]
    fn an_in_stream_error_is_reported_inside_the_stream() {
        let mut forwarder = forwarder();
        let frames = forwarder.frames(Step::Failed {
            status: 429,
            message: "积分不足".to_string(),
        });
        let error = chunks(&frames)
            .into_iter()
            .find_map(|chunk| chunk.get("error").cloned())
            .expect("有 error 帧");
        assert_eq!(error["type"], "rate_limit_error");
        assert_eq!(error["message"], "积分不足");
        assert!(matches!(frames.last(), Some(Frame::Done)));
    }

    /// 状态机只喂一次：ToolStart 的序号来自状态机，不能因预读被加两次
    #[test]
    fn the_state_machine_is_stepped_once_per_event() {
        let mut forwarder = forwarder();
        let event = json!({
            "type": "response.output_item.added",
            "item": { "type": "function_call", "call_id": "c1", "name": "f" }
        });
        assert!(matches!(
            forwarder.state_step(&event),
            Step::ToolStart { index: 0, .. }
        ));
        let second = forwarder.state_step(&event);
        assert!(
            matches!(second, Step::ToolStart { index: 1, .. }),
            "第二个工具调用序号应为 1：{second:?}"
        );
        assert!(forwarder.tools_seen);
    }

    /// 行拆分：粘包 / 半帧 / 非 data 行 / CRLF / 结尾无换行
    #[test]
    fn the_line_buffer_handles_fragments() {
        let mut buffer = LineBuffer::default();
        // 一个 3 字节的汉字被切断，逐片解码会变成替换字符
        let bytes = "data: {\"a\":\"你\"}\n\nevent: x\ndata: {\"b\":1}\n".as_bytes();
        let first = buffer.push(&bytes[..12]);
        assert!(first.is_empty(), "半行不该产出");
        let rest = buffer.push(&bytes[12..]);
        assert_eq!(rest.len(), 2, "{rest:?}");
        assert!(rest[0].contains('你'), "{}", rest[0]);
        assert_eq!(rest[1], "{\"b\":1}");
        // CRLF 与结尾没有换行的最后一条
        let mut buffer = LineBuffer::default();
        assert_eq!(buffer.push(b"data: {\"c\":2}\r\n"), vec!["{\"c\":2}".to_string()]);
        assert_eq!(buffer.push(b"data: {\"d\":3}"), Vec::<String>::new());
        assert_eq!(buffer.finish(), vec!["{\"d\":3}".to_string()]);
    }

    /// 读取器：跳过解不出的行、认 [DONE]、尾部半帧在结束时补上
    #[tokio::test]
    async fn the_reader_turns_bytes_into_events() {
        let payload = "data: {\"type\":\"response.created\"}\n\
                       data: not json\n\
                       data: {\"type\":\"response.completed\"}\n\
                       data: [DONE]\n";
        let stream: Source = Box::pin(futures::stream::iter(vec![Ok(Bytes::from(payload))]));
        let mut reader = EventReader::new(stream);
        let mut kinds: Vec<String> = Vec::new();
        while let Ok(Some(event)) = reader.next().await {
            kinds.push(
                event
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            );
        }
        assert_eq!(kinds, vec!["response.created", "response.completed"]);
    }

    /// 非流式聚合：正文 / 思考 / 工具各归各位，收尾报 tool_calls
    #[test]
    fn the_aggregate_builds_a_complete_answer() {
        let mut acc = Acc::default();
        acc.push(Step::Text("答案是".to_string()));
        acc.push(Step::Think("先算一下".to_string()));
        acc.push(Step::ToolStart {
            index: 0,
            id: "c1".to_string(),
            name: "calc".to_string(),
        });
        acc.push(Step::ToolArgs {
            index: 0,
            delta: "{\"x\":1}".to_string(),
        });
        acc.push(Step::ToolArgs {
            index: 0,
            delta: "}".to_string(),
        });
        acc.push(Step::Done {
            reason: "stop",
            usage: Usage {
                input: 10,
                output: 5,
                cached: 0,
            },
        });
        acc.finish_reason(true);
        let completion = acc.completion(&State::new("spark-x2.6"));
        assert_eq!(completion["object"], "chat.completion");
        assert_eq!(
            completion.pointer("/choices/0/message/content").and_then(Value::as_str),
            Some("答案是")
        );
        assert_eq!(
            completion
                .pointer("/choices/0/message/reasoning_content")
                .and_then(Value::as_str),
            Some("先算一下")
        );
        assert_eq!(
            completion
                .pointer("/choices/0/message/tool_calls/0/function/arguments")
                .and_then(Value::as_str),
            Some("{\"x\":1}}")
        );
        assert_eq!(
            completion.pointer("/choices/0/finish_reason").and_then(Value::as_str),
            Some("tool_calls")
        );
        assert_eq!(completion.pointer("/usage/total_tokens").and_then(Value::as_i64), Some(15));
    }

    /// 没有任何收尾事件时也要给出 `stop`（不能留空）
    #[test]
    fn a_missing_stop_reason_falls_back_to_stop() {
        let mut acc = Acc::default();
        acc.push(Step::Text("半截".to_string()));
        acc.finish_reason(false);
        assert_eq!(acc.reason, "stop");
    }
}
