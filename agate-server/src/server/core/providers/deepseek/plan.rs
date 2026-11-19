//! DeepSeek 网页版请求体构造：**多轮 messages 拍平成单条 prompt**。
//!
//! ── 上游为什么只收一条 prompt（本家与 ChatGLM 同一处境）────────
//! 网页版的 `chat/completion` 收的是 `prompt: <字符串>` —— 那不是
//! 「OpenAI messages 数组」，而是**一条用户输入**。它自己在服务端维护会话
//! （多轮靠 `chat_session_id` + `parent_message_id` 串接，见 `chat.rs`）。
//!
//! 因此本机这一侧必须把客户端发来的 messages 数组**折成一条文本**，
//! 与 ChatGLM 那次「实测上游只认最后一条 user 消息」是同类问题的不同解法：
//! - ChatGLM 的做法是**只留最后一条**（因为实测历史上游根本不读，见
//!   `chatglm/plan.rs` 的模块头：原生数组问「我刚才说的数字」答「不知道」）；
//! - 本家这里选择**全部拍平进一条**（带角色标记）：网页版会把这一条当作
//!   用户输入存进会话，标记能保住「哪句话是谁说的」，丢掉历史反而会
//!   破坏客户端侧的多轮语义（本机网关是无状态转发，每次请求都自带全部历史）。
//!
//! ── 拍平的格式（人可读、模型可懂）────────────────────────────
//! ```text
//!   【系统】
//!   <system / developer 的内容>
//!   【用户】
//!   <user 的内容>
//!   【助手】
//!   <assistant 的内容>
//!   （末尾恒追加一次【用户】，因为接下来轮到模型说话）
//! ```
//! 与 ChatGLM 用 `<|user|>` 那种**模型专有特殊标记**不同：本家拍平后的文本
//! 会**作为用户输入存进上游会话记录**，模型看到的就是这段话。用【】中文方括号
//! 而不是 `<|…|>`，是为了避免撞上模型自己的控制符（DeepSeek 用
//! `<|begin▁of▁sentence|>` 一类），撞上会污染生成。
//!
//! ── 单轮请求原样发（不拍平）──────────────────────────────────
//! 只有一条用户输入、没有历史、没有 system 时**原样发那串文本** ——
//! 拍平后的 `【用户】\n你好\n【用户】\n` 会直接把标记存进用户的网页版对话记录里
//! （用户会在 chat.deepseek.com 的会话列表里看到它们）。单轮请求没有任何
//! 历史要表达，加上标记纯属污染。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::protocol::{string_field, json_text};

/// 拍平后的文本上限（超出即截断后续消息）。
///
/// 网页版的上下文窗口是 128K（pro 档 256K），这个上限远低于它 ——
/// 目的是拦住「历史被无限堆进一条 prompt」这种病态输入，而不是卡到窗口边界。
const MAX_FLAT_CHARS: usize = 120_000;

/// 角色标记（见模块头：刻意避开模型自己的控制符）
const MARK_SYSTEM: &str = "【系统】";
const MARK_USER: &str = "【用户】";
const MARK_ASSISTANT: &str = "【助手】";

/// 从客户端请求体里读多轮消息。
///
/// 两种形态都吃：`messages`（Chat Completions）优先；没有时退回
/// `prompt` / `input`（Responses 与简化客户端会这么发）。
pub fn read_messages(body: &Value) -> Vec<Value> {
    if let Some(list) = body.get("messages").and_then(Value::as_array) {
        if !list.is_empty() {
            return list.clone();
        }
    }
    for key in ["input", "prompt"] {
        match body.get(key) {
            Some(Value::String(text)) if !text.trim().is_empty() => {
                return vec![json!({ "role": "user", "content": text })];
            }
            Some(Value::Array(items)) => {
                let mut out: Vec<Value> = Vec::new();
                for item in items {
                    if let Some(text) = item.as_str() {
                        out.push(json!({ "role": "user", "content": text }));
                    } else if item.is_object() {
                        out.push(item.clone());
                    }
                }
                if !out.is_empty() {
                    return out;
                }
            }
            _ => {}
        }
    }
    Vec::new()
}

/// 一条消息的内容 → 文本。
///
/// content 可能是字符串（常规）或**内容块数组**（多模态客户端）：
/// ```text
///   "你好"
///   [{"type":"text","text":"你好"},{"type":"image_url",...}]
/// ```
/// 本家**没有**图片通道（见 `chat.rs`：二进制里找不到上传路径），
/// 因此非文本块在这里**折成一句可读的缺失说明**而不是静默丢弃 ——
/// 静默丢弃会让模型收到一段「凭空少了一句」的输入，答非所问且无从排查。
fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(items) => {
            let mut out = String::new();
            for item in items {
                if let Some(text) = item.as_str() {
                    out.push_str(text);
                    continue;
                }
                let kind = string_field(item, "type").to_lowercase();
                if kind == "text" || kind.is_empty() {
                    out.push_str(&json_text(item.get("text").unwrap_or(&Value::Null)));
                } else if kind.contains("image") {
                    // 如实告诉模型：这一侧丢了一张图（不假装图不存在）
                    out.push_str("[图片：本网关的 DeepSeek 网页版通道不支持图片，已省略]");
                } else if kind.contains("file") {
                    out.push_str("[文件：本网关的 DeepSeek 网页版通道不支持文件，已省略]");
                }
            }
            out
        }
        Value::Null => String::new(),
        other => json_text(other),
    }
}

/// 只有一段用户输入时给出的**原样文本**（否则 `None`，交给 [`flatten_messages`]）。
///
/// 判据：跳过空白消息之后剩下的必须**恰好一条**，且 role 不是
/// `system` / `developer` / `assistant` —— 那三种在拍平里带语义，省掉标记不成立。
fn plain_single_message(messages: &[Value]) -> Option<String> {
    let mut found: Option<String> = None;
    for message in messages {
        let text = content_text(message.get("content").unwrap_or(&Value::Null));
        if text.trim().is_empty() {
            continue;
        }
        if found.is_some() {
            return None;
        }
        let role = string_field(message, "role");
        if matches!(role.as_str(), "system" | "developer" | "assistant") {
            return None;
        }
        found = Some(text);
    }
    found
}

/// 拍平一批 messages 成一条 prompt（保序、带角色标记）。
///
/// 空消息跳过；超过 [`MAX_FLAT_CHARS`] 即停止追加（后续消息不再进 prompt）。
/// 末尾恒追加一次用户标记（接下来轮到模型说话）。
pub fn flatten_messages(messages: &[Value]) -> String {
    let mut out = String::new();
    for message in messages {
        let role = string_field(message, "role");
        let text = content_text(message.get("content").unwrap_or(&Value::Null));
        if text.trim().is_empty() {
            continue;
        }
        if out.chars().count() >= MAX_FLAT_CHARS {
            break;
        }
        let mark = match role.as_str() {
            "system" | "developer" => MARK_SYSTEM,
            "assistant" => MARK_ASSISTANT,
            // user 与一切未知 role（tool 等）都当作用户侧输入：
            // 上游没有工具消息的概念，丢掉会让上下文断裂，当作输入最不容易出错
            _ => MARK_USER,
        };
        out.push_str(mark);
        out.push('\n');
        out.push_str(&text);
        out.push('\n');
    }
    out.push_str(MARK_USER);
    out.push('\n');
    out
}

/// 一次请求的 prompt（对本家的最终输入）。
pub fn build_prompt(body: &Value) -> String {
    let messages = read_messages(body);
    match plain_single_message(&messages) {
        Some(text) => text,
        None => flatten_messages(&messages),
    }
}

/// 从请求体里读「思考 / 联网」两个开关。
///
/// 客户端可能用几种方式表达：`thinking` / `reasoning_effort` / `web_search`
/// / `search`。本实现认这几种（上游只有 `thinking_enabled` / `search_enabled`
/// 两个布尔），读不到就 false。
pub fn thinking_enabled(body: &Value) -> bool {
    if let Some(flag) = body.get("thinking").and_then(Value::as_bool) {
        return flag;
    }
    // reasoning_effort 非 "off"/"none" 视为开
    if let Some(effort) = body
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase)
    {
        return !matches!(effort.as_str(), "" | "off" | "none" | "disabled");
    }
    body.get("enable_thinking")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// 联网搜索开关（同上，认几种常见字段名）
pub fn search_enabled(body: &Value) -> bool {
    for key in ["web_search", "search", "search_enabled", "enable_search"] {
        if let Some(flag) = body.get(key).and_then(Value::as_bool) {
            return flag;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_user_message_is_sent_verbatim() {
        // 单轮请求不得带标记（否则会污染用户在网页版的对话记录）
        let body = json!({ "messages": [{ "role": "user", "content": "你好" }] });
        assert_eq!(build_prompt(&body), "你好");
    }

    #[test]
    fn multi_turn_is_flattened_with_marks() {
        let body = json!({ "messages": [
            { "role": "system", "content": "你是助手" },
            { "role": "user", "content": "1+1" },
            { "role": "assistant", "content": "2" },
            { "role": "user", "content": "再加一" }
        ]});
        let prompt = build_prompt(&body);
        assert!(prompt.contains("【系统】\n你是助手"));
        assert!(prompt.contains("【用户】\n1+1"));
        assert!(prompt.contains("【助手】\n2"));
        assert!(prompt.ends_with("【用户】\n"), "末尾应追加用户标记");
    }

    #[test]
    fn system_only_message_is_flattened_not_sent_verbatim() {
        // 只有 system、没有 user → 走拍平（原样发会丢掉「这是系统指令」的语义）
        let body = json!({ "messages": [{ "role": "system", "content": "只回 JSON" }] });
        let prompt = build_prompt(&body);
        assert!(prompt.starts_with("【系统】"));
    }

    #[test]
    fn image_blocks_are_declared_not_dropped_silently() {
        let body = json!({ "messages": [{ "role": "user", "content": [
            { "type": "text", "text": "这是什么" },
            { "type": "image_url", "image_url": { "url": "data:..." } }
        ]}]});
        let prompt = build_prompt(&body);
        assert!(prompt.contains("这是什么"));
        // 不静默丢：明确写出「图片被省略」（否则模型答非所问且无从排查）
        assert!(prompt.contains("不支持图片"));
    }

    #[test]
    fn blank_messages_are_skipped() {
        let body = json!({ "messages": [
            { "role": "user", "content": "   " },
            { "role": "user", "content": "真实内容" }
        ]});
        assert_eq!(build_prompt(&body), "真实内容");
    }

    #[test]
    fn prompt_field_is_accepted_as_fallback() {
        let body = json!({ "prompt": "直接给的提示" });
        assert_eq!(build_prompt(&body), "直接给的提示");
    }

    #[test]
    fn input_array_is_accepted_as_fallback() {
        let body = json!({ "input": ["甲", "乙"] });
        let prompt = build_prompt(&body);
        assert!(prompt.contains("甲"));
        assert!(prompt.contains("乙"));
    }

    #[test]
    fn empty_body_yields_mark_only() {
        let body = json!({});
        // 没有消息时给一条空拍平（末尾标记），不是 panic
        assert_eq!(build_prompt(&body), "【用户】\n");
    }

    #[test]
    fn thinking_flag_reads_several_shapes() {
        assert!(thinking_enabled(&json!({ "thinking": true })));
        assert!(thinking_enabled(&json!({ "reasoning_effort": "high" })));
        assert!(!thinking_enabled(&json!({ "reasoning_effort": "off" })));
        assert!(!thinking_enabled(&json!({})));
    }

    #[test]
    fn search_flag_reads_several_shapes() {
        assert!(search_enabled(&json!({ "web_search": true })));
        assert!(search_enabled(&json!({ "search_enabled": true })));
        assert!(!search_enabled(&json!({ "search": false })));
        assert!(!search_enabled(&json!({})));
    }

    #[test]
    fn tool_role_messages_are_kept_as_user_input() {
        // tool 结果丢掉会让上下文断裂 —— 当作用户侧输入
        let body = json!({ "messages": [
            { "role": "user", "content": "查天气" },
            { "role": "tool", "content": "北京 晴 25°C" },
            { "role": "user", "content": "那要带伞吗" }
        ]});
        let prompt = build_prompt(&body);
        assert!(prompt.contains("北京 晴 25°C"), "工具结果不得丢");
    }

    #[test]
    fn very_long_history_is_capped() {
        let big = "字".repeat(70_000);
        let body = json!({ "messages": [
            { "role": "user", "content": big },
            { "role": "user", "content": big }
        ]});
        let prompt = build_prompt(&body);
        // 超过上限即停止追加（不无限堆）
        assert!(prompt.chars().count() < 200_000);
    }
}