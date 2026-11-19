//! LobsterAI 账号：**只能由浏览器登录落账号**（粘贴一律拒绝），落盘要带齐续期载荷件。
//!
//! ── 账号字段 ────────────────────────────────────────────────
//! ```text
//!   { id: "lobsterai-<身份>", provider: "lobsterai",
//!     name, accessToken, refreshToken, tokenTail, uid, userId,
//!     uuid, firstKeyfrom, latestKeyfrom, expiresIn, expiresAt,
//!     source, priority, enabled, addedAt, updatedAt }
//! ```
//!
//! ── 为什么这一家必须存「三件载荷件 + refresh token」───────────────
//! 上游的续期接口不认「只给 refresh token」：它要 `uuid` + `firstKeyfrom` +
//! `latestKeyfrom` + `version`（见 `providers/lobsterai/credentials.rs` 的模块头）。
//! 这三件只在**换码那一刻**由上游下发，此后拿不到 —— 只存两个 token 的账号，看起来
//! 能用到 access 过期，之后既续不了期也修不回来（只能重新登录）。所以落盘时
//! **必须把它们一起存下来**。
//!
//! ── 为什么粘贴入口一律 400（不是界面漏做）──────────────────────
//! 同样因为那三件载荷件：用户从别处粘不出它们。与其落一条看着正常、到期就废的记录，
//! 不如现在就说清只有网页登录一条路 —— 与上游插件同口径。
//!
//! ── 身份（去重）─────────────────────────────────────────────
//! `userId → uid → 令牌尾段`（与 `providers/lobsterai/credentials.rs::identity`
//! **逐字一致**）：两处分叉会让「同一个账号重新登录」变成「新增一条记录」。
//!
//! 本文件全是「读-改-写」存储操作，**没有任何网络请求**（持锁不做网络）。
//! 绝不 unwrap/expect（release 是 panic=abort）。

use serde_json::{Map, Value};

use crate::server::core::providers::lobsterai::oauth::TokenBundle;
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

/// LobsterAI 的 provider id（引用注册表，避免各处再写一份字面量）
pub const LOBSTERAI_PROVIDER_ID: &str = kind_id(ProviderKind::LobsterAi);

impl AccountStore {
    /// 取一条 LobsterAI 账号记录（公开形态的原始 JSON）。
    ///
    /// `account_id` 非空 → 按 id 直查（不属于本家返回 None）；
    /// 为空 → 本家**启用中**账号里优先级最靠前的一条（转发默认用队首账号）。
    pub fn lobsterai_account_record(&self, account_id: &str) -> Option<Value> {
        let guard = self.guard();
        if !account_id.is_empty() {
            let record = self.record_by_id(&guard, account_id)?;
            return (record.provider() == LOBSTERAI_PROVIDER_ID).then(|| record.to_value());
        }
        let mut candidates: Vec<StoredAccount> = self
            .records_for_provider(&guard, LOBSTERAI_PROVIDER_ID)
            .into_iter()
            .filter(StoredAccount::enabled)
            .collect();
        candidates.sort_by_key(StoredAccount::order_key);
        candidates.into_iter().next().map(|item| item.to_value())
    }

    /// 添加 / 更新一条 LobsterAI 账号（**唯一入口 = 网页登录换码成功后**）。
    ///
    /// 调用方必须先完成换码（`providers::lobsterai::oauth::exchange`），并把这一轮
    /// 生成的三件载荷件（`uuid` / 两个 keyfrom）一起传进来 —— 它们与令牌同样重要
    /// （见模块头）。
    ///
    /// 同一身份的记录就地更新（保留优先级与启用状态、沿用用户改过的备注名）。
    pub fn add_lobsterai_account(
        &self,
        bundle: &TokenBundle,
        installation_uuid: &str,
        keyfrom: &str,
        source: &str,
    ) -> Result<Value, AccountStoreError> {
        let access_token = bundle.access_token.trim();
        if access_token.is_empty() {
            return Err(AccountStoreError::new("缺少 LobsterAI 访问令牌", 400));
        }
        if access_token.chars().count() > MAX_TOKEN_LENGTH {
            return Err(AccountStoreError::new("LobsterAI 访问令牌过长", 400));
        }
        let installation_uuid = installation_uuid.trim();
        let keyfrom = keyfrom.trim();
        if installation_uuid.is_empty() || keyfrom.is_empty() {
            // 载荷件缺失 = 续期无望，如实拒绝（见模块头）
            return Err(AccountStoreError::new(
                "LobsterAI 落账号缺少续期载荷件（uuid / keyfrom），无法保存",
                400,
            ));
        }
        // 身份口径与 `credentials::identity` 逐字一致（两处分叉会重复加号）
        let identity = {
            let user_id = bundle.user_id.trim();
            if !user_id.is_empty() {
                user_id.to_string()
            } else {
                let uid = bundle.uid.trim();
                if !uid.is_empty() {
                    uid.to_string()
                } else {
                    let chars: Vec<char> = access_token.chars().collect();
                    let start = chars.len().saturating_sub(12);
                    format!("tok{}", chars[start..].iter().collect::<String>())
                }
            }
        };
        let id = format!("lobsterai-{identity}");

        let guard = self.guard();
        let existing = self.record_by_id(&guard, &id);
        if let Some(existing) = existing.as_ref() {
            if existing.provider() != LOBSTERAI_PROVIDER_ID {
                return Err(AccountStoreError::new(
                    format!("账号 id「{id}」已被其它 provider 占用，无法添加这个 LobsterAI 账号"),
                    400,
                ));
            }
        }
        let record_name = existing
            .as_ref()
            .map(StoredAccount::name)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                crate::server::core::providers::lobsterai::credentials::display_name(
                    &bundle.nickname,
                    &bundle.uid,
                    &id,
                )
            });
        let record_name = truncate_text(&record_name, MAX_NAME_LENGTH);
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
            Value::String(LOBSTERAI_PROVIDER_ID.to_string()),
        );
        record.insert("name".to_string(), Value::String(record_name.clone()));
        mark_name_custom(&mut record, false, existing.as_ref());
        record.insert(
            "accessToken".to_string(),
            Value::String(access_token.to_string()),
        );
        // 续期令牌非空才写（有些响应不带它 —— 那种账号只能重新登录，但不该把
        // 上一次的好值洗掉）
        if !bundle.refresh_token.trim().is_empty() {
            record.insert(
                "refreshToken".to_string(),
                Value::String(bundle.refresh_token.trim().to_string()),
            );
        }
        record.insert("tokenTail".to_string(), Value::String(token_tail_of(access_token)));
        // 三件载荷件（续期的全部前提，见模块头）
        record.insert(
            "uuid".to_string(),
            Value::String(installation_uuid.to_string()),
        );
        record.insert("firstKeyfrom".to_string(), Value::String(keyfrom.to_string()));
        record.insert("latestKeyfrom".to_string(), Value::String(keyfrom.to_string()));
        if !bundle.uid.trim().is_empty() {
            record.insert("uid".to_string(), Value::String(bundle.uid.trim().to_string()));
        }
        if !bundle.user_id.trim().is_empty() {
            record.insert(
                "userId".to_string(),
                Value::String(bundle.user_id.trim().to_string()),
            );
        }
        if bundle.expires_in > 0 {
            record.insert("expiresIn".to_string(), Value::from(bundle.expires_in));
            record.insert(
                "expiresAt".to_string(),
                Value::from(now + bundle.expires_in * 1000),
            );
        }
        record.insert("source".to_string(), Value::String(source.to_string()));
        record.insert("priority".to_string(), Value::from(priority));
        record.insert(
            "enabled".to_string(),
            Value::Bool(existing.as_ref().map(StoredAccount::enabled).unwrap_or(true)),
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
                "✅ LobsterAI 账号已保存: {record_name}（优先级 {priority}，令牌 …{}）",
                token_tail_of(access_token)
            ),
        );
        Ok(self.public_account(&saved))
    }

    /// 续期结果回写（**比较-再写**：记录里的两个令牌仍是刷新前那份才写）。
    ///
    /// ⚠️ **只改三个字段**：access / refresh / expiresAt。三件载荷件与身份字段
    /// **绝不动** —— 它们是登录那一刻的形态（见模块头）。
    pub fn update_lobsterai_credentials_if_current(
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
            .filter(|record| record.provider() == LOBSTERAI_PROVIDER_ID)
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

    /// LobsterAI 账号的**公开形态**：摘掉令牌本体与全部载荷件，只留「能不能续期」。
    ///
    /// `refreshToken` 与三件载荷件**都要摘干净**：前者是长期有效的续期凭据，后者是
    /// 续期的身份字段 —— 一起下发等于把「这个账号能被别人续期」的条件送出去。界面
    /// 需要的「能不能续期」由 `canRefresh` 布尔表达，不给原件。
    pub fn to_lobsterai_public_account(&self, record: &StoredAccount) -> Value {
        let can_refresh = record
            .get("refreshToken")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
            && record
                .get("firstKeyfrom")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
            && record
                .get("latestKeyfrom")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty());
        let mut value = record.to_value();
        if let Some(object) = value.as_object_mut() {
            for key in [
                "accessToken",
                "refreshToken",
                "uuid",
                "firstKeyfrom",
                "latestKeyfrom",
            ] {
                object.remove(key);
            }
            object.insert("available".to_string(), Value::Bool(record.enabled()));
            object.insert("hasToken".to_string(), Value::Bool(true));
            object.insert("hasSession".to_string(), Value::Bool(true));
            // 只为续期那条路准备的插件记录不带 pluginSecret 时才走这里
            object.insert("canRefresh".to_string(), Value::Bool(can_refresh));
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    /// 公开形态必须把**令牌本体 + 载荷件**都摘掉（只留一个 canRefresh 布尔）
    #[test]
    fn public_shape_never_leaks_tokens_or_payload_fields() {
        let record = StoredAccount::from_map({
            let mut map = Map::new();
            map.insert("id".into(), Value::String("lobsterai-42".into()));
            map.insert("provider".into(), Value::String(LOBSTERAI_PROVIDER_ID.into()));
            map.insert("accessToken".into(), Value::String("at".into()));
            map.insert("refreshToken".into(), Value::String("rt".into()));
            map.insert("uuid".into(), Value::String("u-1".into()));
            map.insert("firstKeyfrom".into(), Value::String("fk".into()));
            map.insert("latestKeyfrom".into(), Value::String("lk".into()));
            map
        });
        let can_refresh = record
            .get("refreshToken")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
            && record
                .get("firstKeyfrom")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
            && record
                .get("latestKeyfrom")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty());
        let mut public = record.to_value();
        if let Some(object) = public.as_object_mut() {
            for key in [
                "accessToken",
                "refreshToken",
                "uuid",
                "firstKeyfrom",
                "latestKeyfrom",
            ] {
                object.remove(key);
            }
            object.insert("canRefresh".into(), Value::Bool(can_refresh));
        }
        for key in [
            "accessToken",
            "refreshToken",
            "uuid",
            "firstKeyfrom",
            "latestKeyfrom",
        ] {
            assert!(public.get(key).is_none(), "{key} 不该出现在公开形态里");
        }
        assert_eq!(public.get("canRefresh"), Some(&Value::Bool(true)));
    }

    /// 记录里的载荷件不能在往返中掉字段（续期全靠它们）
    #[test]
    fn record_roundtrip_keeps_payload_fields() {
        let map = serde_json::json!({
            "id": "lobsterai-42",
            "provider": LOBSTERAI_PROVIDER_ID,
            "firstKeyfrom": "fk",
            "latestKeyfrom": "lk",
        });
        let record = StoredAccount::from_map(map.as_object().cloned().unwrap_or_default());
        assert_eq!(
            record.get("firstKeyfrom"),
            Some(&Value::String("fk".into()))
        );
        assert_eq!(
            record.get("latestKeyfrom"),
            Some(&Value::String("lk".into()))
        );
    }
}