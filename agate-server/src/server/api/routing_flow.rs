//! GET /api/routing/flow —— 请求流向（模型 × 客户端 × provider × 账号）。
//!
//! 与上游 `routing_flow.rs` 同形：解析查询串 → 校验 → `spawn_blocking` 里查
//! （流向是 SQL 聚合，不能占 async worker）→ 管理面信封。
//!
//! ── 与上游的差异 ─────────────────────────────────────────────
//! 1. `sessionSource` 参数整体删除：Agate 的选路快照不写这个键，没有数据源
//!    （见 `request_stats::flow` 的模块头）。
//! 2. `client=__unknown` 的语义照旧（映射成空串 = 「没有客户端」那一组）。
//! 3. 其余参数与校验逐条保留：`minutes` 只认 5/10/30/60、`scope` 只认
//!    window/running、`activity` 只认 all/running/idle、`detail` 只认
//!    requests/sessions/models；各字段长度上限也在（防止超长串进入搜索表达式）。
//!
//! 校验放在 handler 而不是存储层：流向面板的查询串是**用户可编辑**的，400 的
//! 文案要直接显示在筛选栏上；存储层只负责「给定条件怎么查」。

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::response::Response;

use crate::server::request_stats::FlowQuery;
use crate::server::{errors, http::ok_json, ServerState};

type Params = HashMap<String, String>;

/// 查询串里「未知客户端」的哨兵值（与上游同一个值）。
const CLIENT_UNKNOWN: &str = "__unknown";

/// 取一个查询串字段（去首尾空白；缺省空串）。
fn text_of(params: &Params, key: &str) -> String {
    params
        .get(key)
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

/// GET /api/routing/flow?minutes=&model=&scope=&search=&sessionSearch=&path=
///   &session=&provider=&account=&activity=&offset=&limit=&detail=
///
/// `client` 单独解析：`__unknown` 表示「没有客户端的那一组」（映射成空串），
/// 缺省则是不按客户端过滤 —— 这两个语义在存储层靠 `Option<String>` 区分。
pub async fn get_flow(State(state): State<ServerState>, Query(params): Query<Params>) -> Response {
    let scope = text_of(&params, "scope");
    let minutes = params
        .get("minutes")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(10);
    if ![5, 10, 30, 60].contains(&minutes) {
        return errors::management_error(400, "时间范围必须为 5、10、30 或 60 分钟");
    }
    let client = params.get("client").map(|value| {
        if value == CLIENT_UNKNOWN {
            String::new()
        } else {
            value.clone()
        }
    });
    let query = FlowQuery {
        model: text_of(&params, "model"),
        minutes,
        running_only: scope == "running",
        search: text_of(&params, "search"),
        session_search: text_of(&params, "sessionSearch"),
        path: text_of(&params, "path"),
        session: text_of(&params, "session"),
        client,
        provider: text_of(&params, "provider"),
        account: text_of(&params, "account"),
        activity: text_of(&params, "activity"),
        offset: params
            .get("offset")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        limit: params
            .get("limit")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(30)
            .clamp(1, 100),
    };
    if query.model.len() > 1024
        || query.search.len() > 1024
        || query.session_search.len() > 1024
        || query.path.len() > 4096
        || query.session.len() > 128
        || query.client.as_ref().is_some_and(|value| value.len() > 256)
        || query.provider.len() > 256
        || query.account.len() > 256
    {
        return errors::management_error(400, "流向查询条件过长");
    }
    if !["", "window", "running"].contains(&scope.as_str())
        || !["", "all", "running", "idle"].contains(&query.activity.as_str())
    {
        return errors::management_error(400, "未知的请求活动范围");
    }
    let detail = text_of(&params, "detail");
    if !["", "requests", "sessions", "models"].contains(&detail.as_str()) {
        return errors::management_error(400, "未知的流向详情类型");
    }
    let stats = state.request_stats();
    match tokio::task::spawn_blocking(move || {
        stats.routing_flow(&query, (!detail.is_empty()).then_some(detail.as_str()))
    })
    .await
    {
        Ok(value) => ok_json(value),
        Err(error) => errors::management_error(500, format!("读取请求流向失败: {error}")),
    }
}
