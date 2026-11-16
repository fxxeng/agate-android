//! 请求流向（GET /api/routing/flow）的查询实现 —— 上游 `rs_flow.rs` 的重写。
//!
//! 回答「一段时间里，哪个客户端、走哪个模型、落到哪家 provider 的哪个账号」：
//! 默认给按 `(client, request_model, provider, account)` 聚合的**路径**清单，
//! `detail` 再分三档下钻（单条请求 / 会话 / 模型候选）。
//!
//! ── 与上游的差异（只有数据源差异，口径不动）───────────────────
//! 1. `client_name`：上游取 `json_extract(client_evidence, '$.clientName')`，
//!    Agate 没有 `client_evidence` 列；`requests.client` 存的就是
//!    `core::client::classify` 判出的**展示值**（`Codex` / `Claude Code` …），
//!    于是它同时充当客户端 id 与客户端名，`client_name` 这一列整体消失。
//! 2. `session_source`：上游取 `json_extract(route, '$.sessionSource')`。
//!    Agate 的选路快照（`core::upstream::rotate` 的 `decision_json`）从不写
//!    这个键 —— 这是**没有数据源**而不是缺列，于是整个维度（SELECT 列与
//!    WHERE 条件）删除，查询串里的 `sessionSource` 参数也就不再存在。
//! 3. 上游把内置助手的三个内部用途标识并成 `assistant` 节点；Agate 的
//!    `classify` 不产出这类标识（没有内置助手入口），合并分支一并删除。
//! 4. 其余口径逐条保留：时间窗（含「本次进程启动后的在途行」例外）、模型 /
//!    客户端 / provider / account / 路径 / 会话搜索过滤、running 判定、
//!    排序与分页上限（路径 300 行、明细 100 行、会话 100 行）。
//!
//! ── 会话维度现状（诚实版）───────────────────────────────────
//! `session_id` 仍按上游口径从 `route` JSON 取 `$.sessionId` / `$.stickyId`，
//! 但 Agate 的选路快照目前只有 `{strategy, sticky, stickyHit, picked,
//! ordered[], blocked[]}` —— **不含**这两个键。因此会话分组目前会把全部请求
//! 归进 `unknownRequests`。保留同一套 SQL 而不是删掉会话维度：写入侧一旦补
//! 上会话键，这一层不需要再改；展示层对空会话本来就有「未知会话」的降级口径。
//!
//! ── 列名从 `sql.rs` 取 ───────────────────────────────────────
//! 明细行的整行读取按 `sql::REQUEST_COLUMNS` 的列序（与请求日志同一份），
//! 不在本文件里再抄一遍列名 —— 抄一份的下场是「加列时只改了一处」。

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::clock::now_ms;
use super::report::entry_json;
use super::sql;
use super::RequestStats;

/// 流向查询条件（HTTP 层的解析与校验在 `api::routing_flow`）。
///
/// 与上游 `FlowQuery` 一一对应，只有 `session_source` 一个字段随数据源一起
/// 删除（见模块头）。`client` 的 `None` 与 `Some("")` **不同义**：
///   - `None`：不按客户端过滤；
///   - `Some("")`：只看「没有客户端」的那一组（查询串 `client=__unknown`）。
pub struct FlowQuery {
    /// 模型名：精确匹配（`COLLATE NOCASE`），空串 = 不限
    pub model: String,
    /// 时间窗（分钟）；HTTP 层已限制在 5 / 10 / 30 / 60
    pub minutes: i64,
    /// 只看在途（status=0 且无 error）
    pub running_only: bool,
    /// 全文搜索：客户端 / 模型 / provider / 账号 / id / 会话拼接后子串匹配
    pub search: String,
    /// 会话搜索：只拼会话 id 与账号名（与 `search` 是两个不同的筛位）
    pub session_search: String,
    /// 路径键（`json_array(client, request_model, provider, account_id, 账号名)`）
    pub path: String,
    /// 会话 id：精确匹配
    pub session: String,
    /// 客户端；`None` / `Some("")` 的差别见类型级说明
    pub client: Option<String>,
    /// provider id：精确匹配
    pub provider: String,
    /// 账号 id：精确匹配
    pub account: String,
    /// 会话分组筛选：`all` / `running` / `idle`（只对 `detail=sessions` 生效）
    pub activity: String,
    /// 分页偏移（HTTP 层已保证非负）
    pub offset: usize,
    /// 每页条数（HTTP 层已夹在 1~100）
    pub limit: usize,
}

/// 流向查询的两段基础 CTE。
///
/// `flow` 把每一行投影成视图要的列（派生 `request_model` / `session_id` /
/// `running` / `last_seen`），`selected` 把全部筛选条件收在一处 —— 四个
/// `detail` 分支都从 `selected` 取数，于是「总数 / 会话 / 路径」看到的必然是
/// 同一个集合（这是上游把筛选收进 CTE 的全部理由）。
///
/// 参数序号（与 [`RequestStats::routing_flow`] 里的 `params!` 逐位对应）：
/// `?1` 当前时刻、`?2` 窗口下界、`?3` 模型、`?4` 只看在途、`?5` 全文搜索、
/// `?6` 路径键、`?7` 会话 id、`?8` 本次进程启动时刻、`?9` 是否按客户端筛选、
/// `?10` 客户端、`?11` provider、`?12` 账号、`?13` 会话搜索。
const BASE: &str = "WITH flow AS (
    SELECT row_id, id, ts,
      client,
      provider, account_id, account_name, status, error, attempts, route,
      COALESCE(NULLIF(client_model, ''), model, '') AS request_model,
      CASE WHEN json_valid(route)
        THEN COALESCE(json_extract(route, '$.sessionId'),
                      json_extract(route, '$.stickyId'), '')
        ELSE '' END AS session_id,
      (status = 0 AND error IS NULL) AS running,
      CASE WHEN status = 0 AND error IS NULL THEN ?1
        ELSE ts + MAX(duration_ms, 0) END AS last_seen
    FROM requests
    WHERE (status <> 0 OR error IS NOT NULL OR ts >= ?8)
      AND ((status = 0 AND error IS NULL)
        OR (?4 = 0 AND ts + MAX(duration_ms, 0) >= ?2))
  ), selected AS (
    SELECT * FROM flow WHERE (?3 = '' OR request_model = ?3 COLLATE NOCASE)
      AND (?5 = '' OR instr(lower(client || ' ' || request_model || ' '
        || provider || ' ' || account_name || ' ' || id || ' ' || session_id),
        lower(?5)) > 0)
      AND (?6 = '' OR json_array(client, request_model, provider, account_id,
        CASE WHEN account_id = '' THEN account_name ELSE '' END) = ?6)
      AND (?7 = '' OR session_id = ?7)
      AND (?9 = 0 OR client = ?10)
      AND (?11 = '' OR provider = ?11)
      AND (?12 = '' OR account_id = ?12)
      AND (?13 = '' OR instr(lower(session_id || ' ' || account_name),
        lower(?13)) > 0)
  )";

/// 单条请求的完整 JSON（明细分页与会话「最新一条」共用）。
///
/// 列序与请求日志完全一致（[`sql::REQUEST_COLUMNS`]），再补一个 `traceKey`：
/// 上游用它把一行明细指回原始报文；Agate 的对应表是 `debug_traffic`，关联键
/// 同为请求 id，透出 row_id 让面板仍能按它再查（与上游同形）。
fn value_at(conn: &Connection, row_id: i64) -> rusqlite::Result<Value> {
    let row_sql = format!(
        "SELECT {}, row_id AS trace_key FROM requests WHERE row_id = ?1",
        sql::REQUEST_COLUMNS
    );
    conn.query_row(&row_sql, params![row_id], |row| {
        let mut value = entry_json(&sql::decode_request(row)?);
        value["traceKey"] = row_id.to_string().into();
        Ok(value)
    })
}

impl RequestStats {
    /// 流向视图的查询入口（`detail` 的三个取值由 `api::routing_flow` 校验）。
    ///
    /// 返回形状与上游一致：默认 `{available, paths, pathCount, truncated,
    /// requests, running, sessions, unknownRequests, now, since}`；`detail` 为
    /// `requests` / `models` / `sessions` 时改成对应的明细形状。库不可用时给
    /// `available: false` 与空表（与 `query_requests` 的降级同一取向：少一块
    /// 面板好过整页 500）。
    pub fn routing_flow(&self, query: &FlowQuery, detail: Option<&str>) -> Value {
        let now = now_ms();
        let since = now.saturating_sub(query.minutes.saturating_mul(60_000));
        let limit = query.limit.min(100);
        let offset = query.offset.min(i64::MAX as usize);
        let loaded = {
            let guard = self.guard();
            self.with_conn(&guard, |conn| {
                // ?1~?13 的次序与 [`BASE`] 的文档逐位对应；`parameters` 是一组
                // 共享引用，各分支可以按需重复传给多条语句。
                let parameters = params![
                    now,
                    since,
                    query.model,
                    query.running_only,
                    query.search,
                    query.path,
                    query.session,
                    crate::server::logging::process_started_at(),
                    query.client.is_some(),
                    query.client.as_deref().unwrap_or(""),
                    query.provider,
                    query.account,
                    query.session_search,
                ];
                if detail == Some("requests") {
                    let count_sql = format!("{BASE} SELECT COUNT(*) FROM selected");
                    let matched: i64 = conn.query_row(&count_sql, parameters, |row| row.get(0))?;
                    let page_sql = format!(
                        "{BASE} SELECT row_id FROM selected \
                         ORDER BY running DESC, ts DESC, row_id DESC \
                         LIMIT {limit} OFFSET {offset}"
                    );
                    let mut statement = conn.prepare(&page_sql)?;
                    let ids = statement
                        .query_map(parameters, |row| row.get::<_, i64>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    let entries = ids
                        .into_iter()
                        .map(|id| value_at(conn, id))
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    return Ok(json!({
                        "available": true,
                        "entries": entries,
                        "matched": matched,
                        "now": now,
                    }));
                }
                // 筛选下拉的模型候选：只列**当前时间窗内真的发过请求**的模型，
                // 并且刻意不带模型条件 —— 否则选中一个模型后下拉里就只剩它
                // 自己。按请求数降序，常用的排前面（与上游一致）。
                if detail == Some("models") {
                    let model_free = params![
                        now,
                        since,
                        "",
                        query.running_only,
                        query.search,
                        query.path,
                        query.session,
                        crate::server::logging::process_started_at(),
                        query.client.is_some(),
                        query.client.as_deref().unwrap_or(""),
                        query.provider,
                        query.account,
                        query.session_search,
                    ];
                    let models_sql = format!(
                        "{BASE} SELECT request_model, COUNT(*) AS n FROM selected \
                         WHERE request_model <> '' GROUP BY request_model \
                         ORDER BY n DESC, request_model ASC LIMIT 300"
                    );
                    let mut statement = conn.prepare(&models_sql)?;
                    let models = statement
                        .query_map(model_free, |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    return Ok(json!({
                        "available": true,
                        "models": models,
                        "now": now,
                    }));
                }
                if detail == Some("sessions") {
                    // 会话分组的两档「活动范围」：同一 (模型, 会话) 组里有没有
                    // 在途请求。写成一个可复用的 EXISTS，running / idle 只差
                    // 一个 NOT —— 两处抄两份迟早漂。
                    let exists = "EXISTS (SELECT 1 FROM flow f \
                                  WHERE f.session_id = selected.session_id \
                                  AND f.request_model = selected.request_model \
                                  COLLATE NOCASE AND f.running = 1)";
                    let activity = match query.activity.as_str() {
                        "running" => format!(" HAVING {exists}"),
                        "idle" => format!(" HAVING NOT {exists}"),
                        _ => String::new(),
                    };
                    let matched_sql = format!(
                        "{BASE} SELECT COUNT(*) FROM (SELECT request_model, session_id \
                         FROM selected WHERE session_id <> '' \
                         GROUP BY request_model, session_id{activity})"
                    );
                    let matched: i64 =
                        conn.query_row(&matched_sql, parameters, |row| row.get(0))?;
                    let unknown_sql =
                        format!("{BASE} SELECT COUNT(*) FROM selected WHERE session_id = ''");
                    let unknown: i64 =
                        conn.query_row(&unknown_sql, parameters, |row| row.get(0))?;
                    let sessions_sql = format!(
                        "{BASE},
  sessions AS (
    SELECT request_model, session_id, COUNT(*) AS request_count FROM selected
    WHERE session_id <> '' GROUP BY request_model, session_id{activity}
  ) SELECT s.request_model, s.session_id, s.request_count,
    SUM(f.running), MAX(f.last_seen),
    (SELECT row_id FROM flow latest
      WHERE latest.session_id = s.session_id
        AND latest.request_model = s.request_model COLLATE NOCASE
      ORDER BY running DESC, ts DESC, row_id DESC LIMIT 1)
  FROM sessions s JOIN flow f ON f.session_id = s.session_id
    AND f.request_model = s.request_model COLLATE NOCASE
  GROUP BY s.request_model, s.session_id
  ORDER BY SUM(f.running) DESC, MAX(f.last_seen) DESC, s.session_id
  LIMIT {limit} OFFSET {offset}"
                    );
                    let mut statement = conn.prepare(&sessions_sql)?;
                    let rows = statement
                        .query_map(parameters, |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, i64>(2)?,
                                row.get::<_, i64>(3)?,
                                row.get::<_, i64>(4)?,
                                row.get::<_, i64>(5)?,
                            ))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    let entries = rows
                        .into_iter()
                        .map(|(model, id, count, running, last_seen, latest)| {
                            Ok(json!({
                                "model": model,
                                "id": id,
                                "requestCount": count,
                                "running": running,
                                "lastSeen": last_seen,
                                "latest": value_at(conn, latest)?,
                            }))
                        })
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    return Ok(json!({
                        "available": true,
                        "entries": entries,
                        "matched": matched,
                        "unknownRequests": unknown,
                        "now": now,
                    }));
                }
                // 路径键：与 `selected` 里的 `?6` 用同一条表达式（元素顺序也
                // 必须一致），否则「点开一条路径」会筛出空集。
                let group = "client, request_model, provider, account_id, \
                    CASE WHEN account_id = '' \
                    THEN account_name ELSE '' END";
                let matched_sql = format!(
                    "{BASE} SELECT COUNT(*) FROM (SELECT 1 FROM selected GROUP BY {group})"
                );
                let matched: i64 = conn.query_row(&matched_sql, parameters, |row| row.get(0))?;
                let totals_sql = format!(
                    "{BASE} SELECT COUNT(*), COALESCE(SUM(running), 0), \
                     COUNT(DISTINCT CASE WHEN session_id <> '' \
                     THEN json_array(request_model, session_id) END), \
                     COALESCE(SUM(session_id = ''), 0) FROM selected"
                );
                let totals: (i64, i64, i64, i64) =
                    conn.query_row(&totals_sql, parameters, |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })?;
                let (requests, running, sessions, unknown) = totals;
                let paths_sql = format!(
                    "{BASE} SELECT json_array({group}), client, request_model, provider, \
                     account_id, MAX(account_name), COUNT(*), SUM(running), \
                     SUM(status >= 200 AND status < 300 AND error IS NULL), \
                     SUM((status <> 0 OR error IS NOT NULL) \
                       AND (status < 200 OR status >= 300 OR error IS NOT NULL)), \
                     COUNT(DISTINCT NULLIF(session_id, '')), \
                     SUM(CASE WHEN json_valid(route) \
                       THEN json_extract(route, '$.stickyHit') = 1 ELSE 0 END), \
                     SUM(attempts > 1), MAX(last_seen), client \
                     FROM selected GROUP BY {group} \
                     ORDER BY SUM(running) DESC, COUNT(*) DESC, MAX(last_seen) DESC \
                     LIMIT 300"
                );
                let mut statement = conn.prepare(&paths_sql)?;
                let paths = statement
                    .query_map(parameters, |row| {
                        Ok(json!({
                            "id": row.get::<_, String>(0)?,
                            "client": row.get::<_, String>(1)?,
                            "model": row.get::<_, String>(2)?,
                            "provider": row.get::<_, String>(3)?,
                            "accountId": row.get::<_, String>(4)?,
                            "accountName": row.get::<_, String>(5)?,
                            "requests": row.get::<_, i64>(6)?,
                            "running": row.get::<_, i64>(7)?,
                            "successful": row.get::<_, i64>(8)?,
                            "failed": row.get::<_, i64>(9)?,
                            "sessions": row.get::<_, i64>(10)?,
                            "stickyHits": row.get::<_, Option<i64>>(11)?.unwrap_or(0),
                            "switched": row.get::<_, i64>(12)?,
                            "lastSeen": row.get::<_, i64>(13)?,
                            "clientLabel": row.get::<_, String>(14)?,
                        }))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(json!({
                    "available": true,
                    "paths": paths,
                    "pathCount": matched,
                    "truncated": matched > 300,
                    "requests": requests,
                    "running": running,
                    "sessions": sessions,
                    "unknownRequests": unknown,
                    "now": now,
                    "since": since,
                }))
            })
        };
        loaded.unwrap_or_else(|| {
            json!({
                "available": false,
                "paths": [],
                "entries": [],
                "matched": 0,
                "now": now,
            })
        })
    }
}
