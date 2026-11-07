//! 豆包工作「签到」的**幂等台账**：一次只提交一回，到不到账以官方为准。
//!
//! ── 为什么需要台账（本模块存在的唯一理由）────────────────────────
//! 本家的「签到」= 领活动里那条 daily 任务（见 `rewards` 的模块头）。领取是
//! **写请求**，而写请求有三种「不知道到底成没成」的失败：
//!   · 连接在发出后断掉 / 读超时（上游可能已经受理）；
//!   · 进程在请求途中重启；
//!   · 上游回了 200，回执却说不清（`classify_claim` 只能判 Granted/AlreadyHeld/
//!     Refused，而 Granted 只是「上游说发了」，**不是到账证明**）。
//!
//! 没有台账时，这几种情况下用户再点一次签到就会**再提交一次领取**；上游若把
//! 重复领取当成异常，轻则报错、重则把账号判成刷奖。所以：
//!
//! ```text
//!   提交前：把「要领哪一条、什么时候提交、第几轮」落盘
//!   再次进入：有 pending 就**只回查**，绝不再提交
//!   确认到账：以官方 checked_in_today 为准，回查成功才清台账
//! ```
//!
//! ── pending 与「明确拒绝」的分界（别把两者合并）─────────────────
//! 只有**能证明写请求没生效**的失败才清台账重来：上游明确拒绝
//! （`ClaimVerdict::Refused`，通常是活动规则不允许 / 不在活动期）。
//! 传输层失败、超时、读不出签到态都**保留** pending —— 那些都证明不了
//! 「没发出去」，删掉它等于给下一次重复提交开门。
//!
//! ── 回查为什么只查三次、且不超过 30 分钟窗口 ────────────────────
//! 到账是上游异步入账的，实测几秒内可见；但**不能**无限等 —— 签到按钮的后端
//! 调用有超时预算。于是：一次调用内 0/2/4 秒查三次（总预算 18 秒），跨调用
//! 用 `retryAt` 约 10 分钟后只再查一次，最多 4 轮；超过 30 分钟窗口就不再自动
//! 重试，只把「没确认到账」如实告诉用户（并说明再点只会回查）。这条窗口也
//! 防着「上游其实没发、我们却一直显示待确认」无限拖下去。
//!
//! ── 跨零点 ────────────────────────────────────────────────
//! 签到按**北京时间**的自然日算（与 `core::auto_checkin` 同源）。回查途中跨过
//! 零点时必须中止（409）而不是把昨天的结果记成今天的签到 —— 那会让用户以为
//! 今天签过了，而实际又得等一整天。
//!
//! ── 返回形状（与别家签到一致的契约）──────────────────────────
//! `{success, msg, alreadyCompleted?, needsAction?, retryAt?, itemCode?}`：
//!   · 真领到且回查确认 → `success: true`；
//!   · 今天已签（上游确认） → `success: false` + `alreadyCompleted: true` +
//!     中性文案。`alreadyCompleted` 是内核台账「今天已完成」的键（见
//!     `billing::checkin::checkin_completed_today`）—— 少了它，这一天既不算
//!     完成也不算失败，签到中心会一直显示「待签」并反复重打；
//!   · 待确认 → `needsAction: true` 且**不带** `alreadyCompleted`，文案里
//!     不出现「已签到 / 已领取」（否则会被判成当日已完成，从此不再回查）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::time::Duration;

use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::core::beijing;
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::credentials::{self, Credentials};
use super::rewards::{self, CheckinState, ClaimVerdict};

/// 台账结构版本（形状变了就抬它，旧台账会被判无效而不是被误读）
const LEDGER_VERSION: u64 = 1;

/// 跨调用重试的最短间隔（`retryAt` 用）
const RETRY_GAP_MS: i64 = 10 * 60 * 1000;

/// 自动重试的确认窗口：提交后超过这个时长只查一次、不再排重试
const CONFIRM_WINDOW_MS: i64 = 30 * 60 * 1000;

/// 一条 pending 最多被回查几轮（含提交那一轮）
const MAX_ROUNDS: u32 = 4;

/// 一次调用内的回查节拍（秒）—— 合计约 6 秒等待 + 查询耗时
const RECONCILE_DELAYS: [u64; 3] = [0, 2, 4];

/// 一次调用的回查总预算（秒）：超过就如实回「查询超时，记录已保留」
const CONFIRM_BUDGET_SECS: u64 = 18;

/// 待确认的一次领取（**先落盘、再发请求**）
struct Attempt {
    /// 领取的条目 id（`campaign_item_code`）—— 只用于回查时认出「领的是哪一条」，
    /// 以及给用户看的 `itemCode`
    item: String,
    /// 提交时刻（毫秒，本机时钟）
    submitted_at: i64,
    /// 已经回查过几轮（提交那一轮算 1）
    rounds: u32,
}

impl Attempt {
    fn parse(value: &Value) -> Result<Self, GatewayError> {
        let item = value
            .get("item")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let submitted_at = value
            .get("submittedAt")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let rounds = value.get("rounds").and_then(Value::as_u64).unwrap_or(0);
        // 三项任一不成立都说明这条台账是手工改坏的：宁可停下来问用户，
        // 也不能拿一条残记录去决定「要不要再提交一次领取」。
        if item.is_empty() || submitted_at <= 0 || rounds == 0 || rounds > u64::from(MAX_ROUNDS) {
            return Err(invalid());
        }
        Ok(Self {
            item,
            submitted_at,
            rounds: rounds as u32,
        })
    }

    fn to_value(&self) -> Value {
        json!({
            "item": self.item,
            "submittedAt": self.submitted_at,
            "rounds": self.rounds,
        })
    }

    /// 下一次自动回查的时刻；`None` = 不再排自动重试（轮次用尽 / 过了窗口 / 跨日）
    fn retry_at(&self, now: i64) -> Option<i64> {
        let next = now.saturating_add(RETRY_GAP_MS);
        (self.rounds < MAX_ROUNDS
            && next <= self.submitted_at.saturating_add(CONFIRM_WINDOW_MS)
            && beijing::date_key_of_ms(next) == beijing::date_key_of_ms(now))
        .then_some(next)
    }
}

/// 当日台账：`{version, day, pending}`，存在账号记录的 `doubaoworkCheckin` 里。
///
/// 只保留**当天**的 pending：`day` 变了就是新的一天，昨天的待确认任务不再有
/// 意义（签到的账只能记到今天）。
struct Ledger {
    day: String,
    pending: Option<Attempt>,
}

impl Ledger {
    fn fresh(day: String) -> Self {
        Self { day, pending: None }
    }

    /// 读台账。**只有明确的「没有台账」才是首次领取**：读失败或形状损坏都报错，
    /// 不能当作「没有 pending」—— 那等于把上次的提交记忆抹掉。
    async fn load(store: &AccountStore, account_id: &str) -> Result<Self, GatewayError> {
        let today = beijing::today_key();
        let Some(value) = store.doubaowork_checkin_ledger(account_id) else {
            return Ok(Self::fresh(today));
        };
        let stored = Self::parse(&value)?;
        // ISO 日期串可以直接比大小；同格式已由 `parse` 保证
        if stored.day > today {
            return Err(GatewayError::with_status(
                500,
                "系统日期早于豆包工作签到记录，请先校准时间",
            ));
        }
        if stored.day < today {
            return Ok(Self::fresh(today));
        }
        Ok(stored)
    }

    fn parse(value: &Value) -> Result<Self, GatewayError> {
        let version = value.get("version").and_then(Value::as_u64).unwrap_or(0);
        if version != LEDGER_VERSION {
            return Err(invalid());
        }
        let day = value
            .get("day")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let parsed = NaiveDate::parse_from_str(&day, "%Y-%m-%d").map_err(|_| invalid())?;
        // 回写一遍比对：`2026-11-7` 这类能被宽松解析但形状不对的串要挡住，
        // 否则字符串比大小会比错（`"2026-11-7" > "2026-11-07"`）。
        if parsed.format("%Y-%m-%d").to_string() != day {
            return Err(invalid());
        }
        let pending = match value.get("pending") {
            None => None,
            Some(raw) if raw.is_null() => None,
            Some(raw) => Some(Attempt::parse(raw)?),
        };
        Ok(Self { day, pending })
    }

    fn to_value(&self) -> Value {
        json!({
            "version": LEDGER_VERSION,
            "day": self.day,
            "pending": self.pending.as_ref().map(Attempt::to_value),
        })
    }

    async fn save(&self, store: &AccountStore, account_id: &str) -> Result<(), GatewayError> {
        store
            .put_doubaowork_checkin_ledger(account_id, &self.to_value())
            .map_err(|error| {
                GatewayError::with_status(502, format!("保存豆包工作签到记录失败：{error}"))
            })
    }

    /// 清掉 pending 并落盘。写失败时**把内存里的 pending 还原**：调用方拿到 Err
    /// 时若还以为「已清空」，下一次就会重复提交。
    async fn clear(&mut self, store: &AccountStore, account_id: &str) -> Result<(), GatewayError> {
        let previous = self.pending.take();
        if let Err(error) = self.save(store, account_id).await {
            self.pending = previous;
            return Err(error);
        }
        Ok(())
    }
}

fn invalid() -> GatewayError {
    GatewayError::with_status(500, "豆包工作签到记录无效，已停止提交领取以避免重复操作")
}

/// 签到入口（`adapter::claim_for_account` 转调这里）。
///
/// 串行锁的必要性：调度器与「手动签到」可能同时进来，两者都会在**落盘之前**
/// 读到「没有 pending」，于是各提交一次领取。锁把这一段也关上（台账只挡得住
/// 「落盘之后」那一段）。
pub async fn claim_for_account(
    store: &AccountStore,
    account_id: &str,
) -> Result<Value, GatewayError> {
    let _serial = rewards::claim_lock().lock().await;
    let record = store.doubaowork_account_record(account_id);
    if !account_id.is_empty() && record.is_none() {
        return Err(GatewayError::with_status(
            401,
            format!("豆包工作账号 {account_id} 不存在（请重新添加）"),
        ));
    }
    let credentials = credentials::from_record(record.as_ref())?;
    // 台账挂在**记录 id** 上（`account_id` 为空时上游语义是「队首账号」，
    // 直接拿空串当键会把不同账号的台账写到一起）。
    let id = record
        .as_ref()
        .and_then(|value| value.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| account_id.trim().to_string());
    if id.is_empty() {
        return Err(GatewayError::with_status(
            401,
            "没有可用的豆包工作账号：请在账号页用「浏览器登录」或粘贴完整 Cookie 添加",
        ));
    }
    let mut ledger = Ledger::load(store, &id).await?;
    execute(store, &id, &credentials, &mut ledger).await
}

async fn execute(
    store: &AccountStore,
    account_id: &str,
    credentials: &Credentials,
    ledger: &mut Ledger,
) -> Result<Value, GatewayError> {
    // ① 有待确认任务 ⇒ 只回查，绝不再提交
    if ledger.pending.is_some() {
        if let Some(attempt) = ledger.pending.as_mut() {
            // 封顶到 MAX_ROUNDS：`Attempt::parse` 拒收超限的轮次（改坏的记录），
            // 无界自增会让一份**正常**台账在几轮之后被自己判成无效。
            attempt.rounds = attempt.rounds.saturating_add(1).min(MAX_ROUNDS);
        }
        ledger.save(store, account_id).await?;
        return reconcile(store, account_id, credentials, ledger, false).await;
    }

    // ② 读今天的签到态
    let state = rewards::fetch_checkin_state(credentials, rewards::REWARD_TIMEOUT_MS).await?;
    ensure_day(&ledger.day)?;
    if state.checked_in_today == Some(true) {
        return Ok(confirmed(&state, false));
    }
    let Some((code, _name)) = rewards::daily_checkin_item(&state) else {
        return Ok(inactive(&state));
    };

    // ③ 先落盘再提交：落盘失败就不发写请求（否则重启后无从得知「提交过」）
    ledger.pending = Some(Attempt {
        item: code.clone(),
        submitted_at: logging::now_ms(),
        rounds: 1,
    });
    ledger.save(store, account_id).await?;
    ensure_day(&ledger.day)?;

    // 传输层失败不能证明写请求没生效 ⇒ pending **故意保留**（`?` 只是把错误带回
    // 上层）：下一次进来只会回查，不会再提交一次。
    let payload = rewards::claim_item(credentials, &code, rewards::REWARD_TIMEOUT_MS).await?;
    match rewards::classify_claim(&payload) {
        // 上游明确拒绝 ⇒ 确实没发放，清台账让下一次可以重新尝试
        ClaimVerdict::Refused { message } => {
            ledger.clear(store, account_id).await?;
            Ok(refused(&message))
        }
        // 「已领过」是成功语义，但**是否到账仍以官方为准** —— 一并回查
        ClaimVerdict::AlreadyHeld { .. } => {
            reconcile(store, account_id, credentials, ledger, false).await
        }
        // 回执说发了 ≠ 到账：必须再读一次官方签到态才算数
        ClaimVerdict::Granted { .. } => {
            reconcile(store, account_id, credentials, ledger, true).await
        }
    }
}

/// 回查官方签到态，直到确认到账 / 预算用尽 / 读不出。
async fn reconcile(
    store: &AccountStore,
    account_id: &str,
    credentials: &Credentials,
    ledger: &mut Ledger,
    submitted_now: bool,
) -> Result<Value, GatewayError> {
    let attempt = ledger.pending.clone().ok_or_else(|| {
        GatewayError::with_status(500, "豆包工作缺少待确认的领取记录，未提交领取")
    })?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(CONFIRM_BUDGET_SECS);
    let mut readable = false;
    for delay in RECONCILE_DELAYS {
        if delay > 0 {
            tokio::time::sleep(Duration::from_secs(delay)).await;
        }
        ensure_day(&ledger.day)?;
        let state = match tokio::time::timeout_at(
            deadline,
            rewards::fetch_checkin_state(credentials, rewards::REWARD_TIMEOUT_MS),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                return Err(GatewayError::with_status(
                    504,
                    "豆包工作签到结果查询超时；领取记录已保留，再次点击只查询结果",
                ))
            }
        };
        ensure_day(&ledger.day)?;
        match state.checked_in_today {
            Some(true) => {
                ledger.clear(store, account_id).await?;
                return Ok(confirmed(&state, submitted_now));
            }
            Some(false) => readable = true,
            // 读不出（上游口径变了）⇒ **不能**当成没到账，也不能继续猜：
            // 停下来如实说「读不出」，记录留着等下一次。
            None => break,
        }
        if attempt.retry_at(logging::now_ms()).is_none() {
            break;
        }
    }
    Ok(waiting(&attempt, readable))
}

/// 北京时间跨日 ⇒ 中止。把昨天的结果记成今天的签到，比失败更糟。
fn ensure_day(day: &str) -> Result<(), GatewayError> {
    if beijing::today_key() != day {
        return Err(GatewayError::with_status(
            409,
            "签到确认已跨过北京时间零点，未将昨日结果计为今日签到，请重新查询今日活动",
        ));
    }
    Ok(())
}

/// 「已到账」：`success` 只在**本次真的提交并确认**时为真。
fn confirmed(state: &CheckinState, submitted_now: bool) -> Value {
    let mut message = if submitted_now {
        "签到成功，已回查确认到账".to_string()
    } else {
        "今天已签到，无需重复领取".to_string()
    };
    if let Some(days) = state.continuous_days {
        message.push_str(&format!("（连续 {days} 天）"));
    }
    json!({
        "success": submitted_now,
        "alreadyCompleted": true,
        "msg": message,
        "needsAction": false,
    })
}

/// 「本次活动期没有可签到的任务」：不谎报成功，也不冒充失败。
fn inactive(state: &CheckinState) -> Value {
    let message = if state.checked_in_today.is_none() {
        "读不出签到状态（上游活动接口的响应里没有 checked_in_today / \
         daily_checkin / task_status，口径可能已变），本次活动期也无签到类奖励可领"
            .to_string()
    } else {
        "本次活动期没有签到类奖励可领".to_string()
    };
    json!({
        "success": false,
        "alreadyCompleted": true,
        "msg": message,
        "needsAction": false,
    })
}

/// 上游明确拒绝：把原文带回去（活动规则千变万化，编一个原因是误导）。
fn refused(message: &str) -> Value {
    let message = if message.trim().is_empty() {
        "签到被上游拒绝（未说明原因）".to_string()
    } else {
        message.trim().to_string()
    };
    json!({
        "success": false,
        "msg": message,
        "needsAction": true,
    })
}

/// 「提交了但没能确认到账」。
///
/// ⚠️ 文案里**不许**出现「已签到 / 已领取」：`checkin_completed_today` 按这几个
/// 字判当日完成，出现就等于告诉调度器「今天办完了」，从此不再回查。
fn waiting(attempt: &Attempt, readable: bool) -> Value {
    let retry_at = attempt.retry_at(logging::now_ms());
    let message = if retry_at.is_some() {
        "领取结果待确认，稍后再次签到只会回查结果，不会重复提交"
    } else if readable {
        "今日签到仍未到账，请前往官方客户端核实；再次点击仅查询结果，不会重复领取"
    } else {
        "读不出签到状态，暂未确认到账；再次点击仅查询结果，不会重复领取"
    };
    json!({
        "success": false,
        "msg": message,
        "needsAction": true,
        "retryAt": retry_at,
        "itemCode": attempt.item,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ledger_value(version: u64, day: &str, pending: Value) -> Value {
        json!({"version": version, "day": day, "pending": pending})
    }

    fn attempt_value(item: &str, submitted_at: i64, rounds: u64) -> Value {
        json!({"item": item, "submittedAt": submitted_at, "rounds": rounds})
    }

    /// 台账形状不对时必须报错，**不能**当成「没有 pending」——否则等于把上次的
    /// 提交记忆抹掉，下一次就会重复提交领取。
    #[test]
    fn malformed_ledger_is_an_error_not_a_fresh_one() {
        assert!(Ledger::parse(&ledger_value(0, "2026-11-07", Value::Null)).is_err());
        assert!(Ledger::parse(&ledger_value(1, "2026-11-7", Value::Null)).is_err());
        assert!(Ledger::parse(&ledger_value(1, "not-a-day", Value::Null)).is_err());
        assert!(Ledger::parse(&ledger_value(1, "2026-11-07", attempt_value("", 1, 1))).is_err());
        assert!(Ledger::parse(&ledger_value(1, "2026-11-07", attempt_value("a", 0, 1))).is_err());
        assert!(Ledger::parse(&ledger_value(1, "2026-11-07", attempt_value("a", 1, 0))).is_err());
        // 轮次超过上限：只可能是改坏的记录
        assert!(Ledger::parse(&ledger_value(1, "2026-11-07", attempt_value("a", 1, 9))).is_err());
        let ok = Ledger::parse(&ledger_value(1, "2026-11-07", attempt_value("a", 1, 1)))
            .expect("合法台账应能读出");
        assert_eq!(ok.day, "2026-11-07");
        assert_eq!(ok.pending.map(|attempt| attempt.item), Some("a".to_string()));
    }

    /// 重试节拍：轮次用尽 / 过了 30 分钟窗口 / 跨日都不再排自动重试。
    #[test]
    fn retry_at_stops_on_rounds_window_and_day_boundary() {
        // 2026-11-07 10:00 北京时间（换算成毫秒，别手算）
        let submitted = 1_794_016_800_000;
        let attempt = Attempt {
            item: "a".to_string(),
            submitted_at: submitted,
            rounds: 1,
        };
        assert_eq!(
            attempt.retry_at(submitted),
            Some(submitted + RETRY_GAP_MS),
            "首轮之后应排下一次回查"
        );
        let last_round = Attempt {
            item: "a".to_string(),
            submitted_at: submitted,
            rounds: MAX_ROUNDS,
        };
        assert_eq!(last_round.retry_at(submitted), None, "轮次用尽不再重试");
        // 距提交已超过窗口（30 分钟后）
        assert_eq!(
            attempt.retry_at(submitted + CONFIRM_WINDOW_MS),
            None,
            "过了确认窗口只查一次，不再排重试"
        );
        // 提交在 23:55，若在 23:58 排下一次（+10 分钟）就落到次日 ⇒ 不排
        let late = Attempt {
            item: "a".to_string(),
            submitted_at: submitted + 13 * 3600 * 1000 + 55 * 60 * 1000,
            rounds: 1,
        };
        assert_eq!(
            late.retry_at(late.submitted_at + 3 * 60 * 1000),
            None,
            "下一次回查会跨日时不排自动重试（跨日由 ensure_day 拦下）"
        );
    }

    /// 「待确认」的文案不能命中 `checkin_completed_today` 的三个判据，
    /// 否则调度器会把「还没到账」当成「今天已办」，从此不再回查。
    ///
    /// `retryAt` 的取值随「现在几点」变（见 `retry_at` 的跨日判据），这里只钉
    /// 语义（该排重试时给、不该给时是 null），绝对值由上面那条测试钉住。
    #[test]
    fn waiting_text_is_not_mistaken_for_completion() {
        let attempt = Attempt {
            item: "daily_checkin_v3".to_string(),
            submitted_at: logging::now_ms(),
            rounds: 2,
        };
        for readable in [true, false] {
            let view = waiting(&attempt, readable);
            assert_eq!(view.get("success").and_then(Value::as_bool), Some(false));
            // 不能带 alreadyCompleted：它会直接判成当日已完成
            assert!(view.get("alreadyCompleted").is_none());
            let message = view.get("msg").and_then(Value::as_str).unwrap_or("");
            assert!(!message.contains("已签到"), "{message}");
            assert!(!message.contains("已领取"), "{message}");
            assert_eq!(
                view.get("needsAction").and_then(Value::as_bool),
                Some(true),
                "待确认必须让调度器再来一次"
            );
        }
    }

    /// 已确认到账：只有本次真的提交过才报 `success: true`
    #[test]
    fn confirmed_success_requires_this_run_to_have_submitted() {
        let state = CheckinState {
            checked_in_today: Some(true),
            claimable: Vec::new(),
            continuous_days: Some(3),
        };
        let granted = confirmed(&state, true);
        assert_eq!(granted.get("success").and_then(Value::as_bool), Some(true));
        assert_eq!(
            granted.get("alreadyCompleted").and_then(Value::as_bool),
            Some(true)
        );
        assert!(granted
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("连续 3 天"));
        // 今天早就签过：没有发放任何东西，不谎报成功
        let already = confirmed(&state, false);
        assert_eq!(already.get("success").and_then(Value::as_bool), Some(false));
    }
}
