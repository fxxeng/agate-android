//! 图片兼容配置：`GET/PUT /api/vision-compat`。
//!
//! ── 一条端点回答什么 ────────────────────────────────────────
//! · `GET`：当前配置 + 修订号 + 各项上限。界面据此渲染规则表与可选值。
//! · `PUT`：整份替换（带 `revision` 防并发覆盖）。**不合并**：规则是有序的，
//!   第一条命中即止，合并会让优先级取决于提交顺序而不是用户排的顺序。
//!
//! 与 `/api/prompt-rules` 同形（整份规则表 + revision），复用同一套读写心智。

use axum::body::Bytes;
use axum::response::Response;
use serde::Deserialize;
use serde_json::json;

use crate::server::config::vision_compat::{self as config, VisionSettings};
use crate::server::errors::{self, GatewayError};
use crate::server::http::{ok_json, parse_body};
use crate::server::logging;

fn view() -> Result<serde_json::Value, GatewayError> {
    let snapshot = config::current();
    let settings = snapshot
        .settings
        .as_ref()
        .map_err(|error| GatewayError::new(error.clone()))?;
    Ok(json!({
        "config": settings,
        "revision": snapshot.revision,
        "limits": {
            "rules": config::MAX_RULES,
            "models": config::MAX_MODELS,
            "targets": config::MAX_TARGETS,
            "maxTimeoutSeconds": config::MAX_TIMEOUT_SECONDS,
        },
    }))
}

fn response(result: Result<serde_json::Value, GatewayError>) -> Response {
    match result {
        Ok(value) => ok_json(value),
        Err(error) => errors::management_error(error.status_code, error.message),
    }
}

pub async fn get_settings() -> Response {
    response(view())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveSettings {
    config: VisionSettings,
    revision: String,
}

pub async fn put_settings(body: Bytes) -> Response {
    let payload = match parse_body(&body).and_then(|value| {
        serde_json::from_value::<SaveSettings>(value)
            .map_err(|error| GatewayError::bad_request(format!("图片兼容设置不完整：{error}")))
    }) {
        Ok(payload) => payload,
        Err(error) => return response(Err(error)),
    };
    let rules = payload.config.rules.len();
    match config::save(payload.config, &payload.revision) {
        Ok(()) => {
            logging::log(
                "[Config]",
                &format!("图片兼容设置已保存：{rules} 条规则，仅对新请求生效"),
            );
            response(view())
        }
        Err(error) => response(Err(error)),
    }
}
