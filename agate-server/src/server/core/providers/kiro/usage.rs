//! Kiro 余额：`Get-Usage-Limits` 的月额度、免费试用与 Bonus 积分。
//!
//! ── 两套调用路径（由凭证类型决定，见 `http::Operation`）──────
//! ```text
//!   OAuth    GET  /Get-Usage-Limits?origin=KIRO_CLI&resourceType=CREDIT&profileArn=…
//!   API key  POST /?origin=KIRO_CLI   X-Amz-Target: …GetUsageLimits
//! ```
//!
//! ── 三个窗口，三种语义 ──────────────────────────────────────
//! ```text
//!   Free trial   有 ACTIVE 状态 + 到期时刻，只用不重置
//!   Bonus        `aside: true`：不重置、只做展示 —— 它有余量**不代表**
//!                「本月还能用」，所以不许参与「主额度是否用完」的判断
//!   月额度        主窗口，`nextDateReset` 时重置
//! ```
//! `aside` 这个标记是给编排层看的：主额度耗尽的账号仍然有 bonus 时，不该因为
//! 「总剩余 > 0」就判定账号可用 —— 那会让它反复排队重试。
//!
//! ── 时间单位：上游给**秒**，我们出**毫秒** ──────────────────
//! `freeTrialExpiry` / `expiresAt` / `nextDateReset` 都是 epoch 秒（AWS 的老
//! 习惯），而网关对外的 `resetsAt` / `runsOutAt` 一律毫秒（前端按数字直接
//! `Date(ms)` 渲染）。换算只在这里做一次。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::proxies::ResolvedProxy;
use crate::server::errors::GatewayError;

use super::credentials::{now_ms, KiroCredentials};
use super::http;

fn number(value: Option<&Value>) -> f64 {
    value
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        })
        .filter(|number| number.is_finite())
        .unwrap_or(0.0)
}

/// epoch **秒** → 毫秒（0 / 负数 / 缺失 → `None`）
fn seconds_to_ms(value: Option<&Value>) -> Option<i64> {
    let seconds = number(value);
    if seconds <= 0.0 {
        return None;
    }
    Some((seconds as i64).saturating_mul(1000))
}

/// 订阅名规整（`KIRO FREE` → `Kiro Free`）。
///
/// 上游给的是**全大写**（`KIRO PRO` / `KIRO FREE`），照原样展示在界面上像系统
/// 报错而不是订阅名，所以逐词小写后首字母大写。`+` 这类符号原样保留
/// （`PRO+` → `Pro+`，不能按单词边界切）。
pub fn plan_name(title: &str) -> String {
    title
        .split_whitespace()
        .map(|word| {
            if word.eq_ignore_ascii_case("kiro") {
                "Kiro".to_string()
            } else {
                let lower = word.to_lowercase();
                let mut chars = lower.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 数字的短写法（去掉无意义的尾零；大数不写成 1e6）
fn short(value: f64) -> String {
    if value.fract().abs() < 0.005 {
        return format!("{}", value.round() as i64);
    }
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn count(used: f64, limit: f64) -> String {
    format!("{} / {}", short(used), short(limit))
}

/// 一条额度窗口的明细行（界面 `credits[]` 契约：面额 / 已用 / 剩余）
fn credit_row(name: &str, used: f64, limit: f64, remaining: f64, remaining_percent: f64) -> Value {
    json!({
        "name": name,
        "remain": remaining,
        "total": limit,
        "used": used,
        "unit": "credits",
        "remainingPercent": remaining_percent.clamp(0.0, 100.0),
    })
}

/// 把 `Get-Usage-Limits` 的返回折成网关的用量报告（**纯函数**）
pub fn report(payload: &Value) -> Value {
    let now = now_ms();
    let mut windows: Vec<Value> = Vec::new();
    let mut credits: Vec<Value> = Vec::new();
    // 主额度窗口的剩余（`available` 取它，不取总和 —— 见模块头的 `aside`）
    let mut primary_left: Option<f64> = None;
    for breakdown in payload
        .get("usageBreakdownList")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(trial) = breakdown.get("freeTrialInfo") {
            let active = trial
                .get("freeTrialStatus")
                .and_then(Value::as_str)
                .is_some_and(|value| value.eq_ignore_ascii_case("ACTIVE"));
            let limit = number(trial.get("usageLimitWithPrecision"));
            if active && limit > 0.0 {
                let used = number(trial.get("currentUsageWithPrecision"));
                let used_percent = (100.0 * used / limit).clamp(0.0, 100.0);
                let mut window = json!({
                    "name": "Free trial",
                    "label": "Free trial",
                    "used": used_percent,
                    "remainingPercent": (100.0 - used_percent).clamp(0.0, 100.0),
                    "remaining": (limit - used).max(0.0),
                    "display": count(used, limit),
                    "amount": used,
                    "limit": limit,
                    "unit": "credits",
                });
                let expiry = seconds_to_ms(trial.get("freeTrialExpiry"));
                if let Some(at) = expiry {
                    window["resetsAt"] = json!(at);
                }
                let mut credit = credit_row(
                    "Free trial",
                    used,
                    limit,
                    (limit - used).max(0.0),
                    100.0 - used_percent,
                );
                if let Some(at) = expiry {
                    credit["expireAt"] = json!(at);
                }
                windows.push(window);
                credits.push(credit);
            }
        }
        // Bonus：不重置、只展示（`aside`）。过期的、没用量的不收
        let mut bonuses: Vec<Value> = Vec::new();
        for bonus in breakdown
            .get("bonuses")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let active = bonus
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|value| value.eq_ignore_ascii_case("ACTIVE"));
            let limit = number(bonus.get("usageLimit"));
            let expires = seconds_to_ms(bonus.get("expiresAt"));
            if !active || limit <= 0.0 {
                continue;
            }
            if expires.is_some_and(|at| at <= now) {
                continue;
            }
            let used = number(bonus.get("currentUsage"));
            let left = (limit - used).max(0.0);
            let name = bonus
                .get("displayName")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("Bonus credits");
            let used_percent = (100.0 * used / limit).clamp(0.0, 100.0);
            let mut window = json!({
                "name": name,
                "label": name,
                "used": used_percent,
                "remainingPercent": (100.0 - used_percent).clamp(0.0, 100.0),
                "remaining": left,
                "display": count(used, limit),
                "amount": used,
                "limit": limit,
                "unit": "credits",
                "aside": true,
            });
            if let Some(at) = expires {
                if left > 0.0 {
                    window["runsOutAt"] = json!(at);
                    window["runsOut"] = json!(left);
                }
            }
            let mut credit = credit_row(name, used, limit, left, 100.0 - used_percent);
            if let Some(at) = expires {
                credit["expireAt"] = json!(at);
            }
            bonuses.push(window);
            credits.push(credit);
        }
        let used = number(breakdown.get("currentUsageWithPrecision"));
        let limit = number(breakdown.get("usageLimitWithPrecision"));
        if limit > 0.0 {
            let reset = seconds_to_ms(breakdown.get("nextDateReset"))
                .or_else(|| seconds_to_ms(payload.get("nextDateReset")));
            // 主额度用完但 bonus 还有余量：主窗口标 `aside`（不让它冒充「还能用」）
            let carried = used >= limit
                && bonuses.iter().any(|bonus| {
                    number(bonus.get("amount")) < number(bonus.get("limit"))
                });
            let name = breakdown
                .get("displayNamePlural")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .or_else(|| breakdown.get("displayName").and_then(Value::as_str))
                .unwrap_or("Credits");
            let used_percent = (100.0 * used / limit).clamp(0.0, 100.0);
            let mut window = json!({
                "name": name,
                "label": name,
                "used": used_percent,
                "remainingPercent": (100.0 - used_percent).clamp(0.0, 100.0),
                "remaining": (limit - used).max(0.0),
                "display": count(used, limit),
                "amount": used,
                "limit": limit,
                "unit": "credits",
                "span": 30 * 24 * 3600,
            });
            if let Some(at) = reset {
                window["resetsAt"] = json!(at);
            }
            if carried {
                window["aside"] = json!(true);
            }
            let mut credit = credit_row(
                name,
                used,
                limit,
                (limit - used).max(0.0),
                100.0 - used_percent,
            );
            if let Some(at) = reset {
                credit["resetAt"] = json!(at);
                credit["rolling"] = json!(true);
            }
            windows.push(window);
            credits.push(credit);
            if primary_left.is_none() {
                primary_left = Some((limit - used).max(0.0));
            }
        }
        windows.extend(bonuses);
    }
    let email = payload
        .pointer("/userInfo/email")
        .and_then(Value::as_str)
        .unwrap_or("");
    let title = payload
        .pointer("/subscriptionInfo/subscriptionTitle")
        .and_then(Value::as_str)
        .unwrap_or("");
    let wallets: Vec<Value> = credits
        .iter()
        .filter_map(|credit| {
            let name = credit.get("name").and_then(Value::as_str)?;
            let remain = credit.get("remain").and_then(Value::as_f64)?;
            Some(json!({
                "type": "credits",
                "displayName": name,
                "balance": remain,
            }))
        })
        .collect();
    let mut out = json!({
        "available": primary_left,
        "unit": "credits",
        "wallets": wallets,
        "credits": credits,
        "windows": windows,
        "raw": payload,
    });
    if let Some(left) = primary_left {
        out["availableView"] = json!(format!("剩 {} 积分", short(left)));
    }
    if !title.is_empty() {
        out["subscription"] = json!({ "planName": plan_name(title) });
    }
    if !email.is_empty() {
        out["user"] = json!(email);
    }
    out
}

/// 查一次余额（凭证由调用方准备好 —— 续期是 `refresh` 的职责）
pub async fn query(
    credentials: &KiroCredentials,
    proxy: Option<&ResolvedProxy>,
) -> Result<Value, GatewayError> {
    let payload = http::control(credentials, http::OP_USAGE, &Value::Null, proxy).await?;
    Ok(report(&payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> Value {
        json!({
            "userInfo": { "email": "a@b.com" },
            "subscriptionInfo": { "subscriptionTitle": "KIRO PRO" },
            "nextDateReset": 1_900_000_000,
            "usageBreakdownList": [{
                "displayName": "Credit",
                "displayNamePlural": "Credits",
                "currentUsageWithPrecision": 250.0,
                "usageLimitWithPrecision": 1000.0,
                "freeTrialInfo": {
                    "freeTrialStatus": "ACTIVE",
                    "currentUsageWithPrecision": 10.0,
                    "usageLimitWithPrecision": 50.0,
                    "freeTrialExpiry": 1_950_000_000
                },
                "bonuses": [{
                    "status": "ACTIVE",
                    "displayName": "Welcome bonus",
                    "currentUsage": 20.0,
                    "usageLimit": 200.0,
                    "expiresAt": 1_950_000_000
                }]
            }]
        })
    }

    /// 三个窗口都要出、主窗口的 `available` 只算主额度
    #[test]
    fn the_report_keeps_trial_bonus_and_monthly_apart() {
        let out = report(&payload());
        let windows = out["windows"].as_array().expect("有 windows");
        assert_eq!(windows.len(), 3, "试用 + 月度 + bonus");
        assert_eq!(out["available"], json!(750.0), "只算主额度，不含 bonus");
        assert_eq!(out["unit"], "credits");
        assert_eq!(out["subscription"]["planName"], "Kiro Pro");
        assert_eq!(out["user"], "a@b.com");
        assert_eq!(out["availableView"], "剩 750 积分");
        // bonus 必须带 aside（只展示，不参与「还能不能用」）
        let bonus = windows
            .iter()
            .find(|window| window["name"] == "Welcome bonus")
            .expect("有 bonus");
        assert_eq!(bonus["aside"], true);
        assert_eq!(bonus["remaining"], json!(180.0));
    }

    /// 秒 → 毫秒：`resetsAt` 是毫秒（前端直接 Date(ms)），输入是 epoch 秒
    #[test]
    fn timestamps_are_converted_from_seconds_to_ms() {
        let out = report(&payload());
        let windows = out["windows"].as_array().expect("有 windows");
        let monthly = windows
            .iter()
            .find(|window| window["name"] == "Credits")
            .expect("有月度窗口");
        assert_eq!(monthly["resetsAt"], json!(1_900_000_000_000_i64));
        let trial = windows
            .iter()
            .find(|window| window["name"] == "Free trial")
            .expect("有试用");
        assert_eq!(trial["resetsAt"], json!(1_950_000_000_000_i64));
    }

    /// 过期 / 非 ACTIVE / 零额度的 bonus 一律不收
    #[test]
    fn inactive_or_expired_bonuses_are_dropped() {
        let mut body = payload();
        body["usageBreakdownList"][0]["bonuses"] = json!([
            { "status": "EXPIRED", "usageLimit": 200.0, "currentUsage": 0.0 },
            { "status": "ACTIVE", "usageLimit": 0.0 },
            { "status": "ACTIVE", "usageLimit": 100.0, "expiresAt": 1 }
        ]);
        let out = report(&body);
        let windows = out["windows"].as_array().expect("有 windows");
        assert_eq!(windows.len(), 2, "只剩试用 + 月度");
    }

    /// 非 ACTIVE 的试用不算（Free trial 只在活动期内展示）
    #[test]
    fn an_inactive_trial_is_skipped() {
        let mut body = payload();
        body["usageBreakdownList"][0]["freeTrialInfo"]["freeTrialStatus"] = json!("EXPIRED");
        let out = report(&body);
        let windows = out["windows"].as_array().expect("有 windows");
        assert!(
            windows.iter().all(|window| window["name"] != "Free trial"),
            "过期试用不该出现"
        );
    }

    /// 主额度用完但 bonus 有余量：主窗口标 aside（不许冒充可用）
    #[test]
    fn an_exhausted_primary_window_is_marked_aside() {
        let mut body = payload();
        body["usageBreakdownList"][0]["currentUsageWithPrecision"] = json!(1000.0);
        let out = report(&body);
        let windows = out["windows"].as_array().expect("有 windows");
        let monthly = windows
            .iter()
            .find(|window| window["name"] == "Credits")
            .expect("有月度窗口");
        assert_eq!(monthly["aside"], true);
        assert_eq!(monthly["remaining"], json!(0.0));
        assert_eq!(out["available"], json!(0.0));
    }

    /// 空 / 缺字段的响应不 panic，给一份空报告
    #[test]
    fn a_payload_without_breakdowns_yields_an_empty_report() {
        for empty in [json!({}), json!(null), json!({ "usageBreakdownList": [] })] {
            let out = report(&empty);
            assert_eq!(out["available"], Value::Null, "判不出就给 null");
            assert_eq!(out["windows"].as_array().map(Vec::len), Some(0));
            assert!(out.get("subscription").is_none());
            assert!(out.get("user").is_none());
        }
    }

    /// 订阅名的规整（全大写 → 首字母大写，KIRO 固定成 Kiro）
    #[test]
    fn plan_names_are_normalised() {
        assert_eq!(plan_name("KIRO FREE"), "Kiro Free");
        assert_eq!(plan_name("kiro pro+"), "Kiro Pro+");
        assert_eq!(plan_name(""), "");
        assert_eq!(plan_name("  POWER  "), "Power");
    }

    /// 数字短写法（尾零、整数、小数）
    #[test]
    fn numbers_are_shortened() {
        assert_eq!(short(42.0), "42");
        assert_eq!(short(42.5), "42.5");
        assert_eq!(short(42.25), "42.25");
        assert_eq!(count(250.0, 1000.0), "250 / 1000");
    }
}
