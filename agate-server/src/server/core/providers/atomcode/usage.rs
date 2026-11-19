//! AtomCode 的额度读数：`status-v2` 的**滚动调用次数窗口**。
//!
//! ── 这家的额度口径与别家都不一样（读之前先看这条）─────────────
//! 不是积分、不是 token 余额，而是**每窗口可调用多少次**。实测（2026-10-01，
//! 记录在本地 Go 参考 `credits.go:37-38`）：
//!
//! > The plan's real quota is a CALL COUNT per window, not a token budget:
//! > the free tier measured 2026-10-01 allows 200 calls per 5 hours.
//!
//! `status-v2` 里同时存在 `window_token_limit`（**可空**）与
//! `rate_limit_windows[]`（真正的口径）。本模块只读后者：
//! 前者在免费档上是 null，把它当主读数会让界面显示一片空白。
//!
//! ── 只展示服务端让展示的窗口 ──────────────────────────────────
//! 每个窗口带 `show_enable`，只有 `1` 的才进界面（与本地 Go 参考
//! `credits.go:131-139` 同一口径）。服务端可能在同一个账号上挂多个规则
//! （5 小时 / 日 / 月），把不展示的那些也画出来会让用户看到互相矛盾的额度。
//!
//! ── 归一形状（与 loomy / lobsterai 同款，界面真正读的那一套）──────
//! ```text
//!   { available, unit, wallets: [{type, displayName, balance, remaining, limit, …}], raw }
//! ```
//! ⚠️ 曾经有一家（灵犀）在这处自造了 `total_balance` / `currency` 两个键，
//! 结果账号页余额列**空白**而探针还全绿（探针跟着实现断言）。所以这里
//! **严格照抄**同族的键名，不自造。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::errors::GatewayError;

use super::credentials::from_record;
use super::endpoints::REQUEST_TIMEOUT_MS;
use super::models::fetch_status;

/// 一个滚动窗口（`rate_limit_windows[]` 的一条）
#[derive(Clone, Debug)]
struct QuotaWindow {
    /// 窗口小时数
    hours: i64,
    /// 本窗口允许的调用次数
    call_limit: i64,
    /// 已用次数
    used: i64,
    /// 服务端给的重置时刻文案（如 `12:13`）
    reset_display: String,
    /// 本窗口是否已耗尽
    exhausted: bool,
}

/// 从 `status-v2` 里取出「服务端让展示」的窗口
fn shown_windows(payload: &Value) -> Vec<QuotaWindow> {
    let mut windows = Vec::new();
    let Some(items) = payload.get("rate_limit_windows").and_then(Value::as_array) else {
        return windows;
    };
    for item in items {
        // 只有 show_enable == 1 的窗口才展示
        if item.get("show_enable").and_then(Value::as_i64) != Some(1) {
            continue;
        }
        let call_limit = item.get("call_limit").and_then(Value::as_i64).unwrap_or(0);
        if call_limit <= 0 {
            continue;
        }
        windows.push(QuotaWindow {
            hours: item.get("window_hours").and_then(Value::as_i64).unwrap_or(0),
            call_limit,
            used: item.get("calls_used").and_then(Value::as_i64).unwrap_or(0),
            reset_display: item
                .get("reset_at_display")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            exhausted: item
                .get("quota_exhausted")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    windows
}

/// 套餐名与到期文案（`codingplan_free` 块）
fn plan_of(payload: &Value) -> (String, String) {
    let plan = payload.get("codingplan_free");
    let name = plan
        .and_then(|plan| plan.get("plan_name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let expires = plan
        .and_then(|plan| plan.get("expires_at"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    (name, expires)
}

/// 把 `status-v2` 归成界面认的额度形状（**纯函数**，便于测试）
///
/// ── 一条**必须**遵守的契约：读不到额度就不要写 `available` ──────
/// 限额判定走 `core::usage_records::extract_remaining`，它的规则是
/// 「`unlimited` → 判不出；否则看 `totalLeft`；否则看 `available`」——
/// 也就是说 **`available` 一旦出现，就会被当成真实读数**。
///
/// 2026-10-17 在这一点上写错过：没有窗口时也写 `available: 0`，于是「读不到额度」
/// 被限制器理解成「余额为 0」，账号当场被标成「余额不足 / 已跳过」——
/// 用户看到的现象是「领完套餐还是余额不足」。**没有窗口 ≠ 余额为 0**：
/// 前者是「这个响应我没读懂」，后者是「确实用完了」，两者必须分开。
/// 所以这里只在**确实解析出窗口**时才写 `available`；一个都没有就只回
/// `wallets: []`（记录会被 `usage_records` 摘除、不拦账号，余额列留空 ——
/// 与本仓「读不到的数值留空显示」同一口径）。
pub fn shape_usage(payload: &Value) -> Value {
    let windows = shown_windows(payload);
    let (plan_name, plan_expires) = plan_of(payload);

    // 诊断：把上游的真实字段形态记进**详细日志**。额度是按字段名解析的，
    // 名字一变就静默归零（今天就栽在这），所以留一条能事后对账的痕迹。
    // 只记键名与两条额度数组（都不含凭据），且不进普通日志。
    crate::server::logging::verbose(
        "[AtomCode]",
        &format!(
            "status-v2 形态：顶层键={:?} rate_limit_windows={} current_usage={}",
            payload
                .as_object()
                .map(|object| object.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default(),
            payload
                .get("rate_limit_windows")
                .map(|value| value.to_string())
                .map(|text| text.chars().take(600).collect::<String>())
                .unwrap_or_else(|| "<缺>".to_string()),
            payload
                .get("current_usage")
                .map(|value| value.to_string())
                .map(|text| text.chars().take(400).collect::<String>())
                .unwrap_or_else(|| "<缺>".to_string()),
        ),
    );

    let wallets: Vec<Value> = windows
        .iter()
        .map(|window| {
            let remaining = (window.call_limit - window.used).max(0);
            let label = if window.hours > 0 {
                format!("近 {} 小时调用次数", window.hours)
            } else {
                "调用次数窗口".to_string()
            };
            let mut wallet = json!({
                "type": "rolling_window",
                "displayName": label,
                "balance": remaining,
                "remaining": remaining,
                "limit": window.call_limit,
            });
            if let Some(object) = wallet.as_object_mut() {
                object.insert("used".to_string(), Value::from(window.used));
                if !window.reset_display.is_empty() {
                    object.insert(
                        "resetAt".to_string(),
                        Value::String(window.reset_display.clone()),
                    );
                }
                if window.exhausted {
                    object.insert("exhausted".to_string(), Value::Bool(true));
                }
            }
            wallet
        })
        .collect();

    // 主读数 = 第一个窗口的剩余次数。
    // ⚠️ **只在真的解析出窗口时才写**（读到 0 条 = 这份响应我没读懂，
    // 不是「余额为 0」）—— 理由见本函数文档「必须遵守的契约」那一节。
    let available = windows
        .first()
        .map(|window| (window.call_limit - window.used).max(0));

    let mut subscription =
        json!({ "status": if plan_name.is_empty() { "unclaimed" } else { "active" } });
    if let Some(object) = subscription.as_object_mut() {
        if !plan_name.is_empty() {
            object.insert("planName".to_string(), Value::String(plan_name));
        }
        // 上游给的是**日期字符串**，而 `expireAt` 在别家是毫秒数字。
        // 混用会让界面把一串日期当毫秒渲染，所以把原文放进独立的键，
        // 不占用 `expireAt`（宁可这一格留空，也不要显示一个错的时刻）。
        if !plan_expires.is_empty() {
            object.insert("planExpiresAt".to_string(), Value::String(plan_expires));
        }
        // 同上：没有窗口就不写这两个数（写了 `available` 的兄弟键也一样会被
        // 当成读数来源）
        if let Some(first) = windows.first() {
            object.insert(
                "remainQuota".to_string(),
                Value::from((first.call_limit - first.used).max(0)),
            );
            object.insert("totalQuota".to_string(), Value::from(first.call_limit));
        }
    }

    let mut shaped = json!({
        "unit": "次",
        "wallets": wallets,
        "subscription": subscription,
        "raw": payload,
    });
    // 只有解析出窗口才回 `available`（见函数文档）：这是限额判定唯一的读数来源
    if let Some(available) = available {
        if let Some(object) = shaped.as_object_mut() {
            object.insert("available".to_string(), Value::from(available));
        }
    }
    shaped
}

/// 查额度：读记录 → 取 `status-v2` → 归一。
pub async fn query(store: &AccountStore, account_id: &str) -> Result<Value, GatewayError> {
    let record = store.atomcode_account_record(account_id);
    if !account_id.is_empty() && record.is_none() {
        return Err(GatewayError::with_status(
            401,
            format!("AtomCode 账号 {account_id} 不存在（请重新添加）"),
        ));
    }
    let credentials = from_record(record.as_ref())?;
    let payload = fetch_status(&credentials, REQUEST_TIMEOUT_MS).await?;
    Ok(shape_usage(&payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 正常形态：窗口进 wallets，主读数 = 第一个窗口的剩余次数
    #[test]
    fn shaped_usage_reads_rolling_window() {
        let shaped = shape_usage(&json!({
            "codingplan_free": {"plan_name": "CodingPlan Lite-体验版", "expires_at": "2026-11-01"},
            "rate_limit_windows": [{
                "show_enable": 1,
                "window_hours": 5,
                "call_limit": 200,
                "calls_used": 37,
                "reset_at_display": "12:13",
                "quota_exhausted": false
            }]
        }));
        assert_eq!(shaped.get("unit").and_then(Value::as_str), Some("次"));
        assert_eq!(shaped.get("available").and_then(Value::as_i64), Some(163));
        let wallet = shaped
            .get("wallets")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(wallet.get("limit").and_then(Value::as_i64), Some(200));
        assert_eq!(wallet.get("used").and_then(Value::as_i64), Some(37));
        assert_eq!(wallet.get("resetAt").and_then(Value::as_str), Some("12:13"));
        assert!(wallet.get("displayName").and_then(Value::as_str).is_some_and(|text| text.contains("5")));
        // 上游给的是日期字符串 → 只进 planExpiresAt，不占用毫秒字段 expireAt
        let subscription = shaped.get("subscription").cloned().unwrap_or(Value::Null);
        assert_eq!(
            subscription.get("status").and_then(Value::as_str),
            Some("active")
        );
        assert!(subscription.get("planExpiresAt").is_some());
        assert!(subscription.get("expireAt").is_none());
    }

    /// ⚠️ 契约：读不到窗口 → **不写 `available`**（写了会被当成余额 0 而被跳过）
    #[test]
    fn shaped_usage_omits_available_without_windows() {
        let shaped = shape_usage(&json!({"codingplan_free": {"plan_name": ""}}));
        assert!(shaped.get("available").is_none());
        assert_eq!(
            shaped.get("wallets").and_then(Value::as_array).map(Vec::len),
            Some(0)
        );
        assert_eq!(
            shaped
                .get("subscription")
                .and_then(|value| value.get("status"))
                .and_then(Value::as_str),
            Some("unclaimed")
        );
    }

    /// 只有 `show_enable == 1` 的窗口展示；`call_limit <= 0` 的跳过
    #[test]
    fn only_shown_windows_are_reported() {
        let shaped = shape_usage(&json!({
            "rate_limit_windows": [
                {"show_enable": 0, "window_hours": 24, "call_limit": 1000, "calls_used": 1},
                {"show_enable": 1, "window_hours": 5, "call_limit": 0, "calls_used": 0},
                {"show_enable": 1, "window_hours": 5, "call_limit": 200, "calls_used": 200,
                 "quota_exhausted": true}
            ]
        }));
        let wallets = shaped
            .get("wallets")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(wallets.len(), 1);
        assert_eq!(wallets[0].get("exhausted"), Some(&Value::Bool(true)));
        // 用满时剩余夹到 0（不出现负数）
        assert_eq!(shaped.get("available").and_then(Value::as_i64), Some(0));
    }

    /// 已用超过上限时剩余夹到 0（计量回滚会让服务端报超额）
    #[test]
    fn remaining_is_clamped_at_zero() {
        let shaped = shape_usage(&json!({
            "rate_limit_windows": [
                {"show_enable": 1, "window_hours": 5, "call_limit": 10, "calls_used": 12}
            ]
        }));
        assert_eq!(shaped.get("available").and_then(Value::as_i64), Some(0));
        assert_eq!(
            shaped
                .get("subscription")
                .and_then(|value| value.get("remainQuota"))
                .and_then(Value::as_i64),
            Some(0)
        );
    }
}
