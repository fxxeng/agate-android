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
//! 本文件只实现状态机（纯函数）：落盘与回查由下一批接线接到账号记录自己的
//! 命名空间里（与 `put_codearts_welfare_ledger` 同一处写法）。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic。

use super::endpoints::TASK_DAILY_CHECK_IN;

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
}
