//! 模型访问：`GET /api/model-access/models` 与 `GET /api/model-access/keys`。
//!
//! ── 这两条端点解决什么 ──────────────────────────────────────
//! 客户端（尤其是先选 Key 再选模型的界面）需要回答两个问题：
//!   · 「我现在这把 Key 到底能用哪些模型」—— `/models` 按**这一把** Key 的
//!     白名单收窄后返回；
//!   · 「我可以从哪些 Key 里选」—— `/keys` 给一份可用的 Key 清单（明文只在
//!     本机管理面回给已登录的调用方，与 `/api/keys` 的取向一致）。
//!
//! ── 与 `/v1/models` 的区别 ──────────────────────────────────
//! `/v1/models` 是**网关面**的免鉴权探针（客户端配 Key 之前就要能拉），它按
//! 请求头里的 Key 尽力收窄，拿不到限制就给全量。这两条是**管理面**的，必须
//! 明确带上一把 Key ——「你问的是这把 Key 的权限」，不带就是 401，不回退全量。
//!
//! ── 为什么鉴权在这里重做一遍 ────────────────────────────────
//! 中间件只负责「这个请求能不能进管理面」，它不知道 handler 关心的是哪一把
//! Key（面板会话也是一条合法通道，但那时没有 Key 身份）。所以这里按
//! 「显式 Key」重新解析一次：`x-api-key` 优先，其次 `Authorization: Bearer`。

use axum::http::HeaderMap;
use serde_json::{json, Value};

use crate::server::core::{api_keys, key_scope::KeyScope};
use crate::server::errors::GatewayError;
use crate::server::request_stats::{ApiKeyIdentity, RequestSource};

/// 环境变量注入的那把 Key 在清单里的固定 id（与配置里的记录区分开）。
pub const ENV_KEY_ID: &str = "env";

/// 一次模型调用的身份：这把 Key 的限制 + 它的来源。
pub struct ModelAccess {
    /// `None` = 不限制（环境变量 Key / 免鉴权）。
    pub scope: Option<KeyScope>,
    pub source: RequestSource,
}

/// 从请求头解析「我要用哪一把 Key」。
///
/// `x-api-key` 优先于 `Authorization`：显式选择不能被管理面带的凭证兜底替换
/// —— 否则用户在界面上选了 A 这把，实际按 B 的权限算，界面与结果对不上。
pub fn presented_key(headers: &HeaderMap) -> Option<&str> {
    if let Some(value) = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
    {
        return Some(value).filter(|key| !key.is_empty());
    }
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(crate::server::http::strip_bearer_prefix)
        .filter(|key| !key.is_empty())
}

/// 解析身份；不带 Key 直接 401（见模块头）。
pub fn authenticate(headers: &HeaderMap, client: &str) -> Result<ModelAccess, GatewayError> {
    let presented = presented_key(headers).ok_or_else(|| {
        GatewayError::with_status(
            401,
            "模型调用必须选择并携带 API Key，请先在「网关 Key」页创建并启用一把 Key",
        )
        .with_code("model_api_key_required")
    })?;
    // 环境变量注入的那把不在配置表里，单独判一次
    let env_key = crate::server::config::parse::env_api_key_value();
    let identity = if env_key.as_deref() == Some(presented) {
        ApiKeyIdentity::env()
    } else {
        api_keys::list()
            .into_iter()
            .find(|entry| entry.is_usable() && entry.key == presented)
            .map(|entry| ApiKeyIdentity::key(entry.id, entry.name))
            .ok_or_else(|| {
                GatewayError::with_status(
                    401,
                    "所选 API Key 已失效、停用或不可用，请重新选择一把启用的 Key",
                )
                .with_code("invalid_api_key")
            })?
    };
    Ok(ModelAccess {
        scope: crate::server::http::key_scope_from_headers(headers),
        source: RequestSource {
            client: client.to_string(),
            api_key: identity,
            ..Default::default()
        },
    })
}

pub async fn models(
    axum::extract::State(state): axum::extract::State<crate::server::ServerState>,
    headers: HeaderMap,
) -> axum::response::Response {
    match authenticate(&headers, "") {
        Ok(access) => {
            crate::server::http::ok_json(crate::server::core::providers::catalog::models_response(
                state.store(),
                access.scope.as_ref(),
            ))
        }
        Err(error) => error.payload_response(),
    }
}

pub async fn list_keys() -> axum::response::Response {
    crate::server::http::ok_json(json!({ "keys": key_choices() }))
}

/// 可选的 Key 清单（明文随列表返回：管理面已登录，与 `/api/keys` 同一取向）。
///
/// 过滤掉不可用的（停用 / 密钥文件丢失后解不开）：让用户选一把注定 401 的 Key
/// 只是在制造一次无意义的失败。
pub fn key_choices() -> Vec<Value> {
    let mut choices: Vec<Value> = api_keys::list()
        .into_iter()
        .filter(api_keys::ApiKeyEntry::is_usable)
        .map(|entry| {
            let mut value = entry.public_json();
            value["source"] = json!("key");
            value
        })
        .collect();
    if let Some(key) = crate::server::config::parse::env_api_key_value() {
        choices.push(json!({
            "id": ENV_KEY_ID,
            "name": "环境变量 Key",
            "masked": api_keys::mask(&key),
            "key": key,
            "source": "env",
            "enabled": true,
            "allowedProviders": [],
            "allowedModels": [],
        }));
    }
    choices
}
