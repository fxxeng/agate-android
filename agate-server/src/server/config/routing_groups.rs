//! 自定义对话路由组。组是独立入口，成员直指提供商的模型，不借用模型映射。
//!
//! ── 组与模型独立配置的关系 ──────────────────────────────────────
//! 组按「对外模型 ID」接管请求：客户端请求组的 model，网关按组的成员与
//! 策略选路。同一个对外 ID 不允许同时被真实模型、映射、其他组占用 ——
//! 占用判定在 save 里做（四路检查），避免「配置保存成功但请求永远路由不到」。
//!
//! ── 与 routing_profiles 的循环引用 ──────────────────────────────
//! profiles.from_raw 调本模块 from_value；本模块 save 调 profiles.from_raw。
//! 两文件必须同批提交（Rust 模块级循环引用合法，但分批会编不过）。
//!
//! ── 与上游的差异（本文件是适配实现）────────────────────────────
//! 1. 占用判定里 model_rules::shared().mappings 扫描改用 Agate 现成的
//!    model_rules::current().has_alias()（同为大小写不敏感，语义一致）。
//! 2. revision 的 to_vec().unwrap_or_default() 改 map().unwrap_or_default()
//!    （clippy unwrap 纪律；序列化 Option<struct> 实际不会失败，兜底不变）。

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{RoutingProfile, RoutingSettings};

pub const MAX_GROUPS: usize = 500;
pub const MAX_MEMBERS: usize = 256;

/// 组内一个成员：某家的某只模型 + 参与账号名单。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutingMember {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub included: bool,
    pub accounts: Vec<String>,
}

/// 一个路由组：对外模型 ID + 策略 + 成员列表。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutingGroup {
    pub id: String,
    pub name: String,
    pub model: String,
    #[serde(default)]
    pub description: String,
    pub strategy: String,
    pub sticky: String,
    pub members: Vec<RoutingMember>,
}

impl RoutingGroup {
    /// 把组的策略/粘滞叠到基础设置上。
    pub fn settings(&self, mut base: RoutingSettings) -> RoutingSettings {
        base.strategy = super::RouteStrategy::from_id(&self.strategy);
        base.sticky = super::StickyMode::from_id(&self.sticky);
        base
    }

    /// 组的参与范围投影成档案（included 成员的 provider 去重）。
    pub fn profile(&self) -> RoutingProfile {
        let mut providers = Vec::new();
        for member in self.members.iter().filter(|member| member.included) {
            if !providers.contains(&member.provider) {
                providers.push(member.provider.clone());
            }
        }
        RoutingProfile {
            provider_scope: Some(super::RoutingProviderScope::Selected),
            providers,
            strategy: Some(self.strategy.clone()),
            sticky: Some(self.sticky.clone()),
            ..Default::default()
        }
    }
}

/// 组的修订号（防覆盖保存用）。None 与「组不存在」同修订号。
pub fn revision(group: Option<&RoutingGroup>) -> String {
    let bytes = serde_json::to_vec(&group).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

/// 组形状校验。返回给界面直接展示的中文原因。
pub fn validate(group: &RoutingGroup) -> Result<(), String> {
    fn identifier(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || b"-_.".contains(&ch))
    }
    if !identifier(&group.id) || group.members.iter().any(|member| !identifier(&member.id)) {
        return Err("路由组或成员标识无效".into());
    }
    if group.name.trim().is_empty()
        || group.name.chars().count() > 64
        || group.name.chars().any(char::is_control)
        || group.description.chars().count() > 500
    {
        return Err("组名不能为空或超过 64 个字符，说明不能超过 500 个字符".into());
    }
    if group.model.is_empty() || super::normalize_routing_model(&group.model)? != group.model {
        return Err("对外模型 ID 不能为空，请去掉首尾空格并使用小写英文字母".into());
    }
    super::validate_routing_profile(&group.profile())?;
    if group.members.len() > MAX_MEMBERS {
        return Err(format!("一个路由组最多允许 {MAX_MEMBERS} 个成员"));
    }
    let mut ids = HashSet::new();
    let mut pairs = HashSet::new();
    let mut count = 0;
    for member in &group.members {
        if !ids.insert(&member.id)
            || !pairs.insert((&member.provider, member.model.to_ascii_lowercase()))
        {
            return Err("同一提供商的同一模型不能重复加入，成员标识也不能重复".into());
        }
        if member.provider.is_empty()
            || member.provider.len() > 256
            || member.provider.trim() != member.provider
            || member.provider.chars().any(char::is_control)
            || member.model.is_empty()
            || member.model.trim() != member.model
            || super::normalize_routing_model(&member.model).is_err()
        {
            return Err("成员的提供商或模型 ID 无效".into());
        }
        let unique: HashSet<_> = member.accounts.iter().collect();
        if unique.len() != member.accounts.len()
            || member
                .accounts
                .iter()
                .any(|id| id.is_empty() || id.len() > 256 || id.chars().any(char::is_control))
        {
            return Err("成员的参与账号为空、重复或格式不正确".into());
        }
        count += member.accounts.len();
    }
    if count > super::routing_profiles::MAX_ROUTING_ACCOUNTS {
        return Err("路由组的账号候选数量过多".into());
    }
    Ok(())
}

/// 从 JSON 解析组表。id 与键不一致或校验失败的条目跳过。
pub(crate) fn from_value(value: Option<&Value>) -> BTreeMap<String, RoutingGroup> {
    value
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .take(MAX_GROUPS)
        .filter_map(|(id, raw)| {
            let group: RoutingGroup = serde_json::from_value(raw.clone()).ok()?;
            (group.id == *id && validate(&group).is_ok()).then(|| (id.clone(), group))
        })
        .collect()
}

/// 保存/删除一个组。`expected` 是该组当前的修订号，对不上拒绝（草稿不丢）。
///
/// 新建/改模型 ID 时做四路占用检查：其他组、模型独立配置、真实模型目录、
/// 模型映射别名 —— 任何一路命中都说明这个对外 ID 已经有主。
pub fn save(id: &str, group: Option<RoutingGroup>, expected: &str) -> Result<(), String> {
    if let Some(group) = &group {
        validate(group)?;
        if group.id != id {
            return Err("路由组标识不一致".into());
        }
    }
    super::try_update_raw_field(
        super::KEY_ROUTING_PROFILES,
        |raw| {
            let mut raw_map = serde_json::Map::new();
            raw_map.insert(super::KEY_ROUTING_PROFILES.into(), raw.clone());
            let mut profiles = (*super::routing_profiles::from_raw(&raw_map)).clone();
            let old = profiles.groups.get(id);
            if revision(old) != expected {
                return Err("路由配置已在其他位置修改，请刷新后重试；草稿未丢失".to_string());
            }
            if let Some(group) = &group {
                if old.is_none() && profiles.groups.len() >= MAX_GROUPS {
                    return Err("自定义路由组数量已达上限".into());
                }
                let taken_by_group = profiles
                    .groups
                    .values()
                    .any(|known| known.id != id && known.model.eq_ignore_ascii_case(&group.model));
                let taken_by_profile = profiles.models.contains_key(&group.model);
                let taken_by_catalog =
                    !crate::server::core::providers::catalog::providers_for_model(&group.model)
                        .is_empty();
                let taken_by_mapping =
                    crate::server::core::model_rules::current().has_alias(&group.model);
                let taken_by_custom =
                    crate::server::core::custom_providers::list()
                        .iter()
                        .any(|provider| {
                            provider
                                .get("mappings")
                                .and_then(Value::as_array)
                                .is_some_and(|items| {
                                    items.iter().any(|item| {
                                        item.get("alias").and_then(Value::as_str).is_some_and(
                                            |alias| alias.eq_ignore_ascii_case(&group.model),
                                        )
                                    })
                                })
                        });
                if taken_by_group
                    || taken_by_profile
                    || taken_by_catalog
                    || taken_by_mapping
                    || taken_by_custom
                {
                    return Err("对外模型 ID 已被模型、映射或其他路由占用，请换一个名称".into());
                }
                for member in &group.members {
                    if profiles
                        .groups
                        .values()
                        .any(|known| known.model.eq_ignore_ascii_case(&member.model))
                        || group.model.eq_ignore_ascii_case(&member.model)
                    {
                        return Err("路由组不能包含自己或其他路由组".into());
                    }
                }
                profiles.groups.insert(id.to_string(), group.clone());
            } else {
                profiles.groups.remove(id);
            }
            *raw = json!(profiles);
            Ok(())
        },
        || "路由配置落盘失败，未应用本次修改".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, provider: &str, model: &str) -> RoutingMember {
        RoutingMember {
            id: id.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            included: true,
            accounts: Vec::new(),
        }
    }

    fn group(model: &str) -> RoutingGroup {
        RoutingGroup {
            id: "g1".to_string(),
            name: "组一".to_string(),
            model: model.to_string(),
            description: String::new(),
            strategy: "order".to_string(),
            sticky: "auto".to_string(),
            members: vec![member("m1", "zcode", "glm-4")],
        }
    }

    #[test]
    fn validate_accepts_well_formed_group() {
        assert!(validate(&group("my-group")).is_ok());
    }

    #[test]
    fn validate_rejects_uppercase_model() {
        assert!(validate(&group("My-Group")).is_err());
    }

    #[test]
    fn validate_rejects_duplicate_member_model() {
        let mut item = group("g");
        item.members.push(member("m2", "zcode", "GLM-4"));
        let error = validate(&item).err().unwrap_or_default();
        assert!(error.contains("不能重复"));
    }

    #[test]
    fn validate_rejects_bad_identifier() {
        let mut item = group("g");
        item.id = "bad id!".to_string();
        assert!(validate(&item).is_err());
    }

    #[test]
    fn profile_projects_included_members() {
        let mut item = group("g");
        item.members.push(RoutingMember {
            id: "m2".to_string(),
            provider: "trae".to_string(),
            model: "doubao".to_string(),
            included: false,
            accounts: Vec::new(),
        });
        let profile = item.profile();
        assert_eq!(profile.providers, vec!["zcode".to_string()]);
    }

    #[test]
    fn revision_none_is_stable() {
        assert_eq!(revision(None), revision(None));
    }

    #[test]
    fn revision_changes_with_content() {
        let a = revision(Some(&group("g")));
        let mut changed = group("g");
        changed.name = "改名".to_string();
        assert_ne!(a, revision(Some(&changed)));
    }

    #[test]
    fn from_value_skips_key_mismatch() {
        let value = serde_json::json!({ "other": group("g") });
        let groups = from_value(Some(&value));
        assert!(groups.is_empty());
    }
}
