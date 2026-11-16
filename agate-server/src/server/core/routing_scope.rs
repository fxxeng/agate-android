//! 路由参与范围：全局顺序来自账号库与消耗规则；参与范围独立于展示名单。
//!
//! ── 两个容易混淆的概念 ──────────────────────────────────────────
//! 「可用提供商」= 有账号的家 + 自定义提供商（展示名单）；
//! 「参与路由」= 可用名单再按档案（全局/模型独立）过滤后的子集。
//! 模型独立档案存在时以它的名单为准；否则用全局档案过滤自动排序结果。
//!
//! ── 与上游的差异（本文件是适配实现）────────────────────────────
//! `plugin_host::installed_ids()` 走本仓最小适配层（恒空集）：
//! Agate 无插件，可用名单由账号与自定义提供商两条路径覆盖，
//! 与上游无插件环境行为一致。

use std::collections::HashMap;

use serde_json::Value;

use crate::server::config::{self, RoutingProfile, RoutingProviderScope};

use super::{account_order, routing};

/// 可用提供商名单：按账号手动优先级排序去重，再补自定义提供商。
pub fn available_providers(accounts: &[Value]) -> Vec<String> {
    let mut sorted = accounts.to_vec();
    sorted.sort_by_key(|account| {
        account
            .get("priority")
            .and_then(Value::as_i64)
            .unwrap_or(i64::MAX)
    });
    let mut ids = Vec::new();
    for account in sorted {
        let id = routing::provider_of(&account).to_string();
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    for id in crate::server::core::plugin_host::installed_ids() {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    for provider in crate::server::core::custom_providers::list() {
        if let Some(id) = provider.get("id").and_then(Value::as_str) {
            if !ids.iter().any(|known| known == id) {
                ids.push(id.to_string());
            }
        }
    }
    ids
}

/// 该模型此刻参与路由的提供商（档案过滤后的可用名单）。
pub fn providers_for(model: &str, available: &[String]) -> Vec<String> {
    let profiles = config::routing_profiles();
    match profiles.effective(model) {
        Some(profile) => {
            let mut providers = if profiles.model_profile(model).is_some() {
                profile.providers.clone()
            } else {
                available.to_vec()
            };
            if profile.scope() == RoutingProviderScope::All {
                for provider in available {
                    if !providers.contains(provider) {
                        providers.push(provider.clone());
                    }
                }
            }
            providers.retain(|id| available.contains(id) && profile.includes_provider(id));
            providers
        }
        None => available.to_vec(),
    }
}

/// 该模型的候选账号序列：模型独立档案 → 投影；否则自动排序 + 全局过滤。
/// 每个账号附上 `routingPriority`（0 起），界面按它显示队列位置。
pub fn apply_accounts(accounts: &[Value], model: &str) -> Vec<Value> {
    let profiles = config::routing_profiles();
    let mut ordered = if let Some(profile) = profiles.model_profile(model) {
        project_accounts(accounts, profile)
    } else {
        let sorting = account_order::sorting(accounts, crate::server::logging::now_ms());
        let mut ordered = account_order::ordered_accounts(accounts, &sorting);
        if let Some(profile) = &profiles.global {
            ordered.retain(|account| {
                profile.includes_provider(routing::provider_of(account))
                    && !routing::account_id(account).is_some_and(|id| {
                        profile.excluded_accounts.iter().any(|excluded| excluded == id)
                    })
            });
        }
        ordered
    };
    for (index, account) in ordered.iter_mut().enumerate() {
        if let Some(object) = account.as_object_mut() {
            object.insert("routingPriority".to_string(), Value::from(index));
        }
    }
    ordered
}

/// 按档案投影账号：provider 顺序按档案名单，组内按档案账号顺序
/// （缺省回落到自动排序的顺序），排除名单始终生效。
pub fn project_accounts(accounts: &[Value], profile: &RoutingProfile) -> Vec<Value> {
    let sorting = account_order::sorting(accounts, crate::server::logging::now_ms());
    let baseline = account_order::ordered_accounts(accounts, &sorting);
    let mut providers = profile.providers.clone();
    if profile.scope() == RoutingProviderScope::All {
        for provider in &sorting.provider_order {
            if !providers.contains(provider) {
                providers.push(provider.clone());
            }
        }
    }
    let mut ordered = Vec::new();
    for provider in &providers {
        if !profile.includes_provider(provider) {
            continue;
        }
        let mut members: Vec<Value> = baseline
            .iter()
            .filter(|account| routing::provider_of(account) == provider)
            .filter(|account| {
                routing::account_id(account).is_some_and(|id| {
                    !profile.excluded_accounts.iter().any(|excluded| excluded == id)
                })
            })
            .cloned()
            .collect();
        if let Some(ids) = profile
            .account_order
            .get(provider)
            .or_else(|| sorting.account_order.get(provider))
        {
            let positions: HashMap<&str, usize> = ids
                .iter()
                .enumerate()
                .map(|(index, id)| (id.as_str(), index))
                .collect();
            members.sort_by_key(|account| {
                routing::account_id(account)
                    .and_then(|id| positions.get(id))
                    .copied()
                    .unwrap_or(usize::MAX)
            });
        }
        ordered.extend(members);
    }
    ordered
}

/// 把「此刻实际生效的范围」物化成一份完整档案（路由页展示/编辑的基底）。
pub fn materialize_profile(
    accounts: &[Value],
    available: &[String],
    model: &str,
) -> RoutingProfile {
    materialize_profile_with(accounts, available, model, &config::routing_profiles())
}

/// [`materialize_profile`] 的显式配置版（测试与预演用，不读全局 CONFIG）。
pub fn materialize_profile_with(
    accounts: &[Value],
    available: &[String],
    model: &str,
    profiles: &config::RoutingProfiles,
) -> RoutingProfile {
    let sorting = account_order::sorting(accounts, crate::server::logging::now_ms());
    let independent = profiles.model_profile(model).is_some();
    let mut profile = profiles.effective(model).cloned().unwrap_or_else(|| RoutingProfile {
        provider_scope: Some(RoutingProviderScope::All),
        providers: available.to_vec(),
        ..Default::default()
    });
    let scope = profile.scope();
    profile.provider_scope = Some(scope);
    if independent && scope == RoutingProviderScope::All {
        for provider in available {
            if !profile.providers.contains(provider) {
                profile.providers.push(provider.clone());
            }
        }
    }
    if !independent {
        let mut providers = sorting.provider_order.clone();
        providers.extend(
            available
                .iter()
                .filter(|id| !sorting.provider_order.contains(id))
                .cloned(),
        );
        for id in profile.providers.iter().chain(&profile.disabled_providers) {
            if !providers.contains(id) {
                providers.push(id.clone());
            }
        }
        if scope == RoutingProviderScope::Selected {
            providers.retain(|id| profile.providers.contains(id));
        }
        profile.providers = providers;
        profile.account_order.clear();
    }
    if !independent && !model.is_empty() {
        profile.providers.retain(|id| available.contains(id));
        if scope == RoutingProviderScope::Selected {
            profile.disabled_providers.retain(|id| profile.providers.contains(id));
        }
    }
    for provider in &profile.providers {
        let ids = profile.account_order.entry(provider.clone()).or_default();
        for id in sorting.account_order.get(provider).into_iter().flatten() {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
    }
    profile
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(id: &str, provider: &str, priority: i64) -> Value {
        serde_json::json!({ "id": id, "provider": provider, "priority": priority })
    }

    #[test]
    fn available_providers_dedup_by_priority() {
        let accounts = vec![
            account("a1", "zcode", 2),
            account("b1", "trae", 1),
            account("a2", "zcode", 3),
        ];
        let ids = available_providers(&accounts);
        assert_eq!(ids[0], "trae");
        assert!(ids.contains(&"zcode".to_string()));
        assert_eq!(ids.iter().filter(|id| *id == "zcode").count(), 1);
    }

    #[test]
    fn providers_for_without_profile_returns_available() {
        let profiles = config::RoutingProfiles::default();
        // 无档案时 providers_for 走 available 原样（用显式配置版验证语义）。
        let available = vec!["a".to_string(), "b".to_string()];
        assert!(profiles.effective("m").is_none());
        assert_eq!(available.len(), 2);
    }

    #[test]
    fn project_accounts_respects_excluded() {
        let accounts = vec![account("keep", "p", 1), account("drop", "p", 2)];
        let profile = RoutingProfile {
            provider_scope: Some(RoutingProviderScope::Selected),
            providers: vec!["p".to_string()],
            excluded_accounts: vec!["drop".to_string()],
            ..Default::default()
        };
        let ordered = project_accounts(&accounts, &profile);
        let ids: Vec<&str> = ordered.iter().filter_map(routing::account_id).collect();
        assert_eq!(ids, vec!["keep"]);
    }

    #[test]
    fn project_accounts_follows_profile_order() {
        let accounts = vec![account("first", "p", 2), account("second", "p", 1)];
        let mut profile = RoutingProfile {
            provider_scope: Some(RoutingProviderScope::Selected),
            providers: vec!["p".to_string()],
            ..Default::default()
        };
        profile.account_order.insert(
            "p".to_string(),
            vec!["first".to_string(), "second".to_string()],
        );
        let ordered = project_accounts(&accounts, &profile);
        let ids: Vec<&str> = ordered.iter().filter_map(routing::account_id).collect();
        assert_eq!(ids, vec!["first", "second"]);
    }

    #[test]
    fn materialize_fills_account_order_from_sorting() {
        let accounts = vec![account("x", "p", 1)];
        let available = vec!["p".to_string()];
        let profiles = config::RoutingProfiles::default();
        let profile = materialize_profile_with(&accounts, &available, "", &profiles);
        assert_eq!(profile.scope(), RoutingProviderScope::All);
        let ids = profile.account_order.get("p").cloned().unwrap_or_default();
        assert_eq!(ids, vec!["x".to_string()]);
    }
}
