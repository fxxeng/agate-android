//! Kiro 账号：粘贴 API key / 登录态 → 可落库账号，以及续期结果的回写。
//!
//! ── 落盘字段 ────────────────────────────────────────────────
//! ```text
//!   method        apikey / social / idc / external-idp
//!   accessToken   访问令牌（API key 时是 ksk_…）
//!   refreshToken  续期令牌（API key 没有）
//!   expiresAt     毫秒时间戳（0 = 不知道）
//!   region        登录地区（profile 缺失时的兜底）
//!   profileArn    AWS 分配的 profile ARN（地区与账号都从它读）
//!   clientId/clientSecret   idc / external-idp 换码用
//!   loginProvider google / github / builderid / enterprise
//!   accountId     邮箱或 AWS 账号（展示 + 去重兜底）
//!   plan          Free / Pro / Pro+ / Power（展示用）
//! ```
//!
//! ── 身份三级判定，顺序不能换 ────────────────────────────────
//! ```text
//!   1  profile ARN   同一 AWS 账号下可以有多个 profile，ARN 才是真身份
//!   2  accountId     邮箱 / 12 位账号（profile 还没补上的那段时间靠它）
//!   3  令牌摘要      API key 与「刚粘贴、还没探活」的登录态只能这样认
//! ```
//! 只按令牌认身份会有一个具体后果：登录态每次续期都换新 access token，
//! 于是每续期一次就在列表里多一个账号。
//!
//! ── 回写为什么要比对身份键 ──────────────────────────────────
//! 单飞只保证同进程内不并发；**用户在这期间重新添加/导入**会换掉记录。
//! 写回前比对身份键，不一致就返回 `Stale` 让调用方重读 —— 不这样，
//! 一次迟到的刷新会把刚贴进去的新凭证覆盖成旧的。
//!
//! 本文件全是「读-改-写」存储操作，**没有任何网络请求**（持锁不做网络）。
//! 绝不 unwrap/expect（release 是 panic=abort）。

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::server::core::providers::kiro::credentials::KiroCredentials;
use crate::server::core::providers::kiro::{self, endpoints};
use crate::server::logging;

use super::priority::next_free_priority;
use super::sql;
use super::state::{mark_name_custom, StoredAccount};
use super::store::{AccountStore, AccountStoreError};
use super::store_util::{token_tail_of, truncate_chars};
use super::CredentialWrite;

/// 本家的 provider id
pub const KIRO_PROVIDER_ID: &str = kiro::ID;

/// 回写前比对「还是不是同一份凭证」的键
const IDENTITY_KEYS: &[&str] = &["accessToken", "refreshToken", "profileArn", "method"];

/// 备注名 / 令牌的长度上限（与别家同一口径）
const MAX_NAME_LENGTH: usize = 100;
const MAX_TOKEN_LENGTH: usize = 32_768;

fn pick(payload: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(value) = payload.get(*key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

fn digest(seed: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(seed);
    format!("{:x}", hasher.finalize())
}

/// 账号身份串（三级判定，见模块头）。
///
/// `pub(crate)` 是给 `account_transfer::identity` 用的：导入判重必须与落库判重
/// **同一个口径**，两处各写一份必然分叉（症状是「同一个账号导两次多出一条」）。
pub(crate) fn identity_of(fields: &Value) -> String {
    let profile = pick(fields, &["profileArn", "profile"]);
    if !profile.is_empty() {
        return format!("profile:{profile}");
    }
    let account = pick(fields, &["accountId", "email"]);
    if !account.is_empty() {
        return format!("account:{account}");
    }
    let refresh = pick(fields, &["refreshToken", "refresh_token", "refresh"]);
    if !refresh.is_empty() {
        return format!("refresh:{}", digest(refresh.as_bytes()));
    }
    let access = pick(fields, &["accessToken", "access_token", "access"]);
    if !access.is_empty() {
        return format!("access:{}", digest(access.as_bytes()));
    }
    String::new()
}

/// 身份串 → 稳定的账号 id
fn account_id_for(identity: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"kiro:account-id:v1\0");
    hasher.update(identity.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    format!("kiro-{}", &hex[..12])
}

impl AccountStore {
    /// 取一条 Kiro 账号记录（原始 JSON）。
    ///
    /// `account_id` 非空 → 按 id 直查；为空 → 启用中优先级最靠前的一条。
    pub fn kiro_account_record(&self, account_id: &str) -> Option<Value> {
        let guard = self.guard();
        if !account_id.is_empty() {
            let record = self.record_by_id(&guard, account_id)?;
            return (record.provider() == KIRO_PROVIDER_ID).then(|| record.to_value());
        }
        let mut candidates: Vec<StoredAccount> = self
            .records_for_provider(&guard, KIRO_PROVIDER_ID)
            .into_iter()
            .filter(StoredAccount::enabled)
            .collect();
        candidates.sort_by_key(StoredAccount::order_key);
        candidates.into_iter().next().map(|item| item.to_value())
    }

    /// 添加/更新一个 Kiro 账号。
    ///
    /// 同一身份就地更新（保留优先级、启用状态与用户改过的备注名）；未知字段
    /// 全量保留；撞到别的 provider 的 id 时报错而不是覆写。
    pub fn add_kiro_account(
        &self,
        payload: &Value,
        name: Option<&str>,
        source: &str,
    ) -> Result<Value, AccountStoreError> {
        let credentials = KiroCredentials::from_fields(payload)
            .map_err(|error| AccountStoreError::new(error.message, 400))?;
        if credentials.access.chars().count() > MAX_TOKEN_LENGTH {
            return Err(AccountStoreError::new("Kiro 凭证过长", 400));
        }
        // 形态闸：这两条都会拼进主机名 / 请求体（见 `endpoints` 的模块头）
        if !endpoints::is_aws_region(&credentials.region) {
            return Err(AccountStoreError::new(
                "Kiro 的地区名不合法（应形如 us-east-1）",
                400,
            ));
        }
        let identity = identity_of(payload);
        if identity.is_empty() {
            return Err(AccountStoreError::new("无法识别这个 Kiro 账号", 400));
        }
        let id = account_id_for(&identity);

        let guard = self.guard();
        let existing = self.record_by_id(&guard, &id);
        if let Some(existing) = existing.as_ref() {
            let provider = existing.provider();
            if provider != KIRO_PROVIDER_ID {
                return Err(AccountStoreError::new(
                    format!(
                        "账号 id「{id}」已被{provider}账号占用，无法添加同一个身份的 Kiro 账号\
                         （请先处理那个账号）"
                    ),
                    400,
                ));
            }
        }
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
                // 备注名兜底：先试能看懂的（邮箱 / 计划），最后退到令牌尾号
                let label = if !credentials.account_id.is_empty() {
                    credentials.account_id.clone()
                } else if !credentials.plan.is_empty() {
                    credentials.plan.clone()
                } else {
                    token_tail_of(&credentials.access)
                };
                if label.is_empty() {
                    "Kiro 账号".to_string()
                } else {
                    format!("Kiro {label}")
                }
            });
        let priority = match existing.as_ref() {
            Some(record) => record.priority(),
            None => {
                let used = self.with_conn(&guard, |conn| sql::priorities_all(conn))?;
                next_free_priority(&used)
            }
        };
        let now = logging::now_ms();
        let mut record = Map::new();
        record.insert("id".to_string(), Value::String(id.clone()));
        record.insert(
            "provider".to_string(),
            Value::String(KIRO_PROVIDER_ID.to_string()),
        );
        record.insert("name".to_string(), Value::String(record_name.clone()));
        mark_name_custom(
            &mut record,
            name.is_some_and(|value| !value.trim().is_empty()),
            existing.as_ref(),
        );
        credentials.write_into(&mut record);
        record.insert(
            "tokenTail".to_string(),
            Value::String(token_tail_of(&credentials.access)),
        );
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
        // 未知字段全量保留
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
                "✅ Kiro 账号已保存: {record_name}（优先级 {priority}，{}，令牌 {}）",
                credentials.method,
                credentials.masked()
            ),
        );
        Ok(self.public_account(&saved))
    }

    /// 续期结果回写，前提是「记录里那份凭证还是当初那一份」。
    ///
    /// 不一致返回 `Stale`（调用方重读而不是覆盖）。
    pub fn update_kiro_credentials_if_current(
        &self,
        expected: &Value,
        credentials: &KiroCredentials,
    ) -> Result<CredentialWrite, AccountStoreError> {
        let id = expected.get("id").and_then(Value::as_str).unwrap_or("");
        let guard = self.guard();
        let Some(mut record) = self
            .record_by_id(&guard, id)
            .filter(|record| record.provider() == KIRO_PROVIDER_ID)
        else {
            return Ok(CredentialWrite::Stale);
        };
        for key in IDENTITY_KEYS {
            if record.get(key) != expected.get(key) {
                return Ok(CredentialWrite::Stale);
            }
        }
        credentials.write_into(record.fields_mut());
        record.set("tokenTail", Value::String(token_tail_of(&credentials.access)));
        record.set_updated_at(logging::now_ms());
        self.with_conn(&guard, |conn| sql::update_in_place(conn, &record))?;
        Ok(CredentialWrite::Written)
    }

    /// Kiro 账号的**公开形态**（进 HTTP 响应）：去掉令牌与 clientSecret。
    pub fn to_kiro_public_account(&self, record: &StoredAccount) -> Value {
        let mut value = record.to_value();
        let has_token = record
            .get("accessToken")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty());
        let can_refresh = record
            .get("refreshToken")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
            && record.get("method").and_then(Value::as_str) != Some("apikey");
        if let Some(object) = value.as_object_mut() {
            object.remove("accessToken");
            object.remove("refreshToken");
            object.remove("clientSecret");
            object.insert("available".to_string(), Value::Bool(record.enabled()));
            object.insert("hasToken".to_string(), Value::Bool(has_token));
            object.insert("canRefresh".to_string(), Value::Bool(can_refresh));
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// profile 优先：同一 AWS 账号下换 profile 是新账号，换令牌不是
    #[test]
    fn identity_prefers_the_profile_arn() {
        let with_profile = json!({
            "profileArn": "arn:aws:codewhisperer:us-east-1:1:profile/A",
            "accountId": "a@b.com",
            "accessToken": "t1",
            "refreshToken": "r1"
        });
        let mut rotated = with_profile.clone();
        rotated["accessToken"] = json!("t2");
        rotated["refreshToken"] = json!("r2");
        assert_eq!(identity_of(&with_profile), identity_of(&rotated));
        assert_eq!(
            identity_of(&with_profile),
            "profile:arn:aws:codewhisperer:us-east-1:1:profile/A"
        );
        // 换 profile 就是另一个身份
        let mut other = with_profile.clone();
        other["profileArn"] = json!("arn:aws:codewhisperer:us-east-1:1:profile/B");
        assert_ne!(identity_of(&with_profile), identity_of(&other));
    }

    /// 没有 profile 时退到账号（邮箱），再退到令牌摘要
    #[test]
    fn identity_falls_back_to_the_account_then_the_token() {
        let account = json!({ "accountId": "a@b.com", "accessToken": "t1" });
        assert_eq!(identity_of(&account), "account:a@b.com");
        let token_only = json!({ "accessToken": "t1" });
        assert!(identity_of(&token_only).starts_with("access:"));
        // 只有 refresh 时优先用它（access 每次续期都变）
        let refresh_only = json!({ "refreshToken": "r1", "accessToken": "t1" });
        assert!(identity_of(&refresh_only).starts_with("refresh:"));
        assert_eq!(identity_of(&json!({ "name": "x" })), "");
    }

    /// id 稳定、且不泄漏令牌明文
    #[test]
    fn account_ids_are_stable_and_opaque() {
        let id = account_id_for("access:eyJhbGciOi.secret.sig");
        assert!(id.starts_with("kiro-"), "{id}");
        assert_eq!(id.len(), 17, "kiro- + 12 位摘要");
        assert!(!id.contains("secret"));
        assert_eq!(id, account_id_for("access:eyJhbGciOi.secret.sig"));
        assert_ne!(id, account_id_for("access:other"));
    }
}
