//! QuWork 账号：粘贴凭证 → 可落库记录；续期结果的比较-再写。
//!
//! ── 落盘字段 ────────────────────────────────────────────────
//! ```text
//!   accessToken   ← llmToken 的**镜像**（本仓转发语义：session_from_record 把
//!                   它放进 auth.accessToken，build_chat_request 从那里取）
//!   llmToken      模型凭证（quwork 内部名）
//!   loginToken    登录凭证（上游叫 accessToken；可选）
//!   user          {uid, username, nickname, phone} 标识
//!   expiresAt     登录凭证过期时刻（毫秒；可选）
//!   tokenTail     模型凭证尾号（展示）
//!   source        paste / manual
//! ```
//! ⚠️ 上游把登录凭证叫 `accessToken`，与本仓 `accessToken`（转发凭证）语义
//! 冲突。因此落库时把 `llmToken` 镜像到 `accessToken`，登录凭证另存
//! `loginToken`；公开视图两个都剥掉（见 `public_shape`）。
//!
//! ── 本文件全是读-改-写存储操作，没有任何网络请求（持锁不做网络）。
//! 写回用 `record_by_id` + `sql::update_in_place`（与 atomcode / kiro 同款），
//! **不经 `store.load()`**，无需处理 `Arc<AccountState>` 的 `(*state).clone()`。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::server::core::providers::quwork::credentials;
use crate::server::core::providers::ProviderKind;
use crate::server::logging;

use super::priority::next_free_priority;
use super::sql;
use super::state::{mark_name_custom, StoredAccount};
use super::store::{AccountStore, AccountStoreError};
use super::store_util::{token_tail_of, truncate_chars};
use super::CredentialWrite;

/// 本家的 provider id（从注册表推导，不写第二份字面量）
pub const QUWORK_PROVIDER_ID: &str =
    crate::server::core::providers::kind_id(ProviderKind::QuWork);

const MAX_NAME_LENGTH: usize = 100;

/// 公开形态里**必须**剥掉的键：落盘名、上游粘贴别名、以及嵌套凭证对象。
/// 漏一个就是把凭据随账号列表下发。
const SECRET_KEYS: &[&str] = &[
    "accessToken",
    "llmToken",
    "llm_token",
    "loginToken",
    "apiKey",
    "key",
    "token",
    "refreshToken",
    "refresh_token",
    "access_token",
    "secret",
    "session",
    "data",
    "credentials",
    "modelBearerToken",
    "model_bearer_token",
    "cookie",
    "cookieHeader",
    "modelFingerprint",
    "deviceIdentifier",
    "deviceUuid",
    "origin",
    "password",
];

/// 身份串 → 稳定的账号 id（摘要前 12 位，绝不带令牌明文）
fn account_id_for(identity: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"quwork:account-id:v1\0");
    hasher.update(identity.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    format!("quwork-{}", &hex[..12])
}

fn default_name(secret: &Value) -> String {
    let view = credentials::public_view(secret);
    let label = view
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("QuWork");
    format!("QuWork {label}")
}

impl AccountStore {
    /// 取一条 QuWork 账号记录（**原始 JSON**，含凭据，供协议层用；不是公开形态）。
    ///
    /// `account_id` 非空 → 按 id 直查（不属于本家返回 None）；为空 → 启用中
    /// 优先级最靠前的一条（转发默认用队首账号）。
    pub fn quwork_account_record(&self, account_id: &str) -> Option<Value> {
        let guard = self.guard();
        if !account_id.is_empty() {
            let record = self.record_by_id(&guard, account_id)?;
            return (record.provider() == QUWORK_PROVIDER_ID).then(|| record.to_value());
        }
        let mut candidates: Vec<StoredAccount> = self
            .records_for_provider(&guard, QUWORK_PROVIDER_ID)
            .into_iter()
            .filter(StoredAccount::enabled)
            .collect();
        candidates.sort_by_key(StoredAccount::order_key);
        candidates.into_iter().next().map(|item| item.to_value())
    }

    /// 添加/更新一个 QuWork 账号（粘贴凭证的唯一落账号入口；`secret` 已由
    /// `quwork::credentials::normalize_paste` 校验过 llmToken）。
    pub fn add_quwork_account(
        &self,
        secret: &Value,
        name: Option<&str>,
        source: &str,
    ) -> Result<Value, AccountStoreError> {
        let llm = credentials::text(secret, &["llmToken"]);
        if llm.is_empty() {
            return Err(AccountStoreError::new("缺少 QuWork 模型凭证 llmToken", 400));
        }
        let login = credentials::login_token_of(secret);
        let identity = credentials::identity(secret);
        if identity.is_empty() {
            return Err(AccountStoreError::new("无法识别这个 QuWork 账号", 400));
        }
        let id = account_id_for(&identity);

        let guard = self.guard();
        let existing = self.record_by_id(&guard, &id);
        if let Some(existing) = existing.as_ref() {
            let provider = existing.provider();
            if provider != QUWORK_PROVIDER_ID {
                return Err(AccountStoreError::new(
                    format!(
                        "账号 id「{id}」已被{provider}账号占用，无法添加同一个身份的 QuWork 账号\
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
            .unwrap_or_else(|| default_name(secret));
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
            Value::String(QUWORK_PROVIDER_ID.to_string()),
        );
        record.insert("name".to_string(), Value::String(record_name.clone()));
        mark_name_custom(
            &mut record,
            name.is_some_and(|value| !value.trim().is_empty()),
            existing.as_ref(),
        );
        // 转发凭证镜像（本仓语义）+ quwork 内部名
        record.insert("accessToken".to_string(), Value::String(llm.clone()));
        record.insert("llmToken".to_string(), Value::String(llm.clone()));
        record.insert("tokenTail".to_string(), Value::String(token_tail_of(&llm)));
        if !login.is_empty() {
            record.insert("loginToken".to_string(), Value::String(login.clone()));
        }
        if let Some(user) = secret.get("user").filter(|value| value.is_object()) {
            record.insert("user".to_string(), user.clone());
        }
        if let Some(expires) = secret.get("expiresAt").filter(|value| value.is_number()) {
            record.insert("expiresAt".to_string(), expires.clone());
        }
        // 设备标识：上游远程兑换码激活要带 `device_identifier`（16 位订单号那条
        // 路不带）。用户能粘就原样留着 —— 不存下来，将来接兑换时只能让他重粘。
        for key in ["deviceIdentifier", "deviceUuid"] {
            if let Some(value) = secret.get(key).filter(|value| value.is_string()) {
                record.insert(key.to_string(), value.clone());
            }
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
        // 未知字段全量保留（用户手工加过的字段不能丢），但新写的凭据键优先
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
                "✅ QuWork 账号已保存: {record_name}（优先级 {priority}，{}，令牌 {}）",
                if login.is_empty() { "仅模型凭证" } else { "含登录凭证" },
                token_tail_of(&llm)
            ),
        );
        Ok(self.public_account(&saved))
    }

    /// QuWork 账号的**公开形态**（进 HTTP 响应）：剥光凭据，只留标识与能力位。
    pub fn to_quwork_public_account(&self, record: &StoredAccount) -> Value {
        public_shape(record)
    }

    /// 续期结果回写（**比较-再写**：记录里的三件凭证仍是刷新前那份才写）。
    ///
    /// 只改 `llmToken` / `accessToken`(镜像) / `loginToken` / `tokenTail` /
    /// `expiresAt` / `updatedAt`，身份字段绝不动。
    pub fn update_quwork_credentials_if_current(
        &self,
        expected: &Value,
        next: &Value,
    ) -> Result<CredentialWrite, AccountStoreError> {
        let id = expected.get("id").and_then(Value::as_str).unwrap_or("");
        let guard = self.guard();
        let Some(mut record) = self
            .record_by_id(&guard, id)
            .filter(|record| record.provider() == QUWORK_PROVIDER_ID)
        else {
            return Ok(CredentialWrite::Stale);
        };
        for key in ["llmToken", "loginToken", "expiresAt"] {
            if record.get(key) != expected.get(key) {
                return Ok(CredentialWrite::Stale);
            }
        }
        let llm = credentials::text(next, &["llmToken"]);
        let login = credentials::login_token_of(next);
        record.set("llmToken", Value::String(llm.clone()));
        record.set("accessToken", Value::String(llm.clone()));
        record.set("tokenTail", Value::String(token_tail_of(&llm)));
        if !login.is_empty() {
            record.set("loginToken", Value::String(login));
        }
        if let Some(expires) = next.get("expiresAt").filter(|value| value.is_number()) {
            record.set("expiresAt", expires.clone());
        }
        record.set_updated_at(logging::now_ms());
        self.with_conn(&guard, |conn| sql::update_in_place(conn, &record))?;
        Ok(CredentialWrite::Written)
    }
}

/// 公开形态的实体（自由函数，方便单测，不必造 AccountStore）
fn public_shape(record: &StoredAccount) -> Value {
    let raw = record.to_value();
    let view = credentials::public_view(&raw);
    let mut value = raw;
    if let Some(object) = value.as_object_mut() {
        for key in SECRET_KEYS {
            object.remove(*key);
        }
        // 嵌套对象里的凭据键也要剥（粘贴整段 JSON 时它们会作为未知字段留下）
        for nested_key in ["secret", "session", "data", "credentials", "user"] {
            if let Some(nested) = object.get_mut(nested_key).and_then(Value::as_object_mut) {
                for key in SECRET_KEYS {
                    nested.remove(*key);
                }
            }
        }
        // 能力位 / 展示位（public_view 输出的非凭据字段）；name 保留备注名
        if let Some(fields) = view.as_object() {
            for (key, field) in fields {
                if key != "name" {
                    object.insert(key.clone(), field.clone());
                }
            }
        }
        object.insert("available".to_string(), Value::Bool(record.enabled()));
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn account_ids_are_stable_and_opaque() {
        let id = account_id_for("quwork:u-1");
        assert!(id.starts_with("quwork-"));
        assert_eq!(id.len(), 19, "quwork- + 12 位摘要");
        assert_eq!(id, account_id_for("quwork:u-1"));
        assert_ne!(id, account_id_for("quwork:u-2"));
    }

    #[test]
    fn the_public_shape_never_carries_a_credential() {
        let record = StoredAccount::from_map(
            json!({
                "id": "quwork-abcdef123456",
                "provider": QUWORK_PROVIDER_ID,
                "name": "QuWork u-1",
                "accessToken": "llm-secret",
                "llmToken": "llm-secret",
                "loginToken": "login-secret",
                "tokenTail": "secret",
                "user": { "uid": "u-1", "phone": "138" },
                "expiresAt": 1_800_000_000_000_i64,
                // 粘贴整段 JSON 时作为未知字段留下来的嵌套形态
                "secret": { "llmToken": "llm-secret", "accessToken": "login-secret" },
                "credentials": "{\"llmToken\":\"llm-secret\"}",
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        );
        let public = public_shape(&record);
        for key in ["accessToken", "llmToken", "loginToken", "secret", "credentials"] {
            assert!(public.get(key).is_none(), "公开形态漏了 {key}：{public}");
        }
        assert_eq!(public.get("hasToken"), Some(&Value::Bool(true)));
        assert_eq!(public.get("uid").and_then(Value::as_str), Some("u-1"));
        assert_eq!(public.get("phone").and_then(Value::as_str), Some("138"));
        assert_eq!(public.get("name").and_then(Value::as_str), Some("QuWork u-1"));
    }
}
