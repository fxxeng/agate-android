//! OpenAI Chat 请求 → Kiro 的 `generateAssistantResponse` 会话体。
//!
//! ── 两套消息模型怎么对上 ────────────────────────────────────
//! ```text
//!   OpenAI Chat                       Kiro conversationState
//!   system / developer          →     拼在第一条 user 的正文前面（Kiro 不收
//!                                     systemPrompt 字段）
//!   user（text / image_url）    →     userInputMessage（content / images）
//!   assistant（content）        →     assistantResponseMessage.content
//!   assistant.tool_calls        →     assistantResponseMessage.toolUses
//!   tool（tool_call_id）        →     下一条 userInputMessage 的 toolResults
//!   tools[].function            →     userInputMessageContext.tools[]
//! ```
//!
//! ── 六条硬规则（都是上游 400 教出来的，别按「看起来更合理」改）──
//! ```text
//!   1  首尾必须是 userInputMessage：开头的 assistant 与「只有工具结果」的 user
//!      删掉；结尾是 assistant 时补一条空 user
//!   2  每个 tool call 必须有一条 result：缺的补 error 结果，多出来的丢掉
//!   3  工具名只收 [a-zA-Z0-9_-]{1,64}，id 只收 [a-zA-Z0-9_.:-]{1,64}；
//!      不合规的按哈希改名，回给客户端时用 `tool_names` 映射回原名
//!   4  工具参数 schema 的根必须是 object（`{}`、无 type、object-or-null 都补）
//!   5  只带最新一组图片：历史里的旧图每轮重传等于白烧额度
//!   6  空 content 用 `Please proceed with the task.`（Kiro 不收空消息）
//! ```
//!
//! ── 思考走哪条路，由目录决定 ────────────────────────────────
//! 目录声明了自适应思考的模型（`additionalModelRequestFieldsSchema` 里有
//! `adaptive`）发 `additionalModelRequestFields`；其余发
//! `<thinking_mode>…</thinking_mode><max_thinking_length>…` 前缀（与 kiro-cli
//! 同款）。**两者不能同时发**：同时发时上游按字段走、前缀被当正文，等于把
//! 一串控制标签塞进用户的问题里。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use super::image::{fit, split_data_url, IMAGE_LIMIT};
use super::models::{self, NativeEffort};

/// Kiro 不接受空消息时的替代正文
const PROCEED: &str = "Please proceed with the task.";
/// 被截断的工具输出的占位说明
const NO_RESULT: &str = "Tool use was interrupted and did not produce a result.";
/// 工具结果的长度上限（字符数）：再长的输出对模型也没用，只会挤爆上下文
const RESULT_LIMIT: usize = 250_000;

/// 一条 user 消息
#[derive(Default, Clone)]
struct UserMessage {
    content: String,
    images: Vec<Value>,
    tool_results: Vec<Value>,
    tools: Vec<Value>,
    model_id: String,
}

/// 一条 assistant 消息
#[derive(Default, Clone)]
struct AssistantMessage {
    content: String,
    tool_uses: Vec<Value>,
}

#[derive(Default, Clone)]
struct Entry {
    user: Option<UserMessage>,
    assistant: Option<AssistantMessage>,
}

/// 思考档位（`reasoning_effort` 与 Anthropic 的 budget 都折到这里）。
///
/// `None` = 这次不要思考。判据先看模型：只有 Claude 与 Auto 支持，
/// 别的模型给了预算只会得到一段废话。
pub fn thinking_budget(body: &Value, model: &str) -> Option<u32> {
    if !models::thinks(model) {
        return None;
    }
    let effort = effort_of(body).unwrap_or_default();
    let budget = match effort.as_str() {
        "low" => 10_000,
        "medium" => 20_000,
        "high" => 30_000,
        "xhigh" | "max" => 50_000,
        _ => 0,
    };
    if budget > 0 {
        return Some(budget);
    }
    // 客户端直接给了预算（Anthropic 风格扩展字段）
    body.pointer("/thinking/budget_tokens")
        .and_then(Value::as_u64)
        .map(|value| value.clamp(1024, 50_000) as u32)
}

/// 客户端要的那个 effort 字面量（决定走原生字段还是前缀）
fn effort_of(body: &Value) -> Option<String> {
    body.get("reasoning_effort")
        .and_then(Value::as_str)
        .or_else(|| body.pointer("/reasoning/effort").and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// 结构化输出：OpenAI 的 `response_format` 折成一句自然语言（Kiro 没有这个字段）
fn format_text(body: &Value) -> String {
    let Some(format) = body
        .get("response_format")
        .or_else(|| body.get("output_format"))
    else {
        return String::new();
    };
    let kind = format.get("type").and_then(Value::as_str).unwrap_or("");
    if kind == "json_object" {
        return "Respond with a single JSON object and nothing else.".to_string();
    }
    let schema = format
        .get("json_schema")
        .and_then(|value| value.get("schema"))
        .or_else(|| format.get("schema"));
    let Some(schema) = schema.filter(|value| value.is_object()) else {
        return String::new();
    };
    format!(
        "Respond with a single JSON value and nothing else, matching this JSON schema:\n{}.",
        serde_json::to_string(schema).unwrap_or_default()
    )
}

/// 工具名规整（Kiro 只收 `[a-zA-Z0-9_-]{1,64}`）。
///
/// 不合规时**保留可读前缀 + 摘要后缀**：只留哈希会让人对不上是哪个工具，
/// 只截断又可能与别的工具撞名。
pub fn tool_name(name: &str) -> String {
    if is_tool_name(name) {
        return name.to_string();
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(name.as_bytes());
    let hex: String = digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let sanitized: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let keep = 64usize.saturating_sub(1 + hex.len());
    let head: String = sanitized.chars().take(keep).collect();
    format!("{head}_{hex}")
}

fn is_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

/// 工具调用 id 规整（Kiro 只收 `[a-zA-Z0-9_.:-]{1,64}`）。
pub fn tool_id(id: &str) -> String {
    if is_tool_id(id) {
        return id.to_string();
    }
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(id.as_bytes());
    let encoded = URL_SAFE_NO_PAD.encode(digest);
    format!("t_{}", encoded.chars().take(32).collect::<String>())
}

fn is_tool_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '-'))
}

/// 规整名 → 客户端原名（Kiro 回的是规整后的名字，回给客户端前要换回来）
pub fn tool_names(body: &Value) -> HashMap<String, String> {
    let mut names = HashMap::new();
    let mut add = |name: &str| {
        let mapped = tool_name(name);
        if mapped != name {
            names.insert(mapped, name.to_string());
        }
    };
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        for tool in tools {
            if let Some(name) = tool
                .pointer("/function/name")
                .and_then(Value::as_str)
                .or_else(|| tool.get("name").and_then(Value::as_str))
            {
                add(name);
            }
        }
    }
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for message in messages {
            for call in message
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                    add(name);
                }
            }
        }
    }
    names
}

/// 文本内容（字符串 / OpenAI 内容块数组）
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

/// 工具参数：OpenAI 的 `arguments` 是 JSON 字符串，Kiro 要对象
fn arguments_value(raw: &Value) -> Value {
    if let Some(text) = raw.as_str() {
        serde_json::from_str::<Value>(text)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}))
    } else if raw.is_object() {
        raw.clone()
    } else {
        json!({})
    }
}

/// 工具参数 schema 的根补成 object（Kiro 的硬要求）。
///
/// 三种要补的形态：`{}`（空 scheme）、没有 `type`、`["object","null"]`
/// （可空对象）。别的形态原样返回 —— 猜错会改变模型看到的结构。
pub fn input_schema(schema: Option<&Value>) -> Value {
    let Some(schema) = schema.filter(|value| value.is_object()) else {
        return json!({ "type": "object", "properties": {} });
    };
    let object = schema.as_object().cloned().unwrap_or_default();
    if object.is_empty() {
        return json!({ "type": "object", "properties": {} });
    }
    match object.get("type") {
        None => {
            let mut next = Map::new();
            next.insert("type".to_string(), json!("object"));
            for (key, value) in object {
                next.insert(key, value);
            }
            Value::Object(next)
        }
        Some(Value::Array(types)) => {
            let nullable_object = !types.is_empty()
                && types
                    .iter()
                    .all(|kind| kind.as_str() == Some("object") || kind.as_str() == Some("null"));
            if nullable_object {
                let mut next = object;
                next.insert("type".to_string(), json!("object"));
                Value::Object(next)
            } else {
                schema.clone()
            }
        }
        _ => schema.clone(),
    }
}

fn join_non_empty(left: &str, right: &str) -> String {
    if left.is_empty() {
        right.to_string()
    } else if right.is_empty() {
        left.to_string()
    } else {
        format!("{left}\n\n{right}")
    }
}

/// 一张图片：只有内联 base64 能发（http 图片没有字节）。
///
/// 超限时缩；缩不动就换成一句说明 —— 输一句「这张图没发出去」比整次请求失败好，
/// 而且说明会留在历史里，下一轮不会重撞同一面墙。
fn push_image(images: &mut Vec<Value>, url: &str) {
    let Some((format, data)) = split_data_url(url) else {
        return;
    };
    if data.is_empty() {
        return;
    }
    if data.len() <= IMAGE_LIMIT {
        images.push(json!({ "format": format, "source": { "bytes": data } }));
        return;
    }
    match fit(&data) {
        super::image::Fitted::Smaller { format, bytes } => {
            images.push(json!({ "format": format, "source": { "bytes": bytes } }));
        }
        super::image::Fitted::Note(note) => {
            images.push(json!({
                "format": "png",
                "note": format!(
                    "[一张图片（{format}，Base64 {:.1} MB）没有随本条消息发送：\
                     Kiro 单张上限 {:.1} MB，{note}。]",
                    data.len() as f64 / (1024.0 * 1024.0),
                    IMAGE_LIMIT as f64 / (1024.0 * 1024.0),
                )
            }));
        }
    }
}

/// 构造请求体。
///
/// `model` 是发上游的模型 id；`profile` 为空时（API key 账号）不写 `profileArn`。
pub fn build(
    body: &Value,
    model: &str,
    profile: &str,
    budget: Option<u32>,
    native: Option<&NativeEffort>,
) -> Value {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut system_parts: Vec<String> = Vec::new();
    let mut entries: Vec<Entry> = Vec::new();
    for message in &messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        if role == "system" || role == "developer" {
            let text = content_text(message.get("content").unwrap_or(&Value::Null));
            if !text.trim().is_empty() {
                system_parts.push(text);
            }
            continue;
        }
        if role == "assistant" {
            let mut assistant = AssistantMessage {
                content: content_text(message.get("content").unwrap_or(&Value::Null)),
                tool_uses: Vec::new(),
            };
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
                if name.is_empty() {
                    continue;
                }
                assistant.tool_uses.push(json!({
                    "name": tool_name(name),
                    "toolUseId": tool_id(
                        call.get("id").and_then(Value::as_str).unwrap_or(name)
                    ),
                    "input": arguments_value(
                        call.pointer("/function/arguments").unwrap_or(&Value::Null)
                    ),
                }));
            }
            if assistant.content.is_empty() && assistant.tool_uses.is_empty() {
                continue;
            }
            push_assistant(&mut entries, assistant);
            continue;
        }
        // user / tool
        let mut user = UserMessage {
            model_id: model.to_string(),
            ..Default::default()
        };
        if role == "tool" {
            let call_id = message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let mut output = content_text(message.get("content").unwrap_or(&Value::Null));
            if output.is_empty() {
                output = "(no output)".to_string();
            }
            if output.len() > RESULT_LIMIT {
                output.truncate(RESULT_LIMIT);
                output.push_str("\n… (cut)");
            }
            user.tool_results.push(json!({
                "toolUseId": tool_id(call_id),
                "status": if message.get("is_error").and_then(Value::as_bool) == Some(true) {
                    "error"
                } else {
                    "success"
                },
                "content": [{ "text": output }],
            }));
            // 工具结果里附带的图片（截图类客户端）跟着 images 字段走
            if let Some(parts) = message.get("content").and_then(Value::as_array) {
                for part in parts {
                    if part.get("type").and_then(Value::as_str) == Some("image_url") {
                        if let Some(url) = part.pointer("/image_url/url").and_then(Value::as_str) {
                            push_image(&mut user.images, url);
                        }
                    }
                }
            }
        } else {
            user.content = content_text(message.get("content").unwrap_or(&Value::Null));
            if let Some(parts) = message.get("content").and_then(Value::as_array) {
                for part in parts {
                    match part.get("type").and_then(Value::as_str) {
                        Some("image_url") => {
                            if let Some(url) =
                                part.pointer("/image_url/url").and_then(Value::as_str)
                            {
                                push_image(&mut user.images, url);
                            }
                        }
                        // Responses API 的写法：`image_url` 直接是字符串
                        Some("input_image") => {
                            if let Some(url) = part.get("image_url").and_then(Value::as_str) {
                                push_image(&mut user.images, url);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        push_user(&mut entries, user);
    }

    // 规则 1 前半：首条必须是 userInputMessage
    while entries.first().is_some_and(|entry| entry.user.is_none()) {
        entries.remove(0);
    }
    // 「只有工具结果」的第一条也要删：它没有对应的 assistant 调用
    while entries.first().is_some_and(|entry| {
        entry
            .user
            .as_ref()
            .is_some_and(|user| !user.tool_results.is_empty() && user.content.is_empty())
    }) {
        entries.remove(0);
    }
    if let Some(first) = entries.first_mut().and_then(|entry| entry.user.as_mut()) {
        first.tool_results.clear();
    }
    // 规则 1 后半：末条必须是要被回答的那条 user
    if entries.last().map(|entry| entry.user.is_none()).unwrap_or(true) {
        entries.push(Entry {
            user: Some(UserMessage {
                model_id: model.to_string(),
                ..Default::default()
            }),
            assistant: None,
        });
    }
    pair_tool_results(&mut entries);
    // 规则 6：空消息补一句
    for entry in &mut entries {
        if let Some(user) = entry.user.as_mut() {
            if user.content.is_empty() && user.tool_results.is_empty() {
                user.content = PROCEED.to_string();
            }
        }
    }
    // 规则 5：只保留最新一组图片
    let latest_images = entries.iter().rposition(|entry| {
        entry
            .user
            .as_ref()
            .is_some_and(|user| !user.images.is_empty())
    });
    for (position, entry) in entries.iter_mut().enumerate() {
        if let Some(user) = entry.user.as_mut() {
            if Some(position) != latest_images {
                user.images.clear();
            }
        }
    }

    // 指令：拼到第一条 user 的正文前面
    let mut system = system_parts.join("\n\n");
    let format = format_text(body);
    if !format.is_empty() {
        system = join_non_empty(&system, &format);
    }
    let effort = effort_of(body);
    let native_effort = budget.is_some()
        && native.is_some_and(|native| {
            effort
                .as_deref()
                .is_some_and(|value| native.levels.iter().any(|level| level == value))
        });
    if let Some(budget) = budget.filter(|_| !native_effort) {
        // 前缀那条路：与 kiro-cli 同款，放在所有指令的最前面
        let prefix = format!(
            "<thinking_mode>enabled</thinking_mode><max_thinking_length>{budget}</max_thinking_length>"
        );
        system = if system.is_empty() {
            prefix
        } else {
            format!("{prefix}\n{system}")
        };
    }
    if let Some(first) = entries.first_mut().and_then(|entry| entry.user.as_mut()) {
        if !system.is_empty() {
            first.content = format!("{system}\n\n{}", first.content);
        }
    }

    // 规则 3：工具声明 = 客户端给的 + 历史里用过但这次没给的（Kiro 拒绝未知工具）
    let tools = collect_tools(body, &entries);
    if let Some(current) = entries.last_mut().and_then(|entry| entry.user.as_mut()) {
        if !tools.is_empty() {
            current.tools = tools;
        }
    }

    // 落成 JSON：最后一条是 currentMessage，其余是 history
    let mut history: Vec<Value> = Vec::new();
    let last = entries.pop();
    for entry in entries {
        if let Some(user) = entry.user {
            history.push(json!({ "userInputMessage": user_json(&user) }));
        } else if let Some(assistant) = entry.assistant {
            history.push(json!({ "assistantResponseMessage": assistant_json(&assistant) }));
        }
    }
    let current = last
        .and_then(|entry| entry.user)
        .unwrap_or_else(|| UserMessage {
            model_id: model.to_string(),
            ..Default::default()
        });
    let mut state = json!({
        "chatTriggerType": "MANUAL",
        "agentTaskType": "vibe",
        "conversationId": super::http::uuid_v4(),
        "currentMessage": { "userInputMessage": user_json(&current) },
    });
    if !history.is_empty() {
        state["history"] = json!(history);
    }
    let mut out = json!({ "conversationState": state, "agentMode": "vibe" });
    if !profile.is_empty() {
        out["profileArn"] = json!(profile);
    }
    if native_effort {
        if let (Some(native), Some(effort)) = (native, effort) {
            let mut fields = Map::new();
            fields.insert("thinking".to_string(), json!({ "type": "adaptive" }));
            fields.insert(native.path.clone(), json!({ "effort": effort }));
            out["additionalModelRequestFields"] = Value::Object(fields);
        }
    }
    out
}

/// 把一条 assistant 消息并进末尾（**同角色连续消息**合并成一条）。
///
/// 只与「末尾那条本身就是 assistant」的 Entry 合并；末尾是 user 时必须新起一条
/// —— 一个 Entry 装一个角色，这是后面「历史上下一配对」的前提。
fn push_assistant(entries: &mut Vec<Entry>, assistant: AssistantMessage) {
    if let Some(last) = entries.last_mut() {
        if let Some(previous) = last.assistant.take() {
            let mut merged = previous;
            merged.content = join_non_empty(&merged.content, &assistant.content);
            merged.tool_uses.extend(assistant.tool_uses);
            last.assistant = Some(merged);
            return;
        }
    }
    entries.push(Entry {
        user: None,
        assistant: Some(assistant),
    });
}

/// 同上，user 侧
fn push_user(entries: &mut Vec<Entry>, user: UserMessage) {
    if let Some(last) = entries.last_mut() {
        if let Some(previous) = last.user.take() {
            let mut merged = previous;
            merged.content = join_non_empty(&merged.content, &user.content);
            merged.images.extend(user.images);
            merged.tool_results.extend(user.tool_results);
            last.user = Some(merged);
            return;
        }
    }
    entries.push(Entry {
        user: Some(user),
        assistant: None,
    });
}

/// 规则 2：工具调用与结果配对。
///
/// 每个 `toolUseId` 恰好一条结果 —— 缺的补 error、重的丢、无主的丢。
/// Kiro 会校验这个配对，缺一条就整次 400。
fn pair_tool_results(entries: &mut [Entry]) {
    for index in 0..entries.len() {
        if entries[index].user.is_none() {
            continue;
        }
        let calls = index
            .checked_sub(1)
            .and_then(|previous| entries[previous].assistant.as_ref())
            .map(|assistant| assistant.tool_uses.clone())
            .unwrap_or_default();
        let Some(user) = entries[index].user.as_mut() else {
            continue;
        };
        let mut answered: Vec<String> = Vec::new();
        let mut kept: Vec<Value> = Vec::new();
        for result in std::mem::take(&mut user.tool_results) {
            let id = result
                .get("toolUseId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let belongs = calls
                .iter()
                .any(|call| call.get("toolUseId").and_then(Value::as_str) == Some(id.as_str()));
            if belongs && !answered.contains(&id) {
                answered.push(id);
                kept.push(result);
            }
        }
        for call in &calls {
            let id = call
                .get("toolUseId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if !answered.contains(&id) {
                answered.push(id.clone());
                kept.push(json!({
                    "toolUseId": id,
                    "status": "error",
                    "content": [{ "text": NO_RESULT }],
                }));
            }
        }
        user.tool_results = kept;
    }
}

/// 规则 3：可下发的工具清单
fn collect_tools(body: &Value, entries: &[Entry]) -> Vec<Value> {
    let mut tools: Vec<Value> = Vec::new();
    let mut offered: Vec<String> = Vec::new();
    if let Some(list) = body.get("tools").and_then(Value::as_array) {
        for tool in list {
            // OpenAI 是 `{type:"function", function:{…}}`；也接受扁平的写法
            let function = tool.get("function").unwrap_or(tool);
            let Some(name) = function.get("name").and_then(Value::as_str) else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            let mapped = tool_name(name);
            if offered.contains(&mapped) {
                continue;
            }
            offered.push(mapped.clone());
            tools.push(json!({
                "toolSpecification": {
                    "name": mapped,
                    "description": function
                        .get("description")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                        .unwrap_or(name),
                    "inputSchema": {
                        "json": input_schema(
                            function.get("parameters").or_else(|| function.get("input_schema"))
                        )
                    }
                }
            }));
        }
    }
    for entry in entries {
        let Some(assistant) = entry.assistant.as_ref() else {
            continue;
        };
        for call in &assistant.tool_uses {
            let Some(name) = call.get("name").and_then(Value::as_str) else {
                continue;
            };
            if offered.iter().any(|item| item == name) {
                continue;
            }
            offered.push(name.to_string());
            tools.push(json!({
                "toolSpecification": {
                    "name": name,
                    "description": "Tool",
                    "inputSchema": { "json": { "type": "object", "properties": {} } }
                }
            }));
        }
    }
    tools
}

fn user_json(user: &UserMessage) -> Value {
    let mut object = Map::new();
    object.insert("content".to_string(), json!(user.content));
    object.insert("modelId".to_string(), json!(user.model_id));
    object.insert("origin".to_string(), json!("KIRO_CLI"));
    if !user.images.is_empty() {
        let images: Vec<Value> = user
            .images
            .iter()
            .filter(|image| image.get("note").is_none())
            .cloned()
            .collect();
        let notes: Vec<String> = user
            .images
            .iter()
            .filter_map(|image| {
                image
                    .get("note")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        if !images.is_empty() {
            object.insert("images".to_string(), json!(images));
        }
        if !notes.is_empty() {
            let content = object
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            object.insert(
                "content".to_string(),
                json!(join_non_empty(&content, &notes.join("\n"))),
            );
        }
    }
    let mut context = Map::new();
    if !user.tools.is_empty() {
        context.insert("tools".to_string(), json!(user.tools));
    }
    if !user.tool_results.is_empty() {
        context.insert("toolResults".to_string(), json!(user.tool_results));
    }
    if !context.is_empty() {
        object.insert("userInputMessageContext".to_string(), Value::Object(context));
    }
    Value::Object(object)
}

fn assistant_json(assistant: &AssistantMessage) -> Value {
    let mut object = Map::new();
    object.insert("content".to_string(), json!(assistant.content));
    if !assistant.tool_uses.is_empty() {
        object.insert("toolUses".to_string(), json!(assistant.tool_uses));
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(out: &Value) -> Vec<Value> {
        out.pointer("/conversationState/history")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    fn current(out: &Value) -> Value {
        out.pointer("/conversationState/currentMessage/userInputMessage")
            .cloned()
            .unwrap_or(Value::Null)
    }

    fn content_of(value: &Value) -> String {
        value
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    /// system 拼到第一条 user 的前面，且不进 history 的条数
    #[test]
    fn the_system_prompt_is_folded_into_the_first_user_message() {
        let body = json!({
            "messages": [
                { "role": "system", "content": "你是助手" },
                { "role": "user", "content": "你好" }
            ]
        });
        let out = build(&body, "auto", "", None, None);
        assert!(history(&out).is_empty(), "只有一条消息时全是 currentMessage");
        assert_eq!(content_of(&current(&out)), "你是助手\n\n你好");
    }

    /// 开头是 assistant：删掉（首条必须是 user）
    #[test]
    fn a_leading_assistant_message_is_dropped() {
        let body = json!({
            "messages": [
                { "role": "assistant", "content": "我先说话" },
                { "role": "user", "content": "接着说" }
            ]
        });
        let out = build(&body, "auto", "", None, None);
        assert!(history(&out).is_empty());
        assert_eq!(content_of(&current(&out)), "接着说");
    }

    /// 结尾是 assistant：补一条空 user（它才是要被回答的那条）
    #[test]
    fn a_trailing_assistant_gets_an_empty_user() {
        let body = json!({
            "messages": [
                { "role": "user", "content": "第一句" },
                { "role": "assistant", "content": "回答" }
            ]
        });
        let out = build(&body, "auto", "", None, None);
        let history = history(&out);
        // 补出来的空 user 是 currentMessage，历史里留下 user + assistant 两条
        assert_eq!(history.len(), 2);
        assert_eq!(
            history[0].pointer("/userInputMessage/content").and_then(Value::as_str),
            Some("第一句")
        );
        assert_eq!(
            history[1]
                .pointer("/assistantResponseMessage/content")
                .and_then(Value::as_str),
            Some("回答")
        );
        assert_eq!(content_of(&current(&out)), PROCEED, "补的空 user 要有一句正文");
    }

    /// 工具的往返：assistant.tool_calls → toolUses，tool 消息 → toolResults
    #[test]
    fn tool_calls_and_results_are_paired() {
        let body = json!({
            "messages": [
                { "role": "user", "content": "查一下天气" },
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "get_weather", "arguments": "{\"city\":\"北京\"}" }
                    }]
                },
                { "role": "tool", "tool_call_id": "call_1", "content": "晴 20 度" },
                { "role": "user", "content": "谢谢" }
            ]
        });
        let out = build(&body, "auto", "", None, None);
        let history = history(&out);
        let uses = history[1]
            .pointer("/assistantResponseMessage/toolUses")
            .and_then(Value::as_array)
            .expect("assistant 那条要带 toolUses");
        assert_eq!(uses[0]["name"], "get_weather");
        assert_eq!(uses[0]["input"]["city"], "北京", "参数要变成对象");
        // tool 结果与紧接的用户消息属于同一角色，合并成 currentMessage
        let message = current(&out);
        let results = message
            .pointer("/userInputMessageContext/toolResults")
            .and_then(Value::as_array)
            .expect("currentMessage 里要有 toolResults");
        assert_eq!(results[0]["toolUseId"], "call_1");
        assert_eq!(results[0]["status"], "success");
        assert_eq!(results[0]["content"][0]["text"], "晴 20 度");
        assert!(
            content_of(&current(&out)).ends_with("谢谢"),
            "工具结果与随后的用户消息合成一条"
        );
    }

    /// 缺结果要补一条 error（Kiro 校验配对）
    #[test]
    fn a_missing_tool_result_is_filled_with_an_error() {
        let body = json!({
            "messages": [
                { "role": "user", "content": "查一下" },
                {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "function": { "name": "get_weather", "arguments": "{}" }
                    }]
                },
                { "role": "user", "content": "算了" }
            ]
        });
        let out = build(&body, "auto", "", None, None);
        let results = current(&out)
            .pointer("/userInputMessageContext/toolResults")
            .and_then(Value::as_array)
            .cloned()
            .expect("补出来的 toolResults");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["status"], "error");
        assert_eq!(results[0]["content"][0]["text"], NO_RESULT);
    }

    /// 无主的结果丢掉（没有对应的 tool call）
    #[test]
    fn an_orphan_tool_result_is_dropped() {
        let body = json!({
            "messages": [
                { "role": "user", "content": "第一句" },
                { "role": "assistant", "content": "回答" },
                { "role": "tool", "tool_call_id": "ghost", "content": "没人要的结果" },
                { "role": "user", "content": "继续" }
            ]
        });
        let out = build(&body, "auto", "", None, None);
        assert!(
            current(&out)
                .pointer("/userInputMessageContext/toolResults")
                .is_none(),
            "无主结果不能发上去"
        );
    }

    /// 首条只有工具结果：删掉（它没有对应的 assistant 调用）
    #[test]
    fn a_lone_tool_result_never_leads_the_conversation() {
        let body = json!({
            "messages": [
                { "role": "tool", "tool_call_id": "x", "content": "结果" },
                { "role": "user", "content": "你好" }
            ]
        });
        let out = build(&body, "auto", "", None, None);
        assert_eq!(content_of(&current(&out)), "你好");
        assert!(history(&out).is_empty());
    }

    /// 工具名不合规要改名，且能映射回原名
    #[test]
    fn illegal_tool_names_are_renamed_and_mapped_back() {
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "type": "function", "function": { "name": "weather/lookup", "parameters": {} } }]
        });
        let out = build(&body, "auto", "", None, None);
        let tools = current(&out)
            .pointer("/userInputMessageContext/tools")
            .and_then(Value::as_array)
            .cloned()
            .expect("有工具");
        let name = tools[0]
            .pointer("/toolSpecification/name")
            .and_then(Value::as_str)
            .expect("有名字");
        assert!(is_tool_name(name), "{name} 必须合法");
        assert!(name.starts_with("weather_lookup_"), "保留可读前缀：{name}");
        let names = tool_names(&body);
        assert_eq!(names.get(name).map(String::as_str), Some("weather/lookup"));
        // 纯中文名同样要变成合法形态（上游把非 ASCII 一律替换成 `_`）
        assert!(is_tool_name(&tool_name("天气/查询")));
    }

    /// schema 的根补成 object（三种形态）
    #[test]
    fn the_input_schema_root_is_forced_to_object() {
        assert_eq!(
            input_schema(None),
            json!({ "type": "object", "properties": {} })
        );
        assert_eq!(
            input_schema(Some(&json!({}))),
            json!({ "type": "object", "properties": {} })
        );
        assert_eq!(
            input_schema(Some(&json!({ "properties": { "a": { "type": "string" } } })))
                ["type"],
            json!("object")
        );
        assert_eq!(
            input_schema(Some(&json!({ "type": ["object", "null"] }))),
            json!({ "type": "object" })
        );
        // 不是 object 的形态原样返回（猜错会改掉模型看到的结构）
        let array = json!({ "type": "array", "items": { "type": "string" } });
        assert_eq!(input_schema(Some(&array)), array);
    }

    /// 只保留最新一组图片（历史里的旧图每轮重传等于白烧额度）
    #[test]
    fn only_the_latest_image_batch_survives() {
        let first = "data:image/png;base64,iVBORw0KGgo=";
        let second = "data:image/png;base64,iVBORw0KGgoAAAA=";
        let body = json!({
            "messages": [
                { "role": "user", "content": [{ "type": "text", "text": "看这张" },
                                              { "type": "image_url", "image_url": { "url": first } }] },
                { "role": "assistant", "content": "看到了" },
                { "role": "user", "content": [{ "type": "text", "text": "再看这张" },
                                              { "type": "image_url", "image_url": { "url": second } }] }
            ]
        });
        let out = build(&body, "auto", "", None, None);
        let history = history(&out);
        assert!(
            history[0].pointer("/userInputMessage/images").is_none(),
            "历史里的旧图要清掉（每轮重传等于白烧额度）"
        );
        let message = current(&out);
        let images = message
            .get("images")
            .and_then(Value::as_array)
            .cloned()
            .expect("最新一组图要留着");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0]["source"]["bytes"], second.trim_start_matches("data:image/png;base64,"));
    }

    /// 空消息补一句（Kiro 不收空 content）
    #[test]
    fn empty_messages_get_the_proceed_sentence() {
        let body = json!({ "messages": [{ "role": "user", "content": "" }] });
        let out = build(&body, "auto", "", None, None);
        assert_eq!(content_of(&current(&out)), PROCEED);
    }

    /// profile 为空（API key 账号）时不写 profileArn
    #[test]
    fn profile_arn_is_only_written_when_known() {
        let body = json!({ "messages": [{ "role": "user", "content": "hi" }] });
        assert!(build(&body, "auto", "", None, None).get("profileArn").is_none());
        let with = build(&body, "auto", "arn:aws:codewhisperer:us-east-1:1:profile/A", None, None);
        assert_eq!(
            with.get("profileArn").and_then(Value::as_str),
            Some("arn:aws:codewhisperer:us-east-1:1:profile/A")
        );
    }

    /// 思考：没原生档位就用前缀，有就用字段（不能同时发）
    #[test]
    fn thinking_goes_either_as_a_prefix_or_as_fields() {
        let body = json!({
            "messages": [{ "role": "user", "content": "难题" }],
            "reasoning_effort": "high"
        });
        let plain = build(&body, "claude-sonnet-4.5", "", thinking_budget(&body, "claude-sonnet-4.5"), None);
        let text = content_of(&current(&plain));
        assert!(text.starts_with("<thinking_mode>enabled</thinking_mode>"), "{text}");
        assert!(text.contains("<max_thinking_length>30000</max_thinking_length>"));
        assert!(plain.get("additionalModelRequestFields").is_none());

        let native = NativeEffort {
            path: "output_config".to_string(),
            levels: vec!["low".to_string(), "high".to_string()],
        };
        let fields = build(
            &body,
            "claude-sonnet-4.5",
            "",
            thinking_budget(&body, "claude-sonnet-4.5"),
            Some(&native),
        );
        assert!(
            !content_of(&current(&fields)).contains("<thinking_mode>"),
            "走字段时不能再带前缀"
        );
        assert_eq!(
            fields.pointer("/additionalModelRequestFields/output_config/effort"),
            Some(&json!("high"))
        );
        assert_eq!(
            fields.pointer("/additionalModelRequestFields/thinking/type"),
            Some(&json!("adaptive"))
        );
    }

    /// 档位不在目录给的列表里 → 退回前缀（不接受上游没声明的值）
    #[test]
    fn an_undeclared_effort_falls_back_to_the_prefix() {
        let body = json!({
            "messages": [{ "role": "user", "content": "难题" }],
            "reasoning_effort": "xhigh"
        });
        let native = NativeEffort {
            path: "output_config".to_string(),
            levels: vec!["low".to_string(), "high".to_string()],
        };
        let out = build(&body, "claude-sonnet-4.5", "", thinking_budget(&body, "claude-sonnet-4.5"), Some(&native));
        assert!(content_of(&current(&out)).contains("<thinking_mode>"));
        assert!(out.get("additionalModelRequestFields").is_none());
    }

    /// 不支持思考的模型：不给预算也不加前缀
    #[test]
    fn models_without_thinking_get_no_directive() {
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "reasoning_effort": "high"
        });
        assert!(thinking_budget(&body, "amazon-nova-pro").is_none());
        let out = build(&body, "amazon-nova-pro", "", None, None);
        assert!(!content_of(&current(&out)).contains("thinking"));
    }

    /// Anthropic 风格的显式预算要夹在 [1024, 50000]
    #[test]
    fn an_explicit_budget_is_clamped() {
        let low = json!({ "thinking": { "budget_tokens": 10 } });
        assert_eq!(thinking_budget(&low, "claude-x"), Some(1024));
        let high = json!({ "thinking": { "budget_tokens": 900000 } });
        assert_eq!(thinking_budget(&high, "claude-x"), Some(50_000));
    }

    /// response_format 折成一句自然语言
    #[test]
    fn response_format_becomes_a_sentence() {
        let body = json!({
            "messages": [{ "role": "user", "content": "给个 JSON" }],
            "response_format": { "type": "json_object" }
        });
        let out = build(&body, "auto", "", None, None);
        assert!(content_of(&current(&out)).contains("single JSON object"));
    }

    /// 工具调用 id 不合规要换成合法形态，且稳定
    #[test]
    fn illegal_tool_ids_are_hashed() {
        assert_eq!(tool_id("call_1"), "call_1");
        let weird = tool_id("调用/一号");
        assert!(is_tool_id(&weird), "{weird}");
        assert!(weird.starts_with("t_"));
        assert_eq!(weird, tool_id("调用/一号"));
    }
}
