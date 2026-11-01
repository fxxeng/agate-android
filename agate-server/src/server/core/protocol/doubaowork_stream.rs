//! 上游豆包工作 SSE（**自有事件流**）→ 标准 chat SSE（状态机）。
//!
//! ── 为什么另开一台（而不是复用任何一台既有状态机）──────────────
//! 上游的流是 SSE，但它的**事件名与载荷载体都与别家不同**：
//! ```text
//!   event: SSE_ACK          ← 会话建立（conversation_id）
//!   event: CHUNK_DELTA      ← 正文增量（多种载体，见下）
//!   event: SSE_REPLY_END    ← 收尾信号（不是 [DONE]）
//!   event: gateway-error    ← 错误（**HTTP 恒 200**，错误在流内）
//! ```
//! 与 OpenAI 的 `choices[].delta`、NDJSON 的行形态、ChatGLM 的 `parts[]`、
//! DeepSeek 的 path-value 全不同 —— 没有可复用的词表。
//!
//! ── `event:` 名是解析的一部分（因此这里自带一个缓冲器）──────────
//! 共享的 `protocol::SseLineBuffer` **刻意丢弃 `event:` 行**（它的调用方都只
//! 看载荷、事件名从 JSON 的 `type` 字段读）。本家不行：`SSE_ACK` /
//! `SSE_REPLY_END` / `gateway-error` 三个信号**只**出现在 `event:` 行上，
//! 载荷里没有等价字段。因此本文件自带 [`EventLineBuffer`]（保留 event 名），
//! 而不是去改共享缓冲器的语义（那会波及另外四家的解析路径）。
//!
//! ── 解析与折帧是两件事（本台只做后者）──────────────────────────
//! 载荷 → [`Delta`] 的解析全部在 `providers::doubaowork::chat::parse_event`
//! 里（纯函数、已被一整套单测覆盖）。本台只负责把那一串 `Delta` 按顺序折成
//! chat SSE 帧 —— 这是刻意的分工：解析规则会随上游变化而变，帧构造不会。
//!
//! ── `HTTP 恒 200` 的后果（与 Command Code 同一处境）────────────
//! 认证失败 / 限流 / 额度不足全部以流内 `gateway-error` 表达，**连接的成败与
//! 这一轮生成成没成是两件事**。因此错误帧由本台折出（下游的聚合器据此转 502、
//! 流式路径原样透给客户端），而不是靠 HTTP 状态码分流。
//!
//! ── 收尾时序（与另外四台一致）────────────────────────────────
//!   1. 内容帧永远先于收尾帧；
//!   2. 收到 `SSE_REPLY_END`（或 `[DONE]`）→ finish 帧 + usage 帧 + `[DONE]`；
//!   3. **没有收尾信号**就 EOF → 按「响应被截断」报错，不补 finish_reason
//!      （补了等于把截断谎报成完整回答）；
//!   4. 一个内容帧都没给就收尾 → 按「上游返回空响应」报错
//!      （防下游把空回答记成成功计费）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::protocol::{chat_frame, random_id};
use crate::server::core::providers::doubaowork::chat::{self, Delta};

/// 保留 `event:` 名的 SSE 行缓冲器（见模块头：本家不能丢事件名）。
///
/// 逐行累积，遇空行即一条事件结束（SSE 规范：事件由空行分隔）。返回
/// `(event 名, data 文本)`；`event` 为 `None` 表示这一条没有 `event:` 行。
pub struct EventLineBuffer {
    tail: Vec<u8>,
    /// 当前事件的 `event:` 名（跨行暂存，遇空行清空）
    pending_event: Option<String>,
    /// 当前事件的 `data:` 行（SSE 允许多行 data，按规范用 \n 拼接）
    pending_data: String,
    /// 当前事件是否已经见到过 data（用于判断「空 data」与「没有 data」）
    has_data: bool,
}

impl EventLineBuffer {
    pub fn new() -> Self {
        Self {
            tail: Vec::new(),
            pending_event: None,
            pending_data: String::new(),
            has_data: false,
        }
    }

    /// 吃一段字节，吐出其中**完整**的事件（0..n 个）。
    pub fn push(&mut self, chunk: &[u8]) -> Vec<(Option<String>, String)> {
        let mut out = Vec::new();
        self.tail.extend_from_slice(chunk);
        let mut start = 0usize;
        while let Some(offset) = self.tail[start..].iter().position(|byte| *byte == b'\n') {
            let end = start + offset;
            let raw = String::from_utf8_lossy(&self.tail[start..end]).to_string();
            let line = raw.strip_suffix('\r').unwrap_or(&raw).to_string();
            self.take_line(&line, &mut out);
            start = end + 1;
        }
        self.tail.drain(..start);
        out
    }

    /// 流结束：冲刷残留半行（上游可能最后一帧不带收尾空行）。
    pub fn finish(&mut self) -> Vec<(Option<String>, String)> {
        let mut out = Vec::new();
        if !self.tail.is_empty() {
            let raw = String::from_utf8_lossy(&self.tail).to_string();
            self.tail.clear();
            let line = raw.strip_suffix('\r').unwrap_or(&raw).to_string();
            self.take_line(&line, &mut out);
        }
        // 还留着未闭合的事件：也交出去（缺收尾空行的上游会这样）
        if self.has_data || self.pending_event.is_some() {
            out.push(self.flush_event());
        }
        out
    }

    /// 处理一行
    fn take_line(&mut self, line: &str, out: &mut Vec<(Option<String>, String)>) {
        if line.is_empty() {
            // 空行 = 事件结束
            if self.has_data || self.pending_event.is_some() {
                out.push(self.flush_event());
            }
            return;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            self.pending_event = Some(rest.trim().to_string());
            return;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let data = rest.strip_prefix(' ').unwrap_or(rest);
            if self.has_data {
                self.pending_data.push('\n');
            }
            self.pending_data.push_str(data);
            self.has_data = true;
            return;
        }
        // id: / retry: / 注释行 → 忽略
    }

    /// 交出当前事件并复位
    fn flush_event(&mut self) -> (Option<String>, String) {
        let event = self.pending_event.take();
        let data = std::mem::take(&mut self.pending_data);
        self.has_data = false;
        (event, data)
    }
}

/// 上游豆包工作 SSE → 标准 chat SSE 增量帧。
pub struct ChatFromDoubaoworkStream {
    buffer: EventLineBuffer,
    /// 输出帧的 model（发给上游的真名；ForwardStream 的回写层按需改写）
    model: String,
    id: String,
    created: i64,
    /// 已下发过首帧（role assistant）
    started: bool,
    finished: bool,
    /// 思考链状态（`parse_event` 需要它跨帧记忆「当前是不是在思维链里」）
    in_thinking: bool,
    /// 上游最后一次给出的会话 id（收尾帧里回填，便于排查）
    conversation_id: Option<String>,
    /// 上游是否给过 token 计数（本家能给 —— 与 DeepSeek 不同，见 `parse_event`）
    usage: Option<Value>,
}

impl ChatFromDoubaoworkStream {
    pub fn new(model: &str) -> Self {
        Self {
            buffer: EventLineBuffer::new(),
            model: model.to_string(),
            id: String::new(),
            created: crate::server::logging::now_ms() / 1000,
            started: false,
            finished: false,
            in_thinking: false,
            conversation_id: None,
            usage: None,
        }
    }

    /// 吃一段上游字节，吐出要下发的 chat SSE 帧
    pub fn push(&mut self, chunk: &[u8]) -> Vec<bytes::Bytes> {
        let mut out = Vec::new();
        for (event, data) in self.buffer.push(chunk) {
            for delta in chat::parse_event(event.as_deref(), &data, &mut self.in_thinking) {
                out.extend(self.consume(delta));
                if self.finished {
                    return out;
                }
            }
        }
        out
    }

    /// 上游流结束：按「有没有见过收尾信号」收尾
    pub fn finish(&mut self) -> Vec<bytes::Bytes> {
        if self.finished {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (event, data) in self.buffer.finish() {
            for delta in chat::parse_event(event.as_deref(), &data, &mut self.in_thinking) {
                out.extend(self.consume(delta));
                if self.finished {
                    return out;
                }
            }
        }
        if self.finished {
            return out;
        }
        // 没有 SSE_REPLY_END 就 EOF：上游契约里是「没走完」。如实报截断，
        // **不补** finish_reason（补了等于把截断谎报成完整回答）。
        out.extend(self.fail_message("上游流在收到完成信号之前结束（响应被截断）", 502));
        out
    }

    /// 一个增量 → 零到多个 chat 帧
    fn consume(&mut self, delta: Delta) -> Vec<bytes::Bytes> {
        if self.finished {
            return Vec::new();
        }
        match delta {
            Delta::Text(text) => self.emit_delta(&text, ""),
            Delta::Thinking(text) => self.emit_delta("", &text),
            // 工具 / 联网提示按正文透出（本家没有原生 function calling，
            // 上游把这些当文本给，见 `doubaowork::chat` 的模块头）
            Delta::Tool(text) => self.emit_delta(&text, ""),
            Delta::Conversation(id) => {
                self.conversation_id = Some(id);
                Vec::new()
            }
            Delta::Error { code, message } => {
                // 额度类业务码按 429（下游据此退避 / 换账号比塌成 502 有用），
                // 其余按 502
                let status = if is_quota_code(code) { 429 } else { 502 };
                self.fail_with_code(&message, code, status)
            }
            Delta::Done => self.complete(),
        }
    }

    /// 下发一段增量（`content` 与 `reasoning_content` 各给各的）
    fn emit_delta(&mut self, content: &str, thinking: &str) -> Vec<bytes::Bytes> {
        if content.is_empty() && thinking.is_empty() {
            return Vec::new();
        }
        let mut out = self.start();
        let mut delta = serde_json::Map::new();
        if !thinking.is_empty() {
            delta.insert(
                "reasoning_content".to_string(),
                Value::String(thinking.to_string()),
            );
        }
        if !content.is_empty() {
            delta.insert("content".to_string(), Value::String(content.to_string()));
        }
        out.push(self.delta_frame(Value::Object(delta)));
        out
    }

    /// 首帧（role assistant），只发一次
    fn start(&mut self) -> Vec<bytes::Bytes> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        if self.id.is_empty() {
            self.id = random_id("chatcmpl");
        }
        vec![self.delta_frame(json!({ "role": "assistant", "content": "" }))]
    }

    /// 正常收尾：finish 帧 + usage 帧 + `[DONE]`
    fn complete(&mut self) -> Vec<bytes::Bytes> {
        if self.finished {
            return Vec::new();
        }
        // 一个内容帧都没给就收尾：成功但什么都没有的流，记成成功会让下游
        // 按空回答计费（判据与另外三台同款）
        if !self.started {
            return self.fail_message("上游返回空响应（没有任何内容）", 429);
        }
        self.finished = true;
        let mut out = self.start();
        out.push(self.finish_frame("stop"));
        out.push(chat_frame(&json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [],
            "usage": self.usage_payload(),
        })));
        out.push(bytes::Bytes::from_static(b"data: [DONE]\n\n"));
        out
    }

    /// 流内错误 → 错误帧 + `[DONE]`（只做一次）
    fn fail_with_code(&mut self, message: &str, code: i64, status: i64) -> Vec<bytes::Bytes> {
        let kind = if status == 429 {
            "rate_limit_error"
        } else {
            "upstream_error"
        };
        self.fail_message_with_kind(message, kind, status, Some(code))
    }

    fn fail_message(&mut self, message: &str, status: i64) -> Vec<bytes::Bytes> {
        let kind = if status == 429 {
            "rate_limit_error"
        } else {
            "upstream_error"
        };
        self.fail_message_with_kind(message, kind, status, None)
    }

    /// 落一个协议级失败帧（错误帧 + `[DONE]`，只做一次）。
    ///
    /// 错误路径**不**补 usage 帧（带 usage 且 choices 为空的帧是 OpenAI 的收尾
    /// 形态，部分客户端据此判定「流已结束」—— 在它之后补错误帧等于让那些客户端
    /// 看不到失败；与另外四台逐字同款）。
    fn fail_message_with_kind(
        &mut self,
        message: &str,
        kind: &str,
        status: i64,
        code: Option<i64>,
    ) -> Vec<bytes::Bytes> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut error = json!({
            "message": message,
            "type": kind,
            "status": status,
        });
        if let Some(code) = code {
            if let Some(object) = error.as_object_mut() {
                object.insert("code".to_string(), Value::from(code));
            }
        }
        let mut frame = json!({ "error": error });
        if status == 429 {
            if let Some(object) = frame.as_object_mut() {
                object.insert("retry_after".to_string(), Value::from(30));
            }
        }
        let mut out = vec![chat_frame(&frame)];
        out.push(bytes::Bytes::from_static(b"data: [DONE]\n\n"));
        out
    }

    /// usage：上游给了就用，没给诚实回 0（不编造数字）
    fn usage_payload(&self) -> Value {
        match self.usage.clone() {
            Some(usage) => usage,
            None => json!({
                "prompt_tokens": 0,
                "completion_tokens": 0,
                "total_tokens": 0,
            }),
        }
    }

    /// 组装一条 chat 增量帧
    fn delta_frame(&self, delta: Value) -> bytes::Bytes {
        chat_frame(&json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": Value::Null }],
        }))
    }

    /// 组装收尾帧
    fn finish_frame(&self, reason: &str) -> bytes::Bytes {
        chat_frame(&json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{ "index": 0, "delta": {}, "finish_reason": reason }],
        }))
    }
}

/// 哪些业务码算「额度 / 限流」（下游按 429 处置更有用）。
///
/// 判据是**码的语义范围**而不是具体枚举：上游把额度类的码集中在 4xxxx 段
/// （`doubaowork::chat` 的解析里能看到的 `error_code` 一族）。这里只放行
/// 「明确是额度」的那些，其余照旧 502 —— 把服务故障误判成限流会让下游
/// 白白退避重试一条永远失败的请求。
fn is_quota_code(code: i64) -> bool {
    // 上游额度 / 风控 / 频控相关的码（依据 `doubaowork::chat` 的常量表）
    matches!(code, 71_010_001 | 71_010_002 | 71_010_003 | 71_010_004 | 71_010_005)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把下行帧折成 (正文, 思考, finish_reason, 是否错误)
    fn collect(frames: &[bytes::Bytes]) -> (String, String, Option<String>, bool) {
        let mut text = String::new();
        let mut thinking = String::new();
        let mut finish = None;
        let mut errored = false;
        for frame in frames {
            let raw = String::from_utf8_lossy(frame);
            let Some(payload) = raw.trim().strip_prefix("data: ") else {
                continue;
            };
            if payload == "[DONE]" {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(payload) else {
                continue;
            };
            if value.get("error").is_some() {
                errored = true;
                continue;
            }
            let Some(choice) = value
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|list| list.first())
            else {
                continue;
            };
            if let Some(delta) = choice.get("delta") {
                if let Some(content) = delta.get("content").and_then(Value::as_str) {
                    text.push_str(content);
                }
                if let Some(reason) = delta.get("reasoning_content").and_then(Value::as_str) {
                    thinking.push_str(reason);
                }
            }
            if let Some(reason) = choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                finish = Some(reason.to_string());
            }
        }
        (text, thinking, finish, errored)
    }

    fn sse(event: &str, data: &str) -> Vec<u8> {
        format!("event: {event}\ndata: {data}\n\n").into_bytes()
    }

    /// 缓冲器：保留 event 名、按空行切事件、多行 data 用 \n 拼接
    #[test]
    fn event_buffer_keeps_event_name_and_joins_data_lines() {
        let mut buffer = EventLineBuffer::new();
        let events = buffer.push(b"event: CHUNK_DELTA\ndata: line1\ndata: line2\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0.as_deref(), Some("CHUNK_DELTA"));
        assert_eq!(events[0].1, "line1\nline2");
    }

    /// 缓冲器：没有 event 行的事件也要交出来（`None`）
    #[test]
    fn event_buffer_handles_nameless_events() {
        let mut buffer = EventLineBuffer::new();
        let events = buffer.push(b"data: {\"a\":1}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, None);
    }

    /// 缓冲器：跨 chunk 的半行要拼起来（网络分片是常态）
    #[test]
    fn event_buffer_reassembles_split_chunks() {
        let mut buffer = EventLineBuffer::new();
        assert!(buffer.push(b"event: CHUNK_").is_empty());
        let events = buffer.push(b"DELTA\ndata: {\"x\":1}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0.as_deref(), Some("CHUNK_DELTA"));
        assert_eq!(events[0].1, "{\"x\":1}");
    }

    /// 缓冲器：finish 冲刷未闭合的尾部事件
    #[test]
    fn event_buffer_finish_flushes_open_event() {
        let mut buffer = EventLineBuffer::new();
        buffer.push(b"event: SSE_REPLY_END\ndata: {}");
        let events = buffer.finish();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0.as_deref(), Some("SSE_REPLY_END"));
    }

    /// 完整链路：ACK → 正文增量 → 收尾
    #[test]
    fn full_flow_emits_content_then_finishes() {
        let mut machine = ChatFromDoubaoworkStream::new("doubao");
        let mut frames = Vec::new();
        frames.extend(machine.push(&sse(
            "SSE_ACK",
            r#"{"ack_client_meta":{"conversation_id":"conv-1"}}"#,
        )));
        frames.extend(machine.push(&sse("CHUNK_DELTA", r#"{"text":"你好"}"#)));
        frames.extend(machine.push(&sse("CHUNK_DELTA", r#"{"text":"世界"}"#)));
        frames.extend(machine.push(&sse("SSE_REPLY_END", "{}")));
        let (text, _, finish, errored) = collect(&frames);
        assert_eq!(text, "你好世界");
        assert_eq!(finish.as_deref(), Some("stop"));
        assert!(!errored);
    }

    /// 收尾必须带 [DONE]
    #[test]
    fn done_marker_is_appended() {
        let mut machine = ChatFromDoubaoworkStream::new("doubao");
        machine.push(&sse("CHUNK_DELTA", r#"{"text":"x"}"#));
        let frames = machine.push(&sse("SSE_REPLY_END", "{}"));
        let joined = frames
            .iter()
            .map(|frame| String::from_utf8_lossy(frame).to_string())
            .collect::<String>();
        assert!(joined.contains("data: [DONE]"));
    }

    /// `[DONE]` 载荷也当收尾信号（兼容）
    #[test]
    fn done_payload_terminates() {
        let mut machine = ChatFromDoubaoworkStream::new("doubao");
        machine.push(&sse("CHUNK_DELTA", r#"{"text":"x"}"#));
        let frames = machine.push(b"data: [DONE]\n\n");
        let (_, _, finish, _) = collect(&frames);
        assert_eq!(finish.as_deref(), Some("stop"));
    }

    /// 流内网关错误 → 错误帧（HTTP 恒 200 的家，错误只能在流内表达）
    #[test]
    fn gateway_error_becomes_error_frame() {
        let mut machine = ChatFromDoubaoworkStream::new("doubao");
        let frames = machine.push(&sse(
            "gateway-error",
            r#"{"code":71010001,"message":"额度不足"}"#,
        ));
        let (_, _, _, errored) = collect(&frames);
        assert!(errored, "应产出错误帧");
        let raw = String::from_utf8_lossy(&frames[0]).to_string();
        assert!(raw.contains("额度不足"), "实际: {raw}");
    }

    /// 截断流（没有 SSE_REPLY_END）→ 报错，不谎报成功
    #[test]
    fn truncated_stream_reports_error() {
        let mut machine = ChatFromDoubaoworkStream::new("doubao");
        machine.push(&sse("CHUNK_DELTA", r#"{"text":"半截"}"#));
        let frames = machine.finish();
        let (_, _, _, errored) = collect(&frames);
        assert!(errored);
        let raw = String::from_utf8_lossy(&frames[0]).to_string();
        assert!(raw.contains("截断"), "实际: {raw}");
    }

    /// 一个内容都没有就收尾 → 空响应报错（不记成空回答成功）
    #[test]
    fn empty_stream_reports_error() {
        let mut machine = ChatFromDoubaoworkStream::new("doubao");
        let frames = machine.push(&sse("SSE_REPLY_END", "{}"));
        let (_, _, _, errored) = collect(&frames);
        assert!(errored);
        let raw = String::from_utf8_lossy(&frames[0]).to_string();
        assert!(raw.contains("空响应"), "实际: {raw}");
    }

    /// ACK 不产出内容帧（会话建立是信号，不是正文）
    #[test]
    fn ack_emits_no_content() {
        let mut machine = ChatFromDoubaoworkStream::new("doubao");
        let frames = machine.push(&sse(
            "SSE_ACK",
            r#"{"ack_client_meta":{"conversation_id":"conv-1"}}"#,
        ));
        assert!(frames.is_empty(), "ACK 不应产出帧");
    }

    /// 非 JSON 载荷不打断整条流（跳过该帧）
    #[test]
    fn malformed_payload_does_not_break_the_stream() {
        let mut machine = ChatFromDoubaoworkStream::new("doubao");
        let frames = machine.push(&sse("CHUNK_DELTA", "not-json"));
        assert!(frames.is_empty(), "坏帧应被跳过而不是报错");
        // 后续正常帧仍然工作
        let frames = machine.push(&sse("CHUNK_DELTA", r#"{"text":"好的"}"#));
        let (text, _, _, _) = collect(&frames);
        assert_eq!(text, "好的");
    }

    /// 额度类错误码走 429（下游据此退避 / 换账号更有用）
    #[test]
    fn quota_code_maps_to_rate_limit() {
        let mut machine = ChatFromDoubaoworkStream::new("doubao");
        let frames = machine.push(&sse(
            "gateway-error",
            r#"{"code":71010001,"message":"额度不足"}"#,
        ));
        let raw = String::from_utf8_lossy(&frames[0]).to_string();
        assert!(raw.contains("rate_limit_error"), "实际: {raw}");
        assert!(raw.contains("429"));
    }

    /// 非额度类错误码走 502
    #[test]
    fn generic_error_maps_to_upstream_error() {
        let mut machine = ChatFromDoubaoworkStream::new("doubao");
        let frames = machine.push(&sse(
            "gateway-error",
            r#"{"code":99999,"message":"服务内部错误"}"#,
        ));
        let raw = String::from_utf8_lossy(&frames[0]).to_string();
        assert!(raw.contains("upstream_error"), "实际: {raw}");
    }

    /// 收敛后不再产出（幂等）
    #[test]
    fn terminated_machine_ignores_further_input() {
        let mut machine = ChatFromDoubaoworkStream::new("doubao");
        machine.push(&sse("CHUNK_DELTA", r#"{"text":"x"}"#));
        machine.push(&sse("SSE_REPLY_END", "{}"));
        let after = machine.push(&sse("CHUNK_DELTA", r#"{"text":"不该出现"}"#));
        assert!(after.is_empty(), "收尾后不应再产出帧");
    }

    /// 无 event 名的裸 data 帧也能走通（上游有两类形态）
    #[test]
    fn nameless_event_is_still_parsed() {
        let mut machine = ChatFromDoubaoworkStream::new("doubao");
        let frames = machine.push("data: {\"text\":\"裸帧\"}\n\n".as_bytes());
        let (text, _, _, _) = collect(&frames);
        assert_eq!(text, "裸帧");
    }

    /// usage 帧在收尾时给出（上游没给数字时诚实回 0）
    #[test]
    fn usage_is_reported_on_completion() {
        let mut machine = ChatFromDoubaoworkStream::new("doubao");
        machine.push(&sse("CHUNK_DELTA", r#"{"text":"x"}"#));
        let frames = machine.push(&sse("SSE_REPLY_END", "{}"));
        let joined = frames
            .iter()
            .map(|frame| String::from_utf8_lossy(frame).to_string())
            .collect::<String>();
        assert!(joined.contains("\"usage\""));
        assert!(joined.contains("total_tokens"));
    }

    /// 额度码判据：只认明确是额度的那些
    #[test]
    fn quota_code_predicate() {
        assert!(is_quota_code(71_010_001));
        assert!(!is_quota_code(0));
        assert!(!is_quota_code(99_999));
    }
}
