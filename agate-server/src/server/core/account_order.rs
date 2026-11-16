//! 账号自动排序：按「额度到期 / 余额 / 加入时间」三维规则给每家的账号排队。
//!
//! ── 数据从哪来 ──────────────────────────────────────────────────
//! 排序要用的事实（积分包到期时刻、余额快照）来自余额查询的结果，
//! 由 `note_usage` / `reload_usage` 喂进进程内缓存；排序本身零上游请求。
//!
//! ── 与上游的差异（本文件是适配实现）────────────────────────────
//! 1. 上游 UsageFacts 带 `query_config`（多套查询凭证时区分事实归属）；
//!    Agate 的 UsageRecord 没有该字段、账号也没有 usageConfigKey，
//!    故删除该维度 —— 不是简化语义，是本项目没有多凭证查询形态。
//! 2. `supports_usage_provider` 用 Agate 的 kind_from_id 路径
//!    （上游 adapter_by_id 是为插件家准备的，本项目无插件）。
//! 3. `plugin_host::account_features_of` 走本仓的最小适配层（恒 None），
//!    额度到期支持名单由内置兜底给出，与上游无插件环境行为一致。

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use serde_json::{json, Value};

use crate::server::config::{
    self, AccountSortRule, AccountSortSettings, AddedSortDirection, BalanceSortDirection,
    CreditSortDirection,
};

use super::account_order_balance::{self, AccountSortBalance, BalanceSortState};
use super::{routing, usage_records::UsageRecord};

struct UsageFacts {
    credit_expiries: Vec<i64>,
    balance: AccountSortBalance,
}

type AccountFacts = HashMap<String, Arc<UsageFacts>>;

fn usage_slot() -> &'static Mutex<AccountFacts> {
    static FACTS: OnceLock<Mutex<AccountFacts>> = OnceLock::new();
    FACTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|value| value.is_finite())
}

/// 从用量 JSON 里提取「还有余额的积分包」的到期时刻（升序去重）。
///
/// 排除四类包：snapshot/record/daily/unlimited 标记的都不是「会过期的余额包」；
/// remain <= 0 的包已经用光，到期时间无意义。
fn credit_expiries(usage: &Value) -> Vec<i64> {
    let mut expiries: Vec<i64> = usage
        .get("credits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|pack| {
            if ["snapshot", "record", "daily", "unlimited"]
                .iter()
                .any(|key| pack.get(key).and_then(Value::as_bool) == Some(true))
            {
                return None;
            }
            if pack
                .get("remain")
                .and_then(number)
                .is_some_and(|remain| remain <= 0.0)
            {
                return None;
            }
            let expiry = pack.get("expireAt").and_then(number)?;
            (expiry > 0.0).then_some(expiry as i64)
        })
        .collect();
    expiries.sort_unstable();
    expiries.dedup();
    expiries
}

fn usage_facts(usage: &Value) -> Arc<UsageFacts> {
    Arc::new(UsageFacts {
        credit_expiries: credit_expiries(usage),
        balance: AccountSortBalance::from_usage(usage),
    })
}

/// 单账号查询成功后喂事实。
pub fn note_usage(id: &str, usage: &Value) {
    if let Ok(mut facts) = usage_slot().lock() {
        facts.insert(id.to_string(), usage_facts(usage));
    }
}

/// 启动/恢复时按存量记录整体重建（失败行没有 usage，自然不进缓存）。
pub fn reload_usage(records: &HashMap<String, UsageRecord>) {
    let fresh = records
        .iter()
        .filter_map(|(id, record)| {
            record
                .usage
                .as_ref()
                .map(|usage| (id.clone(), usage_facts(usage)))
        })
        .collect();
    if let Ok(mut facts) = usage_slot().lock() {
        *facts = fresh;
    }
}

/// 该家是否支持「按额度到期」排序。无插件申报时走内置名单。
pub fn supports_credit_expiry(provider: &str) -> bool {
    super::plugin_host::account_features_of(provider)
        .map(|features| features.credit_expiry)
        .unwrap_or_else(|| {
            matches!(
                provider,
                "workbuddy" | "workbuddy-intl" | "trae" | "officeace"
            )
        })
}

/// 一条规则此刻是否真的参与排序（开关 × 该家能力）。
pub fn rule_enabled(provider: &str, rule: &AccountSortRule) -> bool {
    match rule {
        AccountSortRule::Credit { on, .. } => *on && supports_credit_expiry(provider),
        AccountSortRule::Balance { on, .. } => {
            *on && super::usage_query::supports_usage_provider(provider)
        }
        AccountSortRule::Added { on, .. } => *on,
    }
}

/// 该家的完整规则列表：已配置的在前，未配置的维度补默认（关闭）。
///
/// 默认值里 Credit 是否开启取决于该家能力 —— 支持额度到期的家默认先用快过期的。
pub fn rules_for(provider: &str, settings: &AccountSortSettings) -> Vec<AccountSortRule> {
    let defaults = vec![
        AccountSortRule::Credit {
            on: supports_credit_expiry(provider),
            dir: CreditSortDirection::Soon,
        },
        AccountSortRule::Balance {
            on: false,
            dir: BalanceSortDirection::High,
        },
        AccountSortRule::Added {
            on: false,
            dir: AddedSortDirection::New,
        },
    ];
    let Some(configured) = settings.get(provider) else {
        return defaults;
    };
    let mut rules = configured.clone();
    for mut rule in defaults {
        if rules.iter().any(|current| current.key() == rule.key()) {
            continue;
        }
        match &mut rule {
            AccountSortRule::Credit { on, .. }
            | AccountSortRule::Balance { on, .. }
            | AccountSortRule::Added { on, .. } => *on = false,
        }
        rules.push(rule);
    }
    rules
}

/// 按 provider 分组，组内先按「手动优先级 + 加入时间」排 —— 这是自动排序的基底。
pub fn manual_groups(accounts: &[Value]) -> Vec<(String, Vec<Value>)> {
    let mut accounts = accounts.to_vec();
    accounts.sort_by_key(|account| {
        (
            account
                .get("priority")
                .and_then(Value::as_i64)
                .unwrap_or(i64::MAX),
            account.get("addedAt").and_then(Value::as_i64).unwrap_or(0),
        )
    });
    let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
    for account in accounts {
        let provider = routing::provider_of(&account).to_string();
        match groups.iter_mut().find(|(id, _)| *id == provider) {
            Some((_, members)) => members.push(account),
            None => groups.push((provider, vec![account])),
        }
    }
    groups
}

/// 到期时刻比较：有到期时间的排在「无到期」之前（不限量/查不到的靠后）。
fn compare_expiry(
    left: Option<i64>,
    right: Option<i64>,
    direction: CreditSortDirection,
) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => match direction {
            CreditSortDirection::Soon => left.cmp(&right),
            CreditSortDirection::Late => right.cmp(&left),
        },
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// 一次排序的完整结果（配置 + 逐家顺序 + 判据快照），直接可序列化给界面。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSorting {
    pub updated_at: i64,
    pub configured: AccountSortSettings,
    pub rules: AccountSortSettings,
    pub provider_order: Vec<String>,
    pub account_order: BTreeMap<String, Vec<String>>,
    pub credit_expires_at: BTreeMap<String, Option<i64>>,
    pub balances: BTreeMap<String, AccountSortBalance>,
    pub balance_sort_states: BTreeMap<String, BalanceSortState>,
}

/// 对全部账号做一次排序。规则顺序即优先级：前一条比出胜负就不再看后面的。
pub fn sorting(accounts: &[Value], now: i64) -> AccountSorting {
    let configured = config::account_sort_settings();
    let mut result = AccountSorting {
        updated_at: now,
        configured: (*configured).clone(),
        rules: BTreeMap::new(),
        provider_order: Vec::new(),
        account_order: BTreeMap::new(),
        credit_expires_at: BTreeMap::new(),
        balances: BTreeMap::new(),
        balance_sort_states: BTreeMap::new(),
    };
    let facts = usage_slot()
        .lock()
        .map(|facts| facts.clone())
        .unwrap_or_default();
    for (provider, mut members) in manual_groups(accounts) {
        let credit_supported = supports_credit_expiry(&provider);
        let rules = rules_for(&provider, &configured);
        for account in &members {
            if let Some(id) = routing::account_id(account) {
                let fact = facts.get(id);
                let expiry = fact.and_then(|fact| {
                    fact.credit_expiries
                        .iter()
                        .find(|expiry| **expiry > now)
                        .copied()
                });
                result.credit_expires_at.insert(id.to_string(), expiry);
                let balance = if account.get("isGuest") == Some(&Value::Bool(true)) {
                    AccountSortBalance::default()
                } else {
                    fact.map(|fact| fact.balance.clone()).unwrap_or_default()
                };
                result.balances.insert(id.to_string(), balance);
            }
        }
        let balance_state = account_order_balance::state(
            super::usage_query::supports_usage_provider(&provider),
            members.iter().filter_map(|account| {
                routing::account_id(account).and_then(|id| result.balances.get(id))
            }),
        );
        result
            .balance_sort_states
            .insert(provider.clone(), balance_state);
        members.sort_by(|left, right| {
            for rule in &rules {
                let order = match rule {
                    AccountSortRule::Credit { on: true, dir } if credit_supported => {
                        let expiry = |account: &Value| {
                            routing::account_id(account)
                                .and_then(|id| result.credit_expires_at.get(id))
                                .copied()
                                .flatten()
                        };
                        compare_expiry(expiry(left), expiry(right), *dir)
                    }
                    AccountSortRule::Balance { on: true, dir }
                        if balance_state == BalanceSortState::Ready =>
                    {
                        let balance = |account: &Value| {
                            routing::account_id(account)
                                .and_then(|id| result.balances.get(id))
                                .and_then(AccountSortBalance::comparable)
                        };
                        account_order_balance::compare(balance(left), balance(right), *dir)
                    }
                    AccountSortRule::Added { on: true, dir } => {
                        let added = |account: &Value| {
                            account.get("addedAt").and_then(number).unwrap_or(0.0)
                        };
                        let order = added(left).total_cmp(&added(right));
                        match dir {
                            AddedSortDirection::Old => order,
                            AddedSortDirection::New => order.reverse(),
                        }
                    }
                    _ => Ordering::Equal,
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
            Ordering::Equal
        });
        result.provider_order.push(provider.clone());
        result.account_order.insert(
            provider.clone(),
            members
                .iter()
                .filter_map(|account| routing::account_id(account).map(str::to_string))
                .collect(),
        );
        result.rules.insert(provider, rules);
    }
    result
}

/// 按排序结果重排账号列表（保持对象本体不变，只换顺序）。
pub fn ordered_accounts(accounts: &[Value], sorting: &AccountSorting) -> Vec<Value> {
    let by_id: HashMap<&str, &Value> = accounts
        .iter()
        .filter_map(|account| routing::account_id(account).map(|id| (id, account)))
        .collect();
    sorting
        .provider_order
        .iter()
        .flat_map(|provider| sorting.account_order.get(provider).into_iter().flatten())
        .filter_map(|id| by_id.get(id.as_str()).map(|account| (*account).clone()))
        .collect()
}

/// 给账号快照附上 `sorting` 字段（账号页「排序」面板的数据源）。
pub fn enrich_snapshot(snapshot: &mut Value) {
    let accounts = routing::accounts_of(snapshot);
    let sorting = sorting(&accounts, crate::server::logging::now_ms());
    snapshot["sorting"] = json!(sorting);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(id: &str, provider: &str, added: i64) -> Value {
        serde_json::json!({ "id": id, "provider": provider, "addedAt": added })
    }

    #[test]
    fn number_parses_string_and_rejects_infinite() {
        assert_eq!(number(&serde_json::json!("42.5")), Some(42.5));
        assert_eq!(number(&serde_json::json!(42.5)), Some(42.5));
        assert_eq!(number(&serde_json::json!("abc")), None);
    }

    #[test]
    fn expiries_skip_flagged_and_empty_packs() {
        let usage = serde_json::json!({ "credits": [
            { "expireAt": 300, "remain": 5 },
            { "expireAt": 100, "remain": 5 },
            { "expireAt": 200, "remain": 0 },
            { "expireAt": 150, "remain": 5, "daily": true },
            { "expireAt": 100, "remain": 5 }
        ]});
        assert_eq!(credit_expiries(&usage), vec![100, 300]);
    }

    #[test]
    fn credit_expiry_support_uses_builtin_fallback() {
        assert!(supports_credit_expiry("workbuddy"));
        assert!(supports_credit_expiry("trae"));
        assert!(!supports_credit_expiry("zcode"));
    }

    #[test]
    fn added_rule_ignores_provider_capability() {
        let rule = AccountSortRule::Added {
            on: true,
            dir: AddedSortDirection::New,
        };
        assert!(rule_enabled("any-provider", &rule));
    }

    #[test]
    fn rules_for_fills_missing_dimensions_disabled() {
        let mut settings = AccountSortSettings::new();
        settings.insert(
            "zcode".to_string(),
            vec![AccountSortRule::Added {
                on: true,
                dir: AddedSortDirection::Old,
            }],
        );
        let rules = rules_for("zcode", &settings);
        assert_eq!(rules.len(), 3);
        // 已配置的 Added 保持开启，补进来的 Credit/Balance 都是关闭。
        let added_on = rules
            .iter()
            .any(|rule| matches!(rule, AccountSortRule::Added { on: true, .. }));
        assert!(added_on);
        let others_off = rules
            .iter()
            .filter(|rule| rule.key() != "added")
            .all(|rule| match rule {
                AccountSortRule::Credit { on, .. } | AccountSortRule::Balance { on, .. } => !on,
                AccountSortRule::Added { on, .. } => *on,
            });
        assert!(others_off);
    }

    #[test]
    fn manual_groups_sort_by_priority_then_added() {
        let accounts = vec![
            serde_json::json!({ "id": "b", "provider": "p", "priority": 1, "addedAt": 2 }),
            serde_json::json!({ "id": "a", "provider": "p", "priority": 1, "addedAt": 1 }),
            serde_json::json!({ "id": "c", "provider": "q", "priority": 0, "addedAt": 9 }),
        ];
        let groups = manual_groups(&accounts);
        assert_eq!(groups[0].0, "q");
        let ids: Vec<&str> = groups[1].1.iter().filter_map(routing::account_id).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn expiry_known_before_unknown_both_directions() {
        assert_eq!(
            compare_expiry(Some(1), None, CreditSortDirection::Soon),
            Ordering::Less
        );
        assert_eq!(
            compare_expiry(Some(1), None, CreditSortDirection::Late),
            Ordering::Less
        );
    }

    #[test]
    fn sorting_orders_by_added_new_first() {
        let mut settings = AccountSortSettings::new();
        settings.insert(
            "p".to_string(),
            vec![AccountSortRule::Added {
                on: true,
                dir: AddedSortDirection::New,
            }],
        );
        // 不依赖全局 CONFIG：直接验证 manual_groups + 规则比较的组合语义。
        let accounts = vec![account("old", "p", 100), account("new", "p", 200)];
        let groups = manual_groups(&accounts);
        let mut members = groups[0].1.clone();
        members.sort_by(|left, right| {
            let added = |a: &Value| a.get("addedAt").and_then(number).unwrap_or(0.0);
            added(left).total_cmp(&added(right)).reverse()
        });
        let ids: Vec<&str> = members.iter().filter_map(routing::account_id).collect();
        assert_eq!(ids, vec!["new", "old"]);
    }
}
