//! QuWork 每日奖励：先查状态 → 领取 → 回查；**结果不明不重复提交**。
//!
//! ── 幂等台账（去 SDK 化）─────────────────────────────────────
//! 上游用 `ctx.ledger_get/ledger_put` 把 `confirming / claimed / rejected`
//! 按 `(account, resetAt)` 持久化。本仓没有那套 ledger，用进程内
//! `OnceLock<Mutex<HashMap>>` 记账：状态为 `confirming` 时**只回查、不重发
//! POST**（上游 `run_locked` 的核心判据）。进程重启后台账为空，但每次都先
//! 查 `status`，`claimed` 为真时不会提交 —— 领取状态由上游兜底。
//!
//! ── 结果形状 ────────────────────────────────────────────────
//! `{provider, success, msg, credit}`（本仓 `billing::checkin` 认这几个键）。
//! `alreadyCompleted` / `ineligible` / `inactive` 是给当日台账看的标记
//! （`checkin_completed_today` 只认 success / alreadyCompleted / 文案）。
//! 「结果待确认」的文案**不含**「已签到 / 已领取」，否则会被误记成已完成。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::errors::GatewayError;

use super::{client, credentials, endpoints, refresh, ID};

/// 每日奖励状态（上游 `checkin::Status`）
#[derive(Clone)]
pub struct Status {
    pub claimed: bool,
    pub can_claim: bool,
    pub points: f64,
    pub streak: u32,
    pub reset_at: i64,
}

/// 进程内领取台账：`(account_id, reset_at) → state`
static LEDGER: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn ledger_key(account_id: &str, reset_at: i64) -> String {
    format!("{account_id}:{reset_at}")
}

fn saved_state(account_id: &str, reset_at: i64) -> String {
    LEDGER
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&ledger_key(account_id, reset_at))
        .cloned()
        .unwrap_or_default()
}

fn put_state(account_id: &str, reset_at: i64, state: &str) {
    let mut table = LEDGER
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // 台账只存「进行中 / 今日结论」，规模随账号数线性增长；超过 1024 清一次
    // （上游 sessions 表同款上限，避免长期运行无限增长）
    if table.len() >= 1024 {
        table.clear();
    }
    table.insert(ledger_key(account_id, reset_at), state.to_string());
}

/// 结果行的固定字段
fn row(success: bool, message: &str) -> Value {
    json!({
        "provider": ID,
        "success": success,
        "msg": message,
        "credit": Value::Null,
    })
}

fn mark(mut value: Value, key: &str) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.insert(key.to_string(), Value::Bool(true));
    }
    value
}

/// 解析状态（**缺任一字段就是「格式异常」**，此时不提交领取）。
pub fn parse_status(data: &Value) -> Option<Status> {
    Some(Status {
        claimed: data.get("claimed").and_then(Value::as_bool)?,
        can_claim: data.get("can_claim").and_then(Value::as_bool)?,
        points: client::number(data.get("points"))?,
        streak: data
            .get("consecutive_days")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())?,
        reset_at: client::timestamp(data.get("next_refresh_at"))?,
    })
}

/// 查一次每日奖励状态（走 `protected`：临期自动续期、401 强制刷新重放）。
pub async fn status(store: &AccountStore, account_id: &str) -> Result<Status, GatewayError> {
    let data = refresh::protected(
        store,
        account_id,
        "GET",
        endpoints::DAILY_STATUS_PATH,
        None,
    )
    .await?
    .data("每日奖励查询")?;
    parse_status(&data).ok_or_else(|| {
        GatewayError::with_status(502, "QuWork 每日奖励状态格式异常，未提交领取")
    })
}

/// 只回查、不重复提交（状态为 `confirming` 的那条路径）。
async fn verify_only(
    store: &AccountStore,
    account_id: &str,
    before: &Status,
) -> Value {
    if let Ok(after) = status(store, account_id).await {
        if after.claimed && after.reset_at == before.reset_at {
            put_state(account_id, before.reset_at, "claimed");
            return mark(
                row(true, "QuWork 已确认今日奖励到账"),
                "alreadyCompleted",
            );
        }
    }
    mark(
        row(false, "QuWork 上次领取结果待确认，请稍后查询；不会重复提交领取"),
        "inactive",
    )
}

/// 自动签到框架的 claim（形状见模块头）。
pub async fn claim_for_account(
    store: &AccountStore,
    account_id: &str,
) -> Result<Value, GatewayError> {
    // 只有模型凭证的账号没有账号面能力：一次网络都不发（如实 ineligible）
    let secret = refresh::snapshot(store, account_id).await?;
    if credentials::login_token_of(&secret).is_empty() {
        return Ok(mark(
            row(false, "该账号只有模型凭证（无登录凭证），无法执行每日奖励"),
            "ineligible",
        ));
    }
    let before = status(store, account_id).await?;
    let saved = saved_state(account_id, before.reset_at);
    if before.claimed || saved == "claimed" {
        return Ok(mark(
            row(true, "QuWork 今日奖励已领取"),
            "alreadyCompleted",
        ));
    }
    if !before.can_claim || before.reset_at <= crate::server::logging::now_ms() {
        return Ok(mark(
            row(false, "QuWork 当前没有可领取的每日奖励"),
            "inactive",
        ));
    }
    if saved == "confirming" {
        return Ok(verify_only(store, account_id, &before).await);
    }
    // 提交前先落 "confirming"：进程内即使崩溃/超时，下次也只回查不重发
    put_state(account_id, before.reset_at, "confirming");
    let response = refresh::protected(
        store,
        account_id,
        "POST",
        endpoints::DAILY_CLAIM_PATH,
        None,
    )
    .await;
    // ① 明确领取成功：granted=true
    if let Ok(reply) = &response {
        if let Ok(data) = reply.data("每日奖励领取") {
            if data.get("granted").and_then(Value::as_bool) == Some(true) {
                put_state(account_id, before.reset_at, "claimed");
                let points = client::number(data.get("points"));
                let mut claimed = row(true, "QuWork 每日奖励领取成功");
                if let Some(points) = points {
                    claimed["credit"] = json!(points);
                }
                return Ok(claimed);
            }
        }
    }
    // ② 结果不明也要回查一次：上游可能已经到账，只是响应丢了
    if let Ok(after) = status(store, account_id).await {
        if after.claimed && after.reset_at == before.reset_at {
            put_state(account_id, before.reset_at, "claimed");
            return Ok(mark(
                row(true, "QuWork 已确认今日奖励到账"),
                "alreadyCompleted",
            ));
        }
    }
    // ③ 明确拒绝（4xx 非 408，或 2xx 但 success 为假）→ 记 rejected，今日不再提交
    if let Ok(reply) = &response {
        let rejected = ((400..500).contains(&reply.status) && reply.status != 408)
            || ((200..300).contains(&reply.status)
                && reply.body.get("success").is_some()
                && !client::success(reply.body.get("success")));
        if rejected {
            put_state(account_id, before.reset_at, "rejected");
            let detail = client::message(&reply.body);
            return Ok(row(
                false,
                &format!("QuWork 每日奖励领取未成功{}", if detail.is_empty() {
                    String::new()
                } else {
                    format!("：{detail}")
                }),
            ));
        }
    }
    // ④ 其余（网络失败 / 5xx / 超时）：保持 confirming，只回查、不重复提交
    Ok(mark(
        row(false, "QuWork 领取结果待确认，请稍后查询；不会重复提交领取"),
        "inactive",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_requires_every_field() {
        let data = json!({
            "claimed": false,
            "can_claim": true,
            "points": 30,
            "consecutive_days": 3,
            "next_refresh_at": 1_800_000_000_000_i64,
        });
        let status = parse_status(&data);
        assert!(status.is_some());
        let status = status.unwrap_or(Status {
            claimed: false,
            can_claim: false,
            points: 0.0,
            streak: 0,
            reset_at: 0,
        });
        assert_eq!(status.streak, 3);
        assert_eq!(status.points, 30.0);
        assert!(parse_status(&json!({ "claimed": false })).is_none());
    }

    #[test]
    fn a_pending_message_is_not_mistaken_for_completed() {
        let value = mark(
            row(false, "QuWork 领取结果待确认，请稍后查询；不会重复提交领取"),
            "inactive",
        );
        assert_eq!(value.get("success"), Some(&Value::Bool(false)));
        let msg = value.get("msg").and_then(Value::as_str).unwrap_or("");
        assert!(!msg.contains("已签到"));
        assert!(!msg.contains("已领取"));
    }
}
