//! OpenAI Chat → OpenAI Responses：请求体构造 + 流事件解读。
//!
//! ── 为什么入站 Chat、出站 Responses ────────────────────────
//! AStudio 桌面端写进它自己内核配置的是 `wire_api = responses`，直连的就是
//! `/model-gateway/v1/responses`。网关对外只讲 Chat，所以这一层做两件事：
//!   1. 把 `messages` 折成 Responses 的 `input` 项（含 `function_call` /
//!      `function_call_output`）；
//!   2. 把 Responses 的 SSE **事件**解读成一串 `Step`（正文 / 思考 / 工具 /
//!      收尾 / 失败），由 `chat` 负责写成 chat 帧。
//!
//! ── 为什么事件解读要有状态（`StreamState`）───────────────
//! 上游同一份内容会给**两次**：先一串 `*.delta`，再来一个 `*.done` 带整段。
//! 无脑两处都发，客户端会看到每句话重复一遍；只认 `done` 又会丢掉流式效果。
//! 所以判据必须是「这条流上有没有收到过增量」—— 这就是 `StreamState` 存在的
//! 唯一理由。把它做成值而不是散在转发函数里的几个布尔，是为了能离线喂合成
//! 事件把这些组合全测一遍（`chat` 那边就只剩读写帧）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

/// 非流式回退时给「工具没有输出」占位的文案（上游同一处也这么写）
pub const NO_OUTPUT: &str = "(no output)";

/// Responses 的用量 → chat 的三个数字
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: i64,
    pub output: i64,
    /// 命中缓存的输入（`input_tokens_details.cached_tokens`）
    pub cached: i64,
}

impl Usage {
    /// chat 的 `usage` 对象（`prompt_tokens` 含缓存命中部分，与 OpenAI 一致）
    pub fn to_json(self) -> Value {
        json!({
            "prompt_tokens": self.input.max(0),
            "completion_tokens": self.output.max(0),
            "total_tokens": (self.input + self.output).max(0),
            "prompt_tokens_details": { "cached_tokens": self.cached.max(0) },
        })
    }
}

/// 一条流上的一次「有意义的动作」
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// 元事件 / 重复内容 / 认不出的类型：什么都不用发
    Skip,
    /// 正文增量
    Text(String),
    /// 思考增量（chat 的 `reasoning_content`）
    Think(String),
    /// 一次工具调用的开始（`index` 是本条流内的序号，从 0 起）
    ToolStart { index: i64, id: String, name: String },
    /// 工具参数增量
    ToolArgs { index: i64, delta: String },
    /// 收尾：`reason` 是 chat 的 `finish_reason`
    Done { reason: &'static str, usage: Usage },
    /// 失败：`status` 已按文案分好档（见 `status_for`）
    Failed { status: u16, message: String },
}

/// 一条 Responses 流上的解读状态（见模块头）
#[derive(Clone, Debug, Default)]
pub struct StreamState {
    /// 收到过正文增量（`*.done` 带整段时据此去重）
    text_seen: bool,
    /// 收到过工具参数增量
    args_seen: bool,
    /// 当前工具调用的序号（`-1` = 还没有）
    tool_index: i64,
}

impl StreamState {
    pub fn new() -> Self {
        Self {
            text_seen: false,
            args_seen: false,
            tool_index: -1,
        }
    }

    /// 解读一个事件。
    ///
    /// 调用方每拿到一个事件就调一次；返回 `Done` / `Failed` 即表示这条流结束
    /// （不应再解读后续事件）。
    pub fn step(&mut self, event: &Value) -> Step {
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            // ── 元事件：只是「开始建响应了」，没有内容
            "response.created" | "response.in_progress" => Step::Skip,

            // ── 正文
            "response.output_text.delta" | "response.text.delta" => {
                let text = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if text.is_empty() {
                    return Step::Skip;
                }
                self.text_seen = true;
                Step::Text(text.to_string())
            }
            "response.output_text.done" | "response.text.done" => {
                // 增量已经发过就**不要再发整段**（否则每句话重复一遍）
                if self.text_seen {
                    return Step::Skip;
                }
                let text = event.get("text").and_then(Value::as_str).unwrap_or("");
                if text.is_empty() {
                    return Step::Skip;
                }
                self.text_seen = true;
                Step::Text(text.to_string())
            }

            // ── 思考（三种历史命名都出现过，见 upstream 的 match 臂）
            "response.reasoning_summary_text.delta"
            | "response.reasoning_text.delta"
            | "response.reasoning.delta" => {
                let text = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if text.is_empty() {
                    return Step::Skip;
                }
                Step::Think(text.to_string())
            }

            // ── 工具调用：开始
            "response.output_item.added" => {
                let item = event.get("item").cloned().unwrap_or(Value::Null);
                if item.get("type").and_then(Value::as_str) != Some("function_call") {
                    // message / reasoning 这些 item 的开始事件不带内容
                    return Step::Skip;
                }
                let id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("id").and_then(Value::as_str))
                    .unwrap_or("")
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                self.tool_index += 1;
                self.args_seen = false;
                Step::ToolStart {
                    index: self.tool_index,
                    id,
                    name,
                }
            }
            "response.function_call_arguments.delta" => {
                let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if delta.is_empty() || self.tool_index < 0 {
                    return Step::Skip;
                }
                self.args_seen = true;
                Step::ToolArgs {
                    index: self.tool_index,
                    delta: delta.to_string(),
                }
            }
            "response.function_call_arguments.done" => {
                // 只在增量没来过时补一次（有些实现只在 done 里给完整参数）
                if self.args_seen || self.tool_index < 0 {
                    return Step::Skip;
                }
                let args = event.get("arguments").and_then(Value::as_str).unwrap_or("");
                if args.is_empty() {
                    return Step::Skip;
                }
                self.args_seen = true;
                Step::ToolArgs {
                    index: self.tool_index,
                    delta: args.to_string(),
                }
            }

            // ── 收尾
            "response.completed" | "response.done" => Step::Done {
                reason: "stop",
                usage: usage_of(event),
            },
            // `incomplete` = 被 max_output_tokens（或内容过滤）截断：对客户端
            // 而言就是「没说完」，与 chat 的 `length` 同义
            "response.incomplete" => Step::Done {
                reason: "length",
                usage: usage_of(event),
            },

            // ── 失败
            "error" | "response.failed" => {
                let message = error_message(event);
                Step::Failed {
                    status: status_for(&message),
                    message,
                }
            }

            _ => Step::Skip,
        }
    }

    /// 这条流上是否已经发过任何内容（`chat` 用它决定「要不要先发 role 帧」）
    pub fn has_output(&self) -> bool {
        self.text_seen || self.tool_index >= 0
    }
}

/// 从失败事件里取文案（三处历史上都出现过）
pub fn error_message(event: &Value) -> String {
    let text = event
        .pointer("/response/error/message")
        .and_then(Value::as_str)
        .or_else(|| event.pointer("/error/message").and_then(Value::as_str))
        .or_else(|| event.get("message").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match text {
        Some(message) => message.to_string(),
        None => "AStudio 返回了一个错误".to_string(),
    }
}

/// 流内错误的文案 → HTTP 状态（尽力而为）。
///
/// ── 为什么「上下文过长」给 400 而不是 429 ──────────────────
/// 额度类错误（429）会让编排层**换账号重试**；而「输入太长」换谁发同一份超长
/// body 都会被拒 —— 换号只会白扔另一个账号的额度。`too long` 一类的文案里也
/// 常带 limit 字样，所以这条判据要排在额度**之前**（与 kiro 那条「输入过长
/// 优先于一切判定」同一条纪律）。
pub fn status_for(message: &str) -> u16 {
    let lower = message.to_ascii_lowercase();
    let too_long = [
        "context length",
        "context_length",
        "too long",
        "maximum length",
        "too large",
        "too many tokens",
    ];
    if too_long.iter().any(|needle| lower.contains(needle)) {
        return 400;
    }
    let quota = [
        "rate limit",
        "too many requests",
        "quota",
        "balance",
        "credit",
        "积分",
        "额度",
        "余额",
    ];
    if quota.iter().any(|needle| lower.contains(needle)) {
        return 429;
    }
    502
}

/// 取出 `usage`（顶层或包在 `response` 里，两种事件都有）
pub fn usage_of(value: &Value) -> Usage {
    let usage = value
        .get("usage")
        .filter(|value| value.is_object())
        .or_else(|| value.pointer("/response/usage"))
        .cloned()
        .unwrap_or(Value::Null);
    let number = |key: &str| usage.get(key).and_then(Value::as_i64).unwrap_or(0);
    Usage {
        input: number("input_tokens"),
        output: number("output_tokens"),
        cached: usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
    }
}

/// 文本内容（字符串 / 内容块数组）→ 一段纯文本
fn content_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(parts) = content.as_array() else {
        return String::new();
    };
    parts
        .iter()
        .filter_map(|part| match part.get("type").and_then(Value::as_str) {
            Some("text" | "input_text" | "output_text") => {
                part.get("text").and_then(Value::as_str).map(str::to_string)
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 用户消息的 content 数组（文本 + 图片）
fn user_content(message: &Value) -> Value {
    let content = message.get("content").cloned().unwrap_or(Value::Null);
    if let Some(text) = content.as_str() {
        return json!([{ "type": "input_text", "text": text }]);
    }
    let Some(parts) = content.as_array() else {
        return json!([{ "type": "input_text", "text": "" }]);
    };
    let mut items: Vec<Value> = Vec::new();
    for part in parts {
        match part.get("type").and_then(Value::as_str) {
            Some("text" | "input_text") => {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    items.push(json!({ "type": "input_text", "text": text }));
                }
            }
            // chat 的 `{type:image_url, image_url:{url}}` 与 Responses 的
            // `{type:input_image, image_url:"…"}` 只差一层包装
            Some("image_url") => {
                if let Some(url) = part.pointer("/image_url/url").and_then(Value::as_str) {
                    items.push(json!({ "type": "input_image", "image_url": url }));
                }
            }
            Some("input_image") => {
                if let Some(url) = part.get("image_url").and_then(Value::as_str) {
                    items.push(json!({ "type": "input_image", "image_url": url }));
                }
            }
            _ => {}
        }
    }
    if items.is_empty() {
        items.push(json!({ "type": "input_text", "text": "" }));
    }
    Value::Array(items)
}

/// 把 chat 的 `messages` 折成 Responses 的 `input`
fn build_input(messages: &[Value]) -> Vec<Value> {
    let mut input: Vec<Value> = Vec::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        match role {
            // 系统的交给 `instructions`（见 `build_request`），这里跳过
            "system" | "developer" => {}
            "assistant" => {
                let text = content_text(message.get("content").unwrap_or(&Value::Null));
                let has_tools = message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .is_some_and(|calls| !calls.is_empty());
                // 纯工具调用的 assistant 消息正文是空的：不发一条空 item，
                // 否则上游会把「空 assistant」当成一次真实发言
                if !text.trim().is_empty() || !has_tools {
                    input.push(json!({
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }],
                    }));
                }
                for call in message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let name = call
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    // 没有名字的调用发出去只会被拒（上游按名字找工具）
                    if name.is_empty() {
                        continue;
                    }
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.get("id").and_then(Value::as_str).unwrap_or(""),
                        "name": name,
                        "arguments": call
                            .pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}"),
                    }));
                }
            }
            "tool" => {
                let output = content_text(message.get("content").unwrap_or(&Value::Null));
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": message
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    "output": if output.is_empty() { NO_OUTPUT.to_string() } else { output },
                }));
            }
            _ => {
                input.push(json!({
                    "role": "user",
                    "content": user_content(message),
                }));
            }
        }
    }
    input
}

/// 工具声明：chat 的 `{type:function, function:{…}}` → Responses 的扁平形态
fn build_tools(tools: Option<&Value>) -> Option<Vec<Value>> {
    let list = tools?.as_array()?;
    let converted: Vec<Value> = list
        .iter()
        .filter_map(|tool| {
            // 已经扁平化的声明（有人直接发 Responses 形态）也认
            let function = tool.get("function").unwrap_or(tool);
            let name = function.get("name").and_then(Value::as_str)?;
            if name.is_empty() {
                return None;
            }
            Some(json!({
                "type": "function",
                "name": name,
                "description": function
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                "parameters": function
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                // 上游一律 false：`strict` 为真时它会拒 schema 里任何它不认的
                // 关键字（`format` / `oneOf` 一类），而客户端的 schema 是自由的
                "strict": false,
            }))
        })
        .collect();
    (!converted.is_empty()).then_some(converted)
}

/// `tool_choice` 的形态转换。
///
/// chat 是 `{type:function, function:{name}}`，Responses 是 `{type:function, name}`
/// —— 原样透传会被上游拒（它按 `name` 找，找不到就 400）。
fn convert_tool_choice(choice: &Value) -> Value {
    match choice.as_str() {
        Some(mode @ ("auto" | "none" | "required")) => json!(mode),
        _ => {
            if choice.get("type").and_then(Value::as_str) == Some("function") {
                if let Some(name) = choice
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .or_else(|| choice.get("name").and_then(Value::as_str))
                {
                    return json!({ "type": "function", "name": name });
                }
            }
            choice.clone()
        }
    }
}

/// 构造 Responses 请求体。
///
/// `stream` 由调用方给：本家**所有**请求都按流式发（上游逐事件下发，非流式
/// 由网关侧聚合，与豆包工作等有状态家同一处置）。
pub fn build_request(body: &Value, model: &str, stream: bool) -> Value {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // 系统提示散在多条消息里是常态（客户端会插「工具说明」这类 system），
    // 按顺序拼成一段 `instructions`，中间空两行
    let instructions = messages
        .iter()
        .filter(|message| {
            matches!(
                message.get("role").and_then(Value::as_str),
                Some("system" | "developer")
            )
        })
        .map(|message| content_text(message.get("content").unwrap_or(&Value::Null)))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut request = json!({
        "model": model,
        "input": build_input(&messages),
        "stream": stream,
    });
    if !instructions.is_empty() {
        request["instructions"] = json!(instructions);
    }
    if let Some(tools) = build_tools(body.get("tools")) {
        request["tools"] = json!(tools);
        if let Some(choice) = body.get("tool_choice").filter(|value| !value.is_null()) {
            request["tool_choice"] = convert_tool_choice(choice);
        }
        if let Some(parallel) = body.get("parallel_tool_calls").filter(|value| !value.is_null()) {
            request["parallel_tool_calls"] = parallel.clone();
        }
    }
    for key in ["max_output_tokens", "temperature", "top_p"] {
        if let Some(value) = body.get(key).filter(|value| !value.is_null()) {
            request[key] = value.clone();
        }
    }
    // chat 的 `max_tokens` 是旧名：只在没有 `max_output_tokens` 时兜底
    if request.get("max_output_tokens").is_none() {
        if let Some(value) = body.get("max_tokens").filter(|value| !value.is_null()) {
            request["max_output_tokens"] = value.clone();
        }
    }
    // 思考档位：`reasoning_effort` 是 chat 侧的写法，`reasoning.effort` 是
    // Responses 侧的写法，两个都认；`none` 表示关掉思考 —— 发出去反而会被
    // 当成一个不认识的档位
    let effort = body
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .or_else(|| body.pointer("/reasoning/effort").and_then(Value::as_str))
        .unwrap_or("");
    if !effort.is_empty() && effort != "none" {
        request["reasoning"] = json!({ "effort": effort });
    }
    // 结构化输出：chat 的 `response_format.json_schema` → Responses 的
    // `text.format`（少一层 `json_schema` 包装，字段名也从 `schema` 起）
    if let Some(format) = body.get("response_format") {
        match format.get("type").and_then(Value::as_str).unwrap_or("") {
            "json_object" => request["text"] = json!({ "format": { "type": "json_object" } }),
            "json_schema" => {
                let schema = format.get("json_schema").cloned().unwrap_or(Value::Null);
                request["text"] = json!({
                    "format": {
                        "type": "json_schema",
                        "name": schema.get("name").and_then(Value::as_str).unwrap_or("output"),
                        "schema": schema.get("schema").cloned().unwrap_or(Value::Null),
                        "strict": schema.get("strict").and_then(Value::as_bool).unwrap_or(false),
                    }
                });
            }
            _ => {}
        }
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 系统提示折进 `instructions`，用户/助手/工具各折成对应的 item
    #[test]
    fn the_chat_messages_fold_into_responses_items() {
        let body = json!({
            "model": "astronclaw-auto",
            "messages": [
                { "role": "system", "content": "你是助手" },
                { "role": "system", "content": "回答要简短" },
                { "role": "user", "content": "北京天气" },
                { "role": "assistant", "content": "我查一下", "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "weather", "arguments": "{\"city\":\"北京\"}" }
                }] },
                { "role": "tool", "tool_call_id": "call_1", "content": "晴 25℃" },
            ],
        });
        let request = build_request(&body, "astronclaw-auto", true);
        assert_eq!(request["instructions"], "你是助手\n\n回答要简短");
        assert_eq!(request["model"], "astronclaw-auto");
        assert_eq!(request["stream"], true);
        let input = request["input"].as_array().expect("input 应是数组");
        // user / assistant / function_call / function_call_output：system 不在其中
        assert_eq!(input.len(), 4, "实际：{input:?}");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[1]["content"][0]["type"], "output_text");
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call_1");
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["output"], "晴 25℃");
    }

    /// 工具没有输出时补占位（上游不接受空字符串）
    #[test]
    fn an_empty_tool_output_gets_a_placeholder() {
        let body = json!({
            "messages": [{ "role": "tool", "tool_call_id": "c1", "content": "" }],
        });
        let request = build_request(&body, "m", true);
        assert_eq!(request["input"][0]["output"], NO_OUTPUT);
    }

    /// 纯工具调用的 assistant 消息不该额外发一条空发言
    #[test]
    fn a_tool_only_assistant_message_sends_no_empty_bubble() {
        let body = json!({
            "messages": [{
                "role": "assistant",
                "content": null,
                "tool_calls": [{ "id": "c1", "function": { "name": "f", "arguments": "{}" } }],
            }],
        });
        let request = build_request(&body, "m", true);
        let input = request["input"].as_array().expect("input 应是数组");
        assert_eq!(input.len(), 1, "只该有 function_call：{input:?}");
        assert_eq!(input[0]["type"], "function_call");
    }

    /// 工具声明扁平化；`tool_choice` 与 `parallel_tool_calls` 跟着走
    #[test]
    fn the_tools_are_flattened_and_the_choice_translated() {
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "type": "function", "function": {
                "name": "weather",
                "description": "查天气",
                "parameters": { "type": "object", "properties": { "city": { "type": "string" } } },
            }}],
            "tool_choice": { "type": "function", "function": { "name": "weather" } },
            "parallel_tool_calls": false,
        });
        let request = build_request(&body, "m", true);
        let tools = request["tools"].as_array().expect("tools 应是数组");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "weather", "Responses 的声明是扁平形态");
        assert_eq!(tools[0]["strict"], false);
        assert_eq!(request["tool_choice"]["name"], "weather");
        assert_eq!(request["parallel_tool_calls"], false);
    }

    /// 没有 tools 时不发 `tool_choice`（上游会因为「没有工具却指定了选择」报错）
    #[test]
    fn a_tool_choice_without_tools_is_dropped() {
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "tool_choice": "auto",
        });
        let request = build_request(&body, "m", true);
        assert!(request.get("tool_choice").is_none());
        assert!(request.get("tools").is_none());
    }

    /// 思考档位：两个来源都认；`none` 表示关掉，不发
    #[test]
    fn the_reasoning_effort_comes_from_either_field() {
        let flat = build_request(
            &json!({ "messages": [], "reasoning_effort": "high" }),
            "m",
            true,
        );
        assert_eq!(flat["reasoning"]["effort"], "high");

        let nested = build_request(
            &json!({ "messages": [], "reasoning": { "effort": "low" } }),
            "m",
            true,
        );
        assert_eq!(nested["reasoning"]["effort"], "low");

        let off = build_request(
            &json!({ "messages": [], "reasoning_effort": "none" }),
            "m",
            true,
        );
        assert!(off.get("reasoning").is_none(), "none 不该发出去");
    }

    /// 结构化输出：两种 `response_format` 都翻成 `text.format`
    #[test]
    fn the_structured_output_moves_into_text_format() {
        let object = build_request(
            &json!({
                "messages": [],
                "response_format": { "type": "json_object" },
            }),
            "m",
            true,
        );
        assert_eq!(object["text"]["format"]["type"], "json_object");

        let schema = build_request(
            &json!({
                "messages": [],
                "response_format": { "type": "json_schema", "json_schema": {
                    "name": "report",
                    "schema": { "type": "object" },
                    "strict": true,
                }},
            }),
            "m",
            true,
        );
        assert_eq!(schema["text"]["format"]["name"], "report");
        assert_eq!(schema["text"]["format"]["strict"], true);
        assert_eq!(schema["text"]["format"]["schema"]["type"], "object");
    }

    /// 上限：`max_tokens` 只在没有 `max_output_tokens` 时兜底
    #[test]
    fn the_token_limit_prefers_the_responses_name() {
        let only_legacy = build_request(&json!({ "messages": [], "max_tokens": 100 }), "m", true);
        assert_eq!(only_legacy["max_output_tokens"], 100);

        let both = build_request(
            &json!({ "messages": [], "max_tokens": 100, "max_output_tokens": 200 }),
            "m",
            true,
        );
        assert_eq!(both["max_output_tokens"], 200);
    }

    /// 图片：chat 的 `image_url` 包装换成 Responses 的 `input_image`
    #[test]
    fn an_image_part_is_rewrapped() {
        let body = json!({
            "messages": [{ "role": "user", "content": [
                { "type": "text", "text": "这是什么" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAA" } },
            ]}],
        });
        let request = build_request(&body, "m", true);
        let content = request["input"][0]["content"].as_array().expect("content 数组");
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[1]["type"], "input_image");
        assert_eq!(content[1]["image_url"], "data:image/png;base64,AAA");
    }

    /// 正文增量只发一次：`done` 里的整段在增量来过之后要丢掉
    #[test]
    fn the_done_event_does_not_repeat_what_the_deltas_already_said() {
        let mut state = StreamState::new();
        assert_eq!(
            state.step(&json!({ "type": "response.output_text.delta", "delta": "你" })),
            Step::Text("你".to_string())
        );
        assert_eq!(
            state.step(&json!({ "type": "response.output_text.done", "text": "你好" })),
            Step::Skip,
            "增量已经说过的话不该再说一遍"
        );
        assert!(state.has_output());
    }

    /// 只有 `done` 没有增量时，用 `done` 里的整段兜底
    #[test]
    fn the_done_event_is_the_fallback_when_no_deltas_arrive() {
        let mut state = StreamState::new();
        assert_eq!(
            state.step(&json!({ "type": "response.output_text.done", "text": "整段" })),
            Step::Text("整段".to_string())
        );
        assert_eq!(
            state.step(&json!({ "type": "response.output_text.done", "text": "整段" })),
            Step::Skip,
            "第二次 done 也不该重复"
        );
    }

    /// 思考增量走 `reasoning_content` 那条路
    #[test]
    fn the_reasoning_deltas_are_recognised() {
        let mut state = StreamState::new();
        for kind in [
            "response.reasoning_summary_text.delta",
            "response.reasoning_text.delta",
            "response.reasoning.delta",
        ] {
            assert_eq!(
                state.step(&json!({ "type": kind, "delta": "想" })),
                Step::Think("想".to_string()),
                "{kind} 应被当成思考"
            );
        }
    }

    /// 工具调用：开始帧定 index，参数增量只在有 index 时才发
    #[test]
    fn the_tool_calls_are_indexed_in_order() {
        let mut state = StreamState::new();
        assert_eq!(
            state.step(&json!({
                "type": "response.output_item.added",
                "item": { "type": "function_call", "call_id": "c1", "name": "weather" },
            })),
            Step::ToolStart {
                index: 0,
                id: "c1".to_string(),
                name: "weather".to_string(),
            }
        );
        assert_eq!(
            state.step(&json!({
                "type": "response.function_call_arguments.delta",
                "delta": "{\"ci",
            })).clone(),
            Step::ToolArgs {
                index: 0,
                delta: "{\"ci".to_string(),
            }
        );
        // 第二个工具 → index 1
        assert_eq!(
            state.step(&json!({
                "type": "response.output_item.added",
                "item": { "type": "function_call", "id": "item_2", "name": "time" },
            })),
            Step::ToolStart {
                index: 1,
                // 没有 call_id 时用 item.id 兜底
                id: "item_2".to_string(),
                name: "time".to_string(),
            }
        );
    }

    /// 参数只在 `done` 里给时补一次；已经增量发过就不再重复
    #[test]
    fn the_arguments_are_sent_once_whichever_way_they_arrive() {
        let mut delta_first = StreamState::new();
        delta_first.step(&json!({
            "type": "response.output_item.added",
            "item": { "type": "function_call", "call_id": "c1", "name": "f" },
        }));
        delta_first.step(&json!({
            "type": "response.function_call_arguments.delta",
            "delta": "{}",
        }));
        assert_eq!(
            delta_first.step(&json!({
                "type": "response.function_call_arguments.done",
                "arguments": "{}",
            })),
            Step::Skip,
            "增量已经发过参数，done 不该再来一遍"
        );

        let mut done_only = StreamState::new();
        done_only.step(&json!({
            "type": "response.output_item.added",
            "item": { "type": "function_call", "call_id": "c1", "name": "f" },
        }));
        assert_eq!(
            done_only.step(&json!({
                "type": "response.function_call_arguments.done",
                "arguments": "{\"a\":1}",
            })),
            Step::ToolArgs {
                index: 0,
                delta: "{\"a\":1}".to_string(),
            }
        );
    }

    /// 没有工具时不该冒出 index 0 的参数帧
    #[test]
    fn arguments_without_a_tool_are_ignored() {
        let mut state = StreamState::new();
        assert_eq!(
            state.step(&json!({
                "type": "response.function_call_arguments.delta",
                "delta": "{}",
            })),
            Step::Skip
        );
        assert_eq!(
            state.step(&json!({
                "type": "response.function_call_arguments.done",
                "arguments": "{}",
            })),
            Step::Skip
        );
        assert!(!state.has_output());
    }

    /// 收尾：completed → stop，incomplete → length，且都带用量
    #[test]
    fn the_terminal_events_carry_reason_and_usage() {
        let mut state = StreamState::new();
        let done = state.step(&json!({
            "type": "response.completed",
            "response": {
                "usage": {
                    "input_tokens": 12,
                    "output_tokens": 34,
                    "input_tokens_details": { "cached_tokens": 5 },
                },
            },
        }));
        assert_eq!(
            done,
            Step::Done {
                reason: "stop",
                usage: Usage {
                    input: 12,
                    output: 34,
                    cached: 5,
                },
            }
        );

        let truncated = state.step(&json!({
            "type": "response.incomplete",
            "usage": { "input_tokens": 1, "output_tokens": 2 },
        }));
        assert_eq!(
            truncated,
            Step::Done {
                reason: "length",
                usage: Usage {
                    input: 1,
                    output: 2,
                    cached: 0,
                },
            }
        );
    }

    /// 失败事件：三处文案来源都认；文案分档决定状态
    #[test]
    fn a_failure_carries_the_most_useful_message() {
        let mut state = StreamState::new();
        assert_eq!(
            state.step(&json!({
                "type": "response.failed",
                "response": { "error": { "message": "upstream exploded" } },
            })),
            Step::Failed {
                status: 502,
                message: "upstream exploded".to_string(),
            }
        );
        assert_eq!(
            state.step(&json!({
                "type": "error",
                "error": { "message": "rate limit reached" },
            })),
            Step::Failed {
                status: 429,
                message: "rate limit reached".to_string(),
            }
        );
        // 什么都没有时给一句人话，而不是空串
        let bare = state.step(&json!({ "type": "error" }));
        assert_eq!(
            bare,
            Step::Failed {
                status: 502,
                message: "AStudio 返回了一个错误".to_string(),
            }
        );
    }

    /// 「输入过长」必须排在额度**之前**：判成 429 会换号重试，而换号没用
    #[test]
    fn too_long_wins_over_quota_wording() {
        // 文案里同时有 limit 与 context length
        assert_eq!(
            status_for("This model's maximum context length is 128000 tokens"),
            400
        );
        assert_eq!(status_for("prompt is too long: 200000 tokens > limit"), 400);
        // 真正的额度才给 429
        assert_eq!(status_for("insufficient quota, please recharge"), 429);
        assert_eq!(status_for("积分不足"), 429);
        // 认不出的按上游故障
        assert_eq!(status_for("boom"), 502);
    }

    /// 用量对象是 chat 的形态（含缓存命中明细）
    #[test]
    fn the_usage_json_looks_like_chat() {
        let usage = Usage {
            input: 10,
            output: 5,
            cached: 3,
        };
        let value = usage.to_json();
        assert_eq!(value["prompt_tokens"], 10);
        assert_eq!(value["completion_tokens"], 5);
        assert_eq!(value["total_tokens"], 15);
        assert_eq!(value["prompt_tokens_details"]["cached_tokens"], 3);
        // 负数被夹到 0（上游偶尔给 -1 表示「未知」）
        let clamped = Usage {
            input: -1,
            output: -1,
            cached: -1,
        };
        assert_eq!(clamped.to_json()["prompt_tokens"], 0);
        assert_eq!(clamped.to_json()["total_tokens"], 0);
    }
}
