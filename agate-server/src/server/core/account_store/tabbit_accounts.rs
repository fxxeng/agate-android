//! Tabbit 账号：粘贴网页 Cookie / cURL / 裸 JWT → 可落库账号。
//!
//! ── 站点为什么不落进记录 JSON ────────────────────────────────
//! 与 MonkeyCode / ZCode 同一条理由：站点**已经编码在 provider id 里**
//! （`tabbit` / `tabbit-intl`），而记录自带 `provider` 字段 —— 站点由
//! `Realm::from_provider_id` 直接还原，不需要再存一份 `realm` 字段
//! （存了两处事实来源，漂移时会静默打到错误的站点）。
//!
//! ── 落盘字段 ────────────────────────────────────────────────
//! ```text
//!   accessToken   JWT（键名与别家统一：`has_token()` / `access_token()` 一族认它）
//!   cookie        过滤后的整串 Cookie（上游只带 token 会判 401，见 credentials.rs）
//!   uid           上游 user_id（额度接口必填）
//!   unlockPro     是否发送「已设为默认浏览器」标记（解锁 Premium 模型）
//!   clientVersion 客户端版本号（`x-req-ctx`，空串用默认值）
//!   expiresAt     从 JWT 的 exp 算出来的过期时刻（只用于展示）
//! ```
//!
//! ── 身份与 id ───────────────────────────────────────────────
//! 优先用上游 `uid`（换一次令牌仍是同一个账号）；拿不到时退化成令牌摘要。
//! id 前缀按站点分开 —— 两站的 uid 空间不同，共用前缀会让同一个人在两家的
//! 账号撞 id（与 qwenwork 那次同一类问题）。
//!
//! ── 没有续期（实测）────────────────────────────────────────
//! JWT 7 天有效、上游没有 refresh 端点：`expiresAt` 只是**展示**用的，
//! 过期只能重新导出 Cookie（`supports_refresh() == false`）。
//!
//! 本文件全是「读-改-写」存储操作，**没有任何网络请求**（持锁不做网络）。
//! 绝不 unwrap/expect（release 是 panic=abort）。

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::server::core::providers::tabbit::credentials;
use crate::server::core::providers::tabbit::Realm;
use crate::server::logging;

use super::priority::next_free_priority;
use super::sql;
use super::state::{mark_name_custom, StoredAccount};
use super::store::{AccountStore, AccountStoreError};
use super::store_util::{token_tail_of, truncate_chars};

/// 备注名长度上限（与别家同一口径）
const MAX_NAME_LENGTH: usize = 100;
/// 令牌长度上限
const MAX_TOKEN_LENGTH: usize = 32_768;
/// Cookie 长度上限
const MAX_COOKIE_LENGTH: usize = 32_768;

fn pick(payload: &Value, keys: &[&str]) -> String {
    credentials::pick(payload, keys)
}

/// 账号身份串：有 uid 用 uid，没有就退化成令牌摘要
fn identity_of(fields: &Value) -> String {
    let uid = pick(fields, &["uid", "user_id", "userId"]);
    if !uid.is_empty() {
        return format!("uid:{uid}");
    }
    let token = pick(fields, &["accessToken", "access_token", "token"]);
    if token.is_empty() {
        return String::new();
    }
    let mut hasher = Sha256::new();
    hasher.update(b"tabbit:token-identity:v1\0");
    hasher.update(token.as_bytes());
    format!("token:{:x}", hasher.finalize())
}

/// 身份串 → 稳定的账号 id（站点前缀分开，见模块头）
fn account_id_for(realm: Realm, identity: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"tabbit:account-id:v1\0");
    hasher.update(realm.id().as_bytes());
    hasher.update(b"\0");
    hasher.update(identity.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("{}-{}", realm.id(), &digest[..12])
}

impl AccountStore {
    /// 取一条 Tabbit 账号记录（公开形态的原始 JSON）。
    ///
    /// `account_id` 非空 → 按 id 直查（不属于本站点返回 None）；
    /// 为空 → 本站点**启用中**账号里优先级最靠前的一条（转发默认用队首）。
    pub fn tabbit_account_record(&self, realm: Realm, account_id: &str) -> Option<Value> {
        let guard = self.guard();
        if !account_id.is_empty() {
            let record = self.record_by_id(&guard, account_id)?;
            return (record.provider() == realm.id()).then(|| record.to_value());
        }
        let mut candidates: Vec<StoredAccount> = self
            .records_for_provider(&guard, realm.id())
            .into_iter()
            .filter(StoredAccount::enabled)
            .collect();
        candidates.sort_by_key(StoredAccount::order_key);
        candidates.into_iter().next().map(|item| item.to_value())
    }

    /// 添加/更新一个 Tabbit 账号（粘贴凭证的唯一落账号入口）。
    ///
    /// 同一身份的记录就地更新（保留优先级与启用状态、沿用用户改过的备注名）；
    /// 撞到**别的 provider** 的 id 时报错，绝不覆写（与另外几家同一策略）。
    pub fn add_tabbit_account(
        &self,
        realm: Realm,
        payload: &Value,
        name: Option<&str>,
        source: &str,
    ) -> Result<Value, AccountStoreError> {
        let token = pick(payload, &["accessToken", "access_token", "token"]);
        let cookie = pick(payload, &["cookie"]);
        if token.is_empty() && cookie.is_empty() {
            return Err(AccountStoreError::new(
                "缺少登录凭证：请粘贴网页 Cookie（token=…; user_id=…）、Copy as cURL 整段，或裸 JWT",
                400,
            ));
        }
        if token.chars().count() > MAX_TOKEN_LENGTH || cookie.chars().count() > MAX_COOKIE_LENGTH {
            return Err(AccountStoreError::new("Tabbit 凭证过长", 400));
        }
        // 令牌形态只做「不含空白、非空」这种不误伤的检查：真正的判定在上游
        // （由 `api::accounts` 的探活负责，见 `normalize` 的调用方）
        if !token.is_empty() && !credentials::shape_ok(&token) {
            return Err(AccountStoreError::new(
                "Tabbit 登录令牌格式无效（应是不含空白的一整串）",
                400,
            ));
        }
        let identity = identity_of(payload);
        if identity.is_empty() {
            return Err(AccountStoreError::new("无法识别这个 Tabbit 账号", 400));
        }
        let id = account_id_for(realm, &identity);

        let guard = self.guard();
        let existing = self.record_by_id(&guard, &id);
        if let Some(existing) = existing.as_ref() {
            let existing_provider = existing.provider();
            if existing_provider != realm.id() {
                return Err(AccountStoreError::new(
                    format!(
                        "账号 id「{id}」已被{existing_provider}账号占用，无法添加同一个身份的 \
                         {} 账号（请先处理那个账号）",
                        realm.label()
                    ),
                    400,
                ));
            }
        }
        // 备注名兜底：显式传入 → 既有记录里的备注名 → 「Tabbit 账号 <尾号>」
        let explicit_name = name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| truncate_chars(value, MAX_NAME_LENGTH));
        let record_name = explicit_name
            .or_else(|| {
                existing
                    .as_ref()
                    .map(StoredAccount::name)
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_else(|| {
                let tail = token_tail_of(&token);
                if tail.is_empty() {
                    format!("{} 账号", realm.label())
                } else {
                    format!("{} {}", realm.label(), tail)
                }
            });
        let priority = match existing.as_ref() {
            Some(record) => record.priority(),
            None => {
                // 优先级全局唯一（各家共用一条队列），取全库已用号段
                let used = self.with_conn(&guard, |conn| sql::priorities_all(conn))?;
                next_free_priority(&used)
            }
        };
        let now = logging::now_ms();
        let mut record = Map::new();
        record.insert("id".to_string(), Value::String(id.clone()));
        record.insert(
            "provider".to_string(),
            Value::String(realm.id().to_string()),
        );
        record.insert("name".to_string(), Value::String(record_name.clone()));
        mark_name_custom(
            &mut record,
            name.is_some_and(|value| !value.trim().is_empty()),
            existing.as_ref(),
        );
        record.insert("accessToken".to_string(), Value::String(token.clone()));
        record.insert("cookie".to_string(), Value::String(cookie));
        record.insert(
            "uid".to_string(),
            Value::String(pick(payload, &["uid", "user_id", "userId"])),
        );
        record.insert("tokenTail".to_string(), Value::String(token_tail_of(&token)));
        // 过期时刻只用于展示：上游没有续期接口（见模块头）
        if let Some(expires_at) = credentials::expires_at_ms(&token) {
            record.insert("expiresAt".to_string(), Value::from(expires_at));
        }
        record.insert(
            "unlockPro".to_string(),
            // 宽松布尔：界面把这一项当文本收（「填 0 关闭」），只认 JSON 布尔
            // 会让「填了 0 却仍然发默认浏览器标记」静默成立
            Value::Bool(credentials::bool_field(payload, "unlockPro", true)),
        );
        let client_version = pick(payload, &["clientVersion"]);
        if !client_version.is_empty() {
            record.insert("clientVersion".to_string(), Value::String(client_version));
        }
        record.insert("source".to_string(), Value::String(source.to_string()));
        record.insert("priority".to_string(), Value::from(priority));
        record.insert(
            "enabled".to_string(),
            Value::Bool(
                existing
                    .as_ref()
                    .map(StoredAccount::enabled)
                    .unwrap_or(true),
            ),
        );
        record.insert(
            "addedAt".to_string(),
            Value::from(
                existing
                    .as_ref()
                    .map(StoredAccount::added_at)
                    .filter(|value| *value != 0)
                    .unwrap_or(now),
            ),
        );
        record.insert("updatedAt".to_string(), Value::from(now));
        // 未知字段全量保留（用户手工加过的字段不能因为一次「更新账号」丢掉）
        let mut merged = record;
        if let Some(existing) = existing.as_ref() {
            for (key, value) in existing.fields() {
                merged.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        let saved = StoredAccount::from_map(merged);
        self.with_conn(&guard, |conn| sql::put(conn, &saved))?;
        logging::log(
            "[Accounts]",
            &format!(
                "✅ {} 账号已保存: {record_name}（优先级 {priority}，令牌 {}）",
                realm.label(),
                credentials::masked(token.as_str())
            ),
        );
        Ok(self.public_account(&saved))
    }

    /// Tabbit 账号的**公开形态**（进 HTTP 响应）：去掉 Cookie 与令牌本体，
    /// 只留尾几位。
    pub fn to_tabbit_public_account(&self, record: &StoredAccount) -> Value {
        let mut value = record.to_value();
        let has_token = record
            .get("accessToken")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty());
        if let Some(object) = value.as_object_mut() {
            object.remove("accessToken");
            object.remove("cookie");
            object.insert("available".to_string(), Value::Bool(record.enabled()));
            // 本家语义的字段名：界面按它显示「已配置 / 需重贴」
            object.insert("hasToken".to_string(), Value::Bool(has_token));
            // 没有续期手段：如实告诉界面「这个账号不能自动维护」
            object.insert("canRefresh".to_string(), Value::Bool(false));
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 身份：有 uid 用 uid（换令牌仍是同一账号），没有才退化成令牌摘要
    #[test]
    fn identity_prefers_uid_over_the_token() {
        let with_uid = json!({ "uid": "u-1", "accessToken": "jwt-a" });
        let same_uid_other_token = json!({ "uid": "u-1", "accessToken": "jwt-b" });
        assert_eq!(identity_of(&with_uid), identity_of(&same_uid_other_token));
        assert_eq!(identity_of(&with_uid), "uid:u-1");

        let token_only = json!({ "accessToken": "jwt-a" });
        assert!(identity_of(&token_only).starts_with("token:"));
        // 不同令牌给不同身份
        let other = json!({ "accessToken": "jwt-b" });
        assert_ne!(identity_of(&token_only), identity_of(&other));
    }

    /// 完全没有凭证 → 空身份（调用方据此报错）
    #[test]
    fn identity_is_empty_without_credentials() {
        assert_eq!(identity_of(&json!({ "name": "x" })), "");
    }

    /// 两个站点的 id 前缀必须分开：同一 uid 在两站是不同的账号
    #[test]
    fn account_ids_never_share_a_prefix_across_sites() {
        let identity = "uid:same-person";
        let cn = account_id_for(Realm::Cn, identity);
        let intl = account_id_for(Realm::Intl, identity);
        assert_ne!(cn, intl);
        assert!(cn.starts_with("tabbit-"), "{cn}");
        assert!(intl.starts_with("tabbit-intl-"), "{intl}");
    }

    /// id 对同一身份稳定（再粘一次要更新同一条，而不是新增一条）
    #[test]
    fn account_id_is_deterministic() {
        assert_eq!(
            account_id_for(Realm::Cn, "uid:u1"),
            account_id_for(Realm::Cn, "uid:u1")
        );
        assert_ne!(
            account_id_for(Realm::Cn, "uid:u1"),
            account_id_for(Realm::Cn, "uid:u2")
        );
    }

    /// id 不能泄漏令牌明文（摘要里不含原串）
    #[test]
    fn account_id_does_not_leak_the_token() {
        let id = account_id_for(Realm::Cn, "token:eyJhbGciOi.secret.sig");
        assert!(!id.contains("secret"));
        assert!(!id.contains("eyJhbGci"));
    }
}
