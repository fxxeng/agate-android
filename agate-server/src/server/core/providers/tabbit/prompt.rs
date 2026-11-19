//! 把 OpenAI 请求摊成 Tabbit 能吃的**一段纯文本**。
//!
//! 上游 completion 只收一个 `content` 字符串：没有 messages 数组、没有 role、
//! 没有原生工具调用。所以这里做三件事：
//! 1. 按角色加标记（`[系统指令]` / `[助手]`）让模型看得出对话结构；
//! 2. 按字符预算裁剪（实测 20000 字符成功、25000 必失败，预算取 16000）：
//!    系统指令优先，其余**从最近往回填**（近的上下文更有用，与各家的通行做法一致）；
//! 3. 明确**拒绝**工具声明与工具历史 —— 本适配器不做提示词模拟工具调用
//!    （Web 包口径，与 DeepSeek 网页一致）。
//!
//! 图片同样拒绝：v3 runs 的图片要走上传 + references，当前适配未接，
//! **不能假装看得见**（假装的结果是模型对着一串 URL 胡说）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::Value;

use crate::server::errors::GatewayError;

use super::{
    HISTORY_NOTE, PROMPT_BUDGET, PROMPT_HEAD, PROMPT_SINGLE, PROMPT_TAIL, SYSTEM_BUDGET, TRUNC_NOTE,
};

fn bad(message: impl Into<String>) -> GatewayError {
    GatewayError::with_status(400, message)
}

/// 一条消息的可读文本（字符串或文本内容块数组）。
fn message_text(content: Option<&Value>) -> Result<String, GatewayError> {
    let Some(content) = content else {
        return Ok(String::new());
    };
    match content {
        Value::Null => Ok(String::new()),
        Value::String(text) => Ok(text.trim().to_string()),
        Value::Array(parts) => {
            let mut chunks = Vec::new();
            for part in parts {
                let kind = part.get("type").and_then(Value::as_str).unwrap_or("text");
                match kind {
                    "text" | "input_text" | "output_text" => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            let text = text.trim();
                            if !text.is_empty() {
                                chunks.push(text.to_string());
                            }
                        }
                    }
                    "image" | "image_url" | "input_image" => {
                        return Err(bad(
                            "Tabbit 网页当前适配只支持文本，图片输入尚未接入（请移除图片或改用支持图片的提供商）",
                        ))
                    }
                    other => {
                        return Err(bad(format!(
                            "Tabbit 网页不支持内容块类型「{other}」，只接受文本"
                        )))
                    }
                }
            }
            Ok(chunks.join("\n"))
        }
        _ => Err(bad("Tabbit 消息内容必须是文本或文本内容块数组")),
    }
}

/// 一条消息 → 一段带角色标记的文本（空消息返回 None，直接跳过）
fn render_message(message: &Value) -> Result<Option<String>, GatewayError> {
    let role = message.get("role").and_then(Value::as_str).unwrap_or("user");
    if role == "tool" || role == "function" {
        return Err(bad(
            "Tabbit 网页没有原生工具调用，不接受工具结果历史（请移除 tools / tool 消息）",
        ));
    }
    if message
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty())
    {
        return Err(bad(
            "Tabbit 网页没有原生工具调用，不接受工具调用历史（请移除 tools / tool_calls）",
        ));
    }
    let text = message_text(message.get("content"))?;
    if text.is_empty() {
        return Ok(None);
    }
    let rendered = match role {
        "system" | "developer" => format!("[系统指令]\n{text}"),
        "assistant" => format!("[助手]\n{text}"),
        _ => text,
    };
    Ok(Some(rendered))
}

/// 按字符裁到 `limit`，保留头 + 尾（中间省略）。
fn clip_text(text: &str, limit: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return text.to_string();
    }
    let mut head = PROMPT_HEAD.min(limit.saturating_sub(200).max(1));
    if limit < 200 {
        head = (limit / 2).max(1);
    }
    let tail = PROMPT_TAIL.min(limit.saturating_sub(head + 60));
    let mut out = String::new();
    out.extend(chars.iter().take(head));
    out.push_str(TRUNC_NOTE);
    if tail > 0 {
        out.extend(chars.iter().skip(chars.len() - tail));
    }
    out
}

/// 系统指令优先，其余**从最近往回填**，放不下的整块丢弃。
fn join_within_budget(systems: Vec<String>, others: Vec<String>) -> String {
    let system_text = clip_text(&systems.join("\n\n"), SYSTEM_BUDGET);
    let mut used = system_text.chars().count();
    let mut kept_reversed: Vec<String> = Vec::new();
    let mut dropped = false;
    for block in others.iter().rev() {
        let block = clip_text(block, PROMPT_SINGLE);
        let length = block.chars().count();
        // +2 是块之间的两个换行
        if used + length + 2 > PROMPT_BUDGET {
            dropped = true;
            continue;
        }
        kept_reversed.push(block);
        used += length + 2;
    }
    let mut blocks = Vec::new();
    if !system_text.trim().is_empty() {
        blocks.push(system_text);
    }
    if dropped {
        blocks.push(HISTORY_NOTE.to_string());
    }
    blocks.extend(kept_reversed.into_iter().rev());
    blocks.join("\n\n")
}

/// 返回要送上游的 content。
pub fn build(body: &Value) -> Result<String, GatewayError> {
    // 工具声明：`tool_choice: "none"` 时客户端明确表示这次不用工具，可以继续；
    // 其余情况一律拒绝 —— 避免把一次「本应调工具」的请求静默降级成纯聊天。
    let has_tools = body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    let has_functions = body
        .get("functions")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    let choice = body.get("tool_choice").or_else(|| body.get("function_call"));
    let choice_none = choice
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("none"));
    if (has_tools || has_functions) && !choice_none {
        return Err(bad(
            "Tabbit 网页没有原生工具调用接口，请移除 tools / functions，或改用支持工具调用的提供商",
        ));
    }
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("Tabbit 请求缺少 messages"))?;
    if messages.is_empty() {
        return Err(bad("Tabbit messages 不能为空"));
    }
    // 单条 user 消息直接原文发出：最省 token，也最接近正常网页对话
    if messages.len() == 1 && messages[0].get("role").and_then(Value::as_str) == Some("user") {
        let text = message_text(messages[0].get("content"))?;
        if !text.trim().is_empty() {
            return Ok(text);
        }
    }
    let mut systems = Vec::new();
    let mut others = Vec::new();
    for message in messages {
        let Some(rendered) = render_message(message)? else {
            continue;
        };
        let role = message.get("role").and_then(Value::as_str).unwrap_or("user");
        if matches!(role, "system" | "developer") {
            systems.push(rendered);
        } else {
            others.push(rendered);
        }
    }
    let text = join_within_budget(systems, others);
    if text.trim().is_empty() {
        return Err(bad("Tabbit messages 里没有可发送的文本内容"));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 单条 user 消息原样发出（不加标记、不裁剪）
    #[test]
    fn single_user_message_is_sent_verbatim() {
        let text = build(&json!({ "messages": [{ "role": "user", "content": "你好" }] }))
            .expect("单条消息应能构建");
        assert_eq!(text, "你好");
    }

    /// 多轮：system 进 `[系统指令]`，assistant 进 `[助手]`，user 保持原文
    #[test]
    fn roles_are_marked_in_multi_turn() {
        let text = build(&json!({
            "messages": [
                { "role": "system", "content": "你是助手" },
                { "role": "user", "content": "问题一" },
                { "role": "assistant", "content": "回答一" },
                { "role": "user", "content": "问题二" }
            ]
        }))
        .expect("多轮应能构建");
        assert!(text.starts_with("[系统指令]\n你是助手"));
        assert!(text.contains("[助手]\n回答一"));
        assert!(text.ends_with("问题二"), "{text}");
    }

    /// 内容块数组里的文本块要拼接；图片块必须**明确拒绝**（不假装看得见）
    #[test]
    fn content_parts_are_joined_but_images_are_refused() {
        let text = build(&json!({
            "messages": [{ "role": "user", "content": [
                { "type": "text", "text": "第一段" },
                { "type": "text", "text": "第二段" }
            ]}]
        }))
        .expect("纯文本块应能构建");
        assert_eq!(text, "第一段\n第二段");

        let error = build(&json!({
            "messages": [{ "role": "user", "content": [
                { "type": "image_url", "image_url": { "url": "https://example.com/a.png" } }
            ]}]
        }))
        .expect_err("图片必须拒绝");
        assert_eq!(error.status_code, 400);
        assert!(error.message.contains("只支持文本"));
    }

    /// 带 tools 且没声明 `tool_choice: none` → 拒绝（不静默降级）
    #[test]
    fn tool_declarations_are_refused_unless_explicitly_disabled() {
        let with_tools = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "type": "function", "function": { "name": "f" } }]
        });
        assert!(build(&with_tools).is_err());

        let mut disabled = with_tools.clone();
        disabled["tool_choice"] = json!("none");
        assert!(build(&disabled).is_ok(), "tool_choice=none 时应放行");
    }

    /// 工具历史（role=tool 或带 tool_calls 的 assistant）→ 拒绝
    #[test]
    fn tool_history_is_refused() {
        let with_tool_role = json!({
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "tool", "content": "结果" }
            ]
        });
        assert!(build(&with_tool_role).is_err());

        let with_calls = json!({
            "messages": [
                { "role": "assistant", "content": "", "tool_calls": [{ "id": "1" }] }
            ]
        });
        assert!(build(&with_calls).is_err());
    }

    /// 超预算时：保留最近的若干轮，并插入「历史已省略」的说明
    #[test]
    fn history_is_dropped_from_the_far_end_and_announced() {
        let long = "字".repeat(PROMPT_SINGLE);
        let mut messages = Vec::new();
        for index in 0..8 {
            messages.push(json!({ "role": "user", "content": format!("{index}:{long}") }));
        }
        let text = build(&json!({ "messages": messages })).expect("应能构建");
        assert!(text.chars().count() <= PROMPT_BUDGET + 200, "总长度应受控");
        assert!(text.contains(HISTORY_NOTE), "丢弃历史必须有说明");
        // 最近一轮必须还在
        assert!(text.contains("7:"), "最近一轮不能被丢");
        // 最早一轮必须被丢
        assert!(!text.contains("0:"), "最早一轮应被丢弃");
    }

    /// 超长单条消息：头尾保留、中间省略，且带裁剪标记
    #[test]
    fn oversized_single_message_keeps_head_and_tail() {
        let text = clip_text(&format!("HEAD{}TAIL", "x".repeat(PROMPT_SINGLE)), PROMPT_SINGLE);
        assert!(text.starts_with("HEAD"));
        assert!(text.ends_with("TAIL"));
        assert!(text.contains(TRUNC_NOTE.trim()));
        assert!(text.chars().count() <= PROMPT_SINGLE + TRUNC_NOTE.chars().count());
    }

    /// 一条可发送的内容都没有 → 400（不是发一条空 prompt 换上游 400）
    #[test]
    fn empty_after_render_is_a_readable_400() {
        let error = build(&json!({
            "messages": [
                { "role": "system", "content": "   " },
                { "role": "user", "content": "" }
            ]
        }))
        .expect_err("空内容必须报错");
        assert_eq!(error.status_code, 400);
        assert!(error.message.contains("没有可发送的文本"));
    }
}
