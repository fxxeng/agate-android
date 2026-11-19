//! AStudio 账号：粘贴 modelBearerToken / astron-session.json → 可落库账号。
//!
//! ── 落盘字段 ────────────────────────────────────────────────
//! ```text
//!   key                     modelBearerToken（调模型；**没有续期**）
//!   uid                     账号 uid（展示 + 去重首选）
//!   session                 { token, accountId, ssoSessionId? }（网页会话）
//!   tokenTail               令牌尾号（备注名兜底 / 排障认人）
//!   source                  paste / login
//! ```
//!
//! ── 身份三级判定，顺序不能换 ────────────────────────────────
//! ```text
//!   1  uid           人身上的标识，换令牌不换人
//!   2  accountId     网页会话的账号 id（有会话但没 uid 时）
//!   3  令牌摘要      只有 modelBearerToken 的账号只能这样认
//! ```
//! 判据实现复用 `providers::astudio::credentials::identity` —— 那个函数同时被
//! `account_transfer::identity` 用（导入判重）。两处各写一份必然分叉，症状是
//! 「同一个账号导两次多出一条」。
//!
//! ── 为什么这里没有「回写」函数 ──────────────────────────────
//! Kiro 之所以有 `update_kiro_credentials_if_current`，是因为它会续期、续期
//! 结果要写回。本家**没有续期接口**：凭据一旦落库就不再变，所以不需要那套
//! 「比较-再写」。将来若接上登录页，登录走的是「添加账号」这条路（身份命中
//! 就地更新），也不需要单独的写回。
//!
//! 本文件全是「读-改-写」存储操作，**没有任何网络请求**（持锁不做网络）。
//! 绝不 unwrap/expect（release 是 panic=abort）。

use serde_json::{Map, Value};

use crate::server::core::providers::astudio;
use crate::server::core::providers::astudio::credentials::{self, Credentials};
use crate::server::logging;

use super::priority::next_free_priority;
use super::sql;
use super::state::{mark_name_custom, StoredAccount};
use super::store::{AccountStore, AccountStoreError};
use super::store_util::{token_tail_of, truncate_chars};

/// 本家的 provider id
pub const ASTUDIO_PROVIDER_ID: &str = astudio::ID;

/// 备注名 / 令牌的长度上限（与别家同一口径）
const MAX_NAME_LENGTH: usize = 100;
const MAX_TOKEN_LENGTH: usize = 32_768;

/// 公开形态里**必须**剥掉的键（漏一个就是把凭据随列表下发）。
///
/// 三种形态都要覆盖：本项目落盘名（`key`）、桌面端 session 文件里的原名
/// （`modelBearerToken` / `token` / `ssoSessionId`）、以及粘贴时可能带的别名。
/// 只删 `key` 是不够的 —— 粘贴整段 `astron-session.json` 时，`token` 会作为
/// **未知字段**被原样保留（见 `add_astudio_account` 的「未知字段全量保留」），
/// 那串就是网页会话的凭据。
///
/// 末几个（`refreshToken` / `cookie` / `cookieHeader`）本家当前**用不到**，
/// 留着是防「哪天桌面端文件里多一个键，我们的列表接口就把它整串下发」——
/// 剥错一个非凭据键的代价是界面少一行字，漏剥的代价是凭据泄漏。
const SECRET_KEYS: &[&str] = &[
    "key",
    "modelBearerToken",
    "model_bearer_token",
    "apiKey",
    "token",
    "sessionToken",
    "ssoSessionId",
    "sso_sessionid",
    "refreshToken",
    "refresh_token",
    "cookie",
    "cookieHeader",
];

/// 身份串 → 稳定的账号 id
fn account_id_for(identity: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"astudio:account-id:v1\0");
    hasher.update(identity.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    format!("astudio-{}", &hex[..12])
}

/// 备注名兜底：先试能看懂的（uid / accountId），最后退到令牌尾号。
fn default_name(credentials: &Credentials) -> String {
    let label = if !credentials.uid.is_empty() {
        credentials.uid.clone()
    } else if !credentials.account_id.is_empty() {
        credentials.account_id.clone()
    } else {
        token_tail_of(&credentials.key)
    };
    if label.is_empty() {
        "AStudio 账号".to_string()
    } else {
        format!("AStudio {label}")
    }
}

impl AccountStore {
    /// 取一条 AStudio 账号记录（原始 JSON）。
    ///
    /// `account_id` 非空 → 按 id 直查；为空 → 启用中优先级最靠前的一条。
    pub fn astudio_account_record(&self, account_id: &str) -> Option<Value> {
        let guard = self.guard();
        if !account_id.is_empty() {
            let record = self.record_by_id(&guard, account_id)?;
            return (record.provider() == ASTUDIO_PROVIDER_ID).then(|| record.to_value());
        }
        let mut candidates: Vec<StoredAccount> = self
            .records_for_provider(&guard, ASTUDIO_PROVIDER_ID)
            .into_iter()
            .filter(StoredAccount::enabled)
            .collect();
        candidates.sort_by_key(StoredAccount::order_key);
        candidates.into_iter().next().map(|item| item.to_value())
    }

    /// 添加/更新一个 AStudio 账号。
    ///
    /// 同一身份就地更新（保留优先级、启用状态与用户改过的备注名）；未知字段
    /// 全量保留；撞到别的 provider 的 id 时报错而不是覆写。
    ///
    /// `payload` 应当已经过 `credentials::normalize_paste`（把整段 session 文件 /
    /// 裸令牌 / `Bearer ` 外壳归一）—— 这一步留在 API 层，因为核心里还有别的
    /// 调用方（导入）会自己归一，放两处只会让两处规则慢慢分叉。
    pub fn add_astudio_account(
        &self,
        payload: &Value,
        name: Option<&str>,
        source: &str,
    ) -> Result<Value, AccountStoreError> {
        let credentials = Credentials::from_fields(payload)
            .map_err(|error| AccountStoreError::new(error.message, 400))?;
        if credentials.key.chars().count() > MAX_TOKEN_LENGTH {
            return Err(AccountStoreError::new("AStudio 模型凭据过长", 400));
        }
        if credentials.token.chars().count() > MAX_TOKEN_LENGTH {
            return Err(AccountStoreError::new("AStudio 网页会话令牌过长", 400));
        }
        // 被上游标记封禁的账号收进来只会让人以为是我们坏了：明确拒绝。
        // 上游 `init-app` 会回 `{banned: true}`，用户若是把那份 JSON 整段粘进来
        // 就带着它（见 `credentials::normalize_paste` 的保留未知字段）。
        if payload.get("banned").and_then(Value::as_bool) == Some(true) {
            return Err(AccountStoreError::new(
                "这个 AStudio 账号已被上游标记封禁，无法添加",
                400,
            ));
        }
        let identity = credentials::identity(payload);
        if identity.is_empty() {
            return Err(AccountStoreError::new("无法识别这个 AStudio 账号", 400));
        }
        let id = account_id_for(&identity);

        let guard = self.guard();
        let existing = self.record_by_id(&guard, &id);
        if let Some(existing) = existing.as_ref() {
            let provider = existing.provider();
            if provider != ASTUDIO_PROVIDER_ID {
                return Err(AccountStoreError::new(
                    format!(
                        "账号 id「{id}」已被{provider}账号占用，无法添加同一个身份的 AStudio 账号\
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
            .unwrap_or_else(|| default_name(&credentials));
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
            Value::String(ASTUDIO_PROVIDER_ID.to_string()),
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
            Value::String(token_tail_of(&credentials.key)),
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
        // 未知字段全量保留（⚠️ 也包含粘贴带进来的 `token` 等，公开形态会剥掉）
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
                "✅ AStudio 账号已保存: {record_name}（优先级 {priority}，{}，令牌 {}）",
                if credentials.has_session() {
                    "含网页会话"
                } else {
                    "仅模型凭据"
                },
                credentials.masked()
            ),
        );
        Ok(self.public_account(&saved))
    }

    /// AStudio 账号的**公开形态**（进 HTTP 响应）：剥掉模型凭据与网页会话令牌。
    ///
    /// 主体在自由函数 [`public_shape`] 里（本方法只是让调用点与其他家同形）。
    pub fn to_astudio_public_account(&self, record: &StoredAccount) -> Value {
        public_shape(record)
    }
}

/// 公开形态的实体（见 [`AccountStore::to_astudio_public_account`]）。
///
/// 保留 `session.accountId`：它不是凭据，界面要靠它显示「是哪个账号」。
/// 顶层与嵌套两处都剥 —— 见 [`SECRET_KEYS`] 的说明。
///
/// 做成**自由函数**而不是方法：`&self` 在这里一点用都没有，留着只会让单测
/// 被迫造一个 `AccountStore`（同 `atomcode_accounts::public_shape`）。
fn public_shape(record: &StoredAccount) -> Value {
    let credentials = Credentials::from_fields(&record.to_value()).unwrap_or_default();
    let mut value = record.to_value();
    let has_key = !credentials.key.is_empty();
    let has_session = credentials.has_session();
    if let Some(object) = value.as_object_mut() {
        for key in SECRET_KEYS {
            object.remove(*key);
        }
        if let Some(session) = object.get_mut("session").and_then(Value::as_object_mut) {
            for key in [
                "token",
                "sessionToken",
                "ssoSessionId",
                "sso_sessionid",
                "refreshToken",
                "refresh_token",
            ] {
                session.remove(key);
            }
            if session.is_empty() {
                object.remove("session");
            }
        }
        object.insert("available".to_string(), Value::Bool(record.enabled()));
        object.insert("hasToken".to_string(), Value::Bool(has_key));
        // 「有网页会话」与「能续期」是两件事：本家永远不能续期，
        // 所以只回答会话这一半（界面据此决定要不要显示「余额」入口）
        object.insert("hasSession".to_string(), Value::Bool(has_session));
        object.insert("noSession".to_string(), Value::Bool(!has_session));
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 身份三级：uid 优先，其次 accountId，最后令牌摘要
    #[test]
    fn identity_prefers_the_uid_then_the_account() {
        let both = json!({
            "uid": "u-1",
            "session": { "token": "t", "accountId": "a-1" },
            "key": "mb-1"
        });
        assert_eq!(credentials::identity(&both), "uid:u-1");
        let mut rotated = both.clone();
        rotated["key"] = json!("mb-2");
        assert_eq!(
            credentials::identity(&both),
            credentials::identity(&rotated),
            "换 modelBearerToken 不该产生新账号"
        );
        let no_uid = json!({ "session": { "token": "t", "accountId": "a-1" }, "key": "mb-1" });
        assert_eq!(credentials::identity(&no_uid), "account:a-1");
        let key_only = json!({ "key": "mb-1" });
        assert!(credentials::identity(&key_only).starts_with("key:"));
    }

    /// id 稳定、且不泄漏令牌明文
    #[test]
    fn account_ids_are_stable_and_opaque() {
        let id = account_id_for("uid:10086");
        assert!(id.starts_with("astudio-"), "{id}");
        assert_eq!(id.len(), 20, "astudio- + 12 位摘要");
        assert_eq!(id, account_id_for("uid:10086"));
        assert_ne!(id, account_id_for("uid:10087"));
        assert!(!id.contains("10086"), "id 里不该出现身份明文：{id}");
    }

    /// 公开形态：连粘贴整段 `astron-session.json` 带进来的键也要剥干净
    #[test]
    fn the_public_shape_never_carries_a_credential() {
        let record = StoredAccount::from_map(
            json!({
                "id": "astudio-abcdef123456",
                "provider": ASTUDIO_PROVIDER_ID,
                "name": "AStudio u-1",
                "uid": "u-1",
                // 落盘形态
                "key": "mb-1",
                "session": { "token": "t-1", "accountId": "a-1", "ssoSessionId": "s-1" },
                // 粘贴整段文件时作为**未知字段**留下来的那些
                "modelBearerToken": "mb-1",
                "token": "t-1",
                "ssoSessionId": "s-1",
                "refreshToken": "rt-1",
                "cookie": "token=t-1",
                "cookieHeader": "token=t-1",
                "banned": false,
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        );
        let public = public_shape(&record);
        for key in [
            "key",
            "modelBearerToken",
            "token",
            "ssoSessionId",
            "refreshToken",
            "cookie",
            "cookieHeader",
        ] {
            assert!(public.get(key).is_none(), "公开形态漏了 {key}：{public}");
        }
        // 会话留着（只留 accountId）——「是哪个账号」界面要用
        assert_eq!(
            public.pointer("/session/accountId").and_then(Value::as_str),
            Some("a-1")
        );
        assert!(public.pointer("/session/token").is_none());
        assert_eq!(public.get("hasToken"), Some(&Value::Bool(true)));
        assert_eq!(public.get("hasSession"), Some(&Value::Bool(true)));
        assert_eq!(public.get("noSession"), Some(&Value::Bool(false)));
    }

    /// 只有模型凭据：如实标 `noSession`，不留空 `session` 对象
    #[test]
    fn a_key_only_account_is_marked_without_a_session() {
        let record = StoredAccount::from_map(
            json!({
                "id": "astudio-abcdef123456",
                "provider": ASTUDIO_PROVIDER_ID,
                "key": "mb-1",
                "enabled": false,
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        );
        let public = public_shape(&record);
        assert!(public.get("session").is_none(), "不该留空会话对象");
        assert_eq!(public.get("hasToken"), Some(&Value::Bool(true)));
        assert_eq!(public.get("hasSession"), Some(&Value::Bool(false)));
        assert_eq!(public.get("noSession"), Some(&Value::Bool(true)));
        // `available` 跟着启用状态走（界面用它画开关）
        assert_eq!(public.get("available"), Some(&Value::Bool(false)));
    }

    /// 备注名兜底顺序：uid → accountId → 令牌尾号
    #[test]
    fn the_default_name_uses_the_most_readable_label() {
        let named = Credentials {
            key: "mb-1234".to_string(),
            uid: "u-9".to_string(),
            ..Credentials::default()
        };
        assert_eq!(default_name(&named), "AStudio u-9");
        let account = Credentials {
            key: "mb-1234".to_string(),
            account_id: "a-9".to_string(),
            ..Credentials::default()
        };
        assert_eq!(default_name(&account), "AStudio a-9");
        let token_only = Credentials {
            key: "mb-1234".to_string(),
            ..Credentials::default()
        };
        assert!(default_name(&token_only).starts_with("AStudio "));
        assert!(!default_name(&Credentials::default()).is_empty());
    }
}
