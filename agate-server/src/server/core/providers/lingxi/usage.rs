//! 智点余额解析（**两种来源形状都认**）。
//!
//! 方案 §1 标出的冲突：`Coding2API` 直接读 `total_balance`，
//! `dsh-router-wpslingxi` 说上游没有这个字段、要累加
//! `bonus_credits[].balance + purchased_credits[].balance`。
//! 两个形状都解析；**都拿不到就返回 None**（界面写「暂不可用」），
//! 绝不拿 0 冒充 —— 0 会被读成「额度用完了」，与「读不出」是两回事。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic。

use serde_json::Value;

const EXPIRY_WINDOW_DAYS_MS: i64 = 15 * 24 * 3_600_000;

#[derive(Clone, Debug, PartialEq)]
pub struct Balance {
    pub total: f64,
    /// 15 天内最近一个赠送批次的到期时间戳；没有符合条件的批次则为 None。
    pub earliest_expiry_ms: Option<i64>,
}

fn number(value: &Value, key: &str) -> Option<f64> {
    value
        .get(key)
        .and_then(|node| node.as_f64().or_else(|| node.as_str().and_then(|s| s.parse::<f64>().ok())))
}

fn batch_sum(root: &Value, key: &str) -> f64 {
    root.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| number(item, "balance"))
                .sum::<f64>()
        })
        .unwrap_or(0.0)
}

fn earliest_expiry(root: &Value, now_ms: i64) -> Option<i64> {
    root.get("bonus_credits")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("expire_time")
                        .or_else(|| item.get("expireTime"))
                        .and_then(Value::as_i64)
                })
                .filter(|expire| *expire > now_ms && *expire - now_ms <= EXPIRY_WINDOW_DAYS_MS)
                .min()
        })
        .unwrap_or(None)
}

/// `root` 可以是整包响应，也可以是 `data` 子对象。
pub fn parse_balance(root: &Value, now_ms: i64) -> Option<Balance> {
    let data = root.get("data").unwrap_or(root);
    let direct = number(data, "total_balance").or_else(|| number(root, "total_balance"));
    let total = match direct {
        Some(value) => value,
        None => {
            let summed = batch_sum(data, "bonus_credits") + batch_sum(data, "purchased_credits");
            let has_batches = data.get("bonus_credits").and_then(Value::as_array).is_some()
                || data.get("purchased_credits").and_then(Value::as_array).is_some();
            if !has_batches {
                return None;
            }
            summed
        }
    };
    Some(Balance {
        total,
        earliest_expiry_ms: earliest_expiry(data, now_ms),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn total_balance_shape_is_accepted() {
        let b = parse_balance(&json!({ "data": { "total_balance": 812.5 } }), 0);
        assert_eq!(b, Some(Balance { total: 812.5, earliest_expiry_ms: None }));
    }

    #[test]
    fn batch_shape_is_summed_when_total_is_absent() {
        let b = parse_balance(
            &json!({ "data": {
                "bonus_credits": [{ "balance": 100, "expire_time": 86_400_000 }],
                "purchased_credits": [{ "balance": 50 }]
            } }),
            1_000,
        );
        let b = b.unwrap_or(Balance { total: -1.0, earliest_expiry_ms: None });
        assert_eq!(b.total, 150.0);
        assert_eq!(b.earliest_expiry_ms, Some(86_400_000));
    }

    #[test]
    fn numeric_strings_are_accepted() {
        let b = parse_balance(&json!({ "total_balance": "233" }), 0);
        assert_eq!(b.map(|v| v.total), Some(233.0));
    }

    #[test]
    fn unreadable_balance_is_none_not_zero() {
        assert_eq!(parse_balance(&json!({ "data": {} }), 0), None);
        assert_eq!(parse_balance(&json!({ "error": "upstream busy" }), 0), None);
    }

    #[test]
    fn expiry_outside_the_15_day_window_is_not_reported() {
        let b = parse_balance(
            &json!({ "data": {
                "bonus_credits": [{ "balance": 100, "expire_time": 30 * 86_400_000 }]
            } }),
            0,
        );
        assert_eq!(b.and_then(|v| v.earliest_expiry_ms), None);
    }
}
