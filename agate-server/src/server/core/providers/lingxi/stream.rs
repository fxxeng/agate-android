//! 私有 SSE 帧改写：把灵犀的 `choices[].text` 折成标准 `delta.content`。
//!
//! 三个参考实现一致给出的坑：
//!   · 正文在 `choices[].text`（不是 `delta.content`），当成 delta 会拿到空串；
//!   · 上游**不发 `[DONE]`**，靠 `finish_reason` 收尾，由网关补；
//!   · `usage.total_tokens` 是**当前上下文大小**，不是本次消耗，直接透传会让
//!     报表虚高几十倍 → 按 `prompt + completion` 重算。
//!
//! ⚠️ 内容审核的「HTTP 200 兜底话术」按公开资料只说了现象、没给固定文案。
//! 本模块**不按中文话术猜**（编一句白名单就是谎报），只认结构化的
//! `finish_reason = content_filter / refusal` 信号；真机抓到固定兜底文本后
//! 再补白名单，见方案 L6。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic。

use serde_json::{json, Map, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rewritten {
    pub chunk: Value,
    /// 该帧带 `finish_reason` ⇒ 调用方补 `[DONE]` 并收尾。
    pub finished: bool,
    /// 上游显式拒答（HTTP 200 假成功）⇒ 折成错误，**换号无用**。
    pub refused: bool,
}

fn is_refusal(finish: &str) -> bool {
    matches!(finish, "content_filter" | "refusal" | "content_filtered")
}

/// 按 `prompt + completion` 重算 usage。`cached_tokens` / `reasoning_tokens`
/// 在灵犀是扁平字段且方向与 OpenAI 相反，两个参考实现都这么说但没给映射表，
/// 所以这里**只保留我们自己算得准的两项**，其余不臆造。
pub fn recompute_usage(prompt: u64, completion: u64) -> Value {
    json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt.saturating_add(completion),
    })
}

/// 改写一帧。`None` = 这一帧没有可下发给客户端的内容（例如只有 `run.start`），
/// 调用方应继续读下一帧，**不能把一个空帧塞进流**（否则读侧会挂住，
/// `dsh-router-wpslingxi` README 里「pull 绝不能空手返回」记的就是这个坑）。
pub fn rewrite_frame(frame: &Value, prompt: u64, completion: u64) -> Option<Rewritten> {
    let choices = frame.get("choices").and_then(Value::as_array);
    let has_usage = frame.get("usage").is_some();
    if choices.is_none() && !has_usage {
        return None;
    }

    let mut out = Map::new();
    if let Some(id) = frame.get("id").and_then(Value::as_str) {
        out.insert("id".to_string(), Value::String(id.to_string()));
    }
    if let Some(model) = frame.get("model").and_then(Value::as_str) {
        out.insert("model".to_string(), Value::String(model.to_string()));
    }
    out.insert("object".to_string(), Value::String("chat.completion.chunk".to_string()));
    out.insert("created".to_string(), frame.get("created").cloned().unwrap_or(Value::Null));

    let mut finished = false;
    let mut refused = false;
    let mut rewritten = Vec::new();
    if let Some(list) = choices {
        for choice in list {
            let mut delta = Map::new();
            // 正文：先认 text，再兼容已经标准化过的 delta.content
            let body = choice
                .get("text")
                .and_then(Value::as_str)
                .or_else(|| choice.get("delta").and_then(|d| d.get("content")).and_then(Value::as_str));
            if let Some(text) = body {
                delta.insert("content".to_string(), Value::String(text.to_string()));
            }
            let reasoning = choice
                .get("reasoning_content")
                .and_then(Value::as_str)
                .or_else(|| choice.get("delta").and_then(|d| d.get("reasoning_content")).and_then(Value::as_str));
            if let Some(text) = reasoning {
                delta.insert("reasoning_content".to_string(), Value::String(text.to_string()));
            }
            if let Some(finish) = choice.get("finish_reason").and_then(Value::as_str) {
                finished = true;
                if is_refusal(finish) {
                    refused = true;
                }
                delta.insert("finish_reason".to_string(), Value::String(finish.to_string()));
            }
            if delta.is_empty() {
                continue;
            }
            rewritten.push(json!({
                "index": choice.get("index").and_then(Value::as_u64).unwrap_or(0),
                "delta": delta,
            }));
        }
    }
    if has_usage {
        out.insert("usage".to_string(), recompute_usage(prompt, completion));
    }
    if rewritten.is_empty() && !has_usage {
        return None;
    }
    out.insert("choices".to_string(), Value::Array(rewritten));
    Some(Rewritten {
        chunk: Value::Object(out),
        finished,
        refused,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_becomes_delta_content() {
        let r = rewrite_frame(
            &json!({ "id": "c1", "choices": [{ "index": 0, "text": "你好" }] }),
            10,
            2,
        );
        let r = r.unwrap_or_else(|| Rewritten { chunk: Value::Null, finished: false, refused: false });
        assert_eq!(r.chunk["choices"][0]["delta"]["content"], json!("你好"));
        assert!(!r.finished);
    }

    #[test]
    fn reasoning_delta_is_preserved() {
        let r = rewrite_frame(
            &json!({ "choices": [{ "reasoning_content": "让我想想", "text": "答案" }] }),
            0,
            0,
        );
        let r = r.unwrap_or_else(|| Rewritten { chunk: Value::Null, finished: false, refused: false });
        assert_eq!(r.chunk["choices"][0]["delta"]["reasoning_content"], json!("让我想想"));
        assert_eq!(r.chunk["choices"][0]["delta"]["content"], json!("答案"));
    }

    #[test]
    fn finish_reason_marks_the_end_and_no_done_is_needed_from_upstream() {
        let r = rewrite_frame(
            &json!({ "choices": [{ "finish_reason": "stop", "text": "" }] }),
            0,
            0,
        );
        let r = r.unwrap_or_else(|| Rewritten { chunk: Value::Null, finished: false, refused: false });
        assert!(r.finished, "上游不发 [DONE]，必须靠 finish_reason 收尾");
    }

    #[test]
    fn usage_is_recomputed_not_passed_through() {
        let r = rewrite_frame(
            &json!({ "choices": [], "usage": { "total_tokens": 999999 } }),
            120,
            30,
        );
        let r = r.unwrap_or_else(|| Rewritten { chunk: Value::Null, finished: false, refused: false });
        assert_eq!(r.chunk["usage"]["total_tokens"], json!(150));
        assert_eq!(r.chunk["usage"]["prompt_tokens"], json!(120));
    }

    #[test]
    fn empty_control_frames_do_not_produce_a_chunk() {
        assert!(rewrite_frame(&json!({ "event": "run.start" }), 0, 0).is_none());
    }

    #[test]
    fn content_filter_is_a_refusal_not_a_success() {
        let r = rewrite_frame(
            &json!({ "choices": [{ "finish_reason": "content_filter", "text": "" }] }),
            0,
            0,
        );
        let r = r.unwrap_or_else(|| Rewritten { chunk: Value::Null, finished: false, refused: false });
        assert!(r.refused);
    }
}
