//! 一次对话的三请求 body 构造（**纯函数，不发请求**）。
//!
//! 编排顺序（用户已拍板「run 生命周期上报默认开启」，见方案 §8）：
//! ```text
//!   1. POST sessions             → session_id
//!   2. POST credits/runs         → run_id（计费单元）
//!   3. PUT  execution_status     {running, lease_seconds:90}
//!   4. POST agent-runtime/chat/completions → 私有 SSE
//!   5. 流结束 PUT execution_status {completed}   ← 结算触发点，不能漏
//! ```
//!
//! ⚠️ ②的 body、`base_llm_arguments` / `llm_arguments` 的字段位置来自
//! `Coding2API` 的公开描述，**未真机实测**；方案 §1 把它列为 P0 探针必验项。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic。

use serde_json::{json, Map, Value};

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
                    // 数组型 type 取第一个可识别的标量
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

/// 把 OpenAI 形态的客户端请求折成灵犀三步骤所需的 body 集合。
pub fn build_bodies(request: &Value, model: &str, session_id: &str, run_id: &str) -> StepBodies {
    let messages = request.get("messages").cloned().unwrap_or(Value::Null);
    let stream = request.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let max_tokens = request
        .get("max_tokens")
        .or_else(|| request.get("max_completion_tokens"))
        .and_then(Value::as_u64);
    let effort = request
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut base = Map::new();
    if let Some(value) = max_tokens {
        base.insert("max_tokens".to_string(), json!(value));
    }
    let mut llm = Map::new();
    if let Some(value) = effort.as_ref() {
        llm.insert("reasoning_effort".to_string(), Value::String(value.clone()));
    }

    let mut chat = Map::new();
    chat.insert("session_id".to_string(), Value::String(session_id.to_string()));
    chat.insert("run_id".to_string(), Value::String(run_id.to_string()));
    chat.insert("model".to_string(), Value::String(model.to_string()));
    chat.insert("messages".to_string(), messages);
    chat.insert("stream".to_string(), Value::Bool(stream));
    chat.insert("base_llm_arguments".to_string(), Value::Object(base));
    chat.insert("llm_arguments".to_string(), Value::Object(llm));
    if let Some(tools) = request.get("tools") {
        chat.insert("tools".to_string(), flatten_tools(tools));
    }
    if let Some(value) = effort {
        chat.insert("reasoning_effort".to_string(), Value::String(value));
    }

    StepBodies {
        session: json!({}),
        run: json!({ "model": model }),
        chat: Value::Object(chat),
        running: json!({ "execution_status": "running", "lease_seconds": 90 }),
        completed: json!({ "execution_status": "completed" }),
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
    fn five_bodies_cover_the_whole_run_lifecycle() {
        let b = build_bodies(&json!({ "messages": [], "stream": true }), "qwen3.8_flash", "s1", "r1");
        assert_eq!(b.session, json!({}));
        assert_eq!(b.running["execution_status"], json!("running"));
        assert_eq!(b.running["lease_seconds"], json!(90));
        assert_eq!(b.completed["execution_status"], json!("completed"));
        assert_eq!(b.chat["session_id"], json!("s1"));
        assert_eq!(b.chat["run_id"], json!("r1"));
        assert_eq!(b.chat["stream"], json!(true));
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
            "r1",
        );
        assert_eq!(b.chat["base_llm_arguments"]["max_tokens"], json!(4096));
        assert_eq!(b.chat["llm_arguments"]["reasoning_effort"], json!("high"));
        assert_eq!(b.chat["reasoning_effort"], json!("high"));
        assert!(b.chat.get("max_tokens").is_none());
    }
}
