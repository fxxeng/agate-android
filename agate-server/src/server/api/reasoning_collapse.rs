//! 第三方模型回答完自动折叠过程开关：`GET/PUT /api/reasoning-collapse`。
//!
//! 仅对严格识别为 Codex 的 Responses 请求补阶段标记（见
//! `core::protocol::responses_phase`）：正常结束且无工具调用补 `final_answer`，
//! 工具调用前补 `commentary`。显示规则与来源声明不参与判断。
//!
//! 形态与 `/api/developer-role-compat` 一致（GET 读 / PUT 写、单开关）。

use axum::body::Bytes;
use axum::extract::State;
use axum::response::Response;
use serde_json::{json, Value};

use crate::server::config;
use crate::server::errors;
use crate::server::http::{ok_json, parse_body};
use crate::server::logging;
use crate::server::ServerState;

/// GET /api/reasoning-collapse —— 当前开关状态
pub async fn get_reasoning_collapse(State(_state): State<ServerState>) -> Response {
    ok_json(reasoning_collapse_json())
}

/// PUT /api/reasoning-collapse —— body `{thirdPartyReasoningCollapse: true|false}`
pub async fn put_reasoning_collapse(State(_state): State<ServerState>, body: Bytes) -> Response {
    let payload = match parse_body(&body) {
        Ok(value) => value,
        Err(error) => return errors::management_error(400, error.message),
    };
    let Some(object) = payload.as_object() else {
        return errors::management_error(400, "请求体必须是 JSON 对象");
    };
    let key = config::KEY_THIRD_PARTY_REASONING_COLLAPSE;
    let Some(value) = object.get(key) else {
        return errors::management_error(400, format!("缺少 {key}"));
    };
    let Some(enabled) = value.as_bool() else {
        return errors::management_error(400, format!("{key} 必须是 true 或 false"));
    };
    if !config::set_third_party_reasoning_collapse(enabled) {
        logging::log(
            "[Config]",
            "⚠️  第三方模型回答完自动折叠过程设置写入失败，本次运行内仍生效",
        );
    }
    logging::log(
        "[Config]",
        if enabled {
            "第三方模型回答完自动折叠过程已开启：Responses 助手消息补 final_answer / commentary"
        } else {
            "第三方模型回答完自动折叠过程已关闭"
        },
    );
    ok_json(reasoning_collapse_json())
}

/// 开关状态（GET 与 PUT 共用，前端直接用响应刷新界面）
fn reasoning_collapse_json() -> Value {
    json!({
        config::KEY_THIRD_PARTY_REASONING_COLLAPSE:
            config::current().third_party_reasoning_collapse(),
    })
}
