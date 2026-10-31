//! ModelScope 余额 / 用量查询：魔粒钱包 + 今日已用调用数。
//!
//! ── 两个接口（都在**网页站**，cookie 鉴权）───────────────────────
//! ```text
//! GET {网页站}/openapi/v1/magicubes/balance
//!   → {"success":true,"data":{"total_balance":108,"available_balance":106,"frozen_amount":2}}
//!
//! GET {网页站}/api/v1/inference/rate-limit
//!   → {"Code":200,"Success":true,"Data":{"currentUsagePerDay":72}, …}
//! ```
//! 第一个是**主读数**（魔粒钱包）；第二个是「今天已经用掉多少次调用」。
//!
//! ── 为什么第二个失败只降级、不整条失败 ────────────────────
//! 主问题是「我还有多少魔粒」；`rate-limit` 是附加读数。让一个附加接口的抖动
//! 把余额整条变成红色失败，用户会以为钱包也读不到了 —— 所以它失败只少给那一行，
//! `raw` 里如实标成 `null`（是「没问到」，不是「上游给了 0」）。
//!
//! ── 量纲：三行明细里只有前两行是「魔粒」────────────────────────
//! `currentUsagePerDay` 的单位是**调用次数**，不是魔粒。它不进 `wallets`
//! （那是钱包明细，同一量纲），而是进 `credits[]`（逐份读数，名字自带量纲）。
//! 名字里写明「次」是为了不让界面把它读成「今天用掉了 72 魔粒」。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use super::client;
use super::region::Region;

/// 魔粒钱包路径（相对网页站）
const BALANCE_PATH: &str = "/openapi/v1/magicubes/balance";

/// 今日用量路径（相对网页站）
const RATE_LIMIT_PATH: &str = "/api/v1/inference/rate-limit";

/// 从一份 JSON 里取一个数字（字段缺失 / 非法时 None）。
///
/// 上游有把数字序列化成字符串的形态，两种都认。
fn pick_number(data: &Value, key: &str) -> Option<f64> {
    let value = data.get(key)?;
    if let Some(number) = value.as_f64() {
        return Some(number);
    }
    value.as_str().and_then(|text| text.trim().parse::<f64>().ok())
}

/// 查询魔粒余额与今日用量。
pub async fn query(region: Region, token: &str) -> Result<Value, String> {
    // ── 主读数：魔粒钱包 ────────────────────────────────────
    let (status, balance_payload) =
        client::site_get(region, BALANCE_PATH, token, "魔粒余额查询").await?;
    if status == 401 || status == 403 {
        return Err(client::expired_message(region, status, true));
    }
    if !(200..300).contains(&status) {
        return Err(format!("魔粒余额查询返回 HTTP {status}"));
    }
    if !client::succeeded(&balance_payload) {
        let message = client::message_of(&balance_payload);
        return Err(if message.trim().is_empty() {
            format!("魔粒余额查询失败（上游 Code={}）", client::code_of(&balance_payload))
        } else {
            format!("魔粒余额查询失败：{message}")
        });
    }
    let wallet = client::data_of(&balance_payload);
    let available = pick_number(&wallet, "available_balance").unwrap_or(0.0);
    let total = pick_number(&wallet, "total_balance");
    let frozen = pick_number(&wallet, "frozen_amount").unwrap_or(0.0);

    // ── 附加读数：今日已用调用数（失败只降级，见模块头）──────────
    let (used_today, rate_limit_raw) =
        match client::site_get(region, RATE_LIMIT_PATH, token, "今日用量查询").await {
            Ok((status, payload))
                if (200..300).contains(&status) && client::succeeded(&payload) =>
            {
                (pick_number(&client::data_of(&payload), "currentUsagePerDay"), payload)
            }
            Ok((_, payload)) => (None, payload),
            Err(error) => {
                crate::server::logging::verbose(
                    "[Usage]",
                    &format!("ModelScope 今日用量未取到（余额照常返回）：{error}"),
                );
                (None, Value::Null)
            }
        };

    // ── 归一形状（trait 契约的 `wallets` / `credits` / `available`）──
    let mut wallet_row = json!({
        "type": "magicubes",
        "displayName": "魔粒余额",
        "balance": available,
        "frozen": frozen,
    });
    if let (Some(total), Some(object)) = (total, wallet_row.as_object_mut()) {
        object.insert("total".to_string(), Value::from(total));
    }

    let mut credits = vec![
        json!({ "name": "可用魔粒", "remain": available }),
        json!({ "name": "冻结魔粒", "remain": frozen }),
    ];
    // 量纲是「次」（见模块头）—— 名字里写明，别让界面读成魔粒
    if let Some(used) = used_today {
        credits.push(json!({ "name": "今日已用（次）", "remain": used, "daily": true }));
    }

    Ok(json!({
        "available": available,
        "unit": "魔粒",
        "wallets": [wallet_row],
        "credits": credits,
        "raw": {
            "balance": balance_payload,
            "rateLimit": rate_limit_raw,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 数字与「数字字符串」两种形态都要读出来（上游两种都发过）
    #[test]
    fn numbers_accept_string_form() {
        let data = json!({"available_balance": "106.5", "frozen_amount": 2});
        assert_eq!(pick_number(&data, "available_balance"), Some(106.5));
        assert_eq!(pick_number(&data, "frozen_amount"), Some(2.0));
        assert_eq!(pick_number(&data, "missing"), None);
        assert_eq!(pick_number(&data, "available_balance").unwrap_or(0.0), 106.5);
    }
}
