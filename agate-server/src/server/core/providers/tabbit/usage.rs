//! Tabbit 额度查询（**百分比制**）。
//!
//! ── 上游给的是什么 ──────────────────────────────────────────
//! `GET /api/commerce/quota/v1/usage?user_id=…&timezone=…` 只回
//! `usage_percentage`（已用百分比）与 `member_level`、`remaining_reset_minutes`、
//! `unused_reset_coupon_count`、`current_cycle_start/end` 等字段 —— **没有**
//! 积分包、没有金额。所以余额列显示的是「剩 X%」，不是「剩 N 积分」。
//! 这是本家与 kuku / 小浣熊那种「积分余额」最本质的差别，界面上的单位必须如实
//! 写 `%`（`ProviderAdapter::query_usage` 的归一化形状里 `unit` 就是这个用途）。
//!
//! ── 为什么要带 Cookie 签名请求 ──────────────────────────────
//! 上游只按 `user_id` 查就能出结果，但本适配统一走带 Cookie 的签名请求
//! （与目录 / 签到同一套），少一条特殊路径就少一处「为什么不一致」的疑问。
//! 缺 `user_id` 时上游回 422，本地先给可读错误。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::errors::GatewayError;

use super::credentials::{self, Credentials};
use super::{endpoints, http, Realm};

/// 重置时间的可读文案
fn reset_text(remaining_ms: Option<i64>) -> String {
    let Some(ms) = remaining_ms else {
        return String::new();
    };
    let total_seconds = ms / 1000;
    let days = total_seconds / 86_400;
    let hours = (total_seconds % 86_400) / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    if days > 0 {
        format!("{days} 天 {hours} 小时后重置")
    } else if hours > 0 {
        format!("{hours} 小时 {minutes} 分后重置")
    } else {
        format!("{minutes} 分钟后重置")
    }
}

/// 保留一位小数并去掉多余的 0（`42.50` → `42.5`，`42.00` → `42`）
fn round1(value: f64) -> String {
    let text = format!("{value:.1}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// 查询某账号的额度（归一化形状见 `ProviderAdapter::query_usage` 文档）。
pub async fn query(
    store: &AccountStore,
    realm: Realm,
    account_id: &str,
) -> Result<Value, GatewayError> {
    let record = store.tabbit_account_record(realm, account_id).ok_or_else(|| {
        GatewayError::with_status(
            404,
            format!("还没有 {} 账号", realm.label()),
        )
    })?;
    let credentials = Credentials::from_record(Some(&record))?;
    let proxy = match crate::server::core::proxies::resolve_account_proxy(record.get("proxy")) {
        Some(crate::server::core::proxies::ProxyResolution::Resolved(proxy)) => Some(proxy),
        Some(crate::server::core::proxies::ProxyResolution::Failed(reason)
            | crate::server::core::proxies::ProxyResolution::Broken(reason)) => {
            return Err(GatewayError::with_status(400, reason));
        }
        None => None,
    };
    let client = http::client(proxy.as_ref());
    query_with(&credentials, realm, &client).await
}

/// 用给定凭证查（`login` / 探活路径也能直接复用）
pub async fn query_with(
    credentials: &Credentials,
    realm: Realm,
    client: &reqwest::Client,
) -> Result<Value, GatewayError> {
    let uid = if credentials.uid.trim().is_empty() {
        credentials::uid_from_jwt(&credentials.token)
    } else {
        credentials.uid.clone()
    };
    if uid.is_empty() {
        return Err(GatewayError::with_status(
            400,
            "Tabbit 账号记录缺少 user_id，无法查询额度（请重新导入 Cookie）",
        ));
    }
    let path = endpoints::usage_path(&uid, realm.timezone());
    let value = http::signed_json(credentials, realm, client, "GET", &path, None, "/").await?;
    let data = value
        .get("data")
        .filter(|data| data.is_object())
        .unwrap_or(&value);
    let used = ["usage_percentage", "usage"]
        .iter()
        .find_map(|key| data.get(*key).and_then(http::as_f64))
        .ok_or_else(|| {
            GatewayError::with_status(502, "Tabbit 额度响应里没有 usage_percentage")
        })?;
    let used = used.clamp(0.0, 100.0);
    let remain = (100.0 - used).max(0.0);
    let member_level = ["member_level", "plan", "plan_name"]
        .iter()
        .filter_map(|key| data.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|text| !text.is_empty())
        .unwrap_or("")
        .to_string();
    // 上游给的是「还剩多少分钟/小时重置」，换算成绝对时刻更好展示
    let now = crate::server::logging::now_ms();
    let reset_at = if let Some(minutes) = data
        .get("remaining_reset_minutes")
        .and_then(http::as_i64)
        .filter(|minutes| *minutes > 0)
    {
        Some(now + minutes * 60 * 1000)
    } else if let Some(hours) = data
        .get("remaining_reset_hours")
        .and_then(http::as_i64)
        .filter(|hours| *hours > 0)
    {
        Some(now + hours * 60 * 60 * 1000)
    } else {
        None
    };
    let coupons = data
        .get("unused_reset_coupon_count")
        .and_then(http::as_i64)
        .unwrap_or(0);
    let cycle_start = data
        .get("current_cycle_start")
        .or_else(|| data.get("currentCycleStart"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let cycle_end = data
        .get("current_cycle_end")
        .or_else(|| data.get("currentCycleEnd"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let reset = reset_text(reset_at.map(|at| at - now));
    let balance_view = if reset.is_empty() {
        format!("剩 {}%", round1(remain))
    } else {
        format!("剩 {}% · {reset}", round1(remain))
    };
    let is_member = !member_level.is_empty() && !member_level.eq_ignore_ascii_case("free");

    let mut credits_row = json!({
        "name": "剩余额度",
        "remain": remain,
        "usedPercent": used,
        "percent": remain,
    });
    if !member_level.is_empty() {
        credits_row["plan"] = Value::String(member_level.clone());
    }
    if let Some(reset_at) = reset_at {
        credits_row["expireAt"] = Value::from(reset_at);
        credits_row["resetsAt"] = Value::from(reset_at);
    }
    if coupons > 0 {
        credits_row["resetCoupons"] = Value::from(coupons);
    }
    let mut windows_row = json!({
        "group": "额度",
        "label": "当前额度周期",
        "window": "当前周期",
        "remainingPercent": remain,
    });
    if let Some(reset_at) = reset_at {
        windows_row["resetsAt"] = Value::from(reset_at);
    }
    if !cycle_start.is_empty() || !cycle_end.is_empty() {
        windows_row["cycle"] = Value::String(format!("{cycle_start}–{cycle_end}"));
    }
    Ok(json!({
        "available": remain,
        // 百分比制：单位必须是 %，不能写成「积分」
        "unit": "%",
        "wallets": [{
            "type": "percent",
            "displayName": "剩余额度",
            "balance": remain,
            "total": 100.0,
            "remainingPercent": remain,
            "percent": remain,
            "balanceView": balance_view,
            "planName": member_level,
        }],
        "credits": [credits_row],
        "windows": [windows_row],
        "subscription": {
            "planName": member_level,
            "isMember": is_member,
            "resetAt": reset_at,
            "cycleStart": cycle_start,
            "cycleEnd": cycle_end,
            "resetCoupons": coupons,
        },
        "raw": value,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 剩余百分比 = 100 - 已用，且夹在 0..100（上游偶尔给越界值）
    #[test]
    fn remaining_is_clamped() {
        assert_eq!((100.0f64 - 37.5).clamp(0.0, 100.0), 62.5);
        // 上游给 120% 已用时，剩余必须是 0 而不是 -20
        let remain = (100.0f64 - 120.0f64.clamp(0.0, 100.0)).max(0.0);
        assert_eq!(remain, 0.0);
    }

    /// 重置文案按量级切换（天 / 小时 / 分钟）
    #[test]
    fn reset_text_switches_units() {
        assert!(reset_text(Some(2 * 86_400_000 + 3 * 3_600_000)).starts_with("2 天 3 小时"));
        assert!(reset_text(Some(3 * 3_600_000 + 5 * 60_000)).starts_with("3 小时 5 分"));
        assert!(reset_text(Some(42 * 60_000)).starts_with("42 分钟"));
        assert_eq!(reset_text(None), "");
    }

    /// 一位小数去掉多余 0
    #[test]
    fn round_one_drops_trailing_zeros() {
        assert_eq!(round1(42.50), "42.5");
        assert_eq!(round1(42.00), "42");
        assert_eq!(round1(0.0), "0");
    }
}
