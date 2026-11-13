//! 一次对话的三请求 body 构造（**纯函数，不发请求**）。
//!
//! ⚠️ **2026-11-13 校正**：第一批是按 README 的语义描述猜的 body。逐项复核
//! `ketion/lingxi2api-panel::internal/upstream/lingxi.go` 与
//! `RobbsLuo/Coding2API::src/provider/lingxi/client.py` 源码后全部对齐：
//!   · 建会话 body 不是 `{}`（要 `session_type` / `agent_version` / `env`）；
//!   · 领 run body 不是 `{"model"}`（要 `client_request_id` / `session_id` /
//!     `model_provider` / `model_id` / `agent_input`）；
//!   · **`session_id` / `run_id` 在请求头 `x-cc-session-id` / `x-cc-run-id`，
//!     不在 chat body 里**（`endpoints::chat_headers`）；
//!   · `execution_status` 的字段是 `group_id` / `status` / `run_token` /
//!     `lease_seconds` / `terminal_reason`，不是 `execution_status`。
//!
//! 编排顺序（用户已拍板「run 生命周期上报默认开启」）：
//! ```text
//!   1. POST sessions             → session_id
//!   2. POST credits/runs         → run_id + group_id（计费单元）
//!   3. PUT  execution_status     {status:running, lease_seconds:90}
//!   4. POST agent-runtime/chat/completions → 私有 SSE
//!   5. 流结束 PUT execution_status {status:completed}   ← 结算触发点
//! ```
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic。

use serde_json::{json, Map, Value};

use super::endpoints;

/// 结算口径：默认开启，与官方桌面客户端一致；不沿用参考项目的
/// `report_lifecycle=false`。做成常量函数而不是散落各处的布尔字面量。
pub const fn report_lifecycle() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq)]
pub struct StepBodies {
    pub session: Value,
    pub run: Value,
    pub chat: Value,
    pub running: Value,
    pub completed: Value,
}

/// 工具 schema 压平：上游见到 `anyOf/oneOf/allOf/$ref/数组型 type` 会让整轮
/// 请求回「AI服务异常」。判据与 `protocol/antigravity_schema.rs` 同源，这里只
/// 在本家出站启用，不改公共行为。
pub fn flatten_tools(value: &Value) -> Value {
    const FORBIDDEN: &[&str] = &[
        "$ref", "anyOf", "oneOf", "allOf", "not", "definitions", "$defs", "dependencies",
        "dependentSchemas", "dependentRequired", "$schema", "$comment",
    ];
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, child) in map {
                if FORBIDDEN.contains(&key.as_str()) {
                    continue;
                }
                if key == "type" {
                    let normalized = match child {
                        Value::Array(items) => items
                            .iter()
                            .find_map(|item| item.as_str())
                            .map(|text| Value::String(text.to_string()))
                            .unwrap_or(Value::Null),
                        other => other.clone(),
                    };
                    if !normalized.is_null() {
                        out.insert(key.clone(), normalized);
                    }
                    continue;
                }
                out.insert(key.clone(), flatten_tools(child));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(flatten_tools).collect()),
        other => other.clone(),
    }
}

/// 丢掉**孤立 tool 消息**（前面没有紧邻声明其 id 的 `assistant.tool_calls`）。
/// 上游对这类历史一律 400（两个参考实现同口径）；只丢弃、不重排、不改 content。
pub fn normalize_messages(messages: &Value) -> Value {
    let Some(items) = messages.as_array() else {
        return Value::Array(Vec::new());
    };
    let mut declared: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<Value> = Vec::new();
    for item in items {
        let Some(object) = item.as_object() else { continue };
        let role = object.get("role").and_then(Value::as_str).unwrap_or("");
        if role == "assistant" {
            declared.clear();
            if let Some(calls) = object.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    if let Some(id) = call.get("id").and_then(Value::as_str) {
                        declared.insert(id.to_string());
                    }
                }
            }
        } else if role == "tool" {
            let id = object.get("tool_call_id").and_then(Value::as_str).unwrap_or("");
            if id.is_empty() || !declared.remove(id) {
                continue;
            }
        } else {
            declared.clear();
        }
        out.push(item.clone());
    }
    Value::Array(out)
}

/// 最后一条用户消息的文本（领 run 时给上游做任务摘要，截 60 字）。
pub fn last_user_text(messages: &Value) -> String {
    let text = messages
        .as_array()
        .and_then(|items| {
            items.iter().rev().find(|item| {
                item.get("role").and_then(Value::as_str) == Some("user")
            })
        })
        .and_then(|item| item.get("content"))
        .map(|content| match content {
            Value::String(text) => text.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        })
        .unwrap_or_default();
    text.chars().take(60).collect()
}

/// ① 建 cowork 会话（两个实现逐字一致）。
pub fn session_body(device_id: &str) -> Value {
    json!({
        "session_type": "cowork_client",
        "agent_version": endpoints::AGENT_VERSION,
        "env": {
            "device_id": device_id,
            "device_name": "agate-android",
            "work_space_dir": "/tmp",
        },
    })
}

/// ② 领 credits run。`group_id` 由响应给，缺省回落 `run_id`。
pub fn run_body(session_id: &str, model: &str, messages: &Value, client_request_id: &str) -> Value {
    json!({
        "client_request_id": client_request_id,
        "session_id": session_id,
        "model_provider": "lingxi",
        "model_id": model,
        "agent_input": {
            "text": last_user_text(messages),
            "files": [],
            "file_ids": [],
            "expect_model": model,
            "refs": [],
            "permission_mode": "default",
            "current_mode": "agent",
            "browser_plugin_enabled": false,
        },
    })
}

/// ④ 对话 body。⚠️ 不放 `session_id` / `run_id`（那两个在头里）。
pub fn chat_body(request: &Value, model: &str) -> Value {
    let mut base = Map::new();
    if let Some(max) = request
        .get("max_tokens")
        .or_else(|| request.get("max_completion_tokens"))
        .and_then(Value::as_u64)
    {
        base.insert("max_tokens".to_string(), json!(max));
    }

    let mut chat = Map::new();
    chat.insert("stream".to_string(), Value::Bool(true));
    chat.insert("model".to_string(), Value::String(model.to_string()));
    chat.insert(
        "messages".to_string(),
        normalize_messages(request.get("messages").unwrap_or(&Value::Null)),
    );
    if let Some(tools) = request.get("tools").filter(|value| value.is_array()) {
        chat.insert("tools".to_string(), flatten_tools(tools));
        if let Some(choice) = request.get("tool_choice").and_then(Value::as_str) {
            chat.insert("tool_choice".to_string(), Value::String(choice.to_string()));
        }
    }
    if let Some(temp) = request.get("temperature").and_then(Value::as_f64) {
        chat.insert("temperature".to_string(), json!(temp));
    }
    if !base.is_empty() {
        chat.insert("base_llm_arguments".to_string(), Value::Object(base));
    }
    if let Some(effort) = request.get("reasoning_effort").and_then(Value::as_str) {
        chat.insert("reasoning_effort".to_string(), Value::String(effort.to_string()));
        let mut llm = Map::new();
        llm.insert("reasoning_effort".to_string(), Value::String(effort.to_string()));
        chat.insert("llm_arguments".to_string(), Value::Object(llm));
    }
    Value::Object(chat)
}

/// ③/⑤ run 终态上报。`status` 只认 `"running"` / `"completed"`，
/// 其余值不补 lease/terminal 字段（不给上游喂没依据的形状）。
pub fn execution_body(group_id: &str, status: &str) -> Value {
    let mut body = Map::new();
    body.insert("group_id".to_string(), Value::String(group_id.to_string()));
    body.insert("status".to_string(), Value::String(status.to_string()));
    body.insert("run_token".to_string(), Value::String(group_id.to_string()));
    if status == "running" {
        body.insert("lease_seconds".to_string(), json!(90));
    }
    if status == "completed" {
        body.insert("terminal_reason".to_string(), Value::String("completed".to_string()));
    }
    Value::Object(body)
}

/// 五份 body 的聚合视图（编排与单测共用同一套构造，避免两处各拼一份）。
pub fn build_bodies(
    request: &Value,
    model: &str,
    session_id: &str,
    group_id: &str,
    device_id: &str,
) -> StepBodies {
    StepBodies {
        session: session_body(device_id),
        run: run_body(session_id, model, request.get("messages").unwrap_or(&Value::Null), ""),
        chat: chat_body(request, model),
        running: execution_body(group_id, "running"),
        completed: execution_body(group_id, "completed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lifecycle_reporting_is_on_by_default() {
        assert!(report_lifecycle());
    }

    #[test]
    fn five_bodies_match_the_two_public_implementations() {
        let b = build_bodies(&json!({ "messages": [], "stream": true }), "qwen3.8_flash", "s1", "g1", "dev-1");
        assert_eq!(b.session["session_type"], json!("cowork_client"));
        assert_eq!(b.session["agent_version"], json!(2));
        assert_eq!(b.session["env"]["device_id"], json!("dev-1"));
        assert_eq!(b.run["session_id"], json!("s1"));
        assert_eq!(b.run["model_provider"], json!("lingxi"));
        assert_eq!(b.run["model_id"], json!("qwen3.8_flash"));
        assert_eq!(b.running["group_id"], json!("g1"));
        assert_eq!(b.running["status"], json!("running"));
        assert_eq!(b.running["run_token"], json!("g1"));
        assert_eq!(b.running["lease_seconds"], json!(90));
        assert_eq!(b.completed["status"], json!("completed"));
        assert_eq!(b.completed["terminal_reason"], json!("completed"));
    }

    #[test]
    fn session_and_run_ids_live_in_headers_not_the_chat_body() {
        let b = build_bodies(&json!({ "messages": [], "stream": true }), "glm-5.3", "s1", "g1", "dev-1");
        assert!(b.chat.get("session_id").is_none(), "session_id 不该进 body");
        assert!(b.chat.get("run_id").is_none(), "run_id 不该进 body");
        assert_eq!(b.chat["stream"], json!(true));
        assert_eq!(b.chat["model"], json!("glm-5.3"));
    }

    #[test]
    fn forbidden_schema_keys_are_stripped_only_from_outbound_tools() {
        let tools = json!([{
            "type": "function",
            "function": {
                "name": "lookup",
                "parameters": {
                    "type": ["object", "null"],
                    "properties": { "q": { "$ref": "#/$defs/Q", "type": "string" } },
                    "anyOf": [{ "type": "string" }, { "type": "number" }]
                }
            }
        }]);
        let flat = flatten_tools(&tools);
        let params = &flat[0]["function"]["parameters"];
        assert_eq!(params["type"], json!("object"));
        assert!(params.get("anyOf").is_none());
        assert!(params["properties"]["q"].get("$ref").is_none());
        assert_eq!(params["properties"]["q"]["type"], json!("string"));
    }

    #[test]
    fn max_tokens_and_effort_go_to_the_documented_positions() {
        let b = build_bodies(
            &json!({ "messages": [], "max_tokens": 4096, "reasoning_effort": "high" }),
            "glm-5.3",
            "s1",
            "g1",
            "dev-1",
        );
        assert_eq!(b.chat["base_llm_arguments"]["max_tokens"], json!(4096));
        assert_eq!(b.chat["llm_arguments"]["reasoning_effort"], json!("high"));
        assert_eq!(b.chat["reasoning_effort"], json!("high"));
        assert!(b.chat.get("max_tokens").is_none());
    }

    #[test]
    fn orphan_tool_messages_are_dropped() {
        let messages = json!([
            { "role": "user", "content": "hi" },
            { "role": "tool", "tool_call_id": "orphan", "content": "x" },
            { "role": "assistant", "tool_calls": [{ "id": "c1" }] },
            { "role": "tool", "tool_call_id": "c1", "content": "ok" },
            { "role": "tool", "tool_call_id": "c1", "content": "dup" }
        ]);
        let out = normalize_messages(&messages);
        assert_eq!(out.as_array().map(Vec::len), Some(3));
        assert_eq!(out[1]["tool_calls"][0]["id"], json!("c1"));
        assert_eq!(out[2]["tool_call_id"], json!("c1"));
    }

    #[test]
    fn run_summary_uses_the_last_user_text() {
        let messages = json!([
            { "role": "user", "content": "first" },
            { "role": "assistant", "content": "ok" },
            { "role": "user", "content": "第二句要紧" }
        ]);
        assert_eq!(last_user_text(&messages), "第二句要紧");
    }
}
