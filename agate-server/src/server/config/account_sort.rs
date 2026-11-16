//! 账号排序规则配置（按提供商分别配置）。
//!
//! ── 三种排序维度 ────────────────────────────────────────────────
//! `Credit`  按额度到期时间（谁快到期先用谁）
//! `Balance` 按余额高低
//! `Added`   按账号加入时间
//!
//! 每家提供商最多三条规则，同维度不可重复；规则顺序即优先级。
//!
//! ── 与上游的差异（本文件是适配实现）────────────────────────────
//! 与 L1 的 title_routes / compact_routes 同一处理：上游把解析结果缓存进
//! `RuntimeConfig::account_sort`，Agate 本批不新增 typed 字段，
//! `account_sort_settings()` 每次从 `raw()` 解析。
//! 排序判定只在「候选账号排序」时发生，不是每请求热路径。

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// `raw` 里的顶层键名。与上游同名，便于两边配置对照。
pub const KEY_ACCOUNT_SORT: &str = "accountSort";

/// 每家提供商的规则列表。`BTreeMap` 保证序列化顺序稳定。
pub type AccountSortSettings = BTreeMap<String, Vec<AccountSortRule>>;

/// 单家最多配置多少条规则。
pub const MAX_RULES: usize = 3;

/// 最多给多少家提供商配置排序。
pub const MAX_PROVIDERS: usize = 256;

/// 额度到期排序方向：先用快到期的 / 先用晚到期的。
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CreditSortDirection {
    Soon,
    Late,
}

/// 余额排序方向：先用余额高的 / 先用余额低的。
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BalanceSortDirection {
    High,
    Low,
}

/// 加入时间排序方向：先用新加的 / 先用早加的。
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AddedSortDirection {
    New,
    Old,
}

/// 一条排序规则。`tag = "key"` 让 JSON 形如 `{"key":"credit","on":true,"dir":"soon"}`。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "key", rename_all = "camelCase", deny_unknown_fields)]
pub enum AccountSortRule {
    Credit { on: bool, dir: CreditSortDirection },
    Balance { on: bool, dir: BalanceSortDirection },
    Added { on: bool, dir: AddedSortDirection },
}

impl AccountSortRule {
    /// 规则的维度键。用于去重与日志。
    pub fn key(&self) -> &'static str {
        match self {
            Self::Credit { .. } => "credit",
            Self::Balance { .. } => "balance",
            Self::Added { .. } => "added",
        }
    }
}

/// 校验一家的规则集。返回给界面直接展示的中文原因。
pub fn validate_account_sort(provider: &str, rules: &[AccountSortRule]) -> Result<(), String> {
    if provider.is_empty()
        || provider.trim() != provider
        || provider.len() > 256
        || provider.chars().any(char::is_control)
    {
        return Err("提供商标识无效".to_string());
    }
    if rules.len() > MAX_RULES {
        return Err(format!("账号排序最多允许{MAX_RULES}条规则"));
    }
    let mut keys = HashSet::new();
    for rule in rules {
        let key = rule.key();
        if !keys.insert(key) {
            return Err(format!("账号排序包含重复规则「{key}」"));
        }
    }
    Ok(())
}

/// 从 `raw` 底稿解析。非法条目按「跳过」处理，不让一家坏配置拖垮整张表。
pub(crate) fn from_raw(raw: &Map<String, Value>) -> Arc<AccountSortSettings> {
    let settings = raw
        .get(KEY_ACCOUNT_SORT)
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .take(MAX_PROVIDERS)
                .filter_map(|(provider, raw)| {
                    let rules = serde_json::from_value::<Vec<AccountSortRule>>(raw.clone()).ok()?;
                    validate_account_sort(provider, &rules).ok()?;
                    Some((provider.clone(), rules))
                })
                .collect()
        })
        .unwrap_or_default();
    Arc::new(settings)
}

/// 当前排序配置。每次从 `raw()` 解析（刻意不缓存，见文件头说明）。
pub fn account_sort_settings() -> Arc<AccountSortSettings> {
    from_raw(super::current().raw())
}

/// 保存一家的规则。
///
/// - `rules = None` → 删除该家的配置；
/// - `only_if_unset = true` → 已有配置时不动（用于「首次默认」场景，
///   避免覆盖用户手动改过的规则）。
pub fn save_account_sort(
    provider: &str,
    rules: Option<Vec<AccountSortRule>>,
    only_if_unset: bool,
) -> Result<(), String> {
    validate_account_sort(provider, rules.as_deref().unwrap_or_default())?;
    let provider = provider.to_string();
    super::try_update_raw_field(
        KEY_ACCOUNT_SORT,
        |raw| {
            let mut next: AccountSortSettings = raw
                .as_object()
                .map(|object| {
                    object
                        .iter()
                        .filter_map(|(id, value)| {
                            serde_json::from_value::<Vec<AccountSortRule>>(value.clone())
                                .ok()
                                .map(|rules| (id.clone(), rules))
                        })
                        .collect()
                })
                .unwrap_or_default();
            if only_if_unset && next.contains_key(&provider) {
                return Ok(());
            }
            match &rules {
                Some(rules) => {
                    if next.len() >= MAX_PROVIDERS && !next.contains_key(&provider) {
                        return Err(format!("账号排序最多允许配置 {MAX_PROVIDERS} 家提供商"));
                    }
                    next.insert(provider.clone(), rules.clone());
                }
                None => {
                    next.remove(&provider);
                }
            }
            *raw = serde_json::to_value(&next)
                .map_err(|cause| format!("账号排序序列化失败: {cause}"))?;
            Ok(())
        },
        || "账号排序落盘失败，本次修改仅在当前进程生效".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credit() -> AccountSortRule {
        AccountSortRule::Credit {
            on: true,
            dir: CreditSortDirection::Soon,
        }
    }

    fn balance() -> AccountSortRule {
        AccountSortRule::Balance {
            on: true,
            dir: BalanceSortDirection::High,
        }
    }

    #[test]
    fn rule_keys_are_distinct() {
        assert_eq!(credit().key(), "credit");
        assert_eq!(balance().key(), "balance");
    }

    #[test]
    fn validate_rejects_empty_provider() {
        assert!(validate_account_sort("", &[]).is_err());
    }

    #[test]
    fn validate_rejects_padded_provider() {
        assert!(validate_account_sort(" zcode ", &[]).is_err());
    }

    #[test]
    fn validate_rejects_duplicate_rule() {
        let error = validate_account_sort("zcode", &[credit(), credit()])
            .err()
            .unwrap_or_default();
        assert!(error.contains("重复规则"));
    }

    #[test]
    fn validate_rejects_too_many_rules() {
        let rules = vec![
            credit(),
            balance(),
            AccountSortRule::Added {
                on: true,
                dir: AddedSortDirection::New,
            },
            credit(),
        ];
        assert!(validate_account_sort("zcode", &rules).is_err());
    }

    #[test]
    fn validate_accepts_three_distinct_rules() {
        let rules = vec![
            credit(),
            balance(),
            AccountSortRule::Added {
                on: true,
                dir: AddedSortDirection::New,
            },
        ];
        assert!(validate_account_sort("zcode", &rules).is_ok());
    }

    #[test]
    fn empty_raw_yields_empty_settings() {
        let settings = from_raw(&Map::new());
        assert!(settings.is_empty());
    }

    #[test]
    fn from_raw_skips_malformed_entry() {
        let mut raw = Map::new();
        raw.insert(
            KEY_ACCOUNT_SORT.to_string(),
            serde_json::json!({ "zcode": [{"key":"nope","on":true}] }),
        );
        let settings = from_raw(&raw);
        assert!(settings.is_empty());
    }
}
