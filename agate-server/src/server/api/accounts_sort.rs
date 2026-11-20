//! 账号排序：`GET/PUT /api/accounts/sort`。
//!
//! ── 这一层只补入口，能力早已就绪 ─────────────────────────────
//! 本地 `config::account_sort` 的校验与落盘（`validate_account_sort` /
//! `save_account_sort`）、`core::account_order::sorting` 的排序计算都已实现并被
//! 路由层调用，唯独没有管理端点 —— 客户端拖拽排序只能存在本地，重启即丢，
//! 也看不到「已保存的排序」。本文件把这份能力接到管理面。
//!
//! 相对上游的差异：`list_visible_accounts` → `list_accounts`（Agate 无前者，
//! 见 `routing_groups.rs` 头部说明第 3 条）。

use axum::body::Bytes;
use axum::extract::State;
use axum::response::Response;
use serde_json::Value;

use crate::server::core::{account_order, providers, routing};
use crate::server::http::{ok_json, parse_body};
use crate::server::{config, errors, logging, ServerState};

/// GET /api/accounts/sort —— 当前各家的排序规则投影。
pub async fn get_sorting(State(state): State<ServerState>) -> Response {
    let accounts = routing::accounts_of(&state.store().list_accounts());
    ok_json(serde_json::json!(account_order::sorting(
        &accounts,
        logging::now_ms()
    )))
}

/// PUT /api/accounts/sort —— body `{provider, rules: [...]|null, onlyIfUnset?: bool}`
///
/// `rules: null` 恢复默认；`onlyIfUnset: true` 只在从未配置过时写入（首次默认场景，
/// 避免覆盖用户手动改过的规则）。
pub async fn save_sorting(State(state): State<ServerState>, body: Bytes) -> Response {
    let payload = match parse_body(&body) {
        Ok(value) => value,
        Err(error) => return errors::management_error(400, error.message),
    };
    let Some(provider) = payload.get("provider").and_then(Value::as_str) else {
        return errors::management_error(400, "缺少 provider 字段");
    };
    if !providers::is_known_provider_id(provider)
        && !crate::server::core::custom_providers::is_custom_provider_id(provider)
    {
        return errors::management_error(400, format!("未知提供商「{provider}」"));
    }
    let rules = match payload.get("rules") {
        Some(Value::Null) => None,
        Some(value) => {
            match serde_json::from_value::<Vec<config::AccountSortRule>>(value.clone()) {
                Ok(rules) => Some(rules),
                Err(error) => {
                    return errors::management_error(400, format!("账号排序规则格式错误: {error}"))
                }
            }
        }
        None => return errors::management_error(400, "缺少 rules 字段，恢复默认请传 null"),
    };
    if let Err(error) =
        config::validate_account_sort(provider, rules.as_deref().unwrap_or_default())
    {
        return errors::management_error(400, error);
    }
    let only_if_unset = payload.get("onlyIfUnset") == Some(&Value::Bool(true));
    if let Err(error) = config::save_account_sort(provider, rules, only_if_unset) {
        return errors::management_error(500, error);
    }
    get_sorting(State(state)).await
}
