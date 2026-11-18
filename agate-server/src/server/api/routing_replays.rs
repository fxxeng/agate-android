//! 路由回放明细端点（`GET /api/routing/replays`）：按模型 / 状态翻页取请求
//! 明细里的选路快照（`requests.route`）。
//!
//! 查询实现在 `request_stats::routing`（与上游 Agent2API 的 `routing_replays`
//! 同语义），本文件只做参数解析与分派 —— 与 `api::routing_trace` 同一分工。
//! 别与 `/api/routing/groups/replays`（分组维度回放，见 `api::routing_groups`）
//! 混淆：那是「组里轮到了谁」，这里是「这条请求当时怎么排的」。
//!
//! 响应形状（`ok_json` 包成 `{success, data}`）：
//! `{entries: [...], matched: N, offset, limit}`；库不可用时降级为
//! `{entries: [], matched: 0, available: false}`（见后端方法的注释）。

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::response::Response;

use crate::server::http::ok_json;
use crate::server::{errors, ServerState};

type Params = HashMap<String, String>;

fn model_of(params: &Params) -> Option<String> {
    params
        .get("model")
        .map(|model| model.trim())
        .filter(|model| !model.is_empty())
        .map(str::to_string)
}

/// 回放明细：`model` / `offset` / `limit` / `status` 四个查询参数。
///
/// `offset` 缺省 0、`limit` 缺省 30（真正的 `1..=100` 夹取在后端方法里，
/// 与 `query_requests` / `routing_trace` 同一个地方施加，避免两处口径漂移）。
pub async fn get_replays(
    State(state): State<ServerState>,
    Query(params): Query<Params>,
) -> Response {
    let model = model_of(&params);
    let offset = params
        .get("offset")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = params
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(30);
    let status = params.get("status").cloned().unwrap_or_default();
    let stats = state.request_stats();
    match tokio::task::spawn_blocking(move || {
        stats.routing_replays(model.as_deref(), offset, limit, &status)
    })
    .await
    {
        Ok(value) => ok_json(value),
        Err(error) => errors::management_error(500, format!("读取路由回放失败: {error}")),
    }
}
