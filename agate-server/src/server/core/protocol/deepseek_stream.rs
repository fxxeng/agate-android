//! 上游 DeepSeek 网页版 SSE（**path-value 结构**）→ 标准 chat SSE（状态机）。
//!
//! ── 为什么需要新的一台状态机 ────────────────────────────────────
//! 本家的流是 SSE（`data: {json}` 逐帧，`event: ready` / `event: close` 混在
//! 里面），但帧不是 OpenAI 那种「这一拍的增量 delta」，而是一套
//! **JSON-Pointer 风格的 patch 指令**：
//! ```text
//!   上游：{"p":"response/fragments","o":"APPEND","v":[{"type":"RESPONSE","content":""}]}
//!         {"v":"数字"}                       ← 裸增量（没有 p，正文主体全是这种）
//!         {"p":"response/status","o":"SET","v":"FINISHED"}
//!   OpenAI：{choices:[{delta:{content:"数字"},index:0}]}
//! ```
//!
//! ── 帧语义（二进制字符串簇 + 公开逆向交叉印证）─────────────────
//! 字段只有三个字母：`p`（path）/ `o`（operation）/ `v`（value），外加正文主体
//! **不带 `p`** 的裸帧。三种操作：
//! ```text
//!   APPEND  v 是数组，往 p 指向的片段列表**追加**片段（v[0].type 决定流向）
//!   SET     v 是标量，把 p 指向的位置**置为**该值
//!   BATCH   一次给多条（本实现按数组逐条展开）
//! ```
//! 片段切换靠 `response/fragments` + `o=APPEND`，`v[0].type` 只有两个值：
//! ```text
//!   THINK    → 后续裸增量归到「思考链」→ reasoning_content
//!   RESPONSE → 后续裸增量归到「正文」  → content
//! ```
//! 结束标志是 `response/status` == `FINISHED`（不是 `[DONE]`）。
//!
//! ── 为什么「裸增量」要按当前片段类型分流（最容易错的一处）──────
//! 正文与思考**都是不带 `p` 的裸增量**（`{"v":"几个字"}`），全靠前面那条
//! `APPEND response/fragments` 声明的 `v[0].type` 决定它算哪条流。因此必须
//! **记住当前片段类型**，来一个裸增量就往对应的基准里追加。若把裸增量一律
//! 当正文，思考链会混进回答里（用户看到「嗯，让我想想…」直接出现在答案中）。
//!
//! ── 与 ChatGLM 那台的关键差别（别看错模板）────────────────────
//! ChatGLM 的帧会「增量与全文快照混发」，那台需要**差分**（见
//! `chatglm_stream` 的模块头）。本家不同：**裸增量就是净增量**，从没有
//! 「全文快照」帧 —— 因此这里**不做任何差分**，来一段追加一段。
//! 把 ChatGLM 那套差分逻辑照搬过来会把每个字都判成「与已发内容对不上」而丢弃。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use super::{chat_frame, random_id, string_field, SseLineBuffer};

/// 片段类型（`response/fragments` 的 `v[0].type`）
const FRAGMENT_THINK: &str = "THINK";
const FRAGMENT_RESPONSE: &str = "RESPONSE";

/// 当前裸增量归哪条流
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fragment {
    /// 还没见过片段声明（此时来的裸增量按正文处理 —— 见 `default_fragment`）
    Unknown,
    Think,
    Response,
}

/// 上游 SSE 字节流 → 标准 chat SSE 增量帧。
pub struct ChatFromDeepSeekStream {
    buffer: SseLineBuffer,
    /// 输出帧的 model（发给上游的真名；ForwardStream 的回写层按需改写）
    model: String,
    id: String,
    created: i64,
    /// 已下发过首帧（role assistant）
    started: bool,
    finished: bool,
    /// 当前片段类型（见模块头：裸增量按它分流）
    fragment: Fragment,
    /// 上游最后一次给出的 message id（收尾帧里回填，便于排查多轮串接）
    message_id: Option<Value>,
    /// usage（上游**不给** token 计数，恒为空 —— 见 `usage_payload`）
    usage: Option<Value>,
}

impl ChatFromDeepSeekStream {
    pub fn new(model: &str) -> Self {
        Self {
            buffer: SseLineBuffer::new(),
            model: model.to_string(),
            id: String::new(),
            created: crate::server::logging::now_ms() / 1000,
            started: false,
            finished: false,
            fragment: Fragment::Unknown,
            message_id: None,
            usage: None,
        }
    }

    /// 吃一段上游字节，吐出要下发的 chat SSE 帧
    pub fn push(&mut self, chunk: &[u8]) -> Vec<bytes::Bytes> {
        let mut out = Vec::new();
        for payload in self.buffer.push(chunk) {
            // `None` = 上游发来了 `[DONE]`（本家实测不发，但收下不亏）
            let Some(line) = payload else {
                out.extend(self.finish());
                continue;
            };
            match serde_json::from_str::<Value>(&line) {
                Ok(value) => out.extend(self.consume(&value)),
                Err(error) => {
                    crate::server::logging::verbose(
                        "[DeepSeek]",
                        &format!("上游 SSE 载荷不是合法 JSON（已忽略）：{error}"),
                    );
                }
            }
            if self.finished {
                break;
            }
        }
        out
    }

    /// 上游流结束：按「有没有见过终态」收尾
    pub fn finish(&mut self) -> Vec<bytes::Bytes> {
        if self.finished {
            return Vec::new();
        }
        // 先冲刷残留半行（上游可能最后一帧不带换行）
        let mut out = Vec::new();
        for payload in self.buffer.finish() {
            if let Some(line) = payload {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    out.extend(self.consume(&value));
                }
            }
            if self.finished {
                return out;
            }
        }
        if self.finished {
            return out;
        }
        // 没有 `FINISHED` 就 EOF：上游契约里是「没走完」。如实报截断，
        // **不补** finish_reason（补了等于把截断谎报成完整回答）。
        out.extend(self.fail_message("上游流在收到完成信号之前结束（响应被截断）", 502));
        out
    }

    /// 一帧 → 零到多个 chat 帧
    fn consume(&mut self, event: &Value) -> Vec<bytes::Bytes> {
        if self.finished {
            return Vec::new();
        }
        let Some(object) = event.as_object() else {
            return Vec::new();
        };

        // 业务错误：本家把失败也放在 200 的 SSE 里。两种形态都认：
        //   {"error":{"message":"…"}} 与 {"error":"…"}
        if let Some(error) = object.get("error").filter(|value| !value.is_null()) {
            let message = string_field(error, "message");
            let message = if message.trim().is_empty() {
                let flat = string_field(event, "error");
                if flat.trim().is_empty() {
                    "上游返回错误".to_string()
                } else {
                    flat
                }
            } else {
                message
            };
            return self.fail_message(&message, 502);
        }
        // 业务码非零（部分路径用它表达失败）
        if let Some(code) = object.get("biz_code").and_then(Value::as_i64) {
            if code != 0 {
                let message = string_field(event, "biz_msg");
                let message = if message.trim().is_empty() {
                    format!("上游返回业务码 {code}")
                } else {
                    message
                };
                return self.fail_message(&message, 502);
            }
        }

        // 操作形态：`{"p":…,"o":…,"v":…}`；`o` 与 `p` 都可能缺省
        let path = string_field(event, "p");
        let operation = string_field(event, "o").to_uppercase();
        let value = object.get("v").cloned().unwrap_or(Value::Null);

        // 先记 message id（收尾与多轮串接都要）
        self.absorb_message_id(event);

        match (path.as_str(), operation.as_str()) {
            // ── 片段声明：切换后续裸增量的归属 ──
            ("response/fragments", "APPEND") => {
                if let Some(kind) = fragment_type_of(&value) {
                    self.fragment = kind;
                }
                Vec::new()
            }
            // ── 结束标志（不是 OpenAI 的 [DONE]）──
            ("response/status", "SET") if string_field(event, "v") == "FINISHED" => self.complete(),
            ("response/status", "SET") => Vec::new(),
            // ── 思考链正文（带 p 的形态；有些帧会给 `response/thinking_content`）──
            ("response/thinking_content", "APPEND") | ("response/thinking_content", "SET") => {
                let text = value_to_text(&value);
                self.emit_delta("", &text)
            }
            // ── 正文（带 p 的形态；`response/content` 是整块追加）──
            ("response/content", "APPEND") | ("response/content", "SET") => {
                let text = value_to_text(&value);
                self.emit_delta(&text, "")
            }
            // ── 裸增量（不带 `p`）：本家正文与思考的**主体形态** ──
            ("", "") | ("", _) => {
                let text = value_to_text(&value);
                if text.is_empty() {
                    return Vec::new();
                }
                self.emit_delta_by_fragment(&text)
            }
            // ── 其余路径（roles / fragments 的其它操作 / 元信息）→ 忽略 ──
            _ => Vec::new(),
        }
    }

    /// 记 message id（多轮串接要靠它，字段名见二进制：`response_message_id`）
    fn absorb_message_id(&mut self, event: &Value) {
        for key in ["response_message_id", "message_id"] {
            if let Some(value) = event.get(key).filter(|value| !value.is_null()) {
                self.message_id = Some(value.clone());
                return;
            }
        }
    }

    /// 裸增量按当前片段类型分流（见模块头：这一处错了思考会混进正文）
    fn emit_delta_by_fragment(&mut self, text: &str) -> Vec<bytes::Bytes> {
        match self.fragment {
            Fragment::Think => self.emit_delta("", text),
            // 还没见过片段声明（`Unknown`）时按**正文**处理：本家的第一段
            // 回答之前一般会有片段声明，但若上游省了这一步，正文优先更安全
            // （把正文当思考会让回答整个消失；把思考当正文只是多一段文字）
            Fragment::Response | Fragment::Unknown => self.emit_delta(text, ""),
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
        // 按空回答计费（判据与 Command Code / ChatGLM 两台同款）
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
    fn fail_message(&mut self, message: &str, status: i64) -> Vec<bytes::Bytes> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let kind = if status == 429 {
            "rate_limit_error"
        } else {
            "upstream_error"
        };
        let error = json!({
            "message": message,
            "type": kind,
            "status": status,
        });
        let mut frame = json!({ "error": error });
        if status == 429 {
            if let Some(object) = frame.as_object_mut() {
                object.insert("retry_after".to_string(), Value::from(30));
            }
        }
        // 错误路径**不**补 usage 帧（带 usage 且 choices 为空的帧是 OpenAI 的
        // 收尾形态，部分客户端据此判定「流已结束」—— 在它之后补错误帧等于
        // 让那些客户端看不到失败；与另外两台逐字同款）
        let mut out = vec![chat_frame(&frame)];
        out.push(bytes::Bytes::from_static(b"data: [DONE]\n\n"));
        out
    }

    /// 上游（实测）不给 token 计数 —— 如实回 0，不编造数字
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

/// 从 `response/fragments` 的值里读出片段类型（`v[0].type`，兼容 `v` 直接是对象）
fn fragment_type_of(value: &Value) -> Option<Fragment> {
    let item = match value {
        Value::Array(list) => list.first(),
        other => Some(other),
    }?;
    match string_field(item, "type").to_uppercase().as_str() {
        FRAGMENT_THINK => Some(Fragment::Think),
        FRAGMENT_RESPONSE => Some(Fragment::Response),
        _ => None,
    }
}

/// 把一个 `v` 值折成文本：字符串直接用；数组按元素拼接（片段声明的数组无正文，
/// 会拼成空串）；对象取 `content` / `text`；其余给空串。
fn value_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(list) => {
            let mut out = String::new();
            for item in list {
                out.push_str(&value_to_text(item));
            }
            out
        }
        Value::Object(_) => {
            let content = string_field(value, "content");
            if !content.is_empty() {
                return content;
            }
            string_field(value, "text")
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把下行帧折成 (正文, 思考, finish_reason)
    fn collect(frames: &[bytes::Bytes]) -> (String, String, Option<String>) {
        let mut text = String::new();
        let mut thinking = String::new();
        let mut finish = None;
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
        (text, thinking, finish)
    }

    fn sse(payload: &str) -> Vec<u8> {
        format!("data: {payload}\n\n").into_bytes()
    }

    /// 完整链路：片段声明 → 思考链 → 切换正文 → FINISHED
    /// （帧形态照抄公开逆向抓到的真实流，字段名与二进制一致）
    #[test]
    fn full_flow_splits_thinking_and_answer() {
        let mut machine = ChatFromDeepSeekStream::new("deepseek-web-flash");
        let mut frames = Vec::new();
        // 片段声明：先 THINK
        frames.extend(machine.push(&sse(
            r#"{"p":"response/fragments","o":"APPEND","v":[{"type":"THINK","content":""}]}"#,
        )));
        // 思考链：裸增量
        frames.extend(machine.push(&sse(r#"{"v":"让我想想"}"#)));
        frames.extend(machine.push(&sse(r#"{"v":"…"}"#)));
        // 片段声明：切到 RESPONSE
        frames.extend(machine.push(&sse(
            r#"{"p":"response/fragments","o":"APPEND","v":[{"type":"RESPONSE","content":""}]}"#,
        )));
        // 正文：裸增量
        frames.extend(machine.push(&sse(r#"{"v":"答案是"}"#)));
        frames.extend(machine.push(&sse(r#"{"v":"42"}"#)));
        // 结束
        frames.extend(machine.push(&sse(r#"{"p":"response/status","o":"SET","v":"FINISHED"}"#)));

        let (text, thinking, finish) = collect(&frames);
        assert_eq!(text, "答案是42", "正文只应含正文");
        assert_eq!(thinking, "让我想想…", "思考链不混进正文");
        assert_eq!(finish.as_deref(), Some("stop"));
    }

    /// **关键回归**：没有片段声明时裸增量按正文处理
    /// （否则回答会整个消失 —— 把正文当思考是最坏的一种错）
    #[test]
    fn bare_delta_without_fragment_declaration_is_content() {
        let mut machine = ChatFromDeepSeekStream::new("m");
        let frames = machine.push(&sse(r#"{"v":"没声明片段"}"#));
        let (text, thinking, _) = collect(&frames);
        assert_eq!(text, "没声明片段");
        assert_eq!(thinking, "");
    }

    /// 思考片段期间的裸增量必须进 reasoning_content
    /// （若一律当正文，用户会在答案里看到「嗯，让我想想…」）
    #[test]
    fn bare_delta_during_think_goes_to_reasoning() {
        let mut machine = ChatFromDeepSeekStream::new("m");
        machine.push(&sse(
            r#"{"p":"response/fragments","o":"APPEND","v":[{"type":"THINK","content":""}]}"#,
        ));
        let frames = machine.push(&sse(r#"{"v":"思考内容"}"#));
        let (text, thinking, _) = collect(&frames);
        assert_eq!(text, "");
        assert_eq!(thinking, "思考内容");
    }

    /// 带 `p` 的正文形态（`response/content`）也认
    #[test]
    fn explicit_content_path_is_supported() {
        let mut machine = ChatFromDeepSeekStream::new("m");
        let frames = machine.push(&sse(
            r#"{"p":"response/content","o":"APPEND","v":"整块正文"}"#,
        ));
        let (text, _, _) = collect(&frames);
        assert_eq!(text, "整块正文");
    }

    /// 带 `p` 的思考形态（`response/thinking_content`）也认
    #[test]
    fn explicit_thinking_path_is_supported() {
        let mut machine = ChatFromDeepSeekStream::new("m");
        let frames = machine.push(&sse(
            r#"{"p":"response/thinking_content","o":"APPEND","v":"整块思考"}"#,
        ));
        let (text, thinking, _) = collect(&frames);
        assert_eq!(text, "");
        assert_eq!(thinking, "整块思考");
    }

    /// `FINISHED` 才是结束；其它 status 值不下发收尾
    #[test]
    fn only_finished_terminates() {
        let mut machine = ChatFromDeepSeekStream::new("m");
        machine.push(&sse(r#"{"v":"正文"}"#));
        let frames = machine.push(&sse(r#"{"p":"response/status","o":"SET","v":"RUNNING"}"#));
        let (_, _, finish) = collect(&frames);
        assert_eq!(finish, None, "RUNNING 不是终态");
        // 收尾
        let frames = machine.push(&sse(r#"{"p":"response/status","o":"SET","v":"FINISHED"}"#));
        let (_, _, finish) = collect(&frames);
        assert_eq!(finish.as_deref(), Some("stop"));
    }

    /// `event: ready` / `event: close` 这类非 data 行被 SseLineBuffer 跳过
    #[test]
    fn event_lines_are_skipped() {
        let mut machine = ChatFromDeepSeekStream::new("m");
        let frames = machine.push("event: ready\ndata: {\"v\":\"正文\"}\n\nevent: close\n\n".as_bytes());
        let (text, _, _) = collect(&frames);
        assert_eq!(text, "正文");
    }

    /// 流内错误（对象形态）→ 错误帧
    #[test]
    fn error_object_becomes_error_frame() {
        let mut machine = ChatFromDeepSeekStream::new("m");
        let frames = machine.push(&sse(r#"{"error":{"message":"额度不足"}}"#));
        let raw = String::from_utf8_lossy(&frames[0]).to_string();
        assert!(raw.contains("额度不足"), "实际: {raw}");
        assert!(raw.contains("upstream_error"));
    }

    /// 流内错误（字符串形态）也认
    #[test]
    fn error_string_is_supported() {
        let mut machine = ChatFromDeepSeekStream::new("m");
        let frames = machine.push(&sse(r#"{"error":"登录已过期"}"#));
        let raw = String::from_utf8_lossy(&frames[0]).to_string();
        assert!(raw.contains("登录已过期"), "实际: {raw}");
    }

    /// 业务码非零 → 错误帧
    #[test]
    fn nonzero_biz_code_becomes_error_frame() {
        let mut machine = ChatFromDeepSeekStream::new("m");
        let frames = machine.push(&sse(r#"{"biz_code":40003,"biz_msg":"权限不足"}"#));
        let raw = String::from_utf8_lossy(&frames[0]).to_string();
        assert!(raw.contains("权限不足"), "实际: {raw}");
    }

    /// `biz_code = 0` 是正常的（不能当成错误）
    #[test]
    fn zero_biz_code_is_not_an_error() {
        let mut machine = ChatFromDeepSeekStream::new("m");
        let frames = machine.push(&sse(r#"{"biz_code":0,"v":"正常内容"}"#));
        let (text, _, _) = collect(&frames);
        assert_eq!(text, "正常内容");
    }

    /// 截断流（没有 FINISHED）→ 报错，不谎报成功
    #[test]
    fn truncated_stream_reports_error() {
        let mut machine = ChatFromDeepSeekStream::new("m");
        machine.push(&sse(r#"{"v":"半截"}"#));
        let frames = machine.finish();
        let raw = String::from_utf8_lossy(&frames[0]).to_string();
        assert!(raw.contains("截断"), "实际: {raw}");
    }

    /// 一个内容都没有就 FINISHED → 空响应报错
    #[test]
    fn empty_stream_reports_error() {
        let mut machine = ChatFromDeepSeekStream::new("m");
        let frames = machine.push(&sse(r#"{"p":"response/status","o":"SET","v":"FINISHED"}"#));
        let raw = String::from_utf8_lossy(&frames[0]).to_string();
        assert!(raw.contains("空响应"), "实际: {raw}");
    }

    /// 收尾必须带 [DONE]
    #[test]
    fn done_marker_is_appended() {
        let mut machine = ChatFromDeepSeekStream::new("m");
        machine.push(&sse(r#"{"v":"x"}"#));
        let frames = machine.push(&sse(r#"{"p":"response/status","o":"SET","v":"FINISHED"}"#));
        let joined = frames
            .iter()
            .map(|frame| String::from_utf8_lossy(frame).to_string())
            .collect::<String>();
        assert!(joined.contains("data: [DONE]"));
    }

    /// 片段声明帧本身不产出内容（`v[0].type` 声明不能当正文发出去）
    #[test]
    fn fragment_declaration_emits_no_content() {
        let mut machine = ChatFromDeepSeekStream::new("m");
        let frames = machine.push(&sse(
            r#"{"p":"response/fragments","o":"APPEND","v":[{"type":"RESPONSE","content":""}]}"#,
        ));
        assert!(frames.is_empty(), "片段声明不应产出帧");
    }

    /// 无关路径（roles / 元信息）被忽略
    #[test]
    fn unrelated_paths_are_ignored() {
        let mut machine = ChatFromDeepSeekStream::new("m");
        let frames = machine.push(&sse(r#"{"p":"response/roles","o":"SET","v":"assistant"}"#));
        assert!(frames.is_empty());
    }

    /// `v` 是数组且元素无正文时拼成空串（不误发内容）
    #[test]
    fn empty_fragment_array_yields_no_text() {
        let mut machine = ChatFromDeepSeekStream::new("m");
        let frames = machine.push(&sse(
            r#"{"p":"response/fragments","o":"APPEND","v":[{"type":"RESPONSE","content":""}]}"#,
        ));
        assert!(frames.is_empty());
    }

    /// value_to_text 的形态覆盖
    #[test]
    fn value_to_text_variants() {
        assert_eq!(value_to_text(&json!("abc")), "abc");
        assert_eq!(value_to_text(&json!(["a", "b"])), "ab");
        assert_eq!(value_to_text(&json!({"content": "x"})), "x");
        assert_eq!(value_to_text(&json!({"text": "y"})), "y");
        assert_eq!(value_to_text(&json!(42)), "");
        assert_eq!(value_to_text(&Value::Null), "");
    }
}