//! 签到执行过程台账（schema v16）。
//!
//! 与 `checkin_records` 的展示历史分离：这里保存批次身份、账号执行意图和
//! 完整结果快照，为进程崩溃后的本地补账提供证据。执行入口在 Provider 前写入
//! `in_flight`，结果保存后执行本地幂等投影；启动恢复只补投影并隔离不确定请求，
//! 绝不自动重放任何远端请求。
//!
//! 状态约束：
//! - `in_flight` 必须在调用 Provider **之前**提交；
//! - Provider 返回后，先把完整结果写成 `result_saved`；
//! - 本地展示记录等投影完成后才转为 `projected`；
//! - 启动恢复可把遗留 `in_flight` 隔离为 `uncertain`，不得自动重试。
//!
//! `Db::with_mut` 闭包中只做 SQLite 操作，绝不打日志或出网（日志也写同一库）。

use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use std::collections::HashSet;

use crate::server::db::Db;

pub const RUN_RUNNING: &str = "running";
pub const RUN_COMPLETED: &str = "completed";
pub const RUN_PARTIAL: &str = "partial";
pub const RUN_CANCELLED: &str = "cancelled";
pub const RUN_INTERRUPTED: &str = "interrupted";

pub const ACCOUNT_IN_FLIGHT: &str = "in_flight";
pub const ACCOUNT_RESULT_SAVED: &str = "result_saved";
pub const ACCOUNT_PROJECTED: &str = "projected";
pub const ACCOUNT_UNCERTAIN: &str = "uncertain";

#[derive(Clone, Debug)]
pub struct RunInput<'a> {
    pub run_id: &'a str,
    pub started_at: i64,
    pub source: &'a str,
    pub reason: &'a str,
    pub target_count: i64,
    pub skipped_count: i64,
    pub schedule_id: Option<&'a str>,
    pub auto_checkin: bool,
}

#[derive(Clone, Debug)]
pub struct AccountInput<'a> {
    pub run_id: &'a str,
    pub account_id: &'a str,
    pub account_name: &'a str,
    pub provider: &'a str,
    pub ordinal: i64,
    pub started_at: i64,
}

fn with_db<T>(
    db: &Db,
    operation: impl FnOnce(&mut rusqlite::Connection) -> rusqlite::Result<T>,
) -> Result<T, String> {
    db.with_mut(operation)
        .ok_or_else(|| "签到执行台账数据库不可用（连接锁中毒或未就绪）".to_string())?
        .map_err(|error| format!("签到执行台账写入失败: {error}"))
}

/// 创建一个签到批次。`run_id` 由调用方生成并在整轮生命周期内保持稳定。
pub fn create_run(db: &Db, input: &RunInput<'_>) -> Result<(), String> {
    with_db(db, |conn| {
        conn.execute(
            "INSERT INTO checkin_runs
             (run_id, started_at, source, reason, status, target_count, skipped_count,
              schedule_id, auto_checkin)
             VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7, ?8)",
            params![
                input.run_id, input.started_at, input.source, input.reason,
                input.target_count, input.skipped_count, input.schedule_id,
                input.auto_checkin
            ],
        )?;
        Ok(())
    })
}

/// 在 Provider 调用前写入账号执行意图。父批次必须仍处于 running。
pub fn begin_account(db: &Db, input: &AccountInput<'_>) -> Result<(), String> {
    with_db(db, |conn| {
        let tx = conn.transaction()?;
        let status: Option<String> = tx
            .query_row(
                "SELECT status FROM checkin_runs WHERE run_id = ?1",
                [input.run_id],
                |row| row.get(0),
            )
            .optional()?;
        if status.as_deref() != Some(RUN_RUNNING) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "INSERT INTO checkin_run_accounts
             (run_id, account_id, account_name, provider, ordinal, state, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'in_flight', ?6)",
            params![
                input.run_id,
                input.account_id,
                input.account_name,
                input.provider,
                input.ordinal,
                input.started_at
            ],
        )?;
        tx.commit()
    })
}

/// 保存 Provider 返回的完整归一化结果。
///
/// 同一结果重复提交是幂等成功（返回 `false` 表示本次没有发生状态变更）；
/// 不同结果覆盖已保存结果则拒绝，避免把执行证据静默改写。
pub fn save_result(
    db: &Db,
    run_id: &str,
    account_id: &str,
    saved_at: i64,
    result_json: &str,
) -> Result<bool, String> {
    with_db(db, |conn| {
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE checkin_run_accounts
             SET state = 'result_saved', result_saved_at = ?3,
                 result_json = ?4, last_error = NULL
             WHERE run_id = ?1 AND account_id = ?2 AND state = 'in_flight'",
            params![run_id, account_id, saved_at, result_json],
        )?;
        if changed == 1 {
            tx.commit()?;
            return Ok(true);
        }
        let existing: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT state, result_json FROM checkin_run_accounts
                 WHERE run_id = ?1 AND account_id = ?2",
                params![run_id, account_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match existing {
            Some((state, Some(saved)))
                if (state == ACCOUNT_RESULT_SAVED || state == ACCOUNT_PROJECTED)
                    && saved == result_json =>
            {
                tx.commit()?;
                Ok(false)
            }
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    })
}

/// 本地展示记录等投影完成后，将账号状态推进到 projected。
pub fn mark_projected(
    db: &Db,
    run_id: &str,
    account_id: &str,
    projected_at: i64,
) -> Result<bool, String> {
    with_db(db, |conn| {
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE checkin_run_accounts
             SET state = 'projected', projected_at = ?3, last_error = NULL
             WHERE run_id = ?1 AND account_id = ?2 AND state = 'result_saved'",
            params![run_id, account_id, projected_at],
        )?;
        if changed == 1 {
            tx.commit()?;
            return Ok(true);
        }
        let state: Option<String> = tx
            .query_row(
                "SELECT state FROM checkin_run_accounts
                 WHERE run_id = ?1 AND account_id = ?2",
                params![run_id, account_id],
                |row| row.get(0),
            )
            .optional()?;
        if state.as_deref() == Some(ACCOUNT_PROJECTED) {
            tx.commit()?;
            Ok(false)
        } else {
            Err(rusqlite::Error::InvalidQuery)
        }
    })
}

/// 将无可靠 Provider 结果的遗留请求隔离为 uncertain；绝不重置回 in_flight。
pub fn mark_uncertain(
    db: &Db,
    run_id: &str,
    account_id: &str,
    reason: &str,
) -> Result<bool, String> {
    with_db(db, |conn| {
        let changed = conn.execute(
            "UPDATE checkin_run_accounts
             SET state = 'uncertain', last_error = ?3
             WHERE run_id = ?1 AND account_id = ?2 AND state = 'in_flight'",
            params![run_id, account_id, reason],
        )?;
        Ok(changed == 1)
    })
}

/// 关闭批次。未解决的 in_flight / result_saved 会阻止正常关闭。
pub fn finish_run(
    db: &Db,
    run_id: &str,
    finished_at: i64,
    status: &str,
    target_count: i64,
    result_count: i64,
    skipped_count: i64,
) -> Result<(), String> {
    finish_run_with_summary(
        db, run_id, finished_at, status, target_count, result_count, skipped_count, None,
    )
}

/// 关闭批次并原子保存汇总快照。批量签到传入 summary_json，单账号/活动入口
/// 不生成签到中心的轮次时间线，传 None。
pub fn finish_run_with_summary(
    db: &Db,
    run_id: &str,
    finished_at: i64,
    status: &str,
    target_count: i64,
    result_count: i64,
    skipped_count: i64,
    summary_json: Option<&str>,
) -> Result<(), String> {
    if status != RUN_COMPLETED
        && status != RUN_PARTIAL
        && status != RUN_CANCELLED
        && status != RUN_INTERRUPTED
    {
        return Err("无效的签到批次终态".to_string());
    }
    with_db(db, |conn| {
        let tx = conn.transaction()?;
        let unresolved: i64 = tx.query_row(
            "SELECT COUNT(*) FROM checkin_run_accounts
             WHERE run_id = ?1 AND state IN ('in_flight','result_saved')",
            [run_id],
            |row| row.get(0),
        )?;
        if unresolved != 0 {
            // 即使关闭为 interrupted，也必须先把 in_flight 隔离为 uncertain，
            // 或先把 result_saved 的本地投影补完；批次终态不能掩盖账号悬空状态。
            return Err(rusqlite::Error::InvalidQuery);
        }
        let changed = tx.execute(
            "UPDATE checkin_runs
             SET status = ?2, finished_at = ?3, target_count = ?4,
                 result_count = ?5, skipped_count = ?6, summary_json = ?7
             WHERE run_id = ?1 AND status = 'running'",
            params![run_id, status, finished_at, target_count, result_count, skipped_count, summary_json],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.commit()
    })
}

/// 汇总时间线写入成功后标记投影完成；重复调用安全。
pub fn mark_history_projected(db: &Db, run_id: &str) -> Result<bool, String> {
    with_db(db, |conn| {
        let changed = conn.execute(
            "UPDATE checkin_runs SET history_projected = 1
             WHERE run_id = ?1 AND summary_json IS NOT NULL AND history_projected = 0",
            [run_id],
        )?;
        Ok(changed == 1)
    })
}

/// 启动恢复统计。`uncertain` 代表请求可能已到达上游但没有可靠结果，必须隔离。
#[derive(Clone, Debug, Default)]
pub struct RecoveryReport {
    pub projected: usize,
    pub uncertain: usize,
    pub interrupted_runs: usize,
}

#[derive(Debug)]
struct SavedAccountResult {
    run_id: String,
    account_id: String,
    provider: String,
    source: String,
    reason: String,
    result_saved_at: i64,
    result_json: String,
}

#[derive(Debug)]
struct PendingReconciliationProjection {
    run_id: String,
    account_id: String,
    account_name: String,
    provider: String,
    evidence_id: i64,
    account_started_at: i64,
}

/// 检查某账号今天是否存在结果不确定的签到。仅隔离当前北京时间自然日；
/// 次日上游日额度重置后允许新一轮执行。
pub fn uncertain_accounts_today(db: &Db) -> Result<HashSet<String>, String> {
    db.with(|conn| {
        let mut statement = conn.prepare(
            "SELECT DISTINCT account_id FROM checkin_run_accounts
             WHERE state = 'uncertain' AND reconciled_at IS NULL
               AND date(started_at / 1000, 'unixepoch', '+8 hours') = ?1",
        )?;
        let rows = statement.query_map([crate::server::core::beijing::today_key()], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<Result<HashSet<_>, _>>()
    })
    .ok_or_else(|| "签到执行台账数据库连接不可用".to_string())?
    .map_err(|error| format!("读取不确定签到账号失败: {error}"))
}

/// 查询所有仍需人工核对的执行记录。只返回诊断元数据，不暴露凭证或请求内容；
/// 该接口只读，不能将 uncertain 直接改成成功/失败。
pub fn query_uncertain(db: &Db, limit: usize, offset: usize) -> Result<Value, String> {
    let limit = limit.clamp(1, 500) as i64;
    let offset = offset.min(i64::MAX as usize) as i64;
    db.with(|conn| -> Result<Value, rusqlite::Error> {
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM checkin_run_accounts WHERE state = 'uncertain' AND reconciled_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        let mut statement = conn.prepare(
            "SELECT a.run_id, a.account_id, a.account_name, a.provider,
                    a.started_at, a.last_error, r.source, r.reason, r.started_at,
                    r.schedule_id, r.auto_checkin
             FROM checkin_run_accounts a
             JOIN checkin_runs r ON r.run_id = a.run_id
             WHERE a.state = 'uncertain' AND a.reconciled_at IS NULL
             ORDER BY a.started_at DESC, a.run_id DESC, a.ordinal ASC
             LIMIT ?1 OFFSET ?2",
        )?;
        let rows = statement.query_map(params![limit, offset], |row| {
            Ok(serde_json::json!({
                "runId": row.get::<_, String>(0)?,
                "accountId": row.get::<_, String>(1)?,
                "accountName": row.get::<_, String>(2)?,
                "provider": row.get::<_, String>(3)?,
                "accountStartedAt": row.get::<_, i64>(4)?,
                "lastError": row.get::<_, Option<String>>(5)?,
                "source": row.get::<_, String>(6)?,
                "reason": row.get::<_, String>(7)?,
                "runStartedAt": row.get::<_, i64>(8)?,
                "scheduleId": row.get::<_, Option<String>>(9)?,
                "autoCheckin": row.get::<_, bool>(10)?,
                "state": ACCOUNT_UNCERTAIN,
                "automaticRetry": false,
            }))
        })?;
        let entries = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(serde_json::json!({ "entries": entries, "total": total }))
    })
    .ok_or_else(|| "签到执行台账数据库连接不可用".to_string())?
    .map_err(|error| format!("查询不确定签到记录失败: {error}"))
}

/// 读取单条 uncertain 记录的 Provider 与账号执行开始时间，供只读对账探针使用。
pub fn uncertain_reconciliation_context(
    db: &Db,
    run_id: &str,
    account_id: &str,
) -> Result<Option<(String, i64)>, String> {
    db.with(|conn| {
        conn.query_row(
            "SELECT provider, started_at FROM checkin_run_accounts
             WHERE run_id = ?1 AND account_id = ?2 AND state = 'uncertain' AND reconciled_at IS NULL",
            params![run_id, account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
    })
    .ok_or_else(|| "签到执行台账数据库连接不可用".to_string())?
    .map_err(|error| format!("读取不确定签到对账上下文失败: {error}"))
}

/// 持久化一次 Provider 对账观察。仅保存 allowlist 元数据，不保存原始响应、凭证或请求内容。
/// `confirmed` 必须满足 Provider 专属证据契约（当前为 Qoder campaign 与 WorkBuddy
/// 当日签到状态），并在事务内重新核对账号运行时间；原始 state 仍保留为 uncertain。
pub fn record_reconciliation_evidence(
    db: &Db,
    run_id: &str,
    account_id: &str,
    result: &Value,
) -> Result<Value, String> {
    let verdict = result.get("verdict").and_then(Value::as_str).unwrap_or("unknown");
    if verdict != "confirmed" && verdict != "unknown" {
        return Err("无效的 Provider 对账 verdict".to_string());
    }
    let provider = result.get("provider").and_then(Value::as_str).unwrap_or("");
    let evidence_type = result.get("evidenceType").and_then(Value::as_str).unwrap_or("");
    let observed_at = result.get("observedAt").and_then(Value::as_i64).unwrap_or(0);
    let campaign_id: String = result
        .get("campaignId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .take(256)
        .collect();
    let upstream_status: String = result
        .get("upstreamStatus")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .take(64)
        .collect();
    let window = result.get("businessWindow").cloned().unwrap_or(Value::Null);
    let start_at = window.get("startAt").and_then(Value::as_i64).unwrap_or(0);
    let end_at = window.get("endAt").and_then(Value::as_i64).unwrap_or(0);
    let business_day: String = result
        .get("businessDay")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .take(11)
        .collect();
    let upstream_checked_in = result
        .get("upstreamCheckedIn")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let status_available = result
        .get("statusAvailable")
        .and_then(Value::as_bool);
    let reason: String = result
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .take(500)
        .collect();

    let qoder_confirmed = (provider == "qoder" || provider == "qoder-intl")
        && evidence_type == "campaign_claim_status"
        && !campaign_id.trim().is_empty()
        && upstream_status == "CLAIMED"
        && start_at > 0
        && end_at > start_at;
    let workbuddy_confirmed = evidence_type == "checkin_status"
        && business_day.len() == 10
        && upstream_checked_in
        && upstream_status == "CHECKED_IN"
        && ((provider == "workbuddy") || (provider == "workbuddy-intl" && status_available == Some(true)));
    let confirmed = verdict == "confirmed"
        && observed_at > 0
        && (qoder_confirmed || workbuddy_confirmed);
    if verdict == "confirmed" && !confirmed {
        return Err("confirmed 对账证据缺少必要字段或不满足 Provider 专属证据契约".to_string());
    }

    let evidence_json = serde_json::json!({
        "campaignId": if campaign_id.is_empty() { Value::Null } else { serde_json::json!(campaign_id) },
        "businessWindow": if start_at > 0 && end_at > start_at {
            serde_json::json!({"startAt": start_at, "endAt": end_at})
        } else { Value::Null },
        "businessDay": if business_day.is_empty() { Value::Null } else { serde_json::json!(business_day) },
        "upstreamCheckedIn": if result.get("upstreamCheckedIn").is_some() { serde_json::json!(upstream_checked_in) } else { Value::Null },
        "statusAvailable": status_available.map(Value::Bool).unwrap_or(Value::Null),
        "upstreamStatus": if upstream_status.is_empty() { Value::Null } else { serde_json::json!(upstream_status) },
        "reason": reason,
    }).to_string();

    with_db(db, |conn| {
        let tx = conn.transaction()?;
        let stored_context: Option<(String, i64)> = tx.query_row(
            "SELECT provider, started_at FROM checkin_run_accounts
             WHERE run_id = ?1 AND account_id = ?2 AND state = 'uncertain'",
            params![run_id, account_id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        let Some((stored_provider, account_started_at_ms)) = stored_context else {
            return Err(rusqlite::Error::InvalidQuery);
        };
        if stored_provider != provider {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if confirmed {
            if account_started_at_ms <= 0 || observed_at < account_started_at_ms {
                return Err(rusqlite::Error::InvalidQuery);
            }
            if qoder_confirmed {
                let started_at_sec = account_started_at_ms.div_euclid(1000);
                if started_at_sec < start_at || started_at_sec > end_at {
                    return Err(rusqlite::Error::InvalidQuery);
                }
            }
            if workbuddy_confirmed {
                let run_day = crate::server::core::beijing::date_key_of_ms(account_started_at_ms);
                let observed_day = crate::server::core::beijing::date_key_of_ms(observed_at);
                if run_day.as_deref() != Some(business_day.as_str())
                    || observed_day.as_deref() != Some(business_day.as_str())
                {
                    return Err(rusqlite::Error::InvalidQuery);
                }
            }
        }
        let unresolved: bool = tx.query_row(
            "SELECT reconciled_at IS NULL FROM checkin_run_accounts
             WHERE run_id = ?1 AND account_id = ?2 AND state = 'uncertain'",
            params![run_id, account_id], |row| row.get(0),
        )?;
        let resolved = confirmed && unresolved;
        tx.execute(
            "INSERT INTO checkin_reconciliation_evidence
             (run_id, account_id, provider, verdict, evidence_type, observed_at, evidence_json, resolved)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![run_id, account_id, provider, verdict, evidence_type, observed_at, evidence_json, resolved],
        )?;
        let evidence_id = tx.last_insert_rowid();
        if resolved {
            let changed = tx.execute(
                "UPDATE checkin_run_accounts
                 SET reconciled_at = ?3, reconciliation_evidence_id = ?4
                 WHERE run_id = ?1 AND account_id = ?2
                   AND state = 'uncertain' AND reconciled_at IS NULL",
                params![run_id, account_id, observed_at, evidence_id],
            )?;
            if changed != 1 {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        tx.commit()?;
        Ok(serde_json::json!({
            "evidenceId": evidence_id,
            "verdict": verdict,
            "resolved": resolved,
            "originalState": ACCOUNT_UNCERTAIN,
            "isolationReleased": resolved,
        }))
    })
}

/// 查询某条执行记录的只追加对账证据，供管理端审计展示。
pub fn query_reconciliation_evidence(
    db: &Db,
    run_id: &str,
    account_id: &str,
    limit: usize,
    offset: usize,
) -> Result<Value, String> {
    let limit = limit.clamp(1, 500) as i64;
    let offset = offset.min(i64::MAX as usize) as i64;
    db.with(|conn| -> Result<Value, rusqlite::Error> {
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM checkin_reconciliation_evidence
             WHERE run_id = ?1 AND account_id = ?2",
            params![run_id, account_id], |row| row.get(0),
        )?;
        let mut statement = conn.prepare(
            "SELECT evidence_id, provider, verdict, evidence_type, observed_at, evidence_json, resolved
             FROM checkin_reconciliation_evidence
             WHERE run_id = ?1 AND account_id = ?2 ORDER BY evidence_id ASC LIMIT ?3 OFFSET ?4",
        )?;
        let rows = statement.query_map(params![run_id, account_id, limit, offset], |row| {
            let evidence_json: String = row.get(5)?;
            let details: Value = serde_json::from_str(&evidence_json).unwrap_or(Value::Null);
            Ok(serde_json::json!({
                "evidenceId": row.get::<_, i64>(0)?,
                "provider": row.get::<_, String>(1)?,
                "verdict": row.get::<_, String>(2)?,
                "evidenceType": row.get::<_, String>(3)?,
                "observedAt": row.get::<_, i64>(4)?,
                "details": details,
                "resolved": row.get::<_, bool>(6)?,
            }))
        })?;
        let entries = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(serde_json::json!({ "runId": run_id, "accountId": account_id, "entries": entries, "total": total }))
    })
    .ok_or_else(|| "签到执行台账数据库连接不可用".to_string())?
    .map_err(|error| format!("查询签到对账证据失败: {error}"))
}

/// 读取已正向解决、但签到历史尚未完成投影的记录。
fn pending_reconciliation_projections(
    db: &Db,
) -> Result<Vec<PendingReconciliationProjection>, String> {
    db.with(|conn| -> Result<Vec<PendingReconciliationProjection>, rusqlite::Error> {
        let mut statement = conn.prepare(
            "SELECT a.run_id, a.account_id, a.account_name, a.provider,
                    a.reconciliation_evidence_id, a.started_at
             FROM checkin_run_accounts a
             JOIN checkin_reconciliation_evidence e
               ON e.evidence_id = a.reconciliation_evidence_id
              AND e.run_id = a.run_id AND e.account_id = a.account_id
             WHERE a.state = 'uncertain' AND a.reconciled_at IS NOT NULL
               AND a.reconciliation_projected_at IS NULL
               AND e.verdict = 'confirmed' AND e.resolved = 1
             ORDER BY a.reconciled_at ASC, a.run_id ASC, a.ordinal ASC",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(PendingReconciliationProjection {
                    run_id: row.get(0)?,
                    account_id: row.get(1)?,
                    account_name: row.get(2)?,
                    provider: row.get(3)?,
                    evidence_id: row.get(4)?,
                    account_started_at: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .ok_or_else(|| "签到执行台账数据库连接不可用".to_string())?
    .map_err(|error| format!("读取待投影对账记录失败: {error}"))
}

/// 标记对账历史已投影。历史表以 (run_id, account_id) 幂等，故崩溃后可安全重放。
pub fn mark_reconciliation_projected(
    db: &Db,
    run_id: &str,
    account_id: &str,
    evidence_id: i64,
    projected_at: i64,
) -> Result<bool, String> {
    with_db(db, |conn| {
        let changed = conn.execute(
            "UPDATE checkin_run_accounts
             SET reconciliation_projected_at = ?4
             WHERE run_id = ?1 AND account_id = ?2 AND state = 'uncertain'
               AND reconciliation_evidence_id = ?3 AND reconciled_at IS NOT NULL
               AND reconciliation_projected_at IS NULL
               AND EXISTS (SELECT 1 FROM checkin_reconciliation_evidence e
                           WHERE e.evidence_id = ?3 AND e.run_id = ?1
                             AND e.account_id = ?2 AND e.verdict = 'confirmed'
                             AND e.resolved = 1)",
            params![run_id, account_id, evidence_id, projected_at],
        )?;
        Ok(changed == 1)
    })
}

/// 把所有已确认但尚未投影的对账证据恢复到账号状态与签到历史。
///
/// 账号 checkinAt 写入可重复；历史行由 (run_id, account_id) 唯一索引幂等；
/// 最后才标记投影完成。因此在任意一步崩溃，下一次调用都能安全续跑。
pub fn project_pending_reconciliation_history(
    db: &Db,
    store: &crate::server::core::account_store::AccountStore,
) -> Result<usize, String> {
    let existing_accounts: HashSet<String> = store
        .list_accounts()
        .get("accounts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|account| account.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let pending = pending_reconciliation_projections(db)?;
    let mut projected = 0usize;
    for item in pending {
        if existing_accounts.contains(&item.account_id)
            && !store.mark_checkin(&item.account_id, item.account_started_at)
        {
            return Err(format!(
                "恢复对账签到时间失败（账号 {}）；原 uncertain 记录与证据仍保留",
                item.account_id
            ));
        }
        crate::server::core::checkin_records::record_reconciliation_for_run_at(
            db,
            &item.run_id,
            &item.account_id,
            &item.account_name,
            &item.provider,
            item.evidence_id,
            item.account_started_at,
        )?;
        mark_reconciliation_projected(
            db,
            &item.run_id,
            &item.account_id,
            item.evidence_id,
            crate::server::logging::now_ms(),
        )?;
        projected += 1;
    }
    Ok(projected)
}

/// 启动时恢复已保存结果，并隔离没有结果快照的遗留请求。
///
/// 顺序很重要：先把 in_flight 改成 uncertain（绝不重放 Provider），再将
/// result_saved 做本地幂等投影，最后关闭遗留批次。任何一步失败都返回错误；
/// 未关闭的 running 批次会阻止后续签到入口启动新请求。
pub fn recover_startup(
    db: &Db,
    store: &crate::server::core::account_store::AccountStore,
) -> Result<RecoveryReport, String> {
    let (in_flight, saved) = db
        .with(|conn| -> Result<(Vec<(String, String)>, Vec<SavedAccountResult>), rusqlite::Error> {
            let mut in_flight_stmt = conn.prepare(
                "SELECT run_id, account_id FROM checkin_run_accounts WHERE state = 'in_flight'",
            )?;
            let in_flight = in_flight_stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;

            let mut saved_stmt = conn.prepare(
                "SELECT a.run_id, a.account_id, a.provider, r.source, r.reason,
                        a.result_saved_at, a.result_json
                 FROM checkin_run_accounts a
                 JOIN checkin_runs r ON r.run_id = a.run_id
                 WHERE a.state = 'result_saved'
                 ORDER BY r.started_at, a.ordinal",
            )?;
            let saved = saved_stmt
                .query_map([], |row| {
                    Ok(SavedAccountResult {
                        run_id: row.get(0)?,
                        account_id: row.get(1)?,
                        provider: row.get(2)?,
                        source: row.get(3)?,
                        reason: row.get(4)?,
                        result_saved_at: row.get(5)?,
                        result_json: row.get(6)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok((in_flight, saved))
        })
        .ok_or_else(|| "签到执行台账数据库连接不可用".to_string())?
        .map_err(|error| format!("读取签到恢复任务失败: {error}"))?;

    let mut report = RecoveryReport::default();
    for (run_id, account_id) in in_flight {
        if mark_uncertain(
            db,
            &run_id,
            &account_id,
            "进程退出时 Provider 请求没有可靠结果；为避免重复领取，已隔离且不会自动重试",
        )? {
            report.uncertain += 1;
        }
    }

    let existing_accounts: HashSet<String> = store
        .list_accounts()
        .get("accounts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|account| account.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();

    for saved in saved {
        let row: Value = serde_json::from_str(&saved.result_json)
            .map_err(|error| format!("签到结果快照损坏（{} / {}）：{error}", saved.run_id, saved.account_id))?;
        // 账号仍存在时恢复账号页 checkinAt；已被用户删除的账号不再写账号表，
        // 但仍保留执行结果历史，避免删除账号导致已发生的远端结果失去审计记录。
        if existing_accounts.contains(&saved.account_id)
            && crate::server::core::billing::checkin::day_completed(&row)
            && !store.mark_checkin(&saved.account_id, saved.result_saved_at)
        {
            return Err(format!("恢复签到时间失败（账号 {}）；执行结果仍保留在台账", saved.account_id));
        }
        if saved.source == "activity" {
            // 活动入口把 mode 保存在原始结果快照中；恢复必须沿用原 mode，
            // 尤其 keepalive 的 outcome 判据与普通 claim 不同。
            let mode = row.get("mode").and_then(Value::as_str).unwrap_or("full");
            crate::server::core::checkin_records::record_activity_for_run_at(
                &row,
                &saved.provider,
                mode,
                Some(&saved.run_id),
                Some(saved.result_saved_at),
            )?;
        } else {
            crate::server::core::checkin_records::record_row_for_run_at(
                &row,
                &saved.provider,
                &saved.source,
                &saved.reason,
                Some(&saved.run_id),
                Some(saved.result_saved_at),
            )?;
        }
        mark_projected(db, &saved.run_id, &saved.account_id, crate::server::logging::now_ms())?;
        report.projected += 1;
    }

    report.projected += project_pending_reconciliation_history(db, store)?;

    let open_runs = db
        .with(|conn| -> Result<Vec<(String, i64, i64, i64, String, String)>, rusqlite::Error> {
            let mut statement = conn.prepare(
                "SELECT run_id, target_count, skipped_count, started_at, source, reason
                 FROM checkin_runs WHERE status = 'running' ORDER BY started_at",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .ok_or_else(|| "签到执行台账数据库连接不可用".to_string())?
        .map_err(|error| format!("读取遗留签到批次失败: {error}"))?;

    for (run_id, target_count, skipped_count, started_at, source, reason) in open_runs {
        let accounts = db
            .with(|conn| -> Result<Vec<(String, String, Option<String>)>, rusqlite::Error> {
                let mut statement = conn.prepare(
                    "SELECT account_name, state, result_json FROM checkin_run_accounts
                     WHERE run_id = ?1 ORDER BY ordinal",
                )?;
                let rows = statement
                    .query_map([&run_id], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .ok_or_else(|| "签到执行台账数据库连接不可用".to_string())?
            .map_err(|error| format!("读取遗留批次账号失败（{run_id}）：{error}"))?;

        let mut succeeded = 0usize;
        let mut active = 0usize;
        let mut failures = Vec::new();
        let mut resolved_count = 0usize;
        for (account_name, state, result_json) in &accounts {
            if let Some(result_json) = result_json {
                let row: Value = serde_json::from_str(result_json)
                    .map_err(|error| format!("签到结果快照损坏（{run_id}）：{error}"))?;
                resolved_count += 1;
                if row.get("claim").is_some_and(
                    crate::server::core::billing::checkin::checkin_completed_today,
                ) {
                    succeeded += 1;
                }
                if row
                    .get("activity")
                    .and_then(|activity| activity.get("pokeSucceeded"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || crate::server::core::billing::checkin::activity_report_ok(&row)
                    || crate::server::core::billing::checkin::web_conversation_ok(&row)
                {
                    active += 1;
                }
                if let Some(error) = row.get("error").and_then(Value::as_str) {
                    failures.push(format!("{}（{}）", account_name, error));
                }
            } else if state == ACCOUNT_UNCERTAIN {
                failures.push(format!("{}（结果不确定，未自动重试）", account_name));
            }
        }
        let finished_at = crate::server::logging::now_ms();
        let date = db
            .with(|conn| {
                conn.query_row(
                    "SELECT date(?1 / 1000, 'unixepoch', '+8 hours')",
                    [started_at],
                    |row| row.get::<_, String>(0),
                )
            })
            .ok_or_else(|| "签到执行台账数据库连接不可用".to_string())?
            .map_err(|error| format!("计算签到批次日期失败（{run_id}）：{error}"))?;
        let summary = serde_json::json!({
            "at": started_at,
            "date": date,
            "reason": reason,
            "succeeded": succeeded,
            "active": active,
            "total": accounts.len(),
            "skipped": skipped_count,
            "cancelled": false,
            "failed": failures.iter().take(5).cloned().collect::<Vec<_>>(),
            "failedCount": failures.len(),
            "interrupted": true,
        });
        let summary_json = serde_json::to_string(&summary)
            .map_err(|error| format!("遗留签到批次汇总无法序列化（{run_id}）：{error}"))?;
        finish_run_with_summary(
            db,
            &run_id,
            finished_at,
            RUN_INTERRUPTED,
            if target_count > 0 { target_count } else { accounts.len() as i64 },
            resolved_count as i64,
            skipped_count,
            if source == "activity" || source == "single" {
                None
            } else {
                Some(&summary_json)
            },
        )?;
        report.interrupted_runs += 1;
    }

    // 批次已关闭但时间线尚未写入（例如进程在 finish_run 后被杀）：从批次
    // 汇总快照补写。checkin_history 按 run_id 去重，写入成功后再标记完成。
    let pending_history = db
        .with(|conn| -> Result<Vec<(String, String)>, rusqlite::Error> {
            let mut statement = conn.prepare(
                "SELECT run_id, summary_json FROM checkin_runs
                 WHERE status != 'running' AND summary_json IS NOT NULL
                   AND history_projected = 0 ORDER BY finished_at, started_at",
            )?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .ok_or_else(|| "签到执行台账数据库连接不可用".to_string())?
        .map_err(|error| format!("读取待补签到时间线失败: {error}"))?;
    for (run_id, summary_json) in pending_history {
        let summary: Value = serde_json::from_str(&summary_json)
            .map_err(|error| format!("签到批次汇总快照损坏（{run_id}）：{error}"))?;
        let reason = summary.get("reason").and_then(Value::as_str).unwrap_or("恢复补账");
        crate::server::core::checkin_history::record_for_run(&summary, &run_id, reason)?;
        mark_history_projected(db, &run_id)?;
    }
    project_pending_auto_results(db)?;
    Ok(report)
}

/// 将已关闭 AutoCheckin 批次的摘要幂等投影到兼容配置视图。
///
/// SQLite 执行台账是事实来源；`autoCheckin.lastResult` 和 schedule.lastResult
/// 只是 UI 兼容投影。两处写入都成功后才设置完成标记，任何中断都可在下次
/// 启动/签到前重放本地写入，不涉及 Provider 请求。
pub fn project_pending_auto_results(db: &Db) -> Result<usize, String> {
    let pending = db
        .with(|conn| -> Result<Vec<(String, Option<String>, String)>, rusqlite::Error> {
            let mut statement = conn.prepare(
                "SELECT run_id, schedule_id, summary_json FROM checkin_runs
                 WHERE status != 'running' AND auto_checkin = 1
                   AND auto_result_projected = 0 AND summary_json IS NOT NULL
                 ORDER BY finished_at, started_at",
            )?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .ok_or_else(|| "签到执行台账数据库连接不可用".to_string())?
        .map_err(|error| format!("读取待恢复自动签到摘要失败: {error}"))?;

    let mut projected = 0;
    for (run_id, schedule_id, summary_json) in pending {
        let summary: Value = serde_json::from_str(&summary_json)
            .map_err(|error| format!("自动签到摘要快照损坏（{run_id}）：{error}"))?;
        crate::server::core::auto_checkin::project_result(&summary)?;
        if let Some(schedule_id) = schedule_id.as_deref() {
            crate::server::core::checkin_schedules::mark_result_checked(schedule_id, Some(summary.clone()))?;
        }
        let changed = with_db(db, |conn| {
            conn.execute(
                "UPDATE checkin_runs SET auto_result_projected = 1
                 WHERE run_id = ?1 AND auto_checkin = 1 AND auto_result_projected = 0",
                [&run_id],
            )
        })?;
        if changed == 1 {
            projected += 1;
        }
    }
    Ok(projected)
}

/// 若启动恢复留下 result_saved / in_flight，禁止开启新 Provider 请求。
pub fn ensure_recovered(db: &Db) -> Result<(), String> {
    let unresolved = db
        .with(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM checkin_runs WHERE status = 'running'",
                [],
                |row| row.get::<_, i64>(0),
            )
        })
        .ok_or_else(|| "签到执行台账数据库连接不可用".to_string())?
        .map_err(|error| format!("检查签到恢复状态失败: {error}"))?;
    if unresolved > 0 {
        return Err(format!("存在 {unresolved} 个尚未恢复的签到批次，已阻止新的上游请求"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::db::test_temp::TempDb;

    fn seed_run(db: &Db) {
        create_run(
            db,
            &RunInput {
                run_id: "run-1",
                started_at: 100,
                source: "manual",
                reason: "unit test",
                target_count: 1,
                skipped_count: 0,
                schedule_id: None,
                auto_checkin: false,
            },
        )
        .expect("run should be created");
        begin_account(
            db,
            &AccountInput {
                run_id: "run-1",
                account_id: "account-1",
                account_name: "Account",
                provider: "workbuddy",
                ordinal: 0,
                started_at: 101,
            },
        )
        .expect("account intent should be committed");
    }

    #[test]
    fn auto_checkin_run_persists_schedule_projection_target() {
        let (db, _guard) = TempDb::open("checkin-ledger-auto-metadata");
        create_run(
            &db,
            &RunInput {
                run_id: "auto-run",
                started_at: crate::server::logging::now_ms(),
                source: "schedule",
                reason: "scheduled",
                target_count: 2,
                skipped_count: 0,
                schedule_id: Some("morning"),
                auto_checkin: true,
            },
        )
        .expect("auto run should be created");
        let metadata = db
            .with(|conn| {
                conn.query_row(
                    "SELECT schedule_id, auto_checkin, auto_result_projected
                     FROM checkin_runs WHERE run_id = 'auto-run'",
                    [],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
                )
            })
            .expect("database should be available")
            .expect("metadata should exist");
        assert_eq!(metadata, (Some("morning".to_string()), 1, 0));
    }

    #[test]
    fn uncertain_account_is_isolated_for_today_and_recovery_gate_closes() {
        let (db, _guard) = TempDb::open("checkin-ledger-uncertain-today");
        let now = crate::server::logging::now_ms();
        create_run(
            &db,
            &RunInput {
                run_id: "run-uncertain",
                started_at: now,
                source: "manual",
                reason: "unit test",
                target_count: 1,
                skipped_count: 0,
                schedule_id: None,
                auto_checkin: false,
            },
        )
        .expect("run should be created");
        begin_account(
            &db,
            &AccountInput {
                run_id: "run-uncertain",
                account_id: "account-uncertain",
                account_name: "Account",
                provider: "workbuddy",
                ordinal: 0,
                started_at: now,
            },
        )
        .expect("account intent should be committed");
        assert!(ensure_recovered(&db).is_err());
        assert!(mark_uncertain(
            &db,
            "run-uncertain",
            "account-uncertain",
            "test uncertainty",
        )
        .expect("uncertain transition"));
        assert!(uncertain_accounts_today(&db)
            .expect("uncertain account query")
            .contains("account-uncertain"));
        finish_run(
            &db,
            "run-uncertain",
            now + 1,
            RUN_INTERRUPTED,
            1,
            1,
            0,
        )
        .expect("interrupted run should close after isolation");
        ensure_recovered(&db).expect("closed interrupted run must not block other accounts");
    }

    #[test]
    fn result_save_and_projection_are_idempotent() {
        let (db, _guard) = TempDb::open("checkin-ledger-idempotent");
        seed_run(&db);
        assert!(save_result(&db, "run-1", "account-1", 102, r#"{"ok":true}"#)
            .expect("first result save"));
        assert!(!save_result(&db, "run-1", "account-1", 103, r#"{"ok":true}"#)
            .expect("same result replay is idempotent"));
        assert!(mark_projected(&db, "run-1", "account-1", 104)
            .expect("projection should advance"));
        assert!(!mark_projected(&db, "run-1", "account-1", 105)
            .expect("projection replay is idempotent"));
        finish_run(&db, "run-1", 106, RUN_COMPLETED, 1, 1, 0)
            .expect("fully projected run should close");
    }

    #[test]
    fn batch_summary_is_committed_with_terminal_state_and_projection_marker_is_retryable() {
        let (db, _guard) = TempDb::open("checkin-ledger-summary-durable");
        seed_run(&db);
        save_result(&db, "run-1", "account-1", 102, r#"{"ok":true}"#)
            .expect("result should be saved");
        mark_projected(&db, "run-1", "account-1", 103)
            .expect("account projection should complete");
        let summary = r#"{"at":104,"reason":"manual","succeeded":1}"#;
        finish_run_with_summary(
            &db, "run-1", 104, RUN_COMPLETED, 1, 1, 0, Some(summary),
        )
        .expect("terminal state and summary should commit together");
        let stored: (String, i64) = db
            .with(|conn| {
                conn.query_row(
                    "SELECT summary_json, history_projected FROM checkin_runs WHERE run_id = 'run-1'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
            })
            .expect("database should be available")
            .expect("summary should be readable");
        assert_eq!(stored.0, summary);
        assert_eq!(stored.1, 0);
        assert!(mark_history_projected(&db, "run-1").expect("first marker update"));
        assert!(!mark_history_projected(&db, "run-1").expect("marker replay is idempotent"));
    }

    #[test]
    fn uncertain_query_returns_auditable_metadata_without_result_payload() {
        let (db, _guard) = TempDb::open("checkin-ledger-uncertain-query");
        let now = crate::server::logging::now_ms();
        create_run(
            &db,
            &RunInput {
                run_id: "run-review",
                started_at: now,
                source: "manual",
                reason: "user initiated",
                target_count: 1,
                skipped_count: 0,
                schedule_id: None,
                auto_checkin: false,
            },
        ).expect("run should be created");
        begin_account(
            &db,
            &AccountInput {
                run_id: "run-review",
                account_id: "account-review",
                account_name: "Review Account",
                provider: "workbuddy",
                ordinal: 0,
                started_at: now + 1,
            },
        ).expect("account intent should be committed");
        mark_uncertain(&db, "run-review", "account-review", "response was lost")
            .expect("uncertain transition should succeed");

        let page = query_uncertain(&db, 10, 0).expect("query should succeed");
        assert_eq!(page["total"], 1);
        assert_eq!(page["entries"][0]["runId"], "run-review");
        assert_eq!(page["entries"][0]["accountId"], "account-review");
        assert_eq!(page["entries"][0]["provider"], "workbuddy");
        assert_eq!(page["entries"][0]["source"], "manual");
        assert_eq!(page["entries"][0]["autoCheckin"], false);
        assert_eq!(page["entries"][0]["automaticRetry"], false);
        assert!(page["entries"][0].get("resultJson").is_none());
    }

    #[test]
    fn qoder_confirmed_evidence_is_append_only_and_releases_isolation_atomically() {
        let (db, _guard) = TempDb::open("checkin-ledger-qoder-evidence");
        let now = crate::server::logging::now_ms();
        create_run(&db, &RunInput {
            run_id: "qoder-run", started_at: now, source: "manual", reason: "test",
            target_count: 1, skipped_count: 0, schedule_id: None, auto_checkin: false,
        }).expect("run created");
        begin_account(&db, &AccountInput {
            run_id: "qoder-run", account_id: "qoder-account", account_name: "Qoder",
            provider: "qoder-intl", ordinal: 0, started_at: now,
        }).expect("account started");
        mark_uncertain(&db, "qoder-run", "qoder-account", "response lost")
            .expect("mark uncertain");
        assert!(uncertain_accounts_today(&db).expect("isolation query").contains("qoder-account"));

        let window_start = now.div_euclid(1000) - 60;
        let window_end = now.div_euclid(1000) + 60;
        let unknown = serde_json::json!({
            "verdict": "unknown", "provider": "qoder-intl",
            "evidenceType": "campaign_claim_status", "observedAt": now,
            "campaignId": "campaign-1", "upstreamStatus": "CLAIMABLE",
            "businessWindow": {"startAt": window_start, "endAt": window_end}, "reason": "not claimed",
        });
        let first = record_reconciliation_evidence(&db, "qoder-run", "qoder-account", &unknown)
            .expect("unknown evidence stored");
        assert_eq!(first["resolved"], false);
        assert!(uncertain_accounts_today(&db).expect("still isolated").contains("qoder-account"));

        let confirmed = serde_json::json!({
            "verdict": "confirmed", "provider": "qoder-intl",
            "evidenceType": "campaign_claim_status", "observedAt": now + 1,
            "campaignId": "campaign-1", "upstreamStatus": "CLAIMED",
            "businessWindow": {"startAt": window_start, "endAt": window_end}, "reason": "claimed",
        });
        let second = record_reconciliation_evidence(&db, "qoder-run", "qoder-account", &confirmed)
            .expect("confirmed evidence stored");
        assert_eq!(second["resolved"], true);
        assert!(!uncertain_accounts_today(&db).expect("isolation released").contains("qoder-account"));

        let audit = query_reconciliation_evidence(&db, "qoder-run", "qoder-account", 100, 0)
            .expect("audit query");
        assert_eq!(audit["entries"].as_array().map(Vec::len), Some(2));
        let immutable = db.with(|conn| conn.execute(
            "UPDATE checkin_reconciliation_evidence SET verdict = 'unknown' WHERE evidence_id = ?1",
            [second["evidenceId"].as_i64().unwrap_or_default()],
        )).expect("db available");
        assert!(immutable.is_err(), "evidence updates must be rejected by trigger");
    }

    #[test]
    fn workbuddy_cn_and_intl_status_evidence_release_only_same_day_uncertainty() {
        for (provider, account_id, region, status) in [
            (
                "workbuddy",
                "workbuddy-cn-account",
                crate::server::core::providers::workbuddy::Region::Cn,
                serde_json::json!({"checkedIn": true}),
            ),
            (
                "workbuddy-intl",
                "workbuddy-intl-account",
                crate::server::core::providers::workbuddy::Region::Intl,
                serde_json::json!({"statusAvailable": true, "todayCheckedIn": true}),
            ),
        ] {
            let (db, _guard) = TempDb::open(&format!("checkin-ledger-{provider}-evidence"));
            let now = crate::server::logging::now_ms();
            let run_id = format!("{provider}-run");
            create_run(&db, &RunInput {
                run_id: &run_id, started_at: now, source: "manual", reason: "test",
                target_count: 1, skipped_count: 0, schedule_id: None, auto_checkin: false,
            }).expect("run created");
            begin_account(&db, &AccountInput {
                run_id: &run_id, account_id, account_name: "WorkBuddy",
                provider, ordinal: 0, started_at: now,
            }).expect("account started");
            mark_uncertain(&db, &run_id, account_id, "response lost").expect("mark uncertain");

            let evidence = crate::server::core::providers::workbuddy::reconcile::classify_status(
                region, now, now, &status,
            );
            assert_eq!(evidence["verdict"], "confirmed");
            let audit = record_reconciliation_evidence(&db, &run_id, account_id, &evidence)
                .expect("valid WorkBuddy status evidence should be stored");
            assert_eq!(audit["resolved"], true);
            assert!(!uncertain_accounts_today(&db).expect("isolation query").contains(account_id));
        }
    }

    #[test]
    fn workbuddy_intl_storage_requires_status_available_true() {
        let (db, _guard) = TempDb::open("checkin-ledger-workbuddy-intl-unavailable");
        let now = crate::server::logging::now_ms();
        create_run(&db, &RunInput {
            run_id: "workbuddy-intl-run", started_at: now, source: "manual", reason: "test",
            target_count: 1, skipped_count: 0, schedule_id: None, auto_checkin: false,
        }).expect("run created");
        begin_account(&db, &AccountInput {
            run_id: "workbuddy-intl-run", account_id: "workbuddy-intl-account", account_name: "WorkBuddy Intl",
            provider: "workbuddy-intl", ordinal: 0, started_at: now,
        }).expect("account started");
        mark_uncertain(&db, "workbuddy-intl-run", "workbuddy-intl-account", "response lost")
            .expect("mark uncertain");
        let evidence = serde_json::json!({
            "verdict": "confirmed", "provider": "workbuddy-intl",
            "evidenceType": "checkin_status", "observedAt": now,
            "businessDay": crate::server::core::beijing::date_key_of_ms(now).unwrap_or_default(),
            "upstreamCheckedIn": true, "upstreamStatus": "CHECKED_IN",
            "reason": "missing status availability flag",
        });
        assert!(record_reconciliation_evidence(
            &db, "workbuddy-intl-run", "workbuddy-intl-account", &evidence,
        ).is_err());
    }

    #[test]
    fn workbuddy_status_evidence_for_another_day_cannot_resolve_uncertainty() {
        let (db, _guard) = TempDb::open("checkin-ledger-workbuddy-cross-day");
        let now = crate::server::logging::now_ms();
        let previous_day = now.saturating_sub(24 * 60 * 60 * 1000);
        create_run(&db, &RunInput {
            run_id: "workbuddy-old-run", started_at: previous_day, source: "manual", reason: "test",
            target_count: 1, skipped_count: 0, schedule_id: None, auto_checkin: false,
        }).expect("run created");
        begin_account(&db, &AccountInput {
            run_id: "workbuddy-old-run", account_id: "workbuddy-old-account", account_name: "WorkBuddy",
            provider: "workbuddy", ordinal: 0, started_at: previous_day,
        }).expect("account started");
        mark_uncertain(&db, "workbuddy-old-run", "workbuddy-old-account", "response lost")
            .expect("mark uncertain");

        let evidence = crate::server::core::providers::workbuddy::reconcile::classify_status(
            crate::server::core::providers::workbuddy::Region::Cn,
            previous_day,
            now,
            &serde_json::json!({"checkedIn": true}),
        );
        assert_eq!(evidence["verdict"], "unknown");
        let audit = record_reconciliation_evidence(
            &db, "workbuddy-old-run", "workbuddy-old-account", &evidence,
        ).expect("unknown evidence should be append-only stored");
        assert_eq!(audit["resolved"], false);
        assert!(uncertain_accounts_today(&db).expect("isolation query").is_empty());
    }

    #[test]
    fn workbuddy_storage_rejects_forged_confirmation_for_another_business_day() {
        let (db, _guard) = TempDb::open("checkin-ledger-workbuddy-forged-day");
        let now = crate::server::logging::now_ms();
        let previous_day = now.saturating_sub(24 * 60 * 60 * 1000);
        create_run(&db, &RunInput {
            run_id: "workbuddy-forged-run", started_at: previous_day, source: "manual", reason: "test",
            target_count: 1, skipped_count: 0, schedule_id: None, auto_checkin: false,
        }).expect("run created");
        begin_account(&db, &AccountInput {
            run_id: "workbuddy-forged-run", account_id: "workbuddy-forged-account", account_name: "WorkBuddy",
            provider: "workbuddy", ordinal: 0, started_at: previous_day,
        }).expect("account started");
        mark_uncertain(&db, "workbuddy-forged-run", "workbuddy-forged-account", "response lost")
            .expect("mark uncertain");

        let evidence = serde_json::json!({
            "verdict": "confirmed",
            "provider": "workbuddy",
            "evidenceType": "checkin_status",
            "observedAt": now,
            "businessDay": crate::server::core::beijing::date_key_of_ms(now).unwrap_or_default(),
            "upstreamCheckedIn": true,
            "upstreamStatus": "CHECKED_IN",
            "reason": "forged current-day status",
        });
        assert!(record_reconciliation_evidence(
            &db, "workbuddy-forged-run", "workbuddy-forged-account", &evidence,
        ).is_err());
        let audit = query_reconciliation_evidence(
            &db, "workbuddy-forged-run", "workbuddy-forged-account", 10, 0,
        ).expect("audit query");
        assert_eq!(audit["total"], 0);
    }

    #[test]
    fn confirmed_evidence_without_complete_qoder_fields_is_rejected() {
        let (db, _guard) = TempDb::open("checkin-ledger-invalid-evidence");
        let now = crate::server::logging::now_ms();
        create_run(&db, &RunInput {
            run_id: "qoder-run", started_at: now, source: "manual", reason: "test",
            target_count: 1, skipped_count: 0, schedule_id: None, auto_checkin: false,
        }).expect("run created");
        begin_account(&db, &AccountInput {
            run_id: "qoder-run", account_id: "qoder-account", account_name: "Qoder",
            provider: "qoder", ordinal: 0, started_at: now,
        }).expect("account started");
        mark_uncertain(&db, "qoder-run", "qoder-account", "response lost")
            .expect("mark uncertain");
        let invalid = serde_json::json!({
            "verdict": "confirmed", "provider": "qoder", "evidenceType": "campaign_claim_status",
            "observedAt": now, "upstreamStatus": "CLAIMED",
        });
        assert!(record_reconciliation_evidence(&db, "qoder-run", "qoder-account", &invalid).is_err());
        let mismatched_window = serde_json::json!({
            "verdict": "confirmed", "provider": "qoder", "evidenceType": "campaign_claim_status",
            "observedAt": now, "campaignId": "campaign-1", "upstreamStatus": "CLAIMED",
            "businessWindow": {"startAt": 1, "endAt": 2},
        });
        assert!(record_reconciliation_evidence(&db, "qoder-run", "qoder-account", &mismatched_window).is_err());
        assert!(uncertain_accounts_today(&db).expect("isolation query").contains("qoder-account"));
    }

    #[test]
    fn reconciliation_projection_marker_is_evidence_bound_and_idempotent() {
        let (db, _guard) = TempDb::open("checkin-ledger-reconciliation-projection");
        let now = crate::server::logging::now_ms();
        create_run(&db, &RunInput {
            run_id: "qoder-projection-run", started_at: now, source: "manual", reason: "test",
            target_count: 1, skipped_count: 0, schedule_id: None, auto_checkin: false,
        }).expect("run created");
        begin_account(&db, &AccountInput {
            run_id: "qoder-projection-run", account_id: "qoder-projection-account",
            account_name: "Qoder", provider: "qoder", ordinal: 0, started_at: now,
        }).expect("account started");
        mark_uncertain(&db, "qoder-projection-run", "qoder-projection-account", "response lost")
            .expect("mark uncertain");
        let evidence = serde_json::json!({
            "verdict": "confirmed", "provider": "qoder", "evidenceType": "campaign_claim_status",
            "observedAt": now + 1, "campaignId": "campaign-1", "upstreamStatus": "CLAIMED",
            "businessWindow": {"startAt": now.div_euclid(1000) - 10, "endAt": now.div_euclid(1000) + 10},
        });
        let audit = record_reconciliation_evidence(
            &db, "qoder-projection-run", "qoder-projection-account", &evidence,
        ).expect("confirmed evidence persisted");
        let evidence_id = audit["evidenceId"].as_i64().expect("evidence id");
        let pending = pending_reconciliation_projections(&db).expect("pending projections");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].evidence_id, evidence_id);
        assert!(mark_reconciliation_projected(
            &db, "qoder-projection-run", "qoder-projection-account", evidence_id, now + 2,
        ).expect("first marker"));
        assert!(!mark_reconciliation_projected(
            &db, "qoder-projection-run", "qoder-projection-account", evidence_id, now + 3,
        ).expect("duplicate marker"));
        assert!(pending_reconciliation_projections(&db).expect("no pending").is_empty());
        assert!(mark_reconciliation_projected(
            &db, "qoder-projection-run", "qoder-projection-account", evidence_id + 1, now + 4,
        ).is_ok_and(|changed| !changed));
    }

    #[test]
    fn uncertain_transition_never_reopens_provider_attempt() {
        let (db, _guard) = TempDb::open("checkin-ledger-uncertain");
        seed_run(&db);
        assert!(mark_uncertain(&db, "run-1", "account-1", "process interrupted")
            .expect("in-flight attempt should be quarantined"));
        assert!(!mark_uncertain(&db, "run-1", "account-1", "again")
            .expect("uncertain transition should be idempotent"));
        finish_run(&db, "run-1", 107, RUN_INTERRUPTED, 1, 0, 0)
            .expect("interrupted run may retain uncertain evidence");
    }

    #[test]
    fn finish_rejects_an_account_without_a_durable_outcome() {
        let (db, _guard) = TempDb::open("checkin-ledger-unresolved");
        seed_run(&db);
        assert!(finish_run(&db, "run-1", 106, RUN_COMPLETED, 1, 0, 0).is_err());
    }

    #[test]
    fn crash_after_provider_response_keeps_saved_result_recoverable() {
        let (db, _guard) = TempDb::open("checkin-ledger-crash-after-response");
        seed_run(&db);

        // 模拟 Provider 已返回、result_saved 已提交，但进程在本地历史投影前退出。
        assert!(save_result(&db, "run-1", "account-1", 102, r#"{"ok":true}"#)
            .expect("durable response should be saved"));
        assert!(ensure_recovered(&db).is_err(), "running batch must gate new upstream work");
        assert!(finish_run(&db, "run-1", 103, RUN_INTERRUPTED, 1, 1, 0).is_err(),
            "a saved-but-unprojected result must not be hidden by closing the run");

        let state: (String, Option<i64>, Option<String>) = db
            .with(|conn| conn.query_row(
                "SELECT state, result_saved_at, result_json FROM checkin_run_accounts
                 WHERE run_id = 'run-1' AND account_id = 'account-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ))
            .expect("database should be available")
            .expect("account ledger row should exist");
        assert_eq!(state.0, ACCOUNT_RESULT_SAVED);
        assert_eq!(state.1, Some(102), "the first durable result timestamp is immutable");
        assert_eq!(state.2.as_deref(), Some(r#"{"ok":true}"#));

        assert!(!save_result(&db, "run-1", "account-1", 999, r#"{"ok":true}"#)
            .expect("replaying the same provider response must be idempotent"));
        assert!(mark_projected(&db, "run-1", "account-1", 104)
            .expect("recovery can finish the local projection"));
        finish_run(&db, "run-1", 105, RUN_COMPLETED, 1, 1, 0)
            .expect("run may close after projection is complete");
        ensure_recovered(&db).expect("closed and projected run must release the gate");
    }

    #[test]
    fn uncertain_account_cannot_be_reopened_or_filled_with_late_result() {
        let (db, _guard) = TempDb::open("checkin-ledger-uncertain-no-reopen");
        seed_run(&db);
        assert!(mark_uncertain(&db, "run-1", "account-1", "response lost")
            .expect("in-flight request should be quarantined"));

        assert!(save_result(&db, "run-1", "account-1", 102, r#"{"ok":true}"#).is_err(),
            "late result must not silently rewrite an uncertain attempt");
        assert!(mark_projected(&db, "run-1", "account-1", 103).is_err(),
            "uncertain attempt has no trusted result to project");
        assert!(!mark_uncertain(&db, "run-1", "account-1", "duplicate recovery")
            .expect("recovery replay must not create another transition"));

        let state: (String, Option<String>) = db
            .with(|conn| conn.query_row(
                "SELECT state, result_json FROM checkin_run_accounts
                 WHERE run_id = 'run-1' AND account_id = 'account-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ))
            .expect("database should be available")
            .expect("account ledger row should exist");
        assert_eq!(state.0, ACCOUNT_UNCERTAIN);
        assert!(state.1.is_none(), "uncertain state must not acquire an invented result");

        finish_run(&db, "run-1", 104, RUN_INTERRUPTED, 1, 0, 0)
            .expect("interrupted run may close while retaining uncertain evidence");
        ensure_recovered(&db).expect("closed uncertain run does not block unrelated accounts");
    }

    #[test]
    fn saved_result_cannot_be_silently_overwritten() {
        let (db, _guard) = TempDb::open("checkin-ledger-no-overwrite");
        seed_run(&db);
        save_result(&db, "run-1", "account-1", 102, r#"{"ok":true}"#)
            .expect("first result save");
        assert!(save_result(&db, "run-1", "account-1", 103, r#"{"ok":false}"#)
            .is_err());
    }
}
