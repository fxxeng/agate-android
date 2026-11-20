//! Responses 助手消息的阶段标记（`phase`）：给第三方模型的回答补
//! `final_answer` / `commentary`，让客户端能折叠思考与工具过程。
//!
//! 纯函数，从上游 `protocol/responses_phase.rs` 逐字移植（判据是多家客户端实测
//! 归纳，不重新设计）。只在开关开启且严格识别为 Codex 时由 `responses.rs` 调用。
//!
//! 三条判据：
//! · 正常 `stop` 且无工具调用 → `final_answer`；有工具调用 → `commentary`。
//! · 截断（`length` / `max_length`）或内容过滤 → `incomplete`，不补最终答案。
//! · 空回答、错误响应、缺结束证据 → 不补（避免把半截回答标记成完成）。

use serde_json::Value;

/// 截断类 finish_reason → incomplete 原因（供响应信封的 status 用）。
pub(super) fn incomplete_reason(finish: Option<&str>) -> Option<&'static str> {
    let finish = finish?.trim();
    if crate::server::core::upstream::continuation::is_length_finish(finish)
        || finish.eq_ignore_ascii_case("max_length")
    {
        Some("max_output_tokens")
    } else if finish.eq_ignore_ascii_case("content_filter") {
        Some("content_filter")
    } else {
        None
    }
}

/// 这条助手消息该补哪个 phase（None = 不补）。
pub(super) fn message_phase(
    finish: Option<&str>,
    has_tool_calls: bool,
    completed: bool,
) -> Option<&'static str> {
    if !completed {
        return None;
    }
    let finish = finish?.trim();
    if finish.eq_ignore_ascii_case("stop") {
        Some(if has_tool_calls {
            "commentary"
        } else {
            "final_answer"
        })
    } else if has_tool_calls
        && (finish.eq_ignore_ascii_case("tool_calls")
            || finish.eq_ignore_ascii_case("function_call"))
    {
        Some("commentary")
    } else {
        None
    }
}

/// 响应体是否成功（错误 / 截断态不补 phase）。
pub(super) fn response_is_successful(body: &Value) -> bool {
    !body.get("error").is_some_and(super::is_truthy)
        && !body
            .get(super::FIELD_IS_ERROR)
            .is_some_and(super::is_truthy)
        && body
            .get("status")
            .is_none_or(|status| status.is_null() || status.as_str() == Some("completed"))
}

/// 把 phase 写进一个 output item（已有 phase 不覆盖；final_answer 要求正文非空）。
pub(super) fn apply(item: &mut Value, phase: Option<&str>) {
    let Some(phase) = phase else { return };
    if item.get("phase").is_some_and(|value| !value.is_null()) {
        return;
    }
    if phase == "final_answer"
        && super::content_text(item.get("content").unwrap_or(&Value::Null))
            .trim()
            .is_empty()
    {
        return;
    }
    if let Some(object) = item.as_object_mut() {
        object.insert("phase".into(), Value::String(phase.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normal_answer_gets_final_answer() {
        assert_eq!(
            message_phase(Some("stop"), false, true),
            Some("final_answer")
        );
    }

    #[test]
    fn tool_answer_gets_commentary() {
        assert_eq!(message_phase(Some("stop"), true, true), Some("commentary"));
        assert_eq!(
            message_phase(Some("tool_calls"), true, true),
            Some("commentary")
        );
    }

    #[test]
    fn incomplete_or_error_gets_nothing() {
        assert_eq!(message_phase(Some("length"), false, true), None);
        assert_eq!(message_phase(Some("stop"), false, false), None);
        assert_eq!(message_phase(None, false, true), None);
    }

    #[test]
    fn incomplete_reason_mapping() {
        assert_eq!(incomplete_reason(Some("length")), Some("max_output_tokens"));
        assert_eq!(
            incomplete_reason(Some("max_length")),
            Some("max_output_tokens")
        );
        assert_eq!(
            incomplete_reason(Some("content_filter")),
            Some("content_filter")
        );
        assert_eq!(incomplete_reason(Some("stop")), None);
    }

    #[test]
    fn apply_writes_phase_but_keeps_existing() {
        let mut item =
            json!({"type": "message", "content": [{"type": "output_text", "text": "hi"}]});
        apply(&mut item, Some("final_answer"));
        assert_eq!(item["phase"], "final_answer");
        apply(&mut item, Some("commentary"));
        assert_eq!(item["phase"], "final_answer"); // 不覆盖
    }

    #[test]
    fn apply_skips_final_answer_on_empty_text() {
        let mut item = json!({"type": "message", "content": []});
        apply(&mut item, Some("final_answer"));
        assert!(item.get("phase").is_none());
        apply(&mut item, Some("commentary"));
        assert_eq!(item["phase"], "commentary");
    }

    #[test]
    fn error_response_is_not_successful() {
        assert!(!response_is_successful(&json!({"error": {"message": "x"}})));
        assert!(!response_is_successful(&json!({"_wb_is_error": true})));
        assert!(response_is_successful(&json!({"status": "completed"})));
    }
}
