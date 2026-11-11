//! QuWork 登录续期：单飞 + 比较-再写 + 「新 token 相同且有效期没推进就判失败」。
//!
//! ── 续期只换账号令牌 ─────────────────────────────────────────
//! `GET /api/user/refresh`（Bearer loginToken）→ `data.token` 是新登录凭证、
//! `data.llm_token` 是**可选**的新模型凭证（没有就保留旧的，上游 1.6.0 的
//! 注释逐字如此）、`data.expire` 是新过期时刻。
//!
//! ── model_token 的容错（上游同款，别丢）───────────────────────
//! 账号服务不可用**不应阻断仍有效的独立模型凭证**：`ensure_access_token` 里
//! 临期时尝试刷新，失败就沿用刷新前的 secret 读 `llmToken`。
//!
//! ── 写回账号记录 ─────────────────────────────────────────────
//! 走本仓 `AccountStore` 的 `update_quwork_credentials_if_current`：在账号锁内
//! 比对凭证仍是刷新前那份才写（`CredentialWrite::Stale` 时重读快照）。该范式
//! 用 `record_by_id` + `sql::update_in_place`（与 atomcode / kiro 一致），
//! **不经 `store.load()`**，因此没有 `Arc<AccountState>` 需要 `(*state).clone()`。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic，不持锁跨 await。

use serde_json::{json, Value};

use crate::server::core::account_store::{AccountStore, CredentialWrite};
use crate::server::core::providers::adapter::usage_not_configured;
use crate::server::core::providers::refresh_flight::{self, Join, Table};
use crate::server::core::providers::ProviderKind;
use crate::server::errors::GatewayError;

use super::{client, credentials, endpoints};

/// 续期单飞表（键含两枚令牌的指纹，令牌一变就是新的一轮，与 atomcode 同口径）
static FLIGHTS: std::sync::OnceLock<Table<Value>> = std::sync::OnceLock::new();

/// 续期结果判据（**纯函数**，生产路径与单测共用）。
///
/// 上游 `refresh.rs` 的内联判据：新登录凭证与旧的一致、且有效期没有推进 →
/// 判失败（"未更新凭证或有效期，请重新登录"）。只换回同一枚令牌、或 expire
/// 倒退 / 持平都算这次续期没做成，不能写回账号。
pub fn should_reject_refresh(
    next: &str,
    previous: &str,
    expiry: i64,
    previous_expiry: i64,
) -> bool {
    next == previous && expiry <= previous_expiry
}

/// 取账号记录的内部 secret 形态（公开形态不带凭据，这里读的是原始记录）。
pub async fn snapshot(store: &AccountStore, account_id: &str) -> Result<Value, GatewayError> {
    store
        .quwork_account_record(account_id)
        .ok_or_else(|| GatewayError::with_status(404, "找不到 QuWork 账号"))
}

/// 是否临期（委托 `credentials::expiring`，判据只有一份）
pub fn expiring(secret: &Value) -> bool {
    credentials::expiring(secret)
}

/// 取可用模型凭证（含临期主动刷新；**刷新失败不阻断**）。
pub async fn model_token(store: &AccountStore, account_id: &str) -> Result<String, GatewayError> {
    let secret = snapshot(store, account_id).await?;
    let secret = if expiring(&secret) {
        refresh(store, account_id, false).await.unwrap_or(secret)
    } else {
        secret
    };
    credentials::token_of(&secret).map_err(|error| GatewayError::with_status(401, error.message))
}

/// 续期一次。`force = false`（自动 / 临期）看窗口；`force = true`（401 之后）无视窗口。
pub async fn refresh(
    store: &AccountStore,
    account_id: &str,
    force: bool,
) -> Result<Value, GatewayError> {
    let before = snapshot(store, account_id).await?;
    if !force && !expiring(&before) {
        return Ok(before);
    }
    let seen = credentials::login_token_of(&before);
    if seen.is_empty() {
        return Err(GatewayError::with_status(
            400,
            "QuWork 只提供了模型凭证，不能续期登录会话；请重新导入完整登录态",
        ));
    }
    let llm = credentials::text(&before, &["llmToken"]);
    let key = format!(
        "{}:{}:{}",
        crate::server::core::providers::kind_id(ProviderKind::QuWork),
        refresh_flight::fingerprint(&seen),
        refresh_flight::fingerprint(&llm),
    );
    match FLIGHTS.get_or_init(Table::new).join(&key) {
        Join::Waiter(waiter) => waiter.wait().await,
        Join::Leader(leader) => {
            let result = refresh_and_save(store, &before, &seen, force).await;
            leader.finish(result.clone());
            result
        }
    }
}

/// 真去打一次续期，并把轮换结果写回账号记录（比较-再写）。
async fn refresh_and_save(
    store: &AccountStore,
    before: &Value,
    seen: &str,
    force: bool,
) -> Result<Value, GatewayError> {
    let access = credentials::login_token_of(before);
    if access.is_empty() {
        return Err(GatewayError::with_status(
            400,
            "QuWork 只提供了模型凭证，不能续期登录会话；请重新导入完整登录态",
        ));
    }
    // 飞行途中凭证被换掉 / 已不再临期：这次结果作废，交回最新形态
    if access != seen || (!force && !expiring(before)) {
        return Ok(before.clone());
    }
    let reply = client::request("GET", endpoints::REFRESH_PATH, &access, None).await?;
    let data = reply.data("登录续期")?;
    let next = credentials::token(&credentials::text(&data, &["token"]), "登录凭证")?;
    if next.is_empty() {
        return Err(GatewayError::with_status(502, "QuWork 续期未返回登录凭证"));
    }
    let expiry = client::timestamp(data.get("expire"))
        .or_else(|| credentials::jwt_expiry(&next))
        .unwrap_or_else(|| {
            crate::server::logging::now_ms().saturating_add(2 * 60 * 60_000)
        });
    let previous_expiry = client::timestamp(before.get("expiresAt")).unwrap_or(0);
    if should_reject_refresh(&next, &access, expiry, previous_expiry) {
        return Err(GatewayError::with_status(
            502,
            "QuWork 续期未更新凭证或有效期，请重新登录",
        ));
    }
    let now = crate::server::logging::now_ms();
    let mut secret = before.clone();
    secret["loginToken"] = json!(next);
    secret["expiresAt"] = json!(expiry);
    secret["updatedAt"] = json!(now);
    // 1.6.0 的续期只更换账号令牌；没有新的 llm_token 时保留原模型凭证。
    let llm = credentials::token(&credentials::text(&data, &["llm_token"]), "模型凭证")?;
    if !llm.is_empty() {
        secret["llmToken"] = json!(llm);
    }
    let written = store
        .update_quwork_credentials_if_current(before, &secret)
        .map_err(|error| GatewayError::with_status(error.status_code, error.message))?;
    match written {
        CredentialWrite::Written => Ok(secret),
        // 记录已被别的操作更新：不能把旧结果当成功用，重读本账号的权威快照
        CredentialWrite::Stale => {
            let id = before.get("id").and_then(Value::as_str).unwrap_or("");
            snapshot(store, id).await
        }
    }
}

/// 带登录凭证的账号面请求；401 时强制刷新后重放一次（供 usage / checkin 用）。
pub async fn protected(
    store: &AccountStore,
    account_id: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<client::Reply, GatewayError> {
    let secret = snapshot(store, account_id).await?;
    let secret = if expiring(&secret) {
        refresh(store, account_id, false).await?
    } else {
        secret
    };
    let access = credentials::login_token_of(&secret);
    if access.is_empty() {
        return Err(usage_not_configured(
            "QuWork",
            "登录凭证 accessToken；请重新导入完整登录态",
        ));
    }
    let reply = client::request(method, path, &access, body).await?;
    if !reply.unauthorized() {
        return Ok(reply);
    }
    let refreshed = refresh(store, account_id, true).await?;
    client::request(
        method,
        path,
        &credentials::login_token_of(&refreshed),
        body,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_when_token_same_and_expiry_not_advanced() {
        assert!(should_reject_refresh("same", "same", 1000, 1000));
        assert!(should_reject_refresh("same", "same", 900, 1000));
    }

    #[test]
    fn accept_when_token_rotated_or_expiry_advanced() {
        assert!(!should_reject_refresh("new", "same", 1000, 1000));
        assert!(!should_reject_refresh("same", "same", 2000, 1000));
    }

    #[test]
    fn a_key_only_account_is_never_expiring() {
        let key_only = json!({ "llmToken": "llm-1" });
        assert!(!expiring(&key_only));
        let with_login = json!({
            "llmToken": "llm-1",
            "loginToken": "login-1",
            "expiresAt": 1_700_000_000_000_i64,
        });
        // 一个很早的过期时刻：进入窗口 → 临期
        assert!(expiring(&with_login));
    }
}
