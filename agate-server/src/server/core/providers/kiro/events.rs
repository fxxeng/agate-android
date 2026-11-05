//! Kiro 事件 → 中立的增量事件（正文 / 思考 / 工具 / 用量 / 停止）。
//!
//! ── 思考有两种来源，必须都认 ────────────────────────────────
//! ```text
//!   reasoningContentEvent   单独的思考事件（原生思考档位那条路）
//!   assistantResponseEvent  正文里内联的 <thinking>…</thinking>（Claude 老路）
//! ```
//! 第二种要**跨 chunk** 切：标签可能被拆在两次增量里（`<thin` + `king>`），
//! 所以未闭合的后缀必须暂存到下次 —— 这是 [`ThinkParser`] 存在的全部理由。
//! 直接按 `find("</thinking>")` 切会出现「半个标签漏进正文」「思考片段被当正文
//! 发出去」两类问题，且只在特定分块下复现，最难定位。
//!
//! ── 用量：上游通常只给百分比 ────────────────────────────────
//! Kiro 给 `contextUsagePercentage`（上下文用了百分之几），常常不给 token 数。
//! 此时按「窗口 × 百分比」估输入、按「正文字符数 / 4」估输出。两者都是**估算**，
//! 但比一律报 0 有用：报表能看出量级，且 `reported = false` 让调用方能区分
//! 「上游说 0」与「我们估的」。
//!
//! ── 停止原因：工具优先 ──────────────────────────────────────
//! 只要发过 `toolUseEvent`，停止原因就是 `tool_calls` —— 否则客户端拿到
//! `stop` 就不会去执行工具，整条链路卡在「模型说要调工具但没人调」。上游在
//! 这一点上不给我们明确的 stopReason（常常仍是 `end_turn`）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::collections::VecDeque;

use serde_json::Value;

use super::eventstream::{Reader, Source};
use super::{errors, models};

/// 一条增量事件
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// 正文增量
    Text(String),
    /// 思考增量
    Think(String),
    /// 工具调用开始（同一 id 只发一次）
    ToolStart { id: String, name: String },
    /// 工具参数增量（JSON 片段，可能被拆成多段）
    ToolArgs(String),
    /// 收尾：停止原因 + 用量
    Stop { reason: String, usage: Usage },
    /// 流内错误（`status` 为空表示上游没说清，按 502 处理）
    Error { status: Option<u16>, text: String },
}

/// 一次回复的用量（OpenAI chat 的口径）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    /// 上游真的给了 `tokenUsage`（否则 input / output 里有估算值）
    pub reported: bool,
}

/// 帧流 → 事件流
pub struct Decoder {
    reader: Reader,
    think: ThinkParser,
    pending: VecDeque<Event>,
    usage: Usage,
    /// 最近一次上报的上下文占用百分比（0 = 没报过）
    percentage: f64,
    /// 已发出的正文字符数（估算输出用）
    said: usize,
    /// 发过几次工具调用
    tools: usize,
    /// 当前正在增量的工具 id（同一工具不重复发 ToolStart）
    current_tool: Option<String>,
    stop_reason: String,
    failed: Option<String>,
    finished: bool,
    /// 上游模型 id（查上下文窗口用）
    model: String,
    /// 账号 id（窗口表按账号分）
    account: String,
    /// 正文里内联的 `<thinking>` 要不要切出来
    split_thinking: bool,
}

impl Decoder {
    /// `split_thinking` 通常传 [`models::thinks`]：只有 Claude 与 Auto 会在正文
    /// 里内联思考标签，其余模型切了也没东西可切（但切了也无害，所以不设强约束）
    pub fn new(source: Source, model: &str, account: &str, split_thinking: bool) -> Self {
        Self {
            reader: Reader::new(source),
            think: ThinkParser::default(),
            pending: VecDeque::new(),
            usage: Usage::default(),
            percentage: 0.0,
            said: 0,
            tools: 0,
            current_tool: None,
            stop_reason: String::new(),
            failed: None,
            finished: false,
            model: model.to_string(),
            account: account.to_string(),
            split_thinking,
        }
    }

    /// 读下一个事件（流结束返回 `None`）
    ///
    /// 读流失败也折成一条 [`Event::Error`]：编排层对「读坏了」和「上游报错」
    /// 的处理是同一条路径（收尾 + 记失败），分成两种返回值只会让调用点多一层
    /// 永远走不到的 `Err` 分支。
    pub async fn next(&mut self) -> Option<Event> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(event);
            }
            if self.finished {
                return None;
            }
            let frame = match self.reader.next().await {
                Ok(Some(frame)) => frame,
                Ok(None) => {
                    self.finish();
                    continue;
                }
                Err(error) => {
                    self.failed = Some(error);
                    self.finish();
                    continue;
                }
            };
            if frame.is_error() {
                let text = errors::exception_text(&frame.error_kind(), &frame.payload_text());
                crate::server::logging::verbose("[Kiro]", &format!("事件流错误：{text}"));
                self.failed = Some(text);
                self.finish();
                continue;
            }
            let payload = frame.payload_text();
            let value: Value = match serde_json::from_str(&payload) {
                Ok(value) => value,
                // 不是 JSON 就跳过：AWS 会在流里夹控制帧，那不是错误
                Err(_) => continue,
            };
            match frame.event_type.as_str() {
                "assistantResponseEvent" => {
                    let text = value.get("content").and_then(Value::as_str).unwrap_or("");
                    if text.is_empty() {
                        continue;
                    }
                    self.said += text.chars().count();
                    if self.split_thinking {
                        for event in self.think.feed(text) {
                            self.pending.push_back(event);
                        }
                    } else {
                        self.pending.push_back(Event::Text(text.to_string()));
                    }
                }
                "reasoningContentEvent" => {
                    if let Some(text) = value.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            self.said += text.chars().count();
                            self.pending.push_back(Event::Think(text.to_string()));
                        }
                    }
                }
                "toolUseEvent" => self.tool_event(&value),
                "metadataEvent" | "messageMetadataEvent" => {
                    if let Some(reason) = value.get("stopReason").and_then(Value::as_str) {
                        self.stop_reason = reason.to_string();
                    }
                    self.read_usage(&value);
                }
                "contextUsageEvent" => {
                    let percentage = number_f(value.get("contextUsagePercentage"));
                    if percentage > 0.0 {
                        self.percentage = percentage;
                    }
                }
                // 其余事件（`messageMetadataEvent` 之外的元信息、心跳）忽略
                _ => {}
            }
        }
    }

    /// 元信息帧里的用量
    fn read_usage(&mut self, value: &Value) {
        let Some(usage) = value.get("tokenUsage").filter(|value| value.is_object()) else {
            return;
        };
        self.usage.reported = true;
        // `uncachedInputTokens` 已排除缓存命中，只有它缺失时才用 `inputTokens`
        // （部分实现把总数放在后者）—— 两者相加会把同一批 token 算两遍
        let uncached = number(usage.get("uncachedInputTokens"));
        self.usage.input += if uncached > 0 {
            uncached
        } else {
            number(usage.get("inputTokens"))
        };
        self.usage.output += number(usage.get("outputTokens"));
        self.usage.cache_read += number(usage.get("cacheReadInputTokens"));
        self.usage.cache_write += number(usage.get("cacheWriteInputTokens"));
        let percentage = number_f(value.get("contextUsagePercentage"));
        if percentage > 0.0 {
            self.percentage = percentage;
        }
    }

    /// 工具调用（同一工具的多段增量共用 `toolUseId`）
    fn tool_event(&mut self, value: &Value) {
        let id = value
            .get("toolUseId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !id.is_empty() && self.current_tool.as_deref() != Some(id.as_str()) {
            self.current_tool = Some(id.clone());
            self.tools += 1;
            self.pending.push_back(Event::ToolStart { id, name });
        }
        let Some(input) = value.get("input").filter(|value| !value.is_null()) else {
            return;
        };
        // 字符串就是原样的 JSON 片段；对象要序列化后发（客户端按字符串拼）
        let text = match input.as_str() {
            Some(text) => text.to_string(),
            None => serde_json::to_string(input).unwrap_or_default(),
        };
        if !text.is_empty() && text != "{}" {
            self.said += text.chars().count();
            self.pending.push_back(Event::ToolArgs(text));
        }
    }

    /// 收尾：思考残留、错误、用量估算、停止原因
    fn finish(&mut self) {
        self.finished = true;
        if self.split_thinking {
            for event in self.think.end() {
                self.pending.push_back(event);
            }
        }
        if let Some(error) = self.failed.take() {
            self.pending.push_back(Event::Error {
                status: errors::stream_error_status(&error),
                text: error,
            });
            return;
        }
        // 上游没给 token 数时按百分比与字数估：只在 **0** 的时候估，
        // 已经报上来的值一律不动（估算是兜底，不是校正）
        if self.usage.input == 0 && self.percentage > 0.0 {
            let window = models::window_of(&self.account, &self.model);
            if window > 0 {
                self.usage.input = ((self.percentage / 100.0) * window as f64) as i64;
            }
        }
        if self.usage.output == 0 {
            self.usage.output = ((self.said + 3) / 4) as i64;
        }
        let full = self
            .stop_reason
            .eq_ignore_ascii_case("model_context_window_exceeded");
        // 一个字都没说就被判「窗口满了」：实情是**请求**太长，不是回复太长。
        // 归到「输入过长」那一档（400、不换号），否则编排层会当成模型故障换号，
        // 换谁发同一份超长请求都会被拒
        if full && self.said == 0 && self.tools == 0 {
            self.pending.push_back(Event::Error {
                status: Some(400),
                text: format!(
                    "{}{}",
                    errors::TOO_LONG,
                    "the reply stopped as the model's context window was full \
                     (MODEL_CONTEXT_WINDOW_EXCEEDED)"
                ),
            });
            return;
        }
        let reason = self.stop_reason_now(full);
        self.pending.push_back(Event::Stop {
            reason,
            usage: self.usage,
        });
    }

    /// OpenAI 的三种停止原因（工具优先，见模块头）
    fn stop_reason_now(&self, full: bool) -> String {
        if self.tools > 0 {
            "tool_calls".to_string()
        } else if full || self.stop_reason.eq_ignore_ascii_case("max_tokens") {
            "length".to_string()
        } else if self.stop_reason.eq_ignore_ascii_case("content_filtered") {
            "content_filter".to_string()
        } else {
            "stop".to_string()
        }
    }
}

fn number(value: Option<&Value>) -> i64 {
    value.and_then(Value::as_i64).unwrap_or(0)
}

fn number_f(value: Option<&Value>) -> f64 {
    value.and_then(Value::as_f64).unwrap_or(0.0)
}

/// `<thinking>` 标签切分器（跨 chunk 安全）。
///
/// 三个状态：`0` 还在判开头、`1` 在思考体内、`2` 正文。难点全在状态 0 与 1
/// 的「末尾可能是标签前缀」——比如已经收到 `abc<thin`，不能把 `<thin` 当正文
/// 发出去，必须留着等下一块。那种情况返回、把 buffer 原样保留。
#[derive(Default)]
pub struct ThinkParser {
    state: u8,
    buffer: String,
    /// 已经吐过思考内容（决定要不要吃掉紧随其后的空行）
    said: bool,
}

const OPEN: &str = "<thinking>";
const CLOSE: &str = "</thinking>";

impl ThinkParser {
    fn feed(&mut self, text: &str) -> Vec<Event> {
        self.buffer.push_str(text);
        let mut out = Vec::new();
        loop {
            match self.state {
                // 判开头：允许前导空白，允许「目前只有标签的一段前缀」
                0 => {
                    let trimmed = self.buffer.trim_start_matches([' ', '\t', '\r', '\n']);
                    if trimmed.starts_with(OPEN) {
                        self.buffer = trimmed[OPEN.len()..].to_string();
                        self.state = 1;
                        continue;
                    }
                    if trimmed.len() < OPEN.len() && OPEN.starts_with(trimmed) {
                        return out;
                    }
                    self.state = 2;
                }
                // 思考体内：找闭合标签；找不到就把「可能是闭合标签前缀」的尾巴留住
                1 => {
                    if !self.said {
                        self.buffer =
                            self.buffer.trim_start_matches(['\r', '\n']).to_string();
                        if self.buffer.is_empty() {
                            return out;
                        }
                    }
                    if let Some(index) = self.buffer.find(CLOSE) {
                        if index > 0 {
                            out.push(Event::Think(self.buffer[..index].to_string()));
                            self.said = true;
                        }
                        self.buffer = self.buffer[index + CLOSE.len()..]
                            .trim_start_matches(['\r', '\n'])
                            .to_string();
                        self.state = 2;
                        continue;
                    }
                    let hold = partial_suffix(&self.buffer, CLOSE);
                    let keep = self.buffer.len().saturating_sub(hold);
                    if keep > 0 {
                        out.push(Event::Think(self.buffer[..keep].to_string()));
                        self.buffer = self.buffer[keep..].to_string();
                        self.said = true;
                    }
                    return out;
                }
                // 正文：有多少发多少
                _ => {
                    if !self.buffer.is_empty() {
                        out.push(Event::Text(std::mem::take(&mut self.buffer)));
                    }
                    return out;
                }
            }
        }
    }

    /// 流结束：把残留按当前状态定性吐出（在思考体内就算思考）
    pub fn end(&mut self) -> Vec<Event> {
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let event = if self.state == 1 {
            Event::Think(std::mem::take(&mut self.buffer))
        } else {
            Event::Text(std::mem::take(&mut self.buffer))
        };
        self.state = 2;
        vec![event]
    }
}

/// 结尾有多长的「可能是标签开头」的后缀
fn partial_suffix(text: &str, tag: &str) -> usize {
    let limit = (tag.len() - 1).min(text.len());
    for length in (1..=limit).rev() {
        if text.ends_with(&tag[..length]) {
            return length;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(events: &[Event]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn thinks(events: &[Event]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Think(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// 正文里内联的思考要切出来，正文只留正文
    #[test]
    fn inline_thinking_is_split_from_the_body() {
        let mut parser = ThinkParser::default();
        let events = parser.feed("<thinking>先算一下</thinking>答案是 42");
        assert_eq!(thinks(&events), "先算一下");
        assert_eq!(texts(&events), "答案是 42");
    }

    /// 标签被拆在两块里也要切对（这是整套逻辑存在的理由）
    #[test]
    fn a_tag_split_across_chunks_is_still_recognised() {
        let mut parser = ThinkParser::default();
        let first = parser.feed("<thin");
        assert!(first.is_empty(), "半截标签不能当正文发出去");
        let second = parser.feed("king>想一想</thinking>答案");
        assert_eq!(thinks(&second), "想一想");
        assert_eq!(texts(&second), "答案");
    }

    /// 闭合标签被拆开时，尾巴要留住而不是先发出去
    #[test]
    fn a_half_closed_tag_holds_the_tail() {
        let mut parser = ThinkParser::default();
        let first = parser.feed("<thinking>思考中</thin");
        assert_eq!(thinks(&first), "思考中");
        assert_eq!(texts(&first), "", "`</thin` 不能当正文发出去");
        let second = parser.feed("king>正文");
        assert_eq!(texts(&second), "正文");
    }

    /// 没有思考标签的纯正文一块不落地
    #[test]
    fn plain_text_passes_through() {
        let mut parser = ThinkParser::default();
        assert_eq!(texts(&parser.feed("hello")), "hello");
        assert_eq!(texts(&parser.feed(" world")), " world");
    }

    /// 流结束时没闭合的思考算思考（不能漏进正文）
    #[test]
    fn an_unclosed_thinking_block_becomes_thinking_at_the_end() {
        let mut parser = ThinkParser::default();
        // 能确定是思考的那部分在 feed 阶段就吐了，收尾阶段只处理残留
        let mut events = parser.feed("<thinking>没写完");
        events.extend(parser.end());
        assert_eq!(thinks(&events), "没写完");
        assert_eq!(texts(&events), "");
    }

    /// 只有标签前缀、流就结束：残留按正文吐（它不是思考）
    #[test]
    fn a_bare_prefix_is_text_at_the_end() {
        let mut parser = ThinkParser::default();
        assert!(parser.feed("<thin").is_empty());
        assert_eq!(texts(&parser.end()), "<thin");
    }

    /// 停止原因：发过工具就是 tool_calls（否则客户端不会去执行工具）
    #[test]
    fn tool_calls_win_over_the_reported_stop_reason() {
        let decoder = Decoder {
            tools: 1,
            stop_reason: "end_turn".to_string(),
            ..blank()
        };
        assert_eq!(decoder.stop_reason_now(false), "tool_calls");
    }

    /// 三种停止原因的映射
    #[test]
    fn stop_reasons_map_to_the_openai_vocabulary() {
        let mut decoder = blank();
        assert_eq!(decoder.stop_reason_now(false), "stop");
        decoder.stop_reason = "max_tokens".to_string();
        assert_eq!(decoder.stop_reason_now(false), "length");
        decoder.stop_reason = "content_filtered".to_string();
        assert_eq!(decoder.stop_reason_now(false), "content_filter");
        assert_eq!(decoder.stop_reason_now(true), "length");
        // 大小写不敏感（上游写法不统一）
        decoder.stop_reason = "MAX_TOKENS".to_string();
        assert_eq!(decoder.stop_reason_now(false), "length");
    }

    /// 窗口满 + 一个字没说 = 输入过长（400、不换号），不是模型故障
    #[test]
    fn an_empty_context_overflow_is_reported_as_input_too_long() {
        let mut decoder = blank();
        decoder.stop_reason = "model_context_window_exceeded".to_string();
        decoder.finish();
        match decoder.pending.pop_front() {
            Some(Event::Error { status, text }) => {
                assert_eq!(status, Some(400));
                assert!(text.starts_with(errors::TOO_LONG), "{text}");
            }
            other => panic!("应是输入过长错误，实际 {other:?}"),
        }
    }

    /// 说了话之后的窗口满 = 回复被截断（正常收尾，`length`）
    #[test]
    fn a_truncated_reply_is_a_normal_stop() {
        let mut decoder = blank();
        decoder.said = 20;
        decoder.stop_reason = "model_context_window_exceeded".to_string();
        decoder.finish();
        match decoder.pending.pop_front() {
            Some(Event::Stop { reason, .. }) => assert_eq!(reason, "length"),
            other => panic!("应是正常收尾，实际 {other:?}"),
        }
    }

    /// 没给 token 数时按百分比估输入、按字数估输出，`reported` 仍为 false
    #[test]
    fn missing_token_counts_are_estimated() {
        let mut decoder = blank();
        decoder.said = 400;
        decoder.finish();
        match decoder.pending.pop_front() {
            Some(Event::Stop { usage, .. }) => {
                assert_eq!(usage.input, 0, "没有窗口表时不硬编");
                assert_eq!(usage.output, 100, "400 字 / 4");
                assert!(!usage.reported);
            }
            other => panic!("应是收尾，实际 {other:?}"),
        }
    }

    fn blank() -> Decoder {
        Decoder {
            reader: Reader::new(Box::pin(futures::stream::empty())),
            think: ThinkParser::default(),
            pending: VecDeque::new(),
            usage: Usage::default(),
            percentage: 0.0,
            said: 0,
            tools: 0,
            current_tool: None,
            stop_reason: String::new(),
            failed: None,
            finished: false,
            model: "auto".to_string(),
            account: "kiro:test".to_string(),
            split_thinking: false,
        }
    }
}
