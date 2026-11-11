//! QuWork 积分：`GET /api/user/self` → `points_entitlement`。
//!
//! ── 两处单位（上游 `usage.rs` 的注释，别照抄错）────────────────
//! ```text
//!   points_entitlement.total_points   已经是积分，**不能**再乘 100
//!   顶层的 amount                    单位是「百积分」，要 ×100
//! ```
//! `total_points` 优先（更精确）；没有它才回落到 `amount × 100`。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::errors::GatewayError;

use super::{client, endpoints, refresh};

/// 可用积分（纯函数）。`total_points` 已是积分；仅顶层 `amount` 需要 ×100。
pub fn parse_available(data: &Value) -> Option<f64> {
    let entitlement = data.get("points_entitlement").unwrap_or(&Value::Null);
    client::number(entitlement.get("total_points")).or_else(|| {
        client::number(data.get("amount"))
            .map(|amount| (amount * 100.0).round())
            .filter(|value| value.is_finite())
    })
}

/// 逐份积分读数（`credits[]`，本仓前端按形状探测展示）
fn credits_of(entitlement: &Value) -> Vec<Value> {
    let mut credits = Vec::new();
    for (id, name, field) in [
        ("limited", "限时积分", "limited_points"),
        ("permanent", "永久积分", "permanent_points"),
    ] {
        if let Some(remaining) = client::number(entitlement.get(field)) {
            credits.push(json!({
                "id": id,
                "name": name,
                "balance": remaining,
                "remain": remaining,
                "unit": "积分",
                "snapshot": true,
            }));
        }
    }
    if let Some(subscription) = entitlement
        .get("active_subscription")
        .filter(|value| value.is_object())
    {
        credits.push(json!({
            "id": "subscription",
            "name": subscription
                .get("plan_name")
                .and_then(Value::as_str)
                .unwrap_or("订阅积分"),
            "remain": client::number(subscription.get("remaining_points")),
            "total": client::number(subscription.get("total_points")),
            "used": client::number(subscription.get("used_points")),
            "grantAt": client::timestamp(subscription.get("started_at")),
            "expireAt": client::timestamp(subscription.get("expires_at")),
            "unit": "积分",
        }));
    }
    credits
}

/// 查积分（`api::accounts` 的 `usage` 字段形状：`{available, unit, credits, raw}`）。
pub async fn query(
    store: &AccountStore,
    account_id: &str,
) -> Result<Value, GatewayError> {
    let data = refresh::protected(store, account_id, "GET", endpoints::SELF_PATH, None)
        .await
        .and_then(|reply| reply.data("积分查询"))?;
    if !data.is_object() {
        return Err(GatewayError::with_status(502, "QuWork 积分返回格式异常"));
    }
    let entitlement = data.get("points_entitlement").unwrap_or(&Value::Null);
    let available = parse_available(&data);
    let credits = credits_of(entitlement);
    if available.is_none() && credits.is_empty() {
        return Err(GatewayError::with_status(
            502,
            "QuWork 未返回可识别的积分余额，未将未知余额当作 0",
        ));
    }
    let subscription = entitlement
        .get("active_subscription")
        .filter(|value| value.is_object());
    let plan = subscription
        .and_then(|value| value.get("plan_name"))
        .and_then(Value::as_str);
    Ok(json!({
        "available": available,
        "unit": "积分",
        "plan": plan,
        "credits": credits,
        "until": subscription.and_then(|value| client::timestamp(value.get("expires_at"))),
        "raw": data,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_points_is_already_points_not_scaled() {
        let data = json!({
            "amount": 5,
            "points_entitlement": { "total_points": 830.5 }
        });
        assert_eq!(parse_available(&data), Some(830.5));
    }

    #[test]
    fn amount_is_hundred_points_and_gets_scaled() {
        let data = json!({ "amount": 12, "points_entitlement": {} });
        assert_eq!(parse_available(&data), Some(1200.0));
    }

    #[test]
    fn unknown_balance_stays_none() {
        assert_eq!(parse_available(&json!({})), None);
        assert_eq!(parse_available(&json!({ "amount": -1 })), None);
    }
}
