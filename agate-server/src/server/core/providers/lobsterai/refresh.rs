//! LobsterAI 的续期链路：单飞 + 比较-再写。
//!
//! 见 `credentials.rs` 的模块头：续期会**轮换** refresh token，而三件载荷件
//! （uuid / 两个 keyfrom）**绝不覆盖**。

use serde_json::Value;

use crate::server::core::account_store::{AccountStore, CredentialWrite};
use crate::server::core::providers::refresh_flight::{self, Join, Table};
use crate::server::core::providers::ProviderKind;
use crate::server::errors::GatewayError;

use super::{chat, credentials, oauth};

/// 续期单飞表（进程级；键含两个令牌的指纹，令牌一变就是新的一轮）
static FLIGHTS: std::sync::OnceLock<Table<credentials::LobsterCredentials>> =
    std::sync::OnceLock::new();

/// 取到（必要时刷新出）可用令牌。`force = true` 时不看临期窗口（401 之后）
pub async fn ensure_fresh(
    store: &AccountStore,
    account_id: &str,
    force: bool,
) -> Result<String, GatewayError> {
    let record = store.lobsterai_account_record(account_id).ok_or_else(|| {
        GatewayError::with_status(
            401,
            "没有可用的 LobsterAI 账号：请在账号页用浏览器登录添加",
        )
    })?;
    let credentials = credentials::from_record(Some(&record))?;
    if !force && !credentials.expiring() {
        return Ok(credentials.access_token);
    }
    if !credentials.can_refresh() {
        return Err(GatewayError::with_status(
            401,
            "LobsterAI 账号缺少续期所需的载荷件（uuid / keyfrom 之一为空），请重新登录",
        ));
    }
    let key = format!(
        "{}:{}:{}",
        crate::server::core::providers::kind_id(ProviderKind::LobsterAi),
        refresh_flight::fingerprint(&credentials.access_token),
        refresh_flight::fingerprint(&credentials.refresh_token),
    );
    match FLIGHTS.get_or_init(Table::new).join(&key) {
        Join::Waiter(waiter) => waiter.wait().await.map(|fresh| fresh.access_token),
        Join::Leader(leader) => {
            let result = refresh_and_save(store, &record, &credentials).await;
            leader.finish(result.clone());
            result.map(|fresh| fresh.access_token)
        }
    }
}

/// 真的去打一次续期，并把**轮换后的令牌**写回账号记录。
///
/// ── 为什么只改三个字段 ──────────────────────────────────────
/// 载荷件三件套（uuid / 两个 keyfrom）**绝不能动**：它们是登录那一刻的形态，
/// 续期改写会让下一次续期的请求体与上游期望不符（见 `credentials.rs` 的模块头）。
async fn refresh_and_save(
    store: &AccountStore,
    record: &Value,
    credentials: &credentials::LobsterCredentials,
) -> Result<credentials::LobsterCredentials, GatewayError> {
    let client_version = chat::prime_client_version().await;
    let bundle = oauth::refresh(credentials, &client_version).await?;
    // 过期时刻：刚续期成功，以此刻为基准算（`expires_in` 缺失时退回读 JWT 的 exp）
    let expires_at = credentials::expires_at_from(
        crate::server::logging::now_ms(),
        bundle.expires_in,
        &bundle.access_token,
    );
    // 轮换语义：refresh token 非空才算轮换（上游可能省略，那表示沿用旧的那一个）
    let rotated = !bundle.refresh_token.trim().is_empty();
    let next_refresh = if rotated {
        bundle.refresh_token.clone()
    } else {
        credentials.refresh_token.clone()
    };
    // 比较-再写：记录里的 accessToken / refreshToken 仍是刷新前那份才写
    let written = store
        .update_lobsterai_credentials_if_current(
            record,
            &bundle.access_token,
            &next_refresh,
            expires_at,
        )
        .map_err(|error| GatewayError::with_status(error.status_code, error.message))?;
    match written {
        CredentialWrite::Written => {}
        CredentialWrite::Stale => {
            return Err(GatewayError::with_status(
                409,
                "LobsterAI 续期结果已被别的操作覆盖（账号记录已变）：请重试",
            ))
        }
    }
    crate::server::logging::log(
        "[LobsterAI]",
        "令牌已续期并写回账号（refresh token 已轮换）",
    );
    Ok(credentials::LobsterCredentials {
        id: credentials.id.clone(),
        access_token: bundle.access_token.clone(),
        refresh_token: next_refresh,
        expires_at,
        // 载荷件三件套保持登录时那一份（**不覆盖**）
        uid: credentials.uid.clone(),
        user_id: credentials.user_id.clone(),
        nickname: credentials.nickname.clone(),
        uuid: credentials.uuid.clone(),
        first_keyfrom: credentials.first_keyfrom.clone(),
        latest_keyfrom: credentials.latest_keyfrom.clone(),
    })
}
