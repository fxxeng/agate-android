//! Tabbit 每日签到（上游叫 `daily_sign_in`）。
//!
//! ── 上游是真实运营功能（不是占位接口）──────────────────────────
//! ```text
//! GET  /api/commerce/activity/v1/sign-in/status?scene_codes=daily_sign_in&scene_codes=desktop_pet
//! POST /api/commerce/activity/v1/sign-in
//!      body: {request_no, scene_codes:["daily_sign_in"], usage_percentage:"12.34%"}
//! ```
//! 响应按 `scene_code` 分组：`daily_sign_in` 是每日签到、`desktop_pet` 是养宠
//! （本适配只关心前者）。签到的奖励是**额度重置券**（`unused_reset_coupon_count`），
//! 不是积分 —— 与额度本身是百分比制一致。
//!
//! ── 两个「不空打」的判断 ────────────────────────────────────
//! 1. `activity_open == false` → 如实回「今日无活动」，**不去空打一次签到接口**：
//!    一个必然失败的按钮比没有更糟；
//! 2. `signed_today == true` → 回「今日已签到」，同样不空打。
//!
//! ── 返回形态（与 `billing::checkin` 的约定）──────────────────
//! `{success, alreadyCompleted, outcome, msg}` 四键：`outcome` 是
//! `claimed` / `done` / `inactive` / `failed`，见 `kuku::checkin` 的同一形状。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::errors::GatewayError;

use super::credentials::Credentials;
use super::{endpoints, http, sign, usage, Realm};

/// 宽松布尔（上游三种类型都出现过）
fn bool_of(value: &Value) -> bool {
    match value {
        Value::Bool(flag) => *flag,
        Value::String(text) => text.eq_ignore_ascii_case("true") || text == "1",
        Value::Number(number) => number.as_i64().unwrap_or(0) != 0,
        _ => false,
    }
}

/// 从响应里取 `daily_sign_in` 那一条
fn daily_entry(value: &Value) -> Option<&Value> {
    value
        .get("results")
        .and_then(Value::as_array)
        .and_then(|results| {
            results.iter().find(|entry| {
                entry.get("scene_code").and_then(Value::as_str) == Some("daily_sign_in")
            })
        })
}

fn report(outcome: &str, msg: &str) -> Value {
    json!({
        "success": outcome == "claimed",
        "alreadyCompleted": outcome == "done",
        "outcome": outcome,
        "msg": msg,
    })
}

/// 某账号的每日签到（自动签到框架的 claim）
pub async fn claim_daily_checkin(
    store: &AccountStore,
    realm: Realm,
    account_id: &str,
) -> Result<Value, GatewayError> {
    let record = store.tabbit_account_record(realm, account_id).ok_or_else(|| {
        GatewayError::with_status(404, format!("还没有 {} 账号", realm.label()))
    })?;
    let credentials = Credentials::from_record(Some(&record))?;
    let proxy = match crate::server::core::proxies::resolve_account_proxy(record.get("proxy")) {
        Some(crate::server::core::proxies::ProxyResolution::Resolved(proxy)) => Some(proxy),
        Some(crate::server::core::proxies::ProxyResolution::Failed(reason)) => {
            return Err(GatewayError::with_status(400, reason));
        }
        None => None,
    };
    let client = http::client(proxy.as_ref());
    claim(&credentials, realm, &client).await
}

/// 用给定凭证签到
pub async fn claim(
    credentials: &Credentials,
    realm: Realm,
    client: &reqwest::Client,
) -> Result<Value, GatewayError> {
    // 1) 先查状态：活动没开 / 今天已签，都不空打
    let status = http::signed_json(
        credentials,
        realm,
        client,
        "GET",
        endpoints::CHECKIN_STATUS_PATH,
        None,
        "/member/usage",
    )
    .await?;
    if let Some(entry) = daily_entry(&status) {
        if !bool_of(entry.get("activity_open").unwrap_or(&Value::Bool(true))) {
            return Ok(report("inactive", "Tabbit 今日签到活动未开启"));
        }
        if bool_of(entry.get("signed_today").unwrap_or(&Value::Bool(false))) {
            return Ok(report("done", "今日已签到"));
        }
    }

    // 2) 真签。上游把「当前已用百分比」一起收下（奖励口径），查不到就按 0.00%
    let percent = match usage::query_with(credentials, realm, client).await {
        Ok(report) => report
            .get("credits")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("usedPercent"))
            .and_then(http::as_f64)
            .map(|value| format!("{value:.2}%"))
            .unwrap_or_else(|| "0.00%".to_string()),
        Err(_) => "0.00%".to_string(),
    };
    let body = json!({
        "request_no": sign::random_uuid(),
        "scene_codes": ["daily_sign_in"],
        "usage_percentage": percent,
    });
    let value = http::signed_json(
        credentials,
        realm,
        client,
        "POST",
        endpoints::CHECKIN_PATH,
        Some(body),
        "/member/usage",
    )
    .await?;
    let Some(entry) = daily_entry(&value) else {
        return Ok(report("inactive", "上游未返回每日签到的结果"));
    };
    let result = entry
        .get("sign_in_result")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    match result.as_str() {
        "success" => {
            // 奖励是额度重置券（`skipped` 表示这次没有券，不是失败）
            let reward = entry
                .get("usage_reward_result")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty() && !text.eq_ignore_ascii_case("skipped"));
            let message = match reward {
                Some(reward) => format!("Tabbit 签到成功（额度奖励：{reward}）"),
                None => "Tabbit 签到成功".to_string(),
            };
            Ok(report("claimed", &message))
        }
        "already_signed" => Ok(report("done", "今日已签到")),
        "" => Ok(report("inactive", "上游没有给出明确的签到结论")),
        other => Ok(report("failed", &format!("签到未成功：{other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 四态映射：只有 `claimed` 算 success，只有 `done` 算 alreadyCompleted
    #[test]
    fn report_maps_the_four_outcomes() {
        let claimed = report("claimed", "ok");
        assert_eq!(claimed["success"], true);
        assert_eq!(claimed["alreadyCompleted"], false);

        let done = report("done", "today");
        assert_eq!(done["success"], false);
        assert_eq!(done["alreadyCompleted"], true);

        for outcome in ["inactive", "failed"] {
            let value = report(outcome, "x");
            assert_eq!(value["success"], false);
            assert_eq!(value["alreadyCompleted"], false);
            assert_eq!(value["outcome"], outcome);
        }
    }

    /// `daily_sign_in` 要在 `results` 里被挑出来（`desktop_pet` 忽略）
    #[test]
    fn daily_entry_picks_the_right_scene() {
        let value = json!({
            "results": [
                { "scene_code": "desktop_pet", "signed_today": true },
                { "scene_code": "daily_sign_in", "signed_today": false, "activity_open": true }
            ]
        });
        let entry = daily_entry(&value).expect("应能找到每日签到");
        assert_eq!(entry["signed_today"], false);

        assert!(daily_entry(&json!({ "results": [] })).is_none());
        assert!(daily_entry(&json!({})).is_none());
    }

    /// 宽松布尔：字符串 "0" / "false" 都算假（上游字段类型不稳定）
    #[test]
    fn bool_of_accepts_three_type_shapes() {
        assert!(bool_of(&json!(true)));
        assert!(bool_of(&json!("true")));
        assert!(bool_of(&json!(1)));
        assert!(!bool_of(&json!(false)));
        assert!(!bool_of(&json!("0")));
        assert!(!bool_of(&json!(0)));
        assert!(!bool_of(&Value::Null));
    }
}
