//! Qoder 系账号（`qoder` / `qoder-intl` / `qwenwork`）以 provider（站点身份）+
//! userId 识别；凭证与其它提供商共享存储键名。
//!
//! ── 拆家（2026-10）→ 三家（2026-11）──────────────────────────
//! 每个站点是**一家 provider**（`qoder` 中国版 / `qoder-intl` 国际版 /
//! `qwenwork` 千问办公国内版），站点由 provider id 推导
//! （`Site::from_provider_id`），不再从账号的 `mode` 字段猜。
//! 记录里仍写 `mode`（凭证落盘的既有字段，公开形态的 `edition` 也由它派生），
//! 但**归属判定只认 provider id** —— 与 AutoClaw / Accio / ZCode 同一口径。

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::server::core::providers::qoder::credentials::Credentials;
use crate::server::core::providers::qoder::endpoints::Site;
use crate::server::logging;

use super::priority::next_free_priority;
use super::sql;
use super::state::{mark_name_custom, StoredAccount};
use super::store::{AccountStore, AccountStoreError};
use super::store_util::{token_tail_of, truncate_chars};
use super::CredentialWrite;

impl AccountStore {
    /// 某站点的账号记录：非空 id 按 id 直查（核对归属），空 id 取该站点
    /// **队首可用**账号（启用 + 有令牌，按全局优先级序）—— 与
    /// `autoclaw_account_record` 同一款形状（拆家后「队首」必须限定在本站点）。
    pub fn qoder_account_record(&self, site: Site, account_id: &str) -> Option<Value> {
        let guard = self.guard();
        let provider = site.provider_id();
        if !account_id.is_empty() {
            // 按 id 直查一行后确认归属（同 id 只会有一条，provider 判定是保险）
            let record = self.record_by_id(&guard, account_id)?;
            return (record.provider() == provider).then(|| record.to_value());
        }
        self.records_for_provider(&guard, provider)
            .into_iter()
            .filter(|record| record.enabled() && record.has_token())
            .min_by_key(|record| record.order_key())
            .map(|record| record.to_value())
    }

    pub fn add_qoder_account(
        &self,
        site: Site,
        credentials: &Credentials,
        name: Option<&str>,
        source: &str,
    ) -> Result<Value, AccountStoreError> {
        let mut credentials = credentials.clone();
        // 站点由 **provider 身份**决定（调用方从 kind 反查），凭证里带的
        // `mode` 是 payload 的残留提示 —— 两边不一致时以 provider 为准，
        // 否则「把中国版 PAT 粘到国际版卡片」会落一个两头都不认的记录。
        credentials.site = site;
        credentials.complete_identity()
            .map_err(|error| AccountStoreError::new(error.message, error.status_code))?;
        let guard = self.guard();
        // 身份匹配要「provider（=站点）+ userId」两段信息。provider 收窄已经
        // 把别站挡在外面，这里再核对一次记录里的 `mode` 只是保险 ——
        // 拆家迁移保证了两边一致，不一致的旧数据在启动时就已归位。
        let existing = self
            .records_for_provider(&guard, site.provider_id())
            .into_iter()
            .find(|record| {
                record.user_id() == credentials.user_id
                    && Site::from_record(&record.to_value()).ok() == Some(credentials.site)
            });
        let id = existing.as_ref().map(|record| record.id().to_string()).unwrap_or_else(|| {
            format!(
                "{}-{:x}",
                credentials.site.account_id_prefix(),
                Sha256::digest(credentials.user_id.as_bytes())
            )
        });
        // 新建时确认这个 id 没被任何人（含别家）占用 —— 主键查询，只读一行
        if existing.is_none() && self.record_by_id(&guard, &id).is_some() {
            return Err(AccountStoreError::new("Qoder 账号 ID 已被其它账号占用，请先核对账号记录", 409));
        }
        let record_name = name.map(str::trim).filter(|value| !value.is_empty()).map(str::to_string)
            .or_else(|| existing.as_ref().map(StoredAccount::name).filter(|value| !value.is_empty()))
            .unwrap_or_else(|| {
                if !credentials.name.is_empty() { credentials.name.clone() }
                else if !credentials.email.is_empty() { credentials.email.clone() }
                else { format!("{} {}", credentials.site.product(), credentials.user_id) }
            });
        let mut fields = existing.as_ref().map(|record| record.fields().clone()).unwrap_or_default();
        if let Value::Object(values) = credentials.to_value() {
            for (key, value) in values {
                // 空值不覆盖既有内容：重新添加时只给了一个 access token（或过期
                // 时间缺失）不该把上次的 refreshToken / expiresAt 洗掉 —— 那会让
                // 一个本来能自动续期的账号在下次导入后变成只能等过期。
                if value.is_null() {
                    continue;
                }
                if matches!(&value, Value::String(text) if text.is_empty()) {
                    continue;
                }
                fields.insert(key, value);
            }
        }
        let priority = match existing.as_ref() {
            Some(record) => record.priority(),
            None => {
                // 号段取全部账号（优先级全局唯一）；只取投影列一列数值。
                // `unwrap_or_default` 兜底到 `next_free_priority(&[])` 的默认号，
                // 与旧实现在空账号列表上的行为一致。
                let used = self
                    .with_conn(&guard, |conn| sql::priorities_all(conn))
                    .unwrap_or_default();
                next_free_priority(&used)
            }
        };
        fields.insert("id".to_string(), Value::String(id.clone()));
        fields.insert("provider".to_string(), Value::String(site.provider_id().to_string()));
        fields.insert("name".to_string(), Value::String(truncate_chars(&record_name, 100)));
        mark_name_custom(&mut fields, name.is_some_and(|value| !value.trim().is_empty()), existing.as_ref());
        fields.insert("tokenTail".to_string(), Value::String(token_tail_of(&credentials.access_token)));
        fields.insert("priority".to_string(), Value::from(priority));
        fields.insert("enabled".to_string(), Value::Bool(existing.as_ref().map(StoredAccount::enabled).unwrap_or(true)));
        fields.insert("desktop".to_string(), Value::Bool(false));
        fields.insert("source".to_string(), Value::String(source.to_string()));
        fields.insert("addedAt".to_string(), Value::from(existing.as_ref().map(StoredAccount::added_at).unwrap_or_else(logging::now_ms)));
        fields.insert("updatedAt".to_string(), Value::from(logging::now_ms()));
        fields.insert("rateLimits".to_string(), json!({}));
        for key in ["edition", "endpoint", "prefixPath", "platform", "access", "refresh", "expires", "pat"] {
            fields.remove(key);
        }
        let record = StoredAccount::from_map(fields);
        // 单行落地：`put` = DELETE + INSERT。旧实现在「命中既有记录」时是
        // `state.accounts[index] = record`（**保持原位**），而 `put` 会把它挪到
        // 列表末尾 —— 这一处差异不会被任何消费方观察到：列表顺序无消费方
        // （界面与选路都按优先级排，见 `sql.rs` 模块头「顺序」一节）。
        self.with_conn(&guard, |conn| sql::put(conn, &record))?;
        logging::log(
            "[Accounts]",
            &format!(
                "✅ {} 账号已保存（优先级 {priority}）",
                credentials.site.display_name()
            ),
        );
        Ok(self.public_account(&record))
    }

    pub fn update_qoder_credentials_if_current(
        &self,
        expected: &Value,
        credentials: &Credentials,
    ) -> Result<CredentialWrite, AccountStoreError> {
        let id = expected.get("id").and_then(Value::as_str).unwrap_or("");
        let guard = self.guard();
        let Some(mut record) = self
            .record_by_id(&guard, id)
            .filter(|record| Site::from_provider_id(&record.provider()).is_some())
        else {
            return Ok(CredentialWrite::Stale);
        };
        for key in ["accessToken", "refreshToken", "mode", "userId", "machineId", "addedAt"] {
            if record.get(key) != expected.get(key) {
                return Ok(CredentialWrite::Stale);
            }
        }
        if record.user_id() != credentials.user_id
            || Site::from_record(&record.to_value()).ok() != Some(credentials.site)
        {
            return Err(AccountStoreError::bad_request("Qoder 刷新结果与原账号身份不一致"));
        }
        if let Value::Object(values) = credentials.to_value() {
            for (key, value) in values {
                // 空值不覆盖既有内容：刷新结果里缺某一项（上游没回过期时间）时
                // 不该把记录里仍然有效的值洗成空。
                if value.is_null() || matches!(&value, Value::String(text) if text.is_empty()) {
                    continue;
                }
                record.fields_mut().insert(key, value);
            }
        }
        record.set("tokenTail", Value::String(token_tail_of(&credentials.access_token)));
        record.set_updated_at(logging::now_ms());
        self.with_conn(&guard, |conn| sql::update_in_place(conn, &record))?;
        Ok(CredentialWrite::Written)
    }

    pub fn to_qoder_public_account(&self, record: &StoredAccount) -> Value {
        // 站点先看 provider 身份（拆家后的权威来源），payload 里的 `mode` 只是
        // 旧数据的兼容读取 —— 记录缺 `mode` 时 `from_payload` 会默认国际版，
        // 那个默认对中国版记录是错的，不能当首选。
        // 与 from_record 同一口径：provider 优先，缺了才按 mode 回落
        let site = Site::from_record(&record.to_value()).ok();
        let available = record.has_token() && site.is_some();
        let can_refresh = Credentials::from_payload(&record.to_value())
            .map(|credentials| credentials.can_refresh()).unwrap_or(false);
        let mut public = Map::new();
        for key in ["id", "provider", "name", "userId", "email", "nickname", "mode", "source", "tokenTail", "expiresAt"] {
            public.insert(key.to_string(), record.get(key).cloned().unwrap_or(Value::Null));
        }
        public.insert("edition".to_string(), site.map(|site| Value::String(site.edition().to_string())).unwrap_or(Value::Null));
        public.insert("editionLabel".to_string(), site.map(|site| Value::String(site.label().to_string())).unwrap_or(Value::Null));
        public.insert("hasRefreshToken".to_string(), Value::Bool(can_refresh));
        public.insert("priority".to_string(), Value::from(record.priority()));
        public.insert("enabled".to_string(), Value::Bool(record.enabled()));
        public.insert("addedAt".to_string(), Value::from(record.added_at()));
        public.insert("updatedAt".to_string(), Value::from(record.updated_at()));
        public.insert("proxy".to_string(), crate::server::core::proxies::describe_account_proxy(Some(&record.proxy())));
        public.insert("rateLimits".to_string(), record.get("rateLimits").cloned().unwrap_or_else(|| json!({})));
        public.insert("desktop".to_string(), Value::Bool(false));
        public.insert("available".to_string(), Value::Bool(available));
        // `chatSupported` / `maxConcurrent` **刻意不在这里写**：它们是跨家的统一
        // 事实，由 `store_view::public_account` 在分派点注入（前者按适配器的
        // `supports_chat()`，后者按 `store_util::max_concurrent_public_for`）。
        // 本函数早先硬编码过 `chatSupported: false`（Qoder 只有账号管理能力
        // 那会儿），而那个值会被分派点的注入覆盖 —— 留着它只会在下一次读这段
        // 代码时误导「这家不能转发」，且一旦注入被挪走就会静默失真。
        Value::Object(public)
    }
}
