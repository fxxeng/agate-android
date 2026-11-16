//! 手动调整账号顺序：把用户拖拽的结果写回档案前先做完整性校验。
//!
//! ── 为什么要校验「顺序完整」──────────────────────────────────────
//! 用户看到的是某一时刻的账号列表；提交时库里可能已经增删过账号。
//! 顺序表里缺了任何一家现有账号，说明这份顺序基于过期视图 ——
//! 直接落库会把新账号挤到队尾且用户不知情，所以拒绝并提示刷新。
//!
//! ── 自动排序互斥 ────────────────────────────────────────────────
//! 该家已启用自动消耗排序时不允许手动调序：两套顺序机制同时生效
//! 会让「为什么这个账号排前面」无法解释。先关自动规则再手动调。

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::server::config::RoutingProfile;

use super::{account_order, routing};

/// 按档案里的手动顺序重排全局队列，返回账号 id 序列。
///
/// `account_provider = Some` 只调一家的组内顺序；`None` 调提供商之间的顺序。
pub fn global_queue(
    accounts: &[Value],
    profile: &RoutingProfile,
    account_provider: Option<&str>,
) -> Result<Vec<String>, String> {
    let sorting = account_order::sorting(accounts, crate::server::logging::now_ms());
    if let Some(provider) = account_provider {
        let automatic = sorting
            .rules
            .get(provider)
            .into_iter()
            .flatten()
            .any(|rule| account_order::rule_enabled(provider, rule));
        if automatic {
            return Err(
                "该提供商已启用自动消耗排序，请先关闭排序规则再手动调整账号顺序".to_string(),
            );
        }
    }
    let mut groups = account_order::manual_groups(accounts);
    if let Some(provider) = account_provider {
        let Some((_, members)) = groups.iter_mut().find(|(id, _)| id == provider) else {
            return Err("该提供商没有可重排的账号".to_string());
        };
        let Some(ids) = profile.account_order.get(provider) else {
            return Err("缺少该提供商的账号顺序".to_string());
        };
        let known: HashSet<&str> = members.iter().filter_map(routing::account_id).collect();
        let positions: HashMap<&str, usize> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| (id.as_str(), index))
            .collect();
        if !known.iter().all(|id| positions.contains_key(id)) {
            return Err("账号顺序不完整，请刷新后重试".to_string());
        }
        members.sort_by_key(|account| {
            routing::account_id(account)
                .and_then(|id| positions.get(id))
                .copied()
                .unwrap_or(usize::MAX)
        });
    } else {
        let participating: HashSet<&str> =
            profile.providers.iter().map(String::as_str).collect();
        let mut replacements: Vec<_> = groups
            .iter()
            .filter(|(id, _)| participating.contains(id.as_str()))
            .cloned()
            .collect();
        replacements.sort_by_key(|(id, _)| {
            profile
                .providers
                .iter()
                .position(|provider| provider == id)
                .unwrap_or(usize::MAX)
        });
        let mut replacements = replacements.into_iter();
        for group in &mut groups {
            if participating.contains(group.0.as_str()) {
                if let Some(next) = replacements.next() {
                    *group = next;
                }
            }
        }
    }
    Ok(groups
        .into_iter()
        .flat_map(|(_, accounts)| {
            accounts
                .into_iter()
                .filter_map(|account| routing::account_id(&account).map(str::to_string))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::config::RoutingProviderScope;

    fn account(id: &str, provider: &str) -> Value {
        serde_json::json!({ "id": id, "provider": provider, "priority": 1 })
    }

    fn profile() -> RoutingProfile {
        RoutingProfile {
            provider_scope: Some(RoutingProviderScope::Selected),
            providers: vec!["p".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn missing_provider_group_is_rejected() {
        let accounts = vec![account("a", "other")];
        let error = global_queue(&accounts, &profile(), Some("p"))
            .err()
            .unwrap_or_default();
        assert!(error.contains("没有可重排的账号"));
    }

    #[test]
    fn missing_order_is_rejected() {
        let accounts = vec![account("a", "p")];
        let error = global_queue(&accounts, &profile(), Some("p"))
            .err()
            .unwrap_or_default();
        assert!(error.contains("缺少该提供商的账号顺序"));
    }

    #[test]
    fn incomplete_order_is_rejected() {
        let accounts = vec![account("a", "p"), account("b", "p")];
        let mut item = profile();
        item.account_order
            .insert("p".to_string(), vec!["a".to_string()]);
        let error = global_queue(&accounts, &item, Some("p"))
            .err()
            .unwrap_or_default();
        assert!(error.contains("顺序不完整"));
    }

    #[test]
    fn complete_order_reorders_group() {
        let accounts = vec![account("a", "p"), account("b", "p")];
        let mut item = profile();
        item.account_order
            .insert("p".to_string(), vec!["b".to_string(), "a".to_string()]);
        let ids = global_queue(&accounts, &item, Some("p")).unwrap_or_default();
        assert_eq!(ids, vec!["b".to_string(), "a".to_string()]);
    }

    #[test]
    fn provider_order_follows_profile_list() {
        let accounts = vec![account("a", "p"), account("b", "q")];
        let item = RoutingProfile {
            provider_scope: Some(RoutingProviderScope::Selected),
            providers: vec!["q".to_string(), "p".to_string()],
            ..Default::default()
        };
        let ids = global_queue(&accounts, &item, None).unwrap_or_default();
        assert_eq!(ids, vec!["b".to_string(), "a".to_string()]);
    }
}
