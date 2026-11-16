//! 账号余额排序的比较基准与状态判定。
//!
//! ── 为什么余额不能直接比大小 ────────────────────────────────────
//! 各家的「余额」单位不同：有的是积分、有的是次数、有的是百分比。
//! 单位不同就不能排序（把 80% 和 1200 积分放一起比毫无意义），
//! 所以这里先算出「可比值」`comparable()`，再判整组的状态 `state()`：
//! 只有全部账号单位一致（`Ready`）时才真的按余额排。

use std::cmp::Ordering;
use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Value;

use crate::server::config::BalanceSortDirection;

use super::usage_records;

/// 一个账号的余额快照（排序用的归一化形态）。
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSortBalance {
    pub remaining: Option<f64>,
    pub unit: String,
    pub unlimited: bool,
}

impl AccountSortBalance {
    /// 从归一化的用量 JSON 里提取余额与单位。
    ///
    /// 单位的取法有优先级：顶层 `unit` → `quotaSources` 里 id 为 `account`
    /// 的那条的 `unit` → 有 `totalLeft` 时兜底为「积分」→ 空。
    /// `credit`/`credits` 统一显示成「积分」（各家英文写法不一致）。
    pub fn from_usage(usage: &Value) -> Self {
        let (remaining, unlimited) = usage_records::extract_remaining(usage);
        let account_unit = usage
            .get("quotaSources")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|source| source.get("id").and_then(Value::as_str) == Some("account"))
            .and_then(|source| source.get("unit"))
            .and_then(Value::as_str);
        let unit = usage
            .get("unit")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|unit| !unit.is_empty())
            .or_else(|| account_unit.map(str::trim).filter(|unit| !unit.is_empty()))
            .unwrap_or_else(|| if usage.get("totalLeft").is_some() { "积分" } else { "" });
        let unit = if unit.eq_ignore_ascii_case("credit") || unit.eq_ignore_ascii_case("credits") {
            "积分"
        } else {
            unit
        };
        Self { remaining, unlimited, unit: unit.to_string() }
    }

    /// 可参与排序的数值。不限量、无单位、百分比都不可比。
    pub fn comparable(&self) -> Option<f64> {
        if self.unlimited || self.unit.is_empty() || self.unit == "%" {
            return None;
        }
        self.remaining.filter(|value| value.is_finite())
    }
}

/// 一组账号能否按余额排序。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BalanceSortState {
    /// 单位一致，可以排。
    Ready,
    /// 没有任何可比数值。
    NoData,
    /// 单位不一致，不能排。
    MixedUnits,
    /// 该提供商不支持余额查询。
    Unsupported,
}

/// 判定整组状态。`supported` 由调用方按提供商能力给出。
pub fn state<'a>(
    supported: bool,
    balances: impl Iterator<Item = &'a AccountSortBalance>,
) -> BalanceSortState {
    if !supported {
        return BalanceSortState::Unsupported;
    }
    let units: BTreeSet<&str> = balances
        .filter(|balance| balance.comparable().is_some())
        .map(|balance| balance.unit.as_str())
        .collect();
    match units.len() {
        0 => BalanceSortState::NoData,
        1 => BalanceSortState::Ready,
        _ => BalanceSortState::MixedUnits,
    }
}

/// 比较两个余额。**有值的一律排在无值之前**（无论方向），
/// 因为「查不到余额」不该被当成「余额最低」而排到后面去用。
pub fn compare(
    left: Option<f64>,
    right: Option<f64>,
    direction: BalanceSortDirection,
) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => {
            let order = left.partial_cmp(&right).unwrap_or(Ordering::Equal);
            match direction {
                BalanceSortDirection::High => order.reverse(),
                BalanceSortDirection::Low => order,
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn balance(remaining: Option<f64>, unit: &str) -> AccountSortBalance {
        AccountSortBalance {
            remaining,
            unit: unit.to_string(),
            unlimited: false,
        }
    }

    #[test]
    fn unlimited_is_not_comparable() {
        let item = AccountSortBalance {
            remaining: Some(100.0),
            unit: "积分".into(),
            unlimited: true,
        };
        assert!(item.comparable().is_none());
    }

    #[test]
    fn percent_is_not_comparable() {
        assert!(balance(Some(80.0), "%").comparable().is_none());
    }

    #[test]
    fn empty_unit_is_not_comparable() {
        assert!(balance(Some(80.0), "").comparable().is_none());
    }

    #[test]
    fn finite_value_is_comparable() {
        assert_eq!(balance(Some(80.0), "积分").comparable(), Some(80.0));
    }

    #[test]
    fn infinite_value_is_not_comparable() {
        assert!(balance(Some(f64::INFINITY), "积分").comparable().is_none());
    }

    #[test]
    fn state_unsupported_when_provider_cannot_query() {
        let items = [balance(Some(1.0), "积分")];
        assert_eq!(state(false, items.iter()), BalanceSortState::Unsupported);
    }

    #[test]
    fn state_no_data_when_all_incomparable() {
        let items = [balance(Some(1.0), "%"), balance(None, "积分")];
        assert_eq!(state(true, items.iter()), BalanceSortState::NoData);
    }

    #[test]
    fn state_ready_when_units_agree() {
        let items = [balance(Some(1.0), "积分"), balance(Some(2.0), "积分")];
        assert_eq!(state(true, items.iter()), BalanceSortState::Ready);
    }

    #[test]
    fn state_mixed_when_units_differ() {
        let items = [balance(Some(1.0), "积分"), balance(Some(2.0), "次")];
        assert_eq!(state(true, items.iter()), BalanceSortState::MixedUnits);
    }

    #[test]
    fn compare_high_puts_larger_first() {
        assert_eq!(
            compare(Some(1.0), Some(2.0), BalanceSortDirection::High),
            Ordering::Greater
        );
    }

    #[test]
    fn compare_low_puts_smaller_first() {
        assert_eq!(
            compare(Some(1.0), Some(2.0), BalanceSortDirection::Low),
            Ordering::Less
        );
    }

    #[test]
    fn compare_known_always_before_unknown() {
        assert_eq!(
            compare(Some(1.0), None, BalanceSortDirection::High),
            Ordering::Less
        );
        assert_eq!(
            compare(Some(1.0), None, BalanceSortDirection::Low),
            Ordering::Less
        );
        assert_eq!(
            compare(None, Some(1.0), BalanceSortDirection::Low),
            Ordering::Greater
        );
    }

    #[test]
    fn compare_both_unknown_is_equal() {
        assert_eq!(compare(None, None, BalanceSortDirection::High), Ordering::Equal);
    }

    #[test]
    fn from_usage_normalizes_credit_unit() {
        let usage = serde_json::json!({ "unit": "credits", "totalLeft": 1200 });
        let item = AccountSortBalance::from_usage(&usage);
        assert_eq!(item.unit, "积分");
    }

    #[test]
    fn from_usage_falls_back_to_account_quota_unit() {
        let usage = serde_json::json!({
            "quotaSources": [{ "id": "account", "unit": "次" }]
        });
        let item = AccountSortBalance::from_usage(&usage);
        assert_eq!(item.unit, "次");
    }
}
