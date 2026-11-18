//! 路由回放明细（`GET /api/routing/replays`）：按模型 / 状态翻页取请求明细里的
//! 选路决策快照（`requests.route`），与上游 Agent2API 的 `routing_replays` 同语义。
//!
//! ── 与「分组回放」的区别（不是同一件事，别混）─────────────────
//! `api::routing_groups` 的 `/api/routing/groups/replays` 是**分组维度**的扩展：
//! 它按路由组回答「某个模型在组里轮到了谁」，数据源是分组配置 + 请求统计的聚合
//! 视图。本文件是**请求明细维度**：逐条请求返回当时落库的 `route` JSON
//! （`{strategy, sticky, stickyHit, picked, ordered[], blocked[]}`，采集点在
//! `record::RequestEntry::route` 的注释里），供界面画「当时为什么这么排」。
//! 两者互不替代，也互不改写。
//!
//! ── 与上游的差异（只有列名对齐，口径不动）─────────────────────
//! 1. 模型过滤按本地 `model` 列精确匹配（`COLLATE NOCASE`），与上游
//!    `routing_replays` 逐字一致；上游 `routing_metrics` 的
//!    `COALESCE(NULLIF(client_model, ''), model)` 口径**不**带到本端点 ——
//!    回放要的是「这条明细当时记在哪个模型名下」。
//! 2. 列清单与行解码复用本地现成的 `sql::REQUEST_COLUMNS` /
//!    `sql::decode_request`，行 JSON 化用同模块 `report::entry_json`
//!    （与 `query_requests` / `routing_trace` 同一份），不自造列名。
//! 3. `status` 三档与上游逐字相同：`error` / `sticky` / 其余（无附加条件）。

use rusqlite::params;
use serde_json::{json, Value};

use super::{report, sql, RequestStats};

impl RequestStats {
    /// 翻页取「带选路快照」的请求明细（`route IS NOT NULL` 才算一条回放）。
    ///
    /// 参数口径：
    /// - `model`：`None` / 空串不过滤；非空按 `model = ?1 COLLATE NOCASE` 精确匹配。
    /// - `offset`：跳过行数（缺省在 API 层取 0）；`limit`：页大小（缺省 30），
    ///   这里再夹一次 `1..=100`，与 `query_requests` / `routing_trace` 同口径。
    /// - `status`：`error` / `sticky` / 其他，见模块头第 3 条。
    ///
    /// 排序 `ts DESC, row_id DESC`：`row_id` 是自增物理行号，同 `ts` 的两条
    /// 明细靠它定死顺序（翻页不会漏行 / 重行，理由见 `db::schema` 的建表注释）。
    ///
    /// 降级：库不可用（`with_conn` 返回 `None`）时给
    /// `{entries: [], matched: 0, available: false}` —— 与 `routing_trace` 的
    /// 降级同一取向（少一块回放，好过整页失败）。
    pub fn routing_replays(
        &self,
        model: Option<&str>,
        offset: usize,
        limit: usize,
        status: &str,
    ) -> Value {
        let model = model.unwrap_or("");
        let limit = limit.clamp(1, 100);
        let offset = offset.min(i64::MAX as usize);
        let status_clause = match status {
            "error" => {
                " AND status <> 0 AND NOT (status >= 200 AND status < 300 AND error IS NULL)"
            }
            "sticky" => " AND json_valid(route) AND json_extract(route, '$.stickyHit') = 1",
            _ => "",
        };
        let where_clause = format!(
            "WHERE route IS NOT NULL \
             AND (?1 = '' OR model = ?1 COLLATE NOCASE){status_clause}",
        );
        let loaded = {
            let guard = self.guard();
            self.with_conn(&guard, |conn| {
                let matched: i64 = conn.query_row(
                    &format!("SELECT COUNT(*) FROM requests {where_clause}"),
                    params![model],
                    |row| row.get(0),
                )?;
                let query = format!(
                    "SELECT {} FROM requests {where_clause} \
                     ORDER BY ts DESC, row_id DESC LIMIT ?2 OFFSET ?3",
                    sql::REQUEST_COLUMNS,
                );
                let mut stmt = conn.prepare(&query)?;
                let mut rows = stmt.query(params![model, limit as i64, offset as i64])?;
                let mut entries = Vec::new();
                while let Some(row) = rows.next()? {
                    entries.push(sql::decode_request(row)?);
                }
                Ok((matched, entries))
            })
        };
        let Some((matched, rows)) = loaded else {
            return json!({ "entries": [], "matched": 0, "available": false });
        };
        let entries: Vec<Value> = rows.iter().map(report::entry_json).collect();
        json!({ "entries": entries, "matched": matched, "offset": offset, "limit": limit })
    }
}
