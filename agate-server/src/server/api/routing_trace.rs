//! 路由追踪端点：翻页查请求记录 + 按 id 查单条详情。
//!
//! 与上游差异：日期范围用 start/end 毫秒时间戳直传（Agate 无
//! date_range 模块）；跳过 keepalive::normalize_request_display
//! （entry_json 已含 account_name）。

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::response::Response;

use crate::server::http::ok_json;
use crate::server::{errors, ServerState};

type Params = HashMap<String, String>;

fn text_of(params: &Params, key: &str) -> Option<String> {
    params
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub async fn get_trace(State(state): State<ServerState>, Query(params): Query<Params>) -> Response {
    let model = text_of(&params, "model");
    let search = text_of(&params, "search").unwrap_or_default();
    let status = text_of(&params, "status").unwrap_or_default();
    let offset = text_of(&params, "offset")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = text_of(&params, "limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(30)
        .clamp(1, 100);
    let start = text_of(&params, "start").and_then(|v| v.parse::<i64>().ok());
    let end = text_of(&params, "end").and_then(|v| v.parse::<i64>().ok());
    let bounds = match (start, end) {
        (Some(s), Some(e)) if s < e => Some((s, e)),
        (None, None) => None,
        _ => return errors::management_error(400, "start/end 必须同时给出且 start < end"),
    };
    let stats = state.request_stats();
    match tokio::task::spawn_blocking(move || {
        stats.routing_trace(model.as_deref(), offset, limit, &status, &search, bounds)
    })
    .await
    {
        Ok(value) => ok_json(value),
        Err(error) => errors::management_error(500, format!("读取请求跟踪失败: {error}")),
    }
}

pub async fn get_trace_request(
    State(state): State<ServerState>,
    Query(params): Query<Params>,
) -> Response {
    let id = text_of(&params, "id").unwrap_or_default();
    let ts = text_of(&params, "ts").and_then(|v| v.parse::<i64>().ok());
    if id.is_empty() || id.len() > 256 {
        return errors::management_error(400, "请求 ID 不能为空且不能超过 256 字符");
    }
    let stats = state.request_stats();
    match tokio::task::spawn_blocking(move || stats.routing_trace_request(&id, ts)).await {
        Ok(Some(value)) => ok_json(value),
        Ok(None) => errors::management_error(404, "请求记录已删除或超过留存范围"),
        Err(error) => errors::management_error(500, format!("读取请求详情失败: {error}")),
    }
}
