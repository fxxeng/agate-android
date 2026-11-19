//! `daily_check_in` 幂等台账（照搬豆包台账判据，见方案 L11）。
//!
//! 灵犀上游对**重复领取回 200 假成功**：`result=ok` 但余额不动。所以：
//!   · 先落盘 pending 再发 claim（落盘失败不发）；
//!   · 有 pending 只回查，不重复提交；
//!   · **只有明确拒绝才清台账**；传输失败 / 超时 / 读不出都保留；
//!   · 到账以「领前 / 领后余额差」或官方任务状态 `claimed` 为准，
//!     **绝不看 HTTP 200**；
//!   · 待确认文案不许含「已签到 / 已领取」。
//!
//! ── 2026-11-13 第二轮：端点由源码核实，链路真正启用 ───────────
//! 第一批只按 README 语义猜路径，所以用一个开关把写请求关着。这一批复核了
//! `ketion/lingxi2api-panel::internal/upstream/lingxi.go`（`LXClaim` +
//! `autotask.go` 的真实每日任务链）与
//! `RobbsLuo/Coding2API::src/provider/lingxi/client.py`（`EP_CLAIM` +
//! `checkin()` + 测试），两者给出的端点、字段、流程逐字一致：
//!   先读 `GET /api/public/v1/tasks` → `status == "claimed"` 直接返回已签；
//!   否则 `POST /api/public/v1/tasks/daily_check_in/claim?date=…`；
//!   奖励按领前/领后余额差算，差为 0 时读任务 `extra.today_reward` 回落。
//! `CLAIM_ENDPOINT_VERIFIED` 现已打开；失效时状态机仍保留「待确认」、
//! **不落 done、不写「已领取」、不反复打上游**。真机 2xx 仍是验收项。
//!
//! 台账写在账号记录自己的命名空间（`lingxiCheckin`），写法与
//! `put_codearts_welfare_ledger` 同：锁内现读整条、只写自己的键。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic。

use serde_json::{json, Map, Value};

use crate::server::core::account_store::store::AccountStore;
use crate::server::core::beijing;
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::endpoints::TASK_DAILY_CHECK_IN;
use super::{client, credentials, endpoints};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ledger {
    Idle,
    Pending,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// 还没有结论：保持 pending，界面写「待确认」。
    KeepPending,
    /// 确认到账。
    MarkDone,
    /// 官方任务状态已是 claimed（今天已签过，不是本次领到的）。
    AlreadyDone,
    /// 上游明确拒绝：清台账，下次可重试。
    ClearForRetry,
}

/// `transport_ok` = 请求发出去且响应读得出来；`explicit_reject` = 上游给了
/// 明确的业务拒绝（不是超时、不是读不出）；`before/after` 是领前领后余额；
/// `task_claimed` 是官方任务状态（None = 没读到，不是 false）。
pub fn reconcile(
    ledger: Ledger,
    transport_ok: bool,
    explicit_reject: bool,
    before: Option<f64>,
    after: Option<f64>,
    task_claimed: Option<bool>,
) -> Outcome {
    if ledger == Ledger::Done {
        return Outcome::AlreadyDone;
    }
    if explicit_reject {
        return Outcome::ClearForRetry;
    }
    if !transport_ok {
        return Outcome::KeepPending;
    }
    if task_claimed == Some(true) {
        return Outcome::AlreadyDone;
    }
    match (before, after) {
        (Some(start), Some(end)) if end > start => Outcome::MarkDone,
        // 200 但余额没动 ⇒ 不是成功，继续等回查
        _ => Outcome::KeepPending,
    }
}

/// 待确认文案。**不许**出现「已签到 / 已领取 / 已完成」这类结论词。
pub fn pending_text() -> &'static str {
    "签到结果待确认（上游可能延迟归账）"
}

pub fn task_name() -> &'static str {
    TASK_DAILY_CHECK_IN
}

/// 台账里「今天」的状态。`day` 变了就是新的一天，昨天的 pending 不带过来
/// （与豆包台账同口径）；没有台账 / 形状不认一律 Idle。
fn ledger_state(ledger: &Value, today: &str) -> Ledger {
    if ledger.get("day").and_then(Value::as_str) != Some(today) {
        return Ledger::Idle;
    }
    match ledger.get("state").and_then(Value::as_str) {
        Some("pending") => Ledger::Pending,
        Some("done") => Ledger::Done,
        _ => Ledger::Idle,
    }
}

fn write_ledger(store: &AccountStore, account_id: &str, day: &str, state: &str) -> Result<(), GatewayError> {
    let mut ledger = Map::new();
    ledger.insert("version".to_string(), json!(1));
    ledger.insert("day".to_string(), json!(day));
    ledger.insert("state".to_string(), json!(state));
    if state == "pending" {
        ledger.insert("submittedAt".to_string(), json!(logging::now_ms()));
    }
    store
        .put_lingxi_checkin_ledger(account_id, &Value::Object(ledger))
        .map_err(|error| {
            GatewayError::with_status(
                error.status_code,
                format!("签到台账落盘失败：{}", error.message),
            )
        })
}

/// 签到链的执行体（`billing::checkin::checkin_for` 的 lingxi 分支）。
/// 返回 `{success, msg, alreadyCompleted?, endpointUnverified?}` ——
/// 形状与 `checkin_completed_today` / `claim_pending` 对齐。
pub async fn claim_for_account(
    store: &AccountStore,
    account_id: &str,
) -> Result<Value, GatewayError> {
    let day = beijing::today_key();
    let ledger = store
        .lingxi_checkin_ledger(account_id)
        .unwrap_or(Value::Null);
    let state = ledger_state(&ledger, &day);
    if state == Ledger::Done {
        return Ok(json!({
            "success": false,
            "alreadyCompleted": true,
            "msg": "今日已签到",
        }));
    }

    let Some(record) = store.lingxi_account_record(account_id) else {
        return Err(GatewayError::with_status(401, "没有可用的 WPS 灵犀账号"));
    };
    let (sid, cookie, uid) = client::credentials_of(&record);
    if sid.is_empty() {
        return Err(GatewayError::with_status(400, "灵犀账号缺少 wps_sid，请重新粘贴 Cookie"));
    }
    let identity = if uid.is_empty() { sid.as_str() } else { uid.as_str() };
    let device = credentials::device_id(identity);

    // ① 领前余额：读不出是 None，不拿 0 冒充。
    let before = client::balance(&sid, &device, cookie.as_deref())
        .await
        .map(|balance| balance.total);

    // ② 官方任务状态（只读）。两个参考实现都是「先查再领」：
    //    `status == "claimed"` 说明今天已经签过，不该再提交一次。
    let task = client::task_status(&sid, &device, cookie.as_deref()).await;
    let date = task
        .as_ref()
        .map(|state| state.date.clone())
        .unwrap_or_else(beijing::today_key);
    let claimed_before = task.as_ref().map(|state| state.claimed);
    if claimed_before == Some(true) {
        write_ledger(store, account_id, &day, "done")?;
        return Ok(json!({
            "success": false,
            "alreadyCompleted": true,
            "msg": "今日已签到",
        }));
    }

    // ③ 提交（仅 Idle）/ 只回查（Pending）。
    let (transport_ok, explicit_reject) = if state == Ledger::Pending {
        // 有 pending 只回查，**绝不重复提交**（重复领取回 200 假成功，
        // 再提交一次也不会多给，只会让台账与上游状态更对不上）。
        (endpoints::CLAIM_ENDPOINT_VERIFIED, false)
    } else {
        // 先落盘 pending 再发 claim：落盘失败就不发，避免「发了但没记住」。
        write_ledger(store, account_id, &day, "pending")?;
        match client::task_claim(&sid, &device, cookie.as_deref(), &date).await {
            // status == 0 是「开关关着、没发过请求」，不是上游 404。
            Ok(reply) => (
                endpoints::CLAIM_ENDPOINT_VERIFIED
                    && (200..300).contains(&reply.status)
                    && !reply.explicit_reject,
                reply.explicit_reject,
            ),
            // 传输失败/超时/读不出：台账保持 pending，不重发。
            Err(error) => return Err(error),
        }
    };

    // ④ 领后余额 + 回读任务状态。到账只认这两处证据，绝不看 HTTP 200。
    let after = client::balance(&sid, &device, cookie.as_deref())
        .await
        .map(|balance| balance.total);
    let task_after = client::task_status(&sid, &device, cookie.as_deref()).await;
    let task_claimed = task_after
        .as_ref()
        .map(|state| state.claimed)
        .or(claimed_before);
    let outcome = reconcile(state, transport_ok, explicit_reject, before, after, task_claimed);
    match outcome {
        Outcome::MarkDone | Outcome::AlreadyDone => {
            write_ledger(store, account_id, &day, "done")?;
            if outcome == Outcome::MarkDone {
                Ok(json!({ "success": true, "msg": "签到奖励已到账" }))
            } else {
                Ok(json!({
                    "success": false,
                    "alreadyCompleted": true,
                    "msg": "今日已签到",
                }))
            }
        }
        Outcome::ClearForRetry => {
            write_ledger(store, account_id, &day, "idle")?;
            Ok(json!({ "success": false, "msg": "上游明确拒绝签到领取，可稍后重试" }))
        }
        Outcome::KeepPending => Ok(json!({
            "success": false,
            "msg": pending_text(),
            "endpointUnverified": !endpoints::CLAIM_ENDPOINT_VERIFIED,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_200_with_unchanged_balance_is_not_success() {
        assert_eq!(
            reconcile(Ledger::Pending, true, false, Some(100.0), Some(100.0), None),
            Outcome::KeepPending
        );
        assert!(!pending_text().contains("已签到"));
        assert!(!pending_text().contains("已领取"));
    }

    #[test]
    fn balance_increase_confirms_the_claim() {
        assert_eq!(
            reconcile(Ledger::Pending, true, false, Some(100.0), Some(200.0), None),
            Outcome::MarkDone
        );
    }

    #[test]
    fn official_task_state_can_confirm_without_a_balance_delta() {
        assert_eq!(
            reconcile(Ledger::Pending, true, false, Some(100.0), Some(100.0), Some(true)),
            Outcome::AlreadyDone
        );
    }

    #[test]
    fn only_an_explicit_rejection_clears_the_ledger() {
        assert_eq!(
            reconcile(Ledger::Pending, true, true, Some(100.0), Some(100.0), None),
            Outcome::ClearForRetry
        );
        // 传输失败 / 读不出都保留，不重发
        assert_eq!(
            reconcile(Ledger::Pending, false, false, None, None, None),
            Outcome::KeepPending
        );
    }

    #[test]
    fn done_ledger_never_claims_again() {
        assert_eq!(
            reconcile(Ledger::Done, true, false, Some(1.0), Some(2.0), None),
            Outcome::AlreadyDone
        );
    }

    #[test]
    fn ledger_resets_on_a_new_day() {
        let yesterday = json!({ "day": "2026-11-12", "state": "pending" });
        assert_eq!(ledger_state(&yesterday, "2026-11-13"), Ledger::Idle);
        assert_eq!(ledger_state(&Value::Null, "2026-11-13"), Ledger::Idle);
        let pending = json!({ "day": "2026-11-13", "state": "pending" });
        assert_eq!(ledger_state(&pending, "2026-11-13"), Ledger::Pending);
    }

    #[test]
    fn a_failed_or_disabled_claim_never_reports_success() {
        // 开关被关掉 / 提交失败 ⇒ transport_ok=false ⇒ 只能停在待确认。
        // 这条不依赖开关当前值：真机发现端点变更时一键停发，判据仍成立。
        assert_eq!(
            reconcile(Ledger::Pending, false, false, None, None, None),
            Outcome::KeepPending
        );
        let text = pending_text();
        assert!(!text.contains("已签到") && !text.contains("已领取"));
    }
}
