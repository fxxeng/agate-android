//! 按模型独立的路由参与范围与顺序；不改写账号库的全局优先级。
//!
//! ── 三层生效顺序 ────────────────────────────────────────────────
//! 全局默认（global）→ 模型独立（models）→ 自定义路由组（groups）。
//! 后者覆盖前者的 strategy/sticky；参与范围取「模型独立 ?? 全局」。
//!
//! ── 与上游的差异（本文件是适配实现）────────────────────────────
//! 1. 不新增 RuntimeConfig typed 字段，`routing_profiles()` 每次从 raw() 解析
//!    （与 L1/L2 同一模式；路由配置读取不在每请求热路径上）。
//! 2. 上游 save 的全局分支有一段「旧版全局参与设置须先转换」的兼容闸门
//!    （依赖 routing_workspace::global_accounts）；本项目不留旧数据兼容，
//!    删除该闸门 —— 全局 profile 直接可写。
//! 3. 与 routing_groups 互相引用（profiles.from_raw 调 groups.from_value，
//!    groups.save 调 profiles.from_raw），两文件必须同批提交。

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const KEY_ROUTING_PROFILES: &str = "routingProfiles";
pub const MAX_ROUTING_MODELS: usize = 500;
pub const MAX_ROUTING_PROVIDERS: usize = 256;
pub const MAX_ROUTING_ACCOUNTS: usize = 10_000;

/// 参与范围：全部提供商 / 仅名单内。
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RoutingProviderScope {
    All,
    Selected,
}

/// 一份路由档案：参与哪些家、排除哪些账号、策略与粘滞。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct RoutingProfile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_scope: Option<RoutingProviderScope>,
    pub providers: Vec<String>,
    pub disabled_providers: Vec<String>,
    pub account_order: BTreeMap<String, Vec<String>>,
    pub excluded_accounts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sticky: Option<String>,
}

impl RoutingProfile {
    /// 缺范围标记时按 Selected（名单）处理：无法区分「主动移除」与「后来新增」，
    /// 保留原有限制比放开更安全。
    pub fn scope(&self) -> RoutingProviderScope {
        self.provider_scope
            .unwrap_or(RoutingProviderScope::Selected)
    }

    /// 该家此刻是否参与：范围命中且不在停用名单。
    pub fn includes_provider(&self, provider: &str) -> bool {
        (self.scope() == RoutingProviderScope::All
            || self.providers.iter().any(|id| id == provider))
            && !self.disabled_providers.iter().any(|id| id == provider)
    }
}

/// 整块配置：全局默认 + 逐模型 + 路由组。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RoutingProfiles {
    pub global: Option<RoutingProfile>,
    pub models: BTreeMap<String, RoutingProfile>,
    pub groups: BTreeMap<String, super::RoutingGroup>,
}

impl RoutingProfiles {
    /// 该模型是否被某个路由组接管（组按对外模型 ID 匹配，大小写不敏感）。
    pub fn group_for_model(&self, model: &str) -> Option<&super::RoutingGroup> {
        self.groups
            .values()
            .find(|group| group.model.eq_ignore_ascii_case(model.trim()))
    }

    pub fn model_profile(&self, model: &str) -> Option<&RoutingProfile> {
        self.models.get(&model.trim().to_ascii_lowercase())
    }

    /// 生效档案：模型独立 ?? 全局。
    pub fn effective(&self, model: &str) -> Option<&RoutingProfile> {
        self.model_profile(model).or(self.global.as_ref())
    }

    /// 把三层配置叠到基础路由设置上（全局 → 模型 → 组，后者覆盖前者）。
    pub fn settings_for(
        &self,
        model: &str,
        mut settings: super::RoutingSettings,
    ) -> super::RoutingSettings {
        for profile in [self.global.as_ref(), self.model_profile(model)]
            .into_iter()
            .flatten()
        {
            if let Some(strategy) = &profile.strategy {
                settings.strategy = super::RouteStrategy::from_id(strategy);
            }
            if let Some(sticky) = &profile.sticky {
                settings.sticky = super::StickyMode::from_id(sticky);
            }
        }
        if let Some(group) = self.group_for_model(model) {
            return group.settings(settings);
        }
        settings
    }

    /// 生效范围标签：有模型独立配置时返回模型名，否则空串（= 全局）。
    pub fn scope<'a>(&self, model: &'a str) -> &'a str {
        if self.model_profile(model).is_some() {
            model
        } else {
            ""
        }
    }
}

/// 从 raw 底稿解析。非法条目跳过，不让一条坏配置拖垮整块。
pub(crate) fn from_raw(raw: &Map<String, Value>) -> Arc<RoutingProfiles> {
    let mut profiles = RoutingProfiles::default();
    let Some(object) = raw.get(KEY_ROUTING_PROFILES).and_then(Value::as_object) else {
        return Arc::new(profiles);
    };
    profiles.global = object
        .get("global")
        .and_then(|value| serde_json::from_value::<RoutingProfile>(value.clone()).ok())
        .filter(|profile| validate_shape(profile).is_ok());
    if let Some(models) = object.get("models").and_then(Value::as_object) {
        for (model, value) in models.iter().take(MAX_ROUTING_MODELS) {
            let Ok(model) = normalize_model(model) else {
                continue;
            };
            if model.is_empty() {
                continue;
            }
            if let Ok(profile) = serde_json::from_value::<RoutingProfile>(value.clone()) {
                if validate_shape(&profile).is_ok() {
                    profiles.models.insert(model, profile);
                }
            }
        }
    }
    profiles.groups = super::routing_groups::from_value(object.get("groups"));
    Arc::new(profiles)
}

/// 当前配置。每次从 raw() 解析（刻意不缓存，见文件头说明）。
pub fn routing_profiles() -> Arc<RoutingProfiles> {
    from_raw(super::current().raw())
}

/// 配置 + 基础路由设置的组合快照。
pub fn routing_snapshot() -> (Arc<RoutingProfiles>, super::RoutingSettings) {
    (routing_profiles(), super::routing_settings())
}

/// 该模型此刻生效的路由设置（三层叠加后）。
pub fn settings_for_model(model: &str) -> super::RoutingSettings {
    routing_profiles().settings_for(model, super::routing_settings())
}

/// 模型名归一化：去首尾空格 + 小写；超长/控制字符拒绝。
pub fn normalize_model(model: &str) -> Result<String, String> {
    let model = model.trim();
    if model.chars().count() > 256 || model.chars().any(char::is_control) {
        return Err("模型名不能超过 256 个字符或包含控制字符".to_string());
    }
    Ok(model.to_ascii_lowercase())
}

/// 档案形状校验。返回给界面直接展示的中文原因。
pub fn validate_shape(profile: &RoutingProfile) -> Result<(), String> {
    if profile
        .strategy
        .as_deref()
        .is_some_and(|value| !["order", "rotate", "provider"].contains(&value))
    {
        return Err("路由策略必须为 order、rotate 或 provider".to_string());
    }
    if profile
        .sticky
        .as_deref()
        .is_some_and(|value| !["auto", "session", "turn", "off"].contains(&value))
    {
        return Err("会话粘滞必须为 auto、session、turn 或 off".to_string());
    }
    fn unique(values: &[String], max: usize, label: &str) -> Result<(), String> {
        if values.len() > max {
            return Err(format!("{label}最多允许 {max} 项"));
        }
        let mut seen = HashSet::new();
        for value in values {
            if value.is_empty()
                || value.trim() != value
                || value.len() > 256
                || value.chars().any(char::is_control)
            {
                return Err(format!("{label}包含空白或非法标识"));
            }
            if !seen.insert(value) {
                return Err(format!("{label}包含重复项「{value}」"));
            }
        }
        Ok(())
    }
    unique(&profile.providers, MAX_ROUTING_PROVIDERS, "提供商列表")?;
    unique(
        &profile.disabled_providers,
        MAX_ROUTING_PROVIDERS,
        "停用提供商列表",
    )?;
    unique(
        &profile.excluded_accounts,
        MAX_ROUTING_ACCOUNTS,
        "排除账号列表",
    )?;
    if profile.scope() == RoutingProviderScope::Selected
        && profile
            .disabled_providers
            .iter()
            .any(|id| !profile.providers.contains(id))
    {
        return Err("停用提供商必须保留在当前路由列表中".to_string());
    }
    if profile.account_order.len() > MAX_ROUTING_PROVIDERS {
        return Err("账号顺序的提供商数量过多".to_string());
    }
    let mut all_ids = HashSet::new();
    for (provider, ids) in &profile.account_order {
        if !profile.providers.contains(provider) {
            return Err(format!("账号顺序中的提供商「{provider}」不在当前路由中"));
        }
        unique(ids, MAX_ROUTING_ACCOUNTS, "账号顺序")?;
        for id in ids {
            if !all_ids.insert(id) {
                return Err(format!("账号「{id}」出现在多个提供商中"));
            }
        }
    }
    if all_ids.len() > MAX_ROUTING_ACCOUNTS {
        return Err("路由账号数量过多".to_string());
    }
    Ok(())
}

/// 保存一份档案。`model` 空串 = 全局默认；`profile = None` = 删除。
pub fn save_routing_profile(model: &str, profile: Option<RoutingProfile>) -> Result<(), String> {
    let model = normalize_model(model)?;
    let profile = profile.map(|mut profile| {
        profile.provider_scope = Some(profile.scope());
        profile
    });
    if let Some(profile) = &profile {
        validate_shape(profile)?;
    }
    super::try_update_raw_field(
        KEY_ROUTING_PROFILES,
        |raw| {
            let mut raw_map = Map::new();
            raw_map.insert(KEY_ROUTING_PROFILES.to_string(), raw.clone());
            let mut next = (*from_raw(&raw_map)).clone();
            if next.group_for_model(&model).is_some() {
                return Err("此模型 ID 已被自定义路由组占用".to_string());
            }
            if model.is_empty() {
                next.global = profile;
            } else if let Some(profile) = profile {
                if next.models.len() >= MAX_ROUTING_MODELS && !next.models.contains_key(&model) {
                    return Err(format!("最多允许 {MAX_ROUTING_MODELS} 个模型独立配置"));
                }
                next.models.insert(model, profile);
            } else {
                next.models.remove(&model);
            }
            *raw = serde_json::to_value(&next)
                .map_err(|error| format!("路由配置序列化失败: {error}"))?;
            Ok(())
        },
        || "路由配置落盘失败，本次修改仅在当前进程生效".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(providers: &[&str]) -> RoutingProfile {
        RoutingProfile {
            provider_scope: Some(RoutingProviderScope::Selected),
            providers: providers.iter().map(|id| id.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn scope_defaults_to_selected() {
        let empty = RoutingProfile::default();
        assert_eq!(empty.scope(), RoutingProviderScope::Selected);
    }

    #[test]
    fn includes_provider_respects_disabled_list() {
        let mut item = profile(&["a", "b"]);
        item.disabled_providers.push("b".to_string());
        assert!(item.includes_provider("a"));
        assert!(!item.includes_provider("b"));
        assert!(!item.includes_provider("c"));
    }

    #[test]
    fn all_scope_includes_unknown_provider() {
        let item = RoutingProfile {
            provider_scope: Some(RoutingProviderScope::All),
            ..Default::default()
        };
        assert!(item.includes_provider("anything"));
    }

    #[test]
    fn normalize_model_lowercases_and_trims() {
        assert_eq!(normalize_model("  GPT-4O "), Ok("gpt-4o".to_string()));
        assert!(normalize_model(&"x".repeat(257)).is_err());
    }

    #[test]
    fn validate_rejects_bad_strategy() {
        let mut item = profile(&["a"]);
        item.strategy = Some("nope".to_string());
        assert!(validate_shape(&item).is_err());
    }

    #[test]
    fn validate_rejects_disabled_outside_list() {
        let mut item = profile(&["a"]);
        item.disabled_providers.push("b".to_string());
        let error = validate_shape(&item).err().unwrap_or_default();
        assert!(error.contains("停用提供商"));
    }

    #[test]
    fn validate_rejects_account_in_two_providers() {
        let mut item = profile(&["a", "b"]);
        item.account_order
            .insert("a".to_string(), vec!["x".to_string()]);
        item.account_order
            .insert("b".to_string(), vec!["x".to_string()]);
        let error = validate_shape(&item).err().unwrap_or_default();
        assert!(error.contains("多个提供商"));
    }

    #[test]
    fn effective_falls_back_to_global() {
        let mut profiles = RoutingProfiles::default();
        profiles.global = Some(profile(&["g"]));
        assert!(profiles.effective("unknown-model").is_some());
        profiles.models.insert("m".to_string(), profile(&["m"]));
        let hit = profiles.effective("M");
        let providers = hit.map(|p| p.providers.clone()).unwrap_or_default();
        assert_eq!(providers, vec!["m".to_string()]);
    }

    #[test]
    fn from_raw_skips_invalid_model_entry() {
        let mut raw = Map::new();
        raw.insert(
            KEY_ROUTING_PROFILES.to_string(),
            serde_json::json!({ "models": { "m": { "strategy": "nope" } } }),
        );
        let profiles = from_raw(&raw);
        assert!(profiles.models.is_empty());
    }
}
