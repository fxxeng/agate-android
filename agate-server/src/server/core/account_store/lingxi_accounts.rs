//! WPS 灵犀账号：粘贴 `wps_sid` → 可落库记录 + 公开形态。
//!
//! ── 落盘字段 ────────────────────────────────────────────────
//! ```text
//!   accessToken  ← 整段 Cookie（或 `wps_sid=…`），转发层从会话形态读它
//!   wpsSid       ← 裸 sid（展示/校验用）
//!   uid          ← 整段 Cookie 里的 uid（没有则为空）
//!   tokenTail    ← sid 尾号
//! ```
//! ⚠️ 灵犀**没有 refresh 接口**（方案 L1）：本文件不做有效期推算、不写
//! `expiresAt`。死号只由上游 401 判定，失效后重新粘贴覆盖同一条记录。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic，不持锁做网络。

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::server::core::account_store::priority::next_free_priority;
use crate::server::core::account_store::sql;
use crate::server::core::account_store::state::{mark_name_custom, StoredAccount};
use crate::server::core::account_store::store::{AccountStore, AccountStoreError};
use crate::server::core::account_store::store_util::{token_tail_of, truncate_chars};
use crate::server::core::providers::{kind_id, ProviderKind};
use crate::server::logging;

const MAX_NAME_LENGTH: usize = 100;

pub const LINGXI_PROVIDER_ID: &str = kind_id(ProviderKind::Lingxi);

/// 身份串 → 稳定账号 id（摘要前 12 位，不带 Cookie 明文）。
fn account_id_for(identity: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"lingxi:account-id:v1\0");
    hasher.update(identity.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    format!("lingxi-{}", &hex[..12])
}

impl AccountStore {
    /// 取一条灵犀账号的**原始记录**（含 Cookie）。空 id → 队首启用账号。
    pub fn lingxi_account_record(&self, account_id: &str) -> Option<Value> {
        let guard = self.guard();
        if !account_id.is_empty() {
            let record = self.record_by_id(&guard, account_id)?;
            return (record.provider() == LINGXI_PROVIDER_ID).then(|| record.to_value());
        }
        let mut candidates: Vec<StoredAccount> = self
            .records_for_provider(&guard, LINGXI_PROVIDER_ID)
            .into_iter()
            .filter(StoredAccount::enabled)
            .collect();
        candidates.sort_by_key(StoredAccount::order_key);
        candidates.into_iter().next().map(|item| item.to_value())
    }

    /// 添加/更新一条灵犀账号。`credential` 是
    /// `providers::lingxi::credentials::normalize_paste` 的产物转成的 Value：
    /// `{ "wps_sid": "...", "cookie": "...", "uid": "..." }`。
    pub fn add_lingxi_account(
        &self,
        credential: &Value,
        name: Option<&str>,
        source: &str,
    ) -> Result<Value, AccountStoreError> {
        let sid = credential.get("wps_sid").and_then(Value::as_str).unwrap_or("").trim();
        if sid.is_empty() {
            return Err(AccountStoreError::new("缺少有效的 wps_sid（V02S 前缀）", 400));
        }
        let cookie = credential
            .get("cookie")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_string())
            .unwrap_or_else(|| format!("wps_sid={sid}"));
        let uid = credential.get("uid").and_then(Value::as_str).unwrap_or("").trim().to_string();
        // uid 优先（同一账号重复粘贴能覆盖而不是堆两条）；没有 uid 时用 sid 摘要
        let identity = if uid.is_empty() { format!("sid:{sid}") } else { format!("uid:{uid}") };
        let id = account_id_for(&identity);

        let guard = self.guard();
        let existing = self.record_by_id(&guard, &id);
        if let Some(existing) = existing.as_ref() {
            let provider = existing.provider();
            if provider != LINGXI_PROVIDER_ID {
                return Err(AccountStoreError::new(
                    format!("账号 id「{id}」已被{provider}账号占用（请先处理那个账号）"),
                    400,
                ));
            }
        }
        let explicit_name = name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| truncate_chars(value, MAX_NAME_LENGTH));
        let explicit = explicit_name.is_some();
        let record_name = explicit_name
            .or_else(|| {
                existing
                    .as_ref()
                    .map(StoredAccount::name)
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_else(|| {
                if uid.is_empty() {
                    "WPS 灵犀".to_string()
                } else {
                    format!("WPS 灵犀 {uid}")
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
        record.insert("provider".to_string(), Value::String(LINGXI_PROVIDER_ID.to_string()));
        record.insert("name".to_string(), Value::String(record_name.clone()));
        mark_name_custom(&mut record, explicit, existing.as_ref());
        record.insert("accessToken".to_string(), Value::String(cookie.clone()));
        record.insert("wpsSid".to_string(), Value::String(sid.to_string()));
        record.insert("uid".to_string(), Value::String(uid));
        record.insert("tokenTail".to_string(), Value::String(token_tail_of(sid)));
        record.insert("source".to_string(), Value::String(source.to_string()));
        record.insert("priority".to_string(), Value::from(priority));
        record.insert(
            "enabled".to_string(),
            Value::Bool(existing.as_ref().map(StoredAccount::enabled).unwrap_or(true)),
        );
        record.insert(
            "addedAt".to_string(),
            Value::from(existing.as_ref().map(StoredAccount::added_at).unwrap_or(now)),
        );
        record.insert("updatedAt".to_string(), Value::from(now));
        let mut merged = record;
        if let Some(existing) = existing.as_ref() {
            for (key, value) in existing.fields() {
                merged.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        let saved = StoredAccount::from_map(merged);
        self.with_conn(&guard, |conn| sql::put(conn, &saved))?;
        drop(guard);
        logging::log(
            "[Accounts]",
            &format!("✅ WPS 灵犀账号已保存: {record_name}（{id}，优先级 {priority}）"),
        );
        Ok(self.public_account(&saved))
    }

    /// 当日签到台账写在**账号记录自己的命名空间**里（照
    /// `put_codearts_welfare_ledger`：锁内现读整条、只写自己的键）。
    /// ⚠️ 不做「凭据没变才写」的比对 —— 那会让临期账号每次写都判 Stale。
    pub fn put_lingxi_checkin_ledger(
        &self,
        account_id: &str,
        ledger: &Value,
    ) -> Result<(), AccountStoreError> {
        let guard = self.guard();
        let Some(mut record) = self
            .record_by_id(&guard, account_id)
            .filter(|record| record.provider() == LINGXI_PROVIDER_ID)
        else {
            return Err(AccountStoreError::new(
                "WPS 灵犀账号已不存在，签到台账无处落盘",
                404,
            ));
        };
        record
            .fields_mut()
            .insert("lingxiCheckin".to_string(), ledger.clone());
        record.set_updated_at(logging::now_ms());
        self.with_conn(&guard, |conn| sql::update_in_place(conn, &record))?;
        Ok(())
    }

    pub fn lingxi_checkin_ledger(&self, account_id: &str) -> Option<Value> {
        self.lingxi_account_record(account_id)
            .and_then(|record| record.get("lingxiCheckin").cloned())
    }

    /// 公开形态：**绝不下发 Cookie / sid**（方案 L4 的凭据边界）。
    pub fn to_lingxi_public_account(&self, record: &StoredAccount) -> Value {
        let proxy = crate::server::core::proxies::describe_account_proxy(Some(&record.proxy()));
        let mut public = Map::new();
        public.insert("id".to_string(), Value::String(record.id().to_string()));
        public.insert("provider".to_string(), Value::String(record.provider()));
        public.insert("name".to_string(), Value::String(record.name()));
        public.insert(
            "uid".to_string(),
            Value::String(record.get("uid").and_then(Value::as_str).unwrap_or("").to_string()),
        );
        public.insert(
            "tokenTail".to_string(),
            Value::String(record.get("tokenTail").and_then(Value::as_str).unwrap_or("").to_string()),
        );
        public.insert("source".to_string(), Value::String(record.source()));
        public.insert("priority".to_string(), Value::from(record.priority()));
        public.insert("enabled".to_string(), Value::Bool(record.enabled()));
        public.insert("addedAt".to_string(), Value::from(record.added_at()));
        public.insert("updatedAt".to_string(), Value::from(record.updated_at()));
        public.insert("proxy".to_string(), proxy);
        public.insert(
            "rateLimits".to_string(),
            record.get("rateLimits").cloned().unwrap_or_else(|| Value::Object(Map::new())),
        );
        public.insert("available".to_string(), Value::Bool(true));
        Value::Object(public)
    }
}
