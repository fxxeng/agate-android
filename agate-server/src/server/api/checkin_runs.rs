//! 签到执行台账的诊断与对账审计接口。
//!
//! GET /api/checkin-runs/uncertain 查询结果不确定、需要核对的账号执行记录。
//! 对账只允许 Provider 专属的只读状态探测，并把观察追加到本地证据表；
//! 不提供解除隔离或重放 Provider 的通用操作。只有通过存储层独立校验的
//! 正向证据，才可在事务内释放当日隔离；原始 uncertain 状态永久保留。

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::response::Response;

use crate::server::core::checkin_run_ledger;
use crate::server::http::ok_json;
use crate::server::ServerState;

fn parse_usize(value: Option<&String>, fallback: usize) -> usize {
    value
        .and_then(|text| text.trim().parse::<usize>().ok())
        .unwrap_or(fallback)
}

/// GET /api/checkin-runs/uncertain —— 只读查询待核对执行记录。
pub async fn query_uncertain(
    State(state): State<ServerState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let limit = parse_usize(params.get("limit"), 100).clamp(1, 500);
    let offset = parse_usize(params.get("offset"), 0);
    let Some(db) = state.store().db() else {
        return crate::server::errors::management_error(
            503,
            "签到执行台账数据库不可用，无法查询不确定记录",
        );
    };
    match checkin_run_ledger::query_uncertain(db, limit, offset) {
        Ok(result) => ok_json(result),
        Err(error) => crate::server::errors::management_error(503, &error),
    }
}

/// GET /api/checkin-runs/evidence?runId=...&accountId=...&limit=&offset= —— 查询已持久化的对账证据。
pub async fn query_evidence(
    State(state): State<ServerState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let run_id = params.get("runId").map(String::as_str).unwrap_or("").trim();
    let account_id = params.get("accountId").map(String::as_str).unwrap_or("").trim();
    if run_id.is_empty() || account_id.is_empty() {
        return crate::server::errors::management_error(400, "必须提供 runId 与 accountId");
    }
    let Some(db) = state.store().db() else {
        return crate::server::errors::management_error(503, "签到执行台账数据库不可用");
    };
    let limit = parse_usize(params.get("limit"), 100).clamp(1, 500);
    let offset = parse_usize(params.get("offset"), 0);
    match checkin_run_ledger::query_reconciliation_evidence(db, run_id, account_id, limit, offset) {
        Ok(result) => ok_json(result),
        Err(error) => crate::server::errors::management_error(503, &error),
    }
}

/// POST /api/checkin-runs/reconcile?runId=...&accountId=... —— 查询上游并持久化只追加证据。
pub async fn inspect_provider(
    State(state): State<ServerState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let run_id = params.get("runId").map(String::as_str).unwrap_or("").trim();
    let account_id = params.get("accountId").map(String::as_str).unwrap_or("").trim();
    if run_id.is_empty() || account_id.is_empty() {
        return crate::server::errors::management_error(400, "必须提供 runId 与 accountId");
    }
    let Some(db) = state.store().db() else {
        return crate::server::errors::management_error(
            503, "签到执行台账数据库不可用，无法核对不确定记录",
        );
    };
    let context = match checkin_run_ledger::uncertain_reconciliation_context(db, run_id, account_id) {
        Ok(Some(value)) => value,
        Ok(None) => return crate::server::errors::management_error(
            404, "找不到对应的 uncertain 签到记录，或该记录已不处于待核对状态",
        ),
        Err(error) => return crate::server::errors::management_error(503, &error),
    };
    let (provider, account_started_at_ms) = context;
    let mut result = if let Some(region) =
        crate::server::core::providers::qoder::endpoints::Region::from_provider_id(&provider)
    {
        match crate::server::core::providers::qoder::checkin::inspect_uncertain_claim(
            state.store(), region, account_id, account_started_at_ms,
        ).await {
            Ok(result) => result,
            Err(error) => serde_json::json!({
                "verdict": "unknown",
                "provider": provider,
                "evidenceType": "campaign_claim_status",
                "observedAt": crate::server::logging::now_ms(),
                "reason": format!("对账查询未能完成: {error}"),
            }),
        }
    } else if let Some(region) =
        crate::server::core::providers::workbuddy::Region::from_provider_id(&provider)
    {
        let status = match state.store().get_session_by_id(account_id) {
            Some(session)
                if session.session.get("edition").and_then(serde_json::Value::as_str)
                    == Some(region.id()) =>
            {
                state.billing().get_checkin_status(Some(&session.session)).await
            }
            Some(_) => Err(crate::server::core::billing::BillingError::new(
                "WorkBuddy 账号地区与 CheckinRun Provider 不一致，拒绝跨地区核验", 409,
            )),
            None => Err(crate::server::core::billing::BillingError::new(
                "WorkBuddy 账号会话不可用，无法读取签到状态", 404,
            )),
        };
        match status {
            Ok(status) => crate::server::core::providers::workbuddy::reconcile::classify_status(
                region, account_started_at_ms, crate::server::logging::now_ms(), &status,
            ),
            Err(error) => serde_json::json!({
                "verdict": "unknown",
                "provider": provider,
                "evidenceType": "checkin_status",
                "observedAt": crate::server::logging::now_ms(),
                "reason": format!("WorkBuddy 状态查询未能完成: {error}"),
            }),
        }
    } else {
        serde_json::json!({
            "verdict": "unknown",
            "provider": provider,
            "evidenceType": "unsupported_provider",
            "observedAt": crate::server::logging::now_ms(),
            "reason": "当前尚无该 Provider 的专用只读对账器；原 uncertain 状态保持不变",
        })
    };
    match checkin_run_ledger::record_reconciliation_evidence(db, run_id, account_id, &result) {
        Ok(mut audit) => {
            if audit.get("resolved").and_then(serde_json::Value::as_bool) == Some(true) {
                match checkin_run_ledger::project_pending_reconciliation_history(db, state.store()) {
                    Ok(count) => {
                        if let Some(object) = audit.as_object_mut() {
                            object.insert("historyProjected".to_string(), serde_json::json!(count > 0));
                        }
                    }
                    Err(error) => {
                        // 证据和隔离解决标记已经原子提交；保留成功结果，启动恢复会重试历史投影。
                        if let Some(object) = audit.as_object_mut() {
                            object.insert("historyProjected".to_string(), serde_json::json!(false));
                            object.insert("projectionError".to_string(), serde_json::json!(error));
                        }
                    }
                }
            } else if let Some(object) = audit.as_object_mut() {
                object.insert("historyProjected".to_string(), serde_json::json!(false));
            }
            if let Some(object) = result.as_object_mut() {
                object.insert("audit".to_string(), audit);
            }
            ok_json(result)
        }
        Err(error) => crate::server::errors::management_error(503, &format!("对账结果未能安全持久化: {error}")),
    }
}
