//! 路由组管理及无副作用的草稿预演。
//!
//! ── 预演为什么不发请求 ──────────────────────────────────────────
//! 预演走与转发同一套 `plan_route`，但 `cursor = None`（不推进轮转）、
//! `sticky_account = None`（不读粘滞）—— 看一百遍预演也不会改变
//! 下一次真实请求的选路结果。`fail_first` 只是把首选成员放进排除表
//! 再算一遍，模拟「首选失败后轮到谁」。
//!
//! ── 与上游的差异（本文件是重写实现）────────────────────────────
//! 1. 限流判定走 Agate 的 balance_blocked + token_skip_blocked
//!    （上游是 limiter::{sources,counter,evaluate} 三模块，本项目
//!    限流器是单体文件，判据等价）。
//! 2. 组保存后的粘滞清理走 forget_sticky_sessions（按模型过滤的
//!    clear_route_sticky 上游独有，这里全清 —— 组变更本就少见，
//!    全清的代价是其他会话重新选路一次，可接受）。
//! 3. 账号视图用 list_accounts（Agate 无 list_visible_accounts）。

use axum::body::Bytes;
use axum::extract::State;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::core::routing_groups::{self, RequestNeeds};
use crate::server::core::{routing, routing_scope};
use crate::server::http::{ok_json, parse_body};
use crate::server::{config, errors, ServerState};

/// 组列表 + 新建修订号 + 可选成员目录（编辑器一次拉全）。
pub fn groups_json(state: &ServerState) -> Value {
    let profiles = config::routing_profiles();
    json!({
        "groups": profiles.groups.values().map(|group| json!({
            "group": group,
            "revision": config::routing_group_revision(Some(group)),
        })).collect::<Vec<_>>(),
        "newRevision": config::routing_group_revision(None),
        "catalog": routing_groups::choices(state.store()),
    })
}

pub async fn get_groups(State(state): State<ServerState>) -> Response {
    match tokio::task::spawn_blocking(move || groups_json(&state)).await {
        Ok(value) => ok_json(value),
        Err(error) => errors::management_error(500, error.to_string()),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GroupSave {
    id: String,
    group: Option<config::RoutingGroup>,
    revision: String,
}

pub async fn put_group(State(state): State<ServerState>, body: Bytes) -> Response {
    let payload = match parse_body(&body).and_then(|value| {
        serde_json::from_value::<GroupSave>(value)
            .map_err(|error| errors::GatewayError::bad_request(error.to_string()))
    }) {
        Ok(value) => value,
        Err(error) => return errors::management_error(400, error.message),
    };
    match tokio::task::spawn_blocking(move || {
        let previous = config::routing_profiles();
        let old = previous.groups.get(&payload.id);
        if let Some(group) = &payload.group {
            config::routing_group_validate(group)?;
            let accounts = routing::accounts_of(&state.store().list_accounts());
            routing_groups::validate_members(state.store(), group, old, &accounts)?;
        }
        config::routing_group_save(&payload.id, payload.group.clone(), &payload.revision)?;
        if old.is_some() {
            // 组变更后旧粘滞可能指向已不存在的成员：全清一次，
            // 让会话重新选路（代价与正确性相比可接受，见模块头）。
            state.upstream().forget_sticky_sessions(&[]);
        }
        Ok::<_, String>(groups_json(&state))
    })
    .await
    {
        Ok(Ok(value)) => ok_json(value),
        Ok(Err(error)) => errors::management_error(
            if error.starts_with("路由配置已在其他位置修改") {
                409
            } else {
                400
            },
            error,
        ),
        Err(error) => errors::management_error(500, error.to_string()),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
struct Preview {
    model: String,
    group: Option<config::RoutingGroup>,
    profile: Option<config::RoutingProfile>,
    global: bool,
    needs: RequestNeeds,
    fail_first: bool,
}

impl Default for Preview {
    fn default() -> Self {
        Self {
            model: String::new(),
            group: None,
            profile: None,
            global: false,
            needs: RequestNeeds::default(),
            fail_first: false,
        }
    }
}

pub async fn preview(State(state): State<ServerState>, body: Bytes) -> Response {
    let payload = match parse_body(&body).and_then(|value| {
        serde_json::from_value::<Preview>(value)
            .map_err(|error| errors::GatewayError::bad_request(error.to_string()))
    }) {
        Ok(value) => value,
        Err(error) => return errors::management_error(400, error.message),
    };
    if let Some(group) = &payload.group {
        if let Err(error) = config::routing_group_validate(group) {
            return errors::management_error(400, error);
        }
    }
    if let Some(profile) = &payload.profile {
        if let Err(error) = config::validate_routing_profile(profile) {
            return errors::management_error(400, error);
        }
    }
    if config::normalize_routing_model(&payload.model).is_err() {
        return errors::management_error(400, "模型 ID 格式不正确");
    }
    if payload
        .group
        .as_ref()
        .is_some_and(|group| !group.model.eq_ignore_ascii_case(&payload.model))
    {
        return errors::management_error(400, "预演入口与路由组模型 ID 不一致");
    }
    match tokio::task::spawn_blocking(move || preview_json(&state, payload)).await {
        Ok(value) => ok_json(value),
        Err(error) => errors::management_error(500, error.to_string()),
    }
}

/// 组的队列视图（工作台/组详情复用预演结果）。
pub fn group_queue(state: &ServerState, group: config::RoutingGroup) -> Value {
    let payload = Preview {
        model: group.model.clone(),
        group: Some(group),
        ..Default::default()
    };
    preview_json(state, payload)["queue"].clone()
}

fn preview_json(state: &ServerState, payload: Preview) -> Value {
    let accounts = routing::accounts_of(&state.store().list_accounts());
    let (profiles, base) = config::routing_snapshot();
    let settings;
    let mut member_blocked = Vec::new();
    let group = payload
        .group
        .or_else(|| profiles.group_for_model(&payload.model).cloned());
    let mut candidates = if let Some(group) = &group {
        settings = group.settings(base);
        let projection =
            routing_groups::project(state.store(), group, &accounts, &payload.needs, None);
        member_blocked = projection.blocked;
        projection.accounts
    } else {
        let providers = if payload.model.is_empty() {
            routing_scope::available_providers(&accounts)
        } else {
            crate::server::core::providers::catalog::forwarding_providers(&payload.model)
        };
        let mut snapshot = (*profiles).clone();
        let global_draft = if payload.global && snapshot.model_profile(&payload.model).is_none() {
            payload.profile.clone()
        } else {
            None
        };
        if let Some(profile) = payload.profile {
            if payload.global {
                snapshot.global = Some(profile);
            } else {
                snapshot
                    .models
                    .insert(payload.model.to_ascii_lowercase(), profile);
            }
        }
        settings = snapshot.settings_for(&payload.model, base);
        let profile = if let Some(mut profile) = global_draft {
            let sorting = crate::server::core::account_order::sorting(
                &accounts,
                crate::server::logging::now_ms(),
            );
            for (provider, rules) in &sorting.rules {
                let automatic = rules
                    .iter()
                    .any(|rule| crate::server::core::account_order::rule_enabled(provider, rule));
                if automatic {
                    profile.account_order.remove(provider);
                }
            }
            profile
        } else {
            routing_scope::materialize_profile_with(
                &accounts,
                &providers,
                &payload.model,
                &snapshot,
            )
        };
        routing_scope::project_accounts(&accounts, &profile)
            .into_iter()
            .filter(|account| {
                providers
                    .iter()
                    .any(|id| id == routing::provider_of(account))
            })
            .collect()
    };
    for (index, account) in candidates.iter_mut().enumerate() {
        account["routingPriority"] = index.into();
    }
    let now = crate::server::logging::now_ms();
    let any_ready = candidates.iter().any(|account| {
        !crate::server::core::providers::readiness::gated(routing::provider_of(account))
    });
    candidates.retain(|account| {
        let cooling = account
            .get("upstreamCooldownUntil")
            .and_then(Value::as_i64)
            .is_some_and(|until| until > now);
        let gated = any_ready
            && crate::server::core::providers::readiness::gated(routing::provider_of(account));
        if cooling || gated {
            member_blocked.push(json!({
                "id": routing::account_id(account),
                "candidateId": routing_groups::attempt_key(account),
                "memberId": account.get("routingMemberId"),
                "model": account.get("routingModel"),
                "provider": routing::provider_of(account),
                "name": routing::account_name(account, routing::account_id(account).unwrap_or("")),
                "state": if cooling { "cooldown" } else { "unavailable" },
                "reason": if cooling { "账号处于上游冷却" } else { "提供商暂未就绪" },
            }));
            return false;
        }
        true
    });
    let keys = routing::CooldownKeys::new(&payload.model);
    let counts: std::collections::HashMap<String, usize> = state
        .upstream()
        .connections()
        .snapshot()
        .into_iter()
        .collect();
    let (sunk, resting, open) = state.upstream().route_queue_facts(now);
    let balance = crate::server::core::usage_records::balance_facts();
    let token_facts = crate::server::core::limiter::token_facts();
    let limiter: std::collections::HashSet<String> = candidates
        .iter()
        .filter(|account| {
            crate::server::core::usage_records::balance_blocked(account, &balance)
                || crate::server::core::limiter::token_skip_blocked(account, &token_facts, now)
        })
        .map(routing_groups::attempt_key)
        .collect();
    let meta = routing::PlanMeta {
        strategy: settings.strategy,
        sunk: &sunk,
        resting: &resting,
        open: &open,
        limiter_blocked: &limiter,
        cursor: None,
        provider_cursors: None,
        sticky_account: None,
    };
    let mut plan = routing::plan_route(&candidates, &keys, &counts, &[], now, &meta);
    if payload.fail_first {
        if let Some(first) = plan.ordered.first() {
            let member = first.account.get("routingMemberId").and_then(Value::as_str);
            let excluded: Vec<String> = candidates
                .iter()
                .filter(|account| {
                    member.map_or_else(
                        || routing::provider_of(account) == first.provider,
                        |member| {
                            account.get("routingMemberId").and_then(Value::as_str) == Some(member)
                        },
                    )
                })
                .map(routing_groups::attempt_key)
                .collect();
            plan = routing::plan_route(&candidates, &keys, &counts, &excluded, now, &meta);
        }
    }
    let row = |entry: &routing::PlanEntry| {
        json!({
            "id": entry.id,
            "candidateId": routing_groups::attempt_key(&entry.account),
            "memberId": entry.account.get("routingMemberId"),
            "model": keys.for_account(&entry.account),
            "name": entry.name,
            "provider": entry.provider,
            "providerLabel": crate::server::core::providers::label_of(&entry.provider),
            "state": entry.state,
            "reason": if payload.fail_first && entry.state == "tried" {
                "模拟首选成员失败，未实际发送"
            } else {
                &entry.reason
            },
            "resetAt": entry.reset_at,
        })
    };
    let mut blocked: Vec<Value> = plan.blocked.iter().map(row).collect();
    blocked.extend(member_blocked);
    json!({
        "model": payload.model,
        "customized": group.is_some() || profiles.model_profile(&payload.model).is_some(),
        "group": group,
        "preview": true,
        "settings": super::routing_api::routing_json(settings),
        "queue": {
            "strategy": settings.strategy.as_str(),
            "sticky": settings.sticky.as_str(),
            "pickedId": plan.ordered.first().map(|entry| &entry.id),
            "ordered": plan.ordered.iter().map(row).collect::<Vec<_>>(),
            "blocked": blocked,
        },
    })
}
