//! 路由工作区的整体保存：一次提交 = 档案 + 策略/粘滞 + 全局队列顺序。
//!
//! ── 为什么是「整体保存」而不是三个独立接口 ──────────────────────
//! 路由页上用户改的是同一个画面：参与名单、组内顺序、策略、粘滞。
//! 拆成三个接口会出现「名单存了、顺序没存」的半保存状态；整体保存
//! 用同一个修订号防覆盖，要么全成、要么全不成。
//!
//! ── 与上游的差异（本文件是重写实现）────────────────────────────
//! 1. 上游直接操 CONFIG_UPDATE_LOCK + store.with_conn_mut 写 kv 表；
//!    Agate 走两条现成公开路径：config::try_update_raw_field（配置）
//!    + AccountStore::reorder_accounts（队列，自带排列校验与事件广播）。
//! 2. 删 global_accounts 旧版转换闸门（不留旧数据兼容授权）。
//! 3. 删 cloud_sync 通知（本项目无云同步）。
//! 4. 可见账号用 store.list_accounts()（与 routing_api 同一入口），
//!    不复制上游的 store_view 过滤链。
//! 5. 删 catalog::invalidate()：Agate 的目录是实时计算视图，无缓存可失效。

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{RoutingProfile, RoutingSettings};
use crate::server::core::account_store::store::AccountStore;
use crate::server::core::{routing, routing_scope};

/// 工作区修订号：档案 + 策略 + 粘滞 + 是否模型独立，四者一起哈希。
pub fn revision(profile: &RoutingProfile, settings: RoutingSettings, customized: bool) -> String {
    let value = json!({
        "profile": profile,
        "strategy": settings.strategy.as_str(),
        "sticky": settings.sticky.as_str(),
        "customized": customized,
    });
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}

/// 整体保存路由工作区。
///
/// - `model` 空串 = 全局默认（同时把策略/粘滞写进路由设置、把账号顺序
///   固化成全局 priority）；非空 = 模型独立档案。
/// - `profile = None` = 删除该模型的独立档案（全局不可删，只能改）。
/// - `expected` 是客户端读到的修订号，对不上拒绝且一行不写。
pub fn save_workspace(
    store: &AccountStore,
    model: &str,
    profile: Option<RoutingProfile>,
    expected: &str,
) -> Result<(), String> {
    let visible = routing::accounts_of(&store.list_accounts());
    let available = if model.is_empty() {
        routing_scope::available_providers(&visible)
    } else {
        crate::server::core::providers::catalog::forwarding_providers(model)
    };
    let profiles = super::routing_profiles();
    if profiles.group_for_model(model).is_some() {
        return Err("此模型 ID 已被自定义路由组占用".into());
    }
    let effective =
        routing_scope::materialize_profile_with(&visible, &available, model, &profiles);
    let current_settings = profiles.settings_for(model, super::routing_settings());
    let customized = profiles.model_profile(model).is_some();
    if revision(&effective, current_settings, customized) != expected {
        return Err("路由配置已在其他位置修改，请刷新当前配置后重试；草稿未丢失".to_string());
    }
    if !model.is_empty()
        && profile.is_some()
        && !profiles.models.contains_key(model)
        && profiles.models.len() >= super::routing_profiles::MAX_ROUTING_MODELS
    {
        return Err("模型独立配置数量已达上限".to_string());
    }

    // 全局保存时先把新队列算出来（在写配置之前校验，失败则一行不写）。
    let queue: Option<Vec<String>> = if model.is_empty() {
        profile
            .as_ref()
            .map(|profile| build_global_queue(&visible, profile))
            .transpose()?
    } else {
        None
    };

    // 模型独立：直接走档案保存（它自带组占用与上限检查）。
    if !model.is_empty() {
        return super::save_routing_profile(model, profile);
    }

    // 全局：档案里的策略/粘滞提升到路由设置键，档案本体只留参与范围。
    let mut global = profile;
    let mut strategy_patch: Option<String> = None;
    let mut sticky_patch: Option<String> = None;
    if let Some(profile) = &mut global {
        strategy_patch = profile.strategy.take();
        sticky_patch = profile.sticky.take();
        profile.account_order.clear();
    }
    super::try_update_raw_field(
        super::KEY_ROUTING_PROFILES,
        |raw| {
            let mut raw_map = serde_json::Map::new();
            raw_map.insert(super::KEY_ROUTING_PROFILES.to_string(), raw.clone());
            let mut next = (*super::routing_profiles::from_raw(&raw_map)).clone();
            next.global = global;
            *raw = serde_json::to_value(&next)
                .map_err(|error| format!("路由配置序列化失败: {error}"))?;
            Ok(())
        },
        || "路由配置落盘失败，未应用本次修改".to_string(),
    )?;
    if let Some(strategy) = strategy_patch {
        super::update_raw_field(super::KEY_ROUTING_STRATEGY, Value::from(strategy));
    }
    if let Some(sticky) = sticky_patch {
        super::update_raw_field(super::KEY_ROUTING_STICKY, Value::from(sticky));
    }
    if let Some(ids) = queue {
        store
            .reorder_accounts(&ids)
            .map_err(|error| error.message)?;
    }
    // Agate 的 catalog 是实时计算视图（无缓存），配置落盘即生效，无需失效通知。
    Ok(())
}

/// 把档案里的「组顺位 + 组内顺位」编译成全局账号队列。
///
/// 不在档案名单里的账号保持原相对顺序排在参与账号之后 ——
/// 与上游一致：不参与路由 ≠ 从队列里消失。
fn build_global_queue(visible: &[Value], profile: &RoutingProfile) -> Result<Vec<String>, String> {
    let mut groups = crate::server::core::account_order::manual_groups(visible);
    for (provider, members) in &mut groups {
        if let Some(order) = profile.account_order.get(provider) {
            crate::server::core::routing_order::global_queue(visible, profile, Some(provider))?;
            let positions: std::collections::HashMap<&str, usize> = order
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
    }
    let positions: std::collections::HashMap<&str, usize> = profile
        .providers
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect();
    let mut selected: Vec<_> = groups
        .iter()
        .filter(|(id, _)| positions.contains_key(id.as_str()))
        .cloned()
        .collect();
    selected.sort_by_key(|(id, _)| positions.get(id.as_str()).copied().unwrap_or(usize::MAX));
    let mut replacement = selected.into_iter();
    for group in &mut groups {
        if positions.contains_key(group.0.as_str()) {
            if let Some(next) = replacement.next() {
                *group = next;
            }
        }
    }
    Ok(groups
        .into_iter()
        .flat_map(|(_, members)| {
            members
                .into_iter()
                .filter_map(|account| routing::account_id(&account).map(str::to_string))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::config::RoutingProviderScope;

    fn account(id: &str, provider: &str, priority: i64) -> Value {
        serde_json::json!({ "id": id, "provider": provider, "priority": priority })
    }

    #[test]
    fn revision_changes_with_strategy() {
        let profile = RoutingProfile::default();
        let mut a = RoutingSettings::default();
        let b = RoutingSettings::default();
        a.strategy = super::super::RouteStrategy::from_id("rotate");
        assert_ne!(revision(&profile, a, false), revision(&profile, b, false));
    }

    #[test]
    fn revision_changes_with_customized_flag() {
        let profile = RoutingProfile::default();
        let settings = RoutingSettings::default();
        assert_ne!(
            revision(&profile, settings, true),
            revision(&profile, settings, false)
        );
    }

    #[test]
    fn queue_puts_participating_first() {
        let visible = vec![
            account("out", "other", 0),
            account("in1", "p", 1),
            account("in2", "p", 2),
        ];
        let profile = RoutingProfile {
            provider_scope: Some(RoutingProviderScope::Selected),
            providers: vec!["p".to_string()],
            ..Default::default()
        };
        let ids = build_global_queue(&visible, &profile).unwrap_or_default();
        assert_eq!(ids[0], "in1");
        assert_eq!(ids[1], "in2");
        assert_eq!(ids[2], "out");
    }

    #[test]
    fn queue_respects_provider_order() {
        let visible = vec![account("a", "p", 0), account("b", "q", 1)];
        let profile = RoutingProfile {
            provider_scope: Some(RoutingProviderScope::Selected),
            providers: vec!["q".to_string(), "p".to_string()],
            ..Default::default()
        };
        let ids = build_global_queue(&visible, &profile).unwrap_or_default();
        assert_eq!(ids, vec!["b".to_string(), "a".to_string()]);
    }

    #[test]
    fn queue_keeps_all_accounts() {
        let visible = vec![account("a", "p", 0), account("b", "q", 1), account("c", "r", 2)];
        let profile = RoutingProfile {
            provider_scope: Some(RoutingProviderScope::Selected),
            providers: vec!["p".to_string()],
            ..Default::default()
        };
        let ids = build_global_queue(&visible, &profile).unwrap_or_default();
        assert_eq!(ids.len(), 3);
    }
}
