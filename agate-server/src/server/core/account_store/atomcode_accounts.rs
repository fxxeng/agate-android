//! AtomCode 账号：网页登录（轮询）与**粘贴令牌**共用同一个落盘入口。
//!
//! ── 账号字段 ────────────────────────────────────────────────
//! ```text
//!   { id: "atomcode-<身份>", provider: "atomcode",
//!     name, accessToken, refreshToken?, tokenTail,
//!     userId, username, nickname, email, avatarUrl,
//!     createdAt, expiresIn, expiresAt,
//!     source, priority, enabled, addedAt, updatedAt }
//! ```
//!
//! ── 为什么 `expiresAt` 必须落盘（而不是每次现算）──────────────
//! 本家的令牌是**不透明串**，读不到 JWT 的 `exp`（见
//! `providers/atomcode/credentials.rs` 的模块头）。过期时刻只能由
//! `createdAt + expiresIn` 推出，而 `createdAt` 是**登录那一刻**的读数 ——
//! 每次现算都需要它，落盘一份 `expiresAt` 让维护链路（`credentials_expiring`）
//! 不必每次重新解释单位。续期写回时也只改这一个值。
//!
//! ── 为什么粘贴入口**允许**（与 lobsterai 相反）──────────────────
//! 本家的续期只要一枚 `refreshToken`（不像 lobsterai 还要 uuid + 两个 keyfrom）：
//! 用户在别处拿到的登录输出里就有它，粘贴是能真正用起来的路径。缺 refreshToken
//! 的粘贴也收下 —— 那种账号能转发、到期后必须重新获取（`canRefresh` 会如实为
//! false，界面据此提示）。
//!
//! ── 身份（去重）─────────────────────────────────────────────
//! 走 `providers::atomcode::account_id_of`（`userId → username → 令牌尾号`）——
//! **必须与登录路径同一个函数**：两处分叉会让「同一个账号重新登录」变成
//! 「新增一条记录」。
//!
//! 本文件全是「读-改-写」存储操作，**没有任何网络请求**（持锁不做网络）。
//! 绝不 unwrap/expect（release 是 panic=abort）。

use serde_json::{Map, Value};

use crate::server::core::providers::atomcode::{self, credentials};
use crate::server::core::providers::kind_id;
use crate::server::core::providers::ProviderKind;
use crate::server::logging;

use super::priority::next_free_priority;
use super::sql;
use super::state::{mark_name_custom, StoredAccount};
use super::store::{AccountStore, AccountStoreError};
use super::store_util::{token_tail_of, truncate_text};
use super::CredentialWrite;

/// 备注名长度上限（与别家同一口径）
const MAX_NAME_LENGTH: usize = 100;

/// 令牌长度上限（超过这个长度的输入一定是粘错了东西）
const MAX_TOKEN_LENGTH: usize = 8192;

/// AtomCode 的 provider id（引用注册表，避免各处再写一份字面量）
pub const ATOMCODE_PROVIDER_ID: &str = kind_id(ProviderKind::Atomcode);

/// 记录能不能续期：只要求 `refreshToken` 非空（本家续期只认它）。
///
/// 抽成纯函数是为了让**公开形态**与**登录/续期路径**用同一条判据，
/// 且单测能直接打生产路径（不用为了测试再造一个 `AccountStore`）。
pub(crate) fn can_refresh(record: &StoredAccount) -> bool {
    record
        .get("refreshToken")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
}

/// 账号的**公开形态**：摘掉令牌本体与 refresh token，只留「能不能续期」。
///
/// `refreshToken` 是长期有效的续期凭据 —— 下发等于把「这个账号能被别人续期」
/// 的条件送出去。界面需要的「能不能续期」由 `canRefresh` 布尔表达，不给原件。
pub(crate) fn public_shape(record: &StoredAccount) -> Value {
    let refreshable = can_refresh(record);
    let mut value = record.to_value();
    if let Some(object) = value.as_object_mut() {
        for key in ["accessToken", "refreshToken"] {
            object.remove(key);
        }
        object.insert("available".to_string(), Value::Bool(record.enabled()));
        object.insert("hasToken".to_string(), Value::Bool(true));
        object.insert("hasSession".to_string(), Value::Bool(true));
        object.insert("canRefresh".to_string(), Value::Bool(refreshable));
    }
    value
}

impl AccountStore {
    /// 取一条 AtomCode 账号记录（公开形态的原始 JSON）。
    ///
    /// `account_id` 非空 → 按 id 直查（不属于本家返回 None）；
    /// 为空 → 本家**启用中**账号里优先级最靠前的一条（转发默认用队首账号）。
    pub fn atomcode_account_record(&self, account_id: &str) -> Option<Value> {
        let guard = self.guard();
        if !account_id.is_empty() {
            let record = self.record_by_id(&guard, account_id)?;
            return (record.provider() == ATOMCODE_PROVIDER_ID).then(|| record.to_value());
        }
        let mut candidates: Vec<StoredAccount> = self
            .records_for_provider(&guard, ATOMCODE_PROVIDER_ID)
            .into_iter()
            .filter(StoredAccount::enabled)
            .collect();
        candidates.sort_by_key(StoredAccount::order_key);
        candidates.into_iter().next().map(|item| item.to_value())
    }

    /// 添加 / 更新一条 AtomCode 账号（**网页登录与粘贴共用这一个入口**）。
    ///
    /// `secret` 是已经归一好的令牌包（[`atomcode::secret_from_bundle`] 或
    /// [`atomcode::normalize_paste`] 的产物）：至少含 `accessToken`，可选
    /// `refreshToken` / `userId` / `username` / `nickname` / `email` / `avatarUrl` /
    /// `createdAt` / `expiresIn`。
    ///
    /// 同一身份的记录就地更新（保留优先级与启用状态、沿用用户改过的备注名）。
    pub fn add_atomcode_account(
        &self,
        secret: &Value,
        name_override: Option<&str>,
        source: &str,
    ) -> Result<Value, AccountStoreError> {
        let access_token = credentials::pick(secret, &["accessToken", "access_token", "token"]);
        if access_token.is_empty() {
            return Err(AccountStoreError::new("缺少 AtomCode 访问令牌", 400));
        }
        if access_token.chars().count() > MAX_TOKEN_LENGTH {
            return Err(AccountStoreError::new("AtomCode 访问令牌过长", 400));
        }
        // 身份口径与登录路径**同一函数**（见模块头）
        let id = atomcode::account_id_of(secret);

        let guard = self.guard();
        let existing = self.record_by_id(&guard, &id);
        if let Some(existing) = existing.as_ref() {
            if existing.provider() != ATOMCODE_PROVIDER_ID {
                return Err(AccountStoreError::new(
                    format!("账号 id「{id}」已被其它 provider 占用，无法添加这个 AtomCode 账号"),
                    400,
                ));
            }
        }
        let record_name = name_override
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| existing.as_ref().map(StoredAccount::name))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| atomcode::display_name_of(secret));
        let record_name = truncate_text(&record_name, MAX_NAME_LENGTH);
        let priority = match existing.as_ref() {
            Some(record) => record.priority(),
            None => {
                let used = self.with_conn(&guard, |conn| sql::priorities_all(conn))?;
                next_free_priority(&used)
            }
        };
        let now = logging::now_ms();
        let added_at = existing
            .as_ref()
            .map(StoredAccount::added_at)
            .filter(|value| *value != 0)
            .unwrap_or(now);
        let created_at = {
            // 官方登录输出的形态是 `created_at`（秒），本项目落盘是 `createdAt`
            let value = credentials::pick(secret, &["createdAt", "created_at"]);
            value.parse::<i64>().ok().filter(|value| *value > 0)
        }
        .unwrap_or(now / 1000);
        let expires_in = credential_seconds(secret, &["expiresIn", "expires_in"]);
        let expires_at = credentials::expires_at_from(created_at, expires_in, added_at);

        let mut record = Map::new();
        record.insert("id".to_string(), Value::String(id.clone()));
        record.insert(
            "provider".to_string(),
            Value::String(ATOMCODE_PROVIDER_ID.to_string()),
        );
        record.insert("name".to_string(), Value::String(record_name.clone()));
        // 用户自己打的备注名才算 custom（`name_override` 为空 = 派生名，可被后续替换）
        mark_name_custom(&mut record, name_override.is_some(), existing.as_ref());
        record.insert(
            "accessToken".to_string(),
            Value::String(access_token.clone()),
        );
        // 续期令牌非空才写（粘贴可能只给 accessToken —— 那种账号不能续期，
        // 但不该把上一次的好值洗掉）
        let refresh_token = credentials::pick(secret, &["refreshToken", "refresh_token"]);
        if !refresh_token.is_empty() {
            record.insert("refreshToken".to_string(), Value::String(refresh_token));
        }
        record.insert(
            "tokenTail".to_string(),
            Value::String(token_tail_of(&access_token)),
        );
        // 身份 / 展示字段：落盘键 ← 取值链（与 `credentials::pick` 同一套宽容读法，
        // 驼峰与下划线两种形态都认）。空值不落 —— 写空串等于把「上游没给」伪装成
        // 「给了一个空值」，展示名与续期合并都会据此走错分支。
        for (key, value) in [
            ("userId", credentials::pick(secret, &["userId", "user_id", "uid"])),
            ("username", credentials::pick(secret, &["username", "userName"])),
            ("nickname", credentials::pick(secret, &["nickname", "displayName"])),
            ("email", credentials::pick(secret, &["email"])),
            ("avatarUrl", credentials::pick(secret, &["avatarUrl", "avatar_url"])),
        ] {
            if !value.is_empty() {
                record.insert(key.to_string(), Value::String(value));
            }
        }
        record.insert("createdAt".to_string(), Value::from(created_at));
        if expires_in > 0 {
            record.insert("expiresIn".to_string(), Value::from(expires_in));
        }
        if expires_at > 0 {
            record.insert("expiresAt".to_string(), Value::from(expires_at));
        }
        record.insert("source".to_string(), Value::String(source.to_string()));
        record.insert("priority".to_string(), Value::from(priority));
        record.insert(
            "enabled".to_string(),
            Value::Bool(existing.as_ref().map(StoredAccount::enabled).unwrap_or(true)),
        );
        record.insert("addedAt".to_string(), Value::from(added_at));
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
                "✅ AtomCode 账号已保存: {record_name}（优先级 {priority}，令牌 …{}）",
                token_tail_of(&access_token)
            ),
        );
        Ok(self.public_account(&saved))
    }

    /// 续期结果回写（**比较-再写**：记录里的两个令牌仍是刷新前那份才写）。
    ///
    /// ⚠️ **只改四个字段**：access / refresh / tokenTail / expiresAt。
    /// 身份字段（`userId` / `username` / `nickname` / `createdAt` / `addedAt`）
    /// **绝不动** —— 它们是登录那一刻的形态，改它们等于让记录的「这是谁」漂移。
    pub fn update_atomcode_credentials_if_current(
        &self,
        expected: &Value,
        access_token: &str,
        refresh_token: &str,
        expires_at: i64,
    ) -> Result<CredentialWrite, AccountStoreError> {
        let id = expected.get("id").and_then(Value::as_str).unwrap_or("");
        let guard = self.guard();
        let Some(mut record) = self
            .record_by_id(&guard, id)
            .filter(|record| record.provider() == ATOMCODE_PROVIDER_ID)
        else {
            return Ok(CredentialWrite::Stale);
        };
        // 比较：只要记录里的 access / refresh 与调用方给的那份不一致就算「已被换掉」
        for key in ["accessToken", "refreshToken"] {
            if record.get(key).map(|value| value.as_str().unwrap_or(""))
                != expected.get(key).map(|value| value.as_str().unwrap_or(""))
            {
                return Ok(CredentialWrite::Stale);
            }
        }
        record.set("accessToken", Value::String(access_token.to_string()));
        if !refresh_token.trim().is_empty() {
            record.set("refreshToken", Value::String(refresh_token.trim().to_string()));
        }
        record.set("tokenTail", Value::String(token_tail_of(access_token)));
        if expires_at > 0 {
            record.set("expiresAt", Value::from(expires_at));
        }
        record.set_updated_at(logging::now_ms());
        self.with_conn(&guard, |conn| sql::update_in_place(conn, &record))?;
        Ok(CredentialWrite::Written)
    }

    /// AtomCode 账号的**公开形态**（见 [`public_shape`] 的说明）。
    pub fn to_atomcode_public_account(&self, record: &StoredAccount) -> Value {
        public_shape(record)
    }
}

/// 读一个秒数（`expiresIn`）—— 记录里的值可能是数字也可能是数字字符串
/// （粘贴来的 JSON 常见后者），两种都认。
fn credential_seconds(record: &Value, keys: &[&str]) -> i64 {
    for key in keys {
        if let Some(number) = record.get(*key).and_then(Value::as_i64) {
            if number > 0 {
                return number;
            }
        }
        if let Some(text) = record.get(*key).and_then(Value::as_str) {
            if let Ok(number) = text.trim().parse::<i64>() {
                if number > 0 {
                    return number;
                }
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record_of(map: Value) -> StoredAccount {
        StoredAccount::from_map(map.as_object().cloned().unwrap_or_default())
    }

    /// 公开形态必须把令牌本体与 refresh token 都摘掉，只留 `canRefresh`
    #[test]
    fn public_shape_never_leaks_tokens() {
        let record = record_of(json!({
            "id": "atomcode-42",
            "provider": ATOMCODE_PROVIDER_ID,
            "accessToken": "at",
            "refreshToken": "rt",
        }));
        let public = public_shape(&record);
        assert!(public.get("accessToken").is_none());
        assert!(public.get("refreshToken").is_none());
        assert_eq!(public.get("canRefresh"), Some(&Value::Bool(true)));
        assert_eq!(public.get("hasToken"), Some(&Value::Bool(true)));
    }

    /// `canRefresh` 只看 refreshToken 是否非空（本家续期只认它）
    #[test]
    fn can_refresh_requires_refresh_token() {
        assert!(can_refresh(&record_of(json!({"refreshToken": "rt"}))));
        assert!(!can_refresh(&record_of(json!({"refreshToken": "  "}))));
        assert!(!can_refresh(&record_of(json!({"accessToken": "at"}))));
    }

    /// 身份字段不能在往返中掉（续期与展示都靠它们）
    #[test]
    fn record_roundtrip_keeps_identity_fields() {
        let record = record_of(json!({
            "id": "atomcode-42",
            "provider": ATOMCODE_PROVIDER_ID,
            "userId": "42",
            "username": "qq_1",
            "email": "a@b.c",
            "expiresIn": 604800,
        }));
        assert_eq!(record.get("userId"), Some(&Value::String("42".into())));
        assert_eq!(record.get("username"), Some(&Value::String("qq_1".into())));
        assert_eq!(record.get("email"), Some(&Value::String("a@b.c".into())));
    }

    /// `expiresIn` 的数字字符串也要认（粘贴来的 JSON 常见这种形态）
    #[test]
    fn expires_in_accepts_numeric_strings() {
        assert_eq!(credential_seconds(&json!({"expiresIn": 600}), &["expiresIn"]), 600);
        assert_eq!(
            credential_seconds(&json!({"expiresIn": "600"}), &["expiresIn"]),
            600
        );
        assert_eq!(credential_seconds(&json!({"expiresIn": "junk"}), &["expiresIn"]), 0);
    }
}
