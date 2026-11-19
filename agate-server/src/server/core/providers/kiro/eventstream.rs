//! AWS event stream 分帧（Kiro 的响应格式，**二进制**）。
//!
//! ── 帧结构 ──────────────────────────────────────────────────
//! ```text
//!   [0..4)              total length（含首尾，含 CRC）
//!   [4..8)              headers length
//!   [8..12)             prelude CRC（不校验）
//!   [12..12+headers)    headers
//!   [..total-4)         payload（JSON）
//!   [total-4..total)    message CRC（不校验）
//! ```
//! 每个 header 是 `名字长度(1) 名字 类型(1) 值`，类型值：
//! `0`=true / `1`=false（无字节）、`2`=byte(1) / `3`=short(2) / `4`=int(4) /
//! `5`=long(8) / `8`=timestamp(8) / `9`=uuid(16)、`6`=bytes(2 字节长度)、
//! `7`=string(2 字节长度)。
//!
//! 我们只关心类型 7 的字符串头：`:message-type`（event / exception / error）、
//! `:event-type`、`:exception-type`、`:error-code`。
//!
//! ── 为什么 CRC 不校验 ───────────────────────────────────────
//! 校验失败只能做一件事：把整次回答判失败。而 TCP + TLS 已经把传输错误挡住了，
//! 真正会出现的「坏帧」是协议变更 —— 那用长度字段就能发现（越界 / 超上限），
//! 而且能给出比「CRC 不对」更有用的原因。上游插件同样不校验。
//!
//! ── 为什么分帧要能离线测 ────────────────────────────────────
//! 这段代码是整条链路上唯一「输入是不可信二进制」的地方，任何算错都会表现为
//! 「模型答到一半断了」这种极难定位的现场。所以 [`take_frame`] 是**纯函数**
//! （喂字节、出帧），单元测试直接构造合成帧覆盖：跨 chunk、粘包、多字节 header、
//! 假长度、超大帧。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic；索引全部先判长度。

use std::pin::Pin;

use bytes::Bytes;
use futures::{Stream, StreamExt};

/// 一帧的上限（16MB）：超过这个数不可能是正常帧，一定是长度字段坏了。
/// 不设上限的话，一个坏字段就能让我们一直缓存到内存耗尽。
const MAX_FRAME_BYTES: usize = 16 << 20;

/// 帧头的最小长度（两个长度字段 + CRC 占位，payload 至少要能放下 CRC）
const MIN_FRAME_BYTES: usize = 16;

/// 上游字节流（与 `UpstreamResponse` 的流同型）
pub type Source = Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>;

/// 一个解析出来的帧。
#[derive(Clone, Debug, Default)]
pub struct Frame {
    /// `:event-type`（`assistantResponseEvent` 一类）
    pub event_type: String,
    /// `:message-type`（`event` / `exception` / `error`）
    pub message_type: String,
    /// `:exception-type`（异常帧的类型名）
    pub exception_type: String,
    /// `:error-code`
    pub error_code: String,
    /// JSON 载荷（原始字节）
    pub payload: Vec<u8>,
}

impl Frame {
    /// 这一帧是不是错误（exception / error / 带错误码的事件）
    pub fn is_error(&self) -> bool {
        self.message_type == "exception"
            || self.message_type == "error"
            || matches!(
                self.event_type.as_str(),
                "error"
                    | "throttlingError"
                    | "validationError"
                    | "serviceUnavailableError"
                    | "internalServerException"
            )
    }

    /// 错误的类型名（异常类型优先，其次错误码，最后事件名）
    pub fn error_kind(&self) -> String {
        if !self.exception_type.is_empty() {
            self.exception_type.clone()
        } else if !self.error_code.is_empty() {
            self.error_code.clone()
        } else {
            self.event_type.clone()
        }
    }

    /// 载荷按 UTF-8 读（坏字节用替换字符，不丢整帧）
    pub fn payload_text(&self) -> String {
        String::from_utf8_lossy(&self.payload).to_string()
    }
}

/// 从缓冲区里取一个完整帧；不足一帧返回 `Ok(None)`，帧头不合法返回 `Err`。
///
/// 纯函数：只动缓冲区，不碰网络 —— 单元测试直接喂字节。
pub fn take_frame(buffer: &mut Vec<u8>) -> Result<Option<Frame>, String> {
    if buffer.len() < 12 {
        return Ok(None);
    }
    let total = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
    let header_length = u32::from_be_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]) as usize;
    if total < MIN_FRAME_BYTES || total > MAX_FRAME_BYTES {
        return Err(format!("Kiro 的事件流帧长度不合理（{total} 字节）"));
    }
    if header_length > total.saturating_sub(MIN_FRAME_BYTES) {
        return Err(format!(
            "Kiro 的事件流帧头长度不合理（{header_length} > {total}）"
        ));
    }
    if buffer.len() < total {
        return Ok(None);
    }
    let (event_type, message_type, exception_type, error_code) =
        parse_headers(&buffer[12..12 + header_length]);
    let payload = buffer[12 + header_length..total - 4].to_vec();
    buffer.drain(..total);
    Ok(Some(Frame {
        event_type,
        message_type,
        exception_type,
        error_code,
        payload,
    }))
}

/// 解析 header 段 →（event-type, message-type, exception-type, error-code）。
///
/// 越界就停下并返回已读到的部分：坏 header 不该让整次回答失败 ——
/// 没有事件名的帧会被上层当作未知事件忽略，而正文仍在 payload 里。
pub fn parse_headers(bytes: &[u8]) -> (String, String, String, String) {
    let mut event_type = String::new();
    let mut message_type = String::new();
    let mut exception_type = String::new();
    let mut error_code = String::new();
    let mut index = 0usize;
    while index < bytes.len() {
        let name_length = bytes[index] as usize;
        index += 1;
        if index + name_length + 1 > bytes.len() {
            break;
        }
        let name = String::from_utf8_lossy(&bytes[index..index + name_length]).to_string();
        index += name_length;
        let kind = bytes[index];
        index += 1;
        match kind {
            0 | 1 => {
                // 布尔头没有值字节；`:message-type` 用它表达 event / error
                if name == ":message-type" {
                    message_type = if kind == 0 { "event" } else { "error" }.to_string();
                }
            }
            2 | 3 | 4 | 5 | 8 | 9 => {
                let size = match kind {
                    2 => 1,
                    3 => 2,
                    4 => 4,
                    5 | 8 => 8,
                    _ => 16,
                };
                index += size;
            }
            6 | 7 => {
                if index + 2 > bytes.len() {
                    break;
                }
                let length = u16::from_be_bytes([bytes[index], bytes[index + 1]]) as usize;
                index += 2;
                if index + length > bytes.len() {
                    break;
                }
                if kind == 7 {
                    let value = String::from_utf8_lossy(&bytes[index..index + length]).to_string();
                    match name.as_str() {
                        ":event-type" => event_type = value,
                        ":message-type" => message_type = value,
                        ":exception-type" => exception_type = value,
                        ":error-code" => error_code = value,
                        _ => {}
                    }
                }
                index += length;
            }
            _ => break,
        }
    }
    (event_type, message_type, exception_type, error_code)
}

/// 增量帧读取器：把字节流切成帧。
pub struct Reader {
    source: Source,
    buffer: Vec<u8>,
    ended: bool,
}

impl Reader {
    pub fn new(source: Source) -> Self {
        Self {
            source,
            buffer: Vec::new(),
            ended: false,
        }
    }

    /// 读下一帧（流正常结束返回 `Ok(None)`）
    pub async fn next(&mut self) -> Result<Option<Frame>, String> {
        loop {
            if let Some(frame) = take_frame(&mut self.buffer)? {
                return Ok(Some(frame));
            }
            if self.ended {
                // 流已结束但缓冲区里还留着半帧：上游把帧截断了，这是真错误
                return if self.buffer.is_empty() {
                    Ok(None)
                } else {
                    Err(format!(
                        "Kiro 的事件流提前结束（还剩 {} 字节未成帧）",
                        self.buffer.len()
                    ))
                };
            }
            match self.source.next().await {
                Some(Ok(chunk)) => {
                    if self.buffer.len() + chunk.len() > MAX_FRAME_BYTES * 2 {
                        return Err("Kiro 的事件流缓冲超出上限".to_string());
                    }
                    self.buffer.extend_from_slice(&chunk);
                }
                Some(Err(error)) => return Err(format!("读取 Kiro 的事件流失败：{error}")),
                None => self.ended = true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合成一帧（CRC 填 0：解析不校验它）
    fn build(event_type: &str, message_type: &str, payload: &str) -> Vec<u8> {
        build_full(event_type, message_type, "", "", payload)
    }

    fn build_full(
        event_type: &str,
        message_type: &str,
        exception_type: &str,
        error_code: &str,
        payload: &str,
    ) -> Vec<u8> {
        let mut headers = Vec::new();
        let mut push = |name: &str, value: &str| {
            headers.push(name.len() as u8);
            headers.extend_from_slice(name.as_bytes());
            headers.push(7); // type 7 = string
            headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
            headers.extend_from_slice(value.as_bytes());
        };
        if !event_type.is_empty() {
            push(":event-type", event_type);
        }
        if !message_type.is_empty() {
            push(":message-type", message_type);
        }
        if !exception_type.is_empty() {
            push(":exception-type", exception_type);
        }
        if !error_code.is_empty() {
            push(":error-code", error_code);
        }
        let total = 12 + headers.len() + payload.len() + 4;
        let mut frame = Vec::with_capacity(total);
        frame.extend_from_slice(&(total as u32).to_be_bytes());
        frame.extend_from_slice(&(headers.len() as u32).to_be_bytes());
        frame.extend_from_slice(&0u32.to_be_bytes()); // prelude CRC
        frame.extend_from_slice(&headers);
        frame.extend_from_slice(payload.as_bytes());
        frame.extend_from_slice(&0u32.to_be_bytes()); // message CRC
        assert_eq!(frame.len(), total);
        frame
    }

    /// 粘包：一次给两帧，逐帧取出，payload 不串
    #[test]
    fn two_frames_in_one_chunk_are_split() {
        let mut buffer = build("assistantResponseEvent", "event", r#"{"content":"a"}"#);
        buffer.extend_from_slice(&build("metadataEvent", "event", r#"{"stopReason":"end_turn"}"#));
        let first = take_frame(&mut buffer).expect("第一帧").expect("应有一帧");
        assert_eq!(first.event_type, "assistantResponseEvent");
        assert_eq!(first.payload_text(), r#"{"content":"a"}"#);
        let second = take_frame(&mut buffer).expect("第二帧").expect("应有一帧");
        assert_eq!(second.event_type, "metadataEvent");
        assert!(take_frame(&mut buffer).expect("空缓冲").is_none());
    }

    /// 半帧：不足一帧时留在缓冲里，补齐后正好取出
    #[test]
    fn a_partial_frame_waits_for_the_rest() {
        let full = build("assistantResponseEvent", "event", r#"{"content":"hello"}"#);
        let (head, tail) = full.split_at(20);
        let mut buffer = head.to_vec();
        assert!(take_frame(&mut buffer).expect("半帧不是错误").is_none());
        buffer.extend_from_slice(tail);
        let frame = take_frame(&mut buffer).expect("完整帧").expect("应有一帧");
        assert_eq!(frame.payload_text(), r#"{"content":"hello"}"#);
    }

    /// 异常帧：类型名按 `exception-type` → `error-code` → `event-type` 的顺序取
    #[test]
    fn exception_frames_report_the_right_kind() {
        let mut buffer =
            build_full("", "exception", "ThrottlingException", "", r#"{"message":"slow"}"#);
        let frame = take_frame(&mut buffer).expect("应能解析").expect("应有一帧");
        assert!(frame.is_error());
        assert_eq!(frame.error_kind(), "ThrottlingException");
        assert_eq!(frame.payload_text(), r#"{"message":"slow"}"#);

        let mut buffer = build_full("", "error", "", "InternalServerException", "{}");
        let frame = take_frame(&mut buffer).expect("应能解析").expect("应有一帧");
        assert_eq!(frame.error_kind(), "InternalServerException");

        // 没有 exception/error 头的事件帧不算错误
        let mut buffer = build("assistantResponseEvent", "event", "{}");
        let frame = take_frame(&mut buffer).expect("应能解析").expect("应有一帧");
        assert!(!frame.is_error());
    }

    /// 长度字段坏了：明确报错，而不是一直等着或吃满内存
    #[test]
    fn hostile_lengths_are_rejected() {
        // total 太小
        let mut too_small = vec![0u8; 12];
        too_small[..4].copy_from_slice(&8u32.to_be_bytes());
        assert!(take_frame(&mut too_small).is_err());

        // total 超上限
        let mut too_big = vec![0u8; 12];
        too_big[..4].copy_from_slice(&(MAX_FRAME_BYTES as u32 + 1).to_be_bytes());
        assert!(take_frame(&mut too_big).is_err());

        // header 长度超过 total
        let mut bad_header = vec![0u8; 12];
        bad_header[..4].copy_from_slice(&64u32.to_be_bytes());
        bad_header[4..8].copy_from_slice(&4096u32.to_be_bytes());
        assert!(take_frame(&mut bad_header).is_err());
    }

    /// header 里混入别的类型（int / bool / bytes）也要能正确跳过去
    #[test]
    fn other_header_types_are_skipped_by_their_size() {
        let mut headers = Vec::new();
        // `:message-type` 用布尔 0（= event）
        headers.push(":message-type".len() as u8);
        headers.extend_from_slice(b":message-type");
        headers.push(0);
        // 一个 int 头（4 字节值）
        headers.push(":priority".len() as u8);
        headers.extend_from_slice(b":priority");
        headers.push(4);
        headers.extend_from_slice(&7u32.to_be_bytes());
        // 一个 bytes 头（2 字节长度 + 内容）
        headers.push(":blob".len() as u8);
        headers.extend_from_slice(b":blob");
        headers.push(6);
        headers.extend_from_slice(&3u16.to_be_bytes());
        headers.extend_from_slice(&[1, 2, 3]);
        // 真正的字符串头：事件名
        headers.push(":event-type".len() as u8);
        headers.extend_from_slice(b":event-type");
        headers.push(7);
        headers.extend_from_slice(&5u16.to_be_bytes());
        headers.extend_from_slice(b"hello");

        let payload = b"{}";
        let total = 12 + headers.len() + payload.len() + 4;
        let mut frame = Vec::new();
        frame.extend_from_slice(&(total as u32).to_be_bytes());
        frame.extend_from_slice(&(headers.len() as u32).to_be_bytes());
        frame.extend_from_slice(&0u32.to_be_bytes());
        frame.extend_from_slice(&headers);
        frame.extend_from_slice(payload);
        frame.extend_from_slice(&0u32.to_be_bytes());

        let parsed = take_frame(&mut frame).expect("应能解析").expect("应有一帧");
        assert_eq!(parsed.message_type, "event");
        assert_eq!(parsed.event_type, "hello");
        assert_eq!(parsed.payload_text(), "{}");
    }

    /// 坏 header（截断）不 panic，返回已读到的部分
    #[test]
    fn truncated_headers_do_not_panic() {
        let (event, message, exception, code) = parse_headers(&[200u8, b':', b'e']);
        assert!(event.is_empty() && message.is_empty() && exception.is_empty() && code.is_empty());
        let (event, _, _, _) = parse_headers(&[]);
        assert!(event.is_empty());
        // 未知类型码 → 停止解析（不越界读）
        let (event, _, _, _) = parse_headers(&[1, b'x', 99]);
        assert!(event.is_empty());
    }
}
