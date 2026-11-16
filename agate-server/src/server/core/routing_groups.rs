//! 路由组的运行时投影：把「成员 × 参与账号」展开成候选队列。
//!
//! ── 运行时字段只存在于候选副本 ──────────────────────────────────
//! `routingCandidateId` / `routingMemberId` / `routingModel` 只写进
//! 投影出来的账号副本，不落账号库 —— 同一个账号可以在多个组里
//! 以不同模型参与，库里只有一份本体。
//!
//! ── 与上游的差异（本文件是重写实现）────────────────────────────
//! 1. 模型清单走 Agate 的 `catalog::active_manifests`（内置家）+
//!    `custom_providers::get`（自定义家），不复制上游的
//!    manifest_for_provider / executable_provider_ids。
//! 2. 生图判定用模型条目的 `kind == "image"`（Agate 无 images 模块，
//!    且图像功能优先级最低，不引入）。
//! 3. 删 vision_compat 分支（视觉兼容层未建）。
//! 4. 账号可见性：Agate 的 list_accounts 已是用户可见视图，
//!    不再复制上游 store_view::account_visible 过滤链。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::server::config::{self, RoutingGroup};
use crate::server::core::account_store::store::AccountStore;
use crate::server::core::key_scope::{self, KeyScope};
use crate::server::core::models::model_id;
use crate::server::core::{capability, custom_providers, providers, routing};

/// 一次请求携带的能力需求（从请求体推断）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct RequestNeeds {
    pub tools: bool,
    pub images: bool,
    pub video: bool,
}

impl RequestNeeds {
    /// 从 OpenAI 形态的请求体推断需求：tools/functions 数组、
    /// tool/function 角色消息、多模态 content 分片。
    pub fn from_body(body: &Value) -> Self {
        let mut needs = Self {
            tools: ["tools", "functions"].iter().any(|key| {
                body.get(key)
                    .and_then(Value::as_array)
                    .is_some_and(|tools| !tools.is_empty())
            }),
            ..Default::default()
        };
        for message in body
            .get("messages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            needs.tools |= matches!(
                message.get("role").and_then(Value::as_str),
                Some("tool" | "function")
            ) || message.get("function_call").is_some()
                || message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .is_some_and(|calls| !calls.is_empty());
            for part in message
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                match part.get("type").and_then(Value::as_str) {
                    Some("image_url" | "input_image" | "image") => needs.images = true,
                    Some("video_url" | "input_video" | "video") => needs.video = true,
                    _ => {}
                }
            }
        }
        needs
    }

    /// 该模型条目是否满足需求；不满足时给出跳过原因。
    pub fn reason(&self, item: &Value) -> Option<String> {
        for (required, key, label) in [
            (self.tools, "supportsToolCall", "工具调用"),
            (self.images, "supportsImages", "图片输入"),
            (self.video, "supportsVideo", "视频输入"),
        ] {
            if required && item.get(key).and_then(Value::as_bool) != Some(true) {
                return Some(if item.get(key).and_then(Value::as_bool) == Some(false) {
                    format!("该模型不支持{label}，已跳过")
                } else {
                    format!("该模型的{label}能力未声明，请先在模型管理中确认")
                });
            }
        }
        None
    }
}

/// 一家的模型清单：自定义家读记录上的 models（补元数据与能力覆盖），
/// 内置家走目录的活跃清单。
fn manifest(store: &AccountStore, provider: &str) -> Vec<Value> {
    if let Some(owner) = custom_providers::get(provider) {
        return owner
            .get("models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|mut item| {
                crate::server::core::model_meta::apply(&mut item);
                let overrides = capability::normalize_object(item.get("capabilities"));
                capability::apply_overrides(&mut item, &overrides);
                item
            })
            .collect();
    }
    let Some(kind) = providers::kind_from_id(provider) else {
        return Vec::new();
    };
    providers::catalog::active_manifests(store)
        .into_iter()
        .find(|(active, _)| *active == kind)
        .map(|(_, items)| items)
        .unwrap_or_default()
}

/// 生图模型判定（Agate 的模型条目带 kind 字段）。
fn model_is_image(item: &Value) -> bool {
    item.get("kind").and_then(Value::as_str) == Some("image")
}

/// 按 id 找一家的某只模型（大小写不敏感）。
pub fn catalog_model(store: &AccountStore, provider: &str, model: &str) -> Option<Value> {
    manifest(store, provider)
        .into_iter()
        .find(|item| model_id(item).eq_ignore_ascii_case(model))
}

/// 路由组编辑器的「可选成员」清单：全部可见家的非生图模型。
pub fn choices(store: &AccountStore) -> Vec<Value> {
    let accounts = routing::accounts_of(&store.list_accounts());
    let mut ids: Vec<String> = accounts
        .iter()
        .map(|account| routing::provider_of(account).to_string())
        .collect();
    ids.extend(
        custom_providers::list()
            .iter()
            .filter_map(|provider| {
                provider
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
    );
    ids.sort();
    ids.dedup();
    let mut result = Vec::new();
    for provider in ids {
        let models = manifest(store, &provider);
        let mut seen = HashSet::new();
        for item in models {
            let model = model_id(&item);
            if model.is_empty() || !seen.insert(model.to_ascii_lowercase()) {
                continue;
            }
            if model_is_image(&item) {
                continue;
            }
            result.push(json!({
                "provider": provider,
                "providerLabel": providers::label_of(&provider),
                "model": model,
                "name": item.get("name").and_then(Value::as_str).unwrap_or(&model),
                "capabilities": capability::effective(&item),
                "accountCount": accounts.iter()
                    .filter(|account| routing::provider_of(account) == provider).count(),
            }));
        }
    }
    result
}

/// 候选键：成员 id + 账号 id（\0 分隔，两者都不含该字符）。
pub fn candidate_key(member: &str, account: &str) -> String {
    format!("{member}\0{account}")
}

pub fn account_key(key: &str) -> &str {
    key.split_once('\0').map(|(_, account)| account).unwrap_or(key)
}

pub fn member_key(key: &str) -> Option<&str> {
    key.split_once('\0').map(|(member, _)| member)
}

pub fn attempt_key(account: &Value) -> String {
    account
        .get("routingCandidateId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| routing::account_id(account).unwrap_or(""))
        .to_string()
}

pub fn candidate_model(account: &Value) -> Option<&str> {
    account.get("routingModel").and_then(Value::as_str)
}

pub fn family_key(account: &Value) -> &str {
    account
        .get("routingMemberId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| routing::provider_of(account))
}

/// 一次投影的结果：候选队列 + 被跳过的成员/账号（各带原因）。
#[derive(Clone, Default)]
pub struct GroupProjection {
    pub accounts: Vec<Value>,
    pub blocked: Vec<Value>,
}

/// 该家此刻是否可参与路由：内置家看适配器存在，自定义家看启用位。
fn provider_active(provider: &str) -> bool {
    match custom_providers::get(provider) {
        Some(owner) => owner.get("enabled") != Some(&Value::Bool(false)),
        None => providers::kind_from_id(provider).is_some(),
    }
}

/// 把组展开成候选队列。`scope` 是网关 Key 的白名单（None = 不限制）。
pub fn project(
    store: &AccountStore,
    group: &RoutingGroup,
    accounts: &[Value],
    needs: &RequestNeeds,
    scope: Option<&KeyScope>,
) -> GroupProjection {
    let mut result = GroupProjection::default();
    let by_id: HashMap<_, _> = accounts
        .iter()
        .filter_map(|account| routing::account_id(account).map(|id| (id, account)))
        .collect();
    let manifests: HashMap<String, Vec<Value>> = group
        .members
        .iter()
        .map(|member| member.provider.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|id| {
            let items = manifest(store, &id);
            (id, items)
        })
        .collect();
    for member in group.members.iter().filter(|member| member.included) {
        let item = manifests.get(&member.provider).and_then(|items| {
            items
                .iter()
                .find(|item| model_id(item).eq_ignore_ascii_case(&member.model))
                .cloned()
        });
        let reason = if !key_scope::allows_provider(scope, &member.provider) {
            Some("网关 Key 不允许此提供商".to_string())
        } else if !provider_active(&member.provider) {
            Some("提供商当前不可用".to_string())
        } else if let Some(item) = &item {
            if model_is_image(item) {
                Some("生图模型不能用于对话路由".to_string())
            } else {
                needs.reason(item)
            }
        } else {
            Some("模型已不在提供商目录中，配置保留但不参与转发".to_string())
        };
        let actual = item
            .as_ref()
            .map(model_id)
            .unwrap_or_else(|| member.model.clone());
        if let Some(reason) = reason {
            result.blocked.push(json!({
                "id": member.id, "memberId": member.id, "provider": member.provider,
                "providerLabel": providers::label_of(&member.provider),
                "model": actual, "name": actual, "state": "unavailable", "reason": reason,
            }));
            continue;
        }
        if member.accounts.is_empty() {
            result.blocked.push(json!({
                "id": member.id, "memberId": member.id, "provider": member.provider,
                "model": actual, "name": actual, "state": "excluded",
                "reason": "该成员没有参与账号",
            }));
        }
        for id in &member.accounts {
            let Some(account) = by_id
                .get(id.as_str())
                .filter(|account| routing::provider_of(account) == member.provider)
            else {
                result.blocked.push(json!({
                    "id": id, "memberId": member.id, "provider": member.provider,
                    "model": actual, "name": id, "state": "unavailable",
                    "reason": "参与账号已移除或不属于此提供商",
                }));
                continue;
            };
            let mut account = (*account).clone();
            account["routingCandidateId"] = candidate_key(&member.id, id).into();
            account["routingMemberId"] = member.id.clone().into();
            account["routingModel"] = actual.clone().into();
            account["routingPriority"] = result.accounts.len().into();
            result.accounts.push(account);
        }
    }
    result
}

/// 保存前校验成员：模型必须还在目录里（旧成员允许保留失效模型），
/// 参与账号必须存在且属于所选提供商（旧账号允许保留已移除的）。
pub fn validate_members(
    store: &AccountStore,
    group: &RoutingGroup,
    old: Option<&RoutingGroup>,
    accounts: &[Value],
) -> Result<(), String> {
    for member in &group.members {
        let previous = old.and_then(|group| {
            group.members.iter().find(|item| {
                item.id == member.id
                    && item.provider == member.provider
                    && item.model == member.model
            })
        });
        match catalog_model(store, &member.provider, &member.model) {
            Some(item) if model_is_image(&item) => {
                return Err("对话路由组不能加入生图模型".into())
            }
            None if previous.is_none() => {
                return Err(format!("提供商中找不到模型 {}，请刷新后重试", member.model))
            }
            _ => {}
        }
        for id in &member.accounts {
            if let Some(account) = accounts
                .iter()
                .find(|account| routing::account_id(account) == Some(id))
            {
                if routing::provider_of(account) != member.provider {
                    return Err("参与账号不属于所选提供商".into());
                }
            } else if !previous.is_some_and(|item| item.accounts.contains(id)) {
                return Err("参与账号不存在，请刷新后重试".into());
            }
        }
    }
    Ok(())
}

/// 把组作为虚拟模型条目补进对外模型列表（/v1/models 的组入口）。
///
/// 能力位取「全部可用成员都支持才支持」的交集口径；组没有任何
/// 可用成员时不出现（避免客户端选到一个永远失败的模型）。
pub fn append_public(
    store: &AccountStore,
    scope: Option<&KeyScope>,
    data: &mut Vec<Value>,
) {
    let accounts = routing::accounts_of(&store.list_accounts());
    for group in config::routing_profiles().groups.values() {
        if !key_scope::allows_model(scope, &group.model)
            || data
                .iter()
                .any(|item| model_id(item).eq_ignore_ascii_case(&group.model))
        {
            continue;
        }
        let members: Vec<Value> = group
            .members
            .iter()
            .filter(|member| {
                member.included
                    && key_scope::allows_provider(scope, &member.provider)
                    && provider_active(&member.provider)
                    && member.accounts.iter().any(|id| {
                        accounts.iter().any(|account| {
                            routing::account_id(account) == Some(id)
                                && routing::provider_of(account) == member.provider
                                && account.get("enabled") != Some(&Value::Bool(false))
                        })
                    })
            })
            .filter_map(|member| catalog_model(store, &member.provider, &member.model))
            .filter(|item| !model_is_image(item))
            .collect();
        if members.is_empty() {
            continue;
        }
        let mut item = json!({
            "id": group.model, "name": group.name, "object": "model",
            "owned_by": "routing-group", "kind": "chat", "routing_group": true,
        });
        for (key, public) in [
            ("supportsToolCall", "supports_tool_call"),
            ("supportsImages", "supports_images"),
            ("supportsVideo", "supports_video"),
            ("supportsReasoning", "supports_reasoning"),
        ] {
            item[public] = members
                .iter()
                .all(|item| item.get(key).and_then(Value::as_bool) == Some(true))
                .into();
        }
        data.push(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_from_body_detects_tools_array() {
        let body = serde_json::json!({ "tools": [{ "type": "function" }] });
        assert!(RequestNeeds::from_body(&body).tools);
    }

    #[test]
    fn needs_from_body_detects_tool_role_message() {
        let body = serde_json::json!({ "messages": [{ "role": "tool", "content": "x" }] });
        assert!(RequestNeeds::from_body(&body).tools);
    }

    #[test]
    fn needs_from_body_detects_image_part() {
        let body = serde_json::json!({ "messages": [{ "role": "user", "content": [
            { "type": "image_url", "image_url": { "url": "x" } }
        ]}]});
        let needs = RequestNeeds::from_body(&body);
        assert!(needs.images);
        assert!(!needs.tools);
    }

    #[test]
    fn reason_flags_unsupported_capability() {
        let needs = RequestNeeds { tools: true, ..Default::default() };
        let item = serde_json::json!({ "supportsToolCall": false });
        let reason = needs.reason(&item).unwrap_or_default();
        assert!(reason.contains("不支持工具调用"));
    }

    #[test]
    fn reason_flags_undeclared_capability() {
        let needs = RequestNeeds { images: true, ..Default::default() };
        let item = serde_json::json!({});
        let reason = needs.reason(&item).unwrap_or_default();
        assert!(reason.contains("未声明"));
    }

    #[test]
    fn reason_passes_when_supported() {
        let needs = RequestNeeds { tools: true, ..Default::default() };
        let item = serde_json::json!({ "supportsToolCall": true });
        assert!(needs.reason(&item).is_none());
    }

    #[test]
    fn candidate_key_roundtrip() {
        let key = candidate_key("m1", "acc-9");
        assert_eq!(account_key(&key), "acc-9");
        assert_eq!(member_key(&key), Some("m1"));
    }

    #[test]
    fn plain_key_without_separator() {
        assert_eq!(account_key("plain"), "plain");
        assert_eq!(member_key("plain"), None);
    }

    #[test]
    fn image_model_detected_by_kind() {
        assert!(model_is_image(&serde_json::json!({ "kind": "image" })));
        assert!(!model_is_image(&serde_json::json!({ "kind": "chat" })));
        assert!(!model_is_image(&serde_json::json!({})));
    }

    #[test]
    fn attempt_key_prefers_candidate_id() {
        let account = serde_json::json!({ "id": "a", "routingCandidateId": "m1\0a" });
        assert_eq!(attempt_key(&account), "m1\0a");
        let plain = serde_json::json!({ "id": "a" });
        assert_eq!(attempt_key(&plain), "a");
    }
}
