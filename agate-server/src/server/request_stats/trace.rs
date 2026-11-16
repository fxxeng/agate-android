//! 路由追踪：按模型/状态/关键词翻页查请求记录，附 traceKey（row_id）。
//!
//! ── 与上游的差异 ────────────────────────────────────────────────
//! 上游 trace 依赖 `date_range::date_bounds`（自定义日期范围）与
//! `keepalive::normalize_request_display`（账号显示名归一化）。
//! Agate 两者均无：日期范围改用 start/end 毫秒时间戳直传（与
//! stats_api 的 requests 端点同口径）；显示名归一化跳过（entry_json
//! 已含 account_name，界面直接用）。

use rusqlite::params;
use serde_json::{json, Value};

use super::{sql, RequestStats};

fn trace_json(entry: &super::RequestEntry, trace_key: String) -> Value {
    let mut value = super::report::entry_json(entry);
    value["traceKey"] = Value::String(trace_key);
    value
}

impl RequestStats {
    /// 翻页查请求记录。`bounds` 是 (start_ms, end_ms) 时间窗口。
    pub fn routing_trace(
        &self,
        model: Option<&str>,
        offset: usize,
        limit: usize,
        status: &str,
        search: &str,
        bounds: Option<(i64, i64)>,
    ) -> Value {
        let model = model.unwrap_or("");
        let limit = limit.clamp(1, 100);
        let offset = offset.min(i64::MAX as usize);
        let (start, end) = bounds.unwrap_or((0, i64::MAX));
        let status_clause = match status {
            "error" => {
                " AND (error IS NOT NULL OR (status <> 0 AND (status < 200 OR status >= 300)))"
            }
            "ok" => " AND status >= 200 AND status < 300 AND error IS NULL",
            "running" => " AND status = 0 AND error IS NULL",
            "switched" => " AND attempts > 1",
            "sticky" => {
                " AND CASE WHEN json_valid(route) \
                 THEN json_extract(route, '$.stickyHit') ELSE 0 END = 1"
            }
            _ => "",
        };
        let where_clause = format!(
            "WHERE (?1 = '' OR model = ?1 COLLATE NOCASE \
             OR client_model = ?1 COLLATE NOCASE \
             OR upstream_model = ?1 COLLATE NOCASE) \
             AND ts >= ?2 AND ts < ?3 \
             AND (?4 = '' OR instr(lower(model), lower(?4)) > 0 \
                 OR instr(lower(client_model), lower(?4)) > 0 \
                 OR instr(lower(upstream_model), lower(?4)) > 0 \
                 OR instr(lower(id), lower(?4)) > 0){status_clause}",
        );
        let loaded = {
            let guard = self.guard();
            self.with_conn(&guard, |conn| {
                let matched: i64 = conn.query_row(
                    &format!("SELECT COUNT(*) FROM requests {where_clause}"),
                    params![model, start, end, search],
                    |row| row.get(0),
                )?;
                let query = format!(
                    "SELECT {}, row_id AS trace_key FROM requests {where_clause} \
                     ORDER BY ts DESC, row_id DESC LIMIT ?5 OFFSET ?6",
                    sql::REQUEST_COLUMNS,
                );
                let mut stmt = conn.prepare(&query)?;
                let mut rows =
                    stmt.query(params![model, start, end, search, limit as i64, offset as i64])?;
                let mut entries = Vec::new();
                while let Some(row) = rows.next()? {
                    entries.push((
                        sql::decode_request(row)?,
                        row.get::<_, i64>("trace_key")?.to_string(),
                    ));
                }
                Ok((matched, entries))
            })
        };
        let Some((matched, rows)) = loaded else {
            return json!({ "available": false, "entries": [], "matched": 0 });
        };
        let entries: Vec<Value> = rows
            .into_iter()
            .map(|(entry, key)| trace_json(&entry, key))
            .collect();
        json!({
            "available": true, "entries": entries, "matched": matched,
            "offset": offset, "limit": limit,
        })
    }

    /// 按请求 id 查单条详情（ts 可选，用于同 id 多行时精确定位）。
    pub fn routing_trace_request(&self, id: &str, ts: Option<i64>) -> Option<Value> {
        let loaded = {
            let guard = self.guard();
            self.with_conn(&guard, |conn| {
                let query = format!(
                    "SELECT {}, row_id AS trace_key FROM requests \
                     WHERE id = ?1 AND (?2 IS NULL OR ts = ?2) \
                     ORDER BY (status = 0) DESC, ts DESC, row_id DESC LIMIT 1",
                    sql::REQUEST_COLUMNS,
                );
                let mut stmt = conn.prepare(&query)?;
                let mut rows = stmt.query(params![id, ts])?;
                let Some(row) = rows.next()? else {
                    return Ok(None);
                };
                Ok(Some((
                    sql::decode_request(row)?,
                    row.get::<_, i64>("trace_key")?.to_string(),
                )))
            })
        };
        let (entry, key) = loaded??;
        Some(trace_json(&entry, key))
    }
}
