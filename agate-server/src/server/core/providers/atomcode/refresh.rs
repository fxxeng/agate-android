//! AtomCode 的续期链路：单飞 + 比较-再写。
//!
//! ── 为什么必须单飞 ──────────────────────────────────────────
//! 续期**轮换** refresh token：代理同时下发新 access 与新 refresh，旧 access
//! 立即失效。两个并发续期会互相作废（后写覆盖前者），因此同一账号同一时刻
//! 只能有一次续期在跑 —— 单飞表的键含**两个令牌的指纹**（令牌一变就是新的
//! 一轮），与 lobsterai 同一口径。
//!
//! ── 为什么是「比较-再写」而不是无条件覆盖 ─────────────────────
//! 用户重登 / 面板「刷新 Token」可能在本轮续期飞行途中落盘。无条件覆盖会把
//! 那次更新的结果抹掉（旧的 refresh 已作废，写回去等于让账号只能重登）。
//! 落盘前比对「记录里的 access/refresh 仍是刷新前那份」才写；不等就返回
//! 409 让上层重读 —— 与 lobsterai 的 `CredentialWrite::Stale` 处置逐字相同。
//!
//! ── 合并规则 ────────────────────────────────────────────────
//! **非空才覆盖**（见 `oauth::TokenBundle::merge_into`）：代理可能省略
//! `refresh_token` / `expires_in` / `user`，整包替换会让一次「看起来成功的」
//! 续期抹掉 refresh，账号从此只能重登。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic，不持锁跨 await。

use serde_json::Value;

use crate::server::core::account_store::{AccountStore, CredentialWrite};
use crate::server::core::providers::refresh_flight::{self, Join, Table};
use crate::server::core::providers::ProviderKind;
use crate::server::errors::GatewayError;

use super::credentials::{self, AtomcodeCredentials, TokenBundle};
use super::oauth;

/// 续期单飞表（进程级；键含两个令牌的指纹，令牌一变就是新的一轮）
static FLIGHTS: std::sync::OnceLock<Table<credentials::AtomcodeCredentials>> =
    std::sync::OnceLock::new();

/// 取到（必要时刷新出）可用令牌。`force = true` 时不看临期窗口（401 之后）
pub async fn ensure_fresh(
    store: &AccountStore,
    account_id: &str,
    force: bool,
) -> Result<String, GatewayError> {
    let record = store.atomcode_account_record(account_id).ok_or_else(|| {
        GatewayError::with_status(
            401,
            "没有可用的 AtomCode 账号：请在账号页用 AtomGit 浏览器登录添加",
        )
    })?;
    let credentials = credentials::from_record(Some(&record))?;
    if !force && !credentials.expiring() {
        return Ok(credentials.access_token);
    }
    if credentials.refresh_token.trim().is_empty() {
        return Err(GatewayError::with_status(
            401,
            "AtomCode 账号没有 refresh_token，无法续期，请重新登录",
        ));
    }
    let key = format!(
        "{}:{}:{}",
        crate::server::core::providers::kind_id(ProviderKind::Atomcode),
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

/// 续期结果合并成「要写回的三件东西」：`(access_token, refresh_token, expires_at)`。
///
/// ── 为什么单独抽成纯函数 ────────────────────────────────────
/// 这三行是**轮换语义的全部**（「非空才覆盖」用在哪、过期时刻以什么为基准），
/// 而它们埋在一条要打网络的函数中间，只能靠集成测试碰运气。抽出来之后
/// 单测打的是**生产路径**（[`refresh_and_save`] 调的也是它）。
fn merge_refresh(
    previous: &AtomcodeCredentials,
    bundle: TokenBundle,
    now_ms: i64,
) -> (String, String, i64) {
    let baseline = TokenBundle {
        access_token: previous.access_token.clone(),
        refresh_token: previous.refresh_token.clone(),
        ..Default::default()
    };
    let merged = bundle.merge_into(&baseline);
    // 过期时刻：刚续期成功，以此刻为基准算（`expires_in` 缺失时 0 = 判不出来）
    let expires_at = merged.expires_at(now_ms);
    (merged.access_token, merged.refresh_token, expires_at)
}

/// 真的去打一次续期，并把**轮换后的令牌对**写回账号记录。
async fn refresh_and_save(
    store: &AccountStore,
    record: &Value,
    credentials: &AtomcodeCredentials,
) -> Result<AtomcodeCredentials, GatewayError> {
    let bundle = oauth::refresh(&credentials.refresh_token).await?;
    let (access_token, refresh_token, expires_at) =
        merge_refresh(credentials, bundle, crate::server::logging::now_ms());
    // 比较-再写：记录里的两个令牌仍是刷新前那份才写（见模块头）
    let written = store
        .update_atomcode_credentials_if_current(record, &access_token, &refresh_token, expires_at)
        .map_err(|error| GatewayError::with_status(error.status_code, error.message))?;
    match written {
        CredentialWrite::Written => {}
        CredentialWrite::Stale => {
            return Err(GatewayError::with_status(
                409,
                "AtomCode 续期结果已被别的操作覆盖（账号记录已变）：请重试",
            ))
        }
    }
    crate::server::logging::log(
        "[AtomCode]",
        "令牌已续期并写回账号（refresh token 已轮换）",
    );
    Ok(credentials::AtomcodeCredentials {
        id: credentials.id.clone(),
        access_token,
        refresh_token,
        expires_at,
        user_id: credentials.user_id.clone(),
        name: credentials.name.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials(access: &str, refresh: &str) -> AtomcodeCredentials {
        AtomcodeCredentials {
            id: "atomcode-user-1".to_string(),
            access_token: access.to_string(),
            refresh_token: refresh.to_string(),
            expires_at: 0,
            user_id: "1".to_string(),
            name: "atomgit".to_string(),
        }
    }

    /// 轮换：上游给了新 refresh 就用新的；过期时刻以**此刻**为基准算
    #[test]
    fn rotation_replaces_both_tokens() {
        let previous = credentials("old-a", "old-r");
        let bundle = TokenBundle {
            access_token: "new-a".to_string(),
            refresh_token: "new-r".to_string(),
            expires_in: 3_600,
            ..Default::default()
        };
        let (access, refresh, expires_at) = merge_refresh(&previous, bundle, 1_700_000_000_000);
        assert_eq!(access, "new-a");
        assert_eq!(refresh, "new-r");
        assert_eq!(expires_at, 1_700_003_600_000);
    }

    /// ⚠️ 上游省略 `refresh_token` / `expires_in` 时**保留旧值**（整包替换会让账号失去续期能力）
    #[test]
    fn omitted_fields_keep_previous_values() {
        let previous = credentials("old-a", "old-r");
        let bundle = TokenBundle {
            access_token: "new-a".to_string(),
            ..Default::default()
        };
        let (access, refresh, expires_at) = merge_refresh(&previous, bundle, 1_700_000_000_000);
        assert_eq!(access, "new-a");
        assert_eq!(refresh, "old-r");
        // 判不出过期时刻 = 0（不猜一个值）
        assert_eq!(expires_at, 0);
    }
}
