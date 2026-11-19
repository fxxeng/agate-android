//! ModelScope（魔搭）账号：两个站点共一份落账号逻辑，**按地区分开 provider id**。
//!
//! ── 账号字段 ────────────────────────────────────────────────
//! ```text
//!   { id: "modelscope-<HavanaId>" / "modelscope-intl-<HavanaId>",
//!     provider: "modelscope" | "modelscope-intl",
//!     name, accessToken(= ms-令牌), tokenTail, userId(= HavanaId),
//!     source, priority, enabled, addedAt, updatedAt }
//! ```
//! `accessToken` 落盘存的是**令牌本体**（键名与别家统一：`has_token()` /
//! `access_token()` 一族认这个键，转发时从 `auth.accessToken` 取）。
//! **没有** `expiresAt` / `refreshToken`：`ms-` 是长期 API key，上游没有续期接口
//! （`supports_refresh = false`，见 `providers/modelscope/credentials.rs`）。
//!
//! ── id 为什么用上游的 HavanaId 而不是令牌派生 ────────────────────
//! 令牌是不透明的 opaque 串（不是 JWT，解不出身份）。按令牌派生 id 的话，
//! **同一个账号换一次令牌就变成两条记录** —— 用户会看到「我只有一个魔搭账号，
//! 怎么越加越多」。所以添加时先探活换回 `HavanaId`（`providers::modelscope::probe`），
//! 用它做 id。这也是探活失败就**不放行添加**的原因：猜出来的身份比一条明确的
//! 失败更难收拾。
//!
//! ── 两地区为什么带不同前缀 ──────────────────────────────────
//! 两站的身份 id 空间互不相通，但同一个人在两地的 `HavanaId` 完全可能相同
//! （各自注册、id 生成规则同形）。共用 `modelscope-` 前缀的话，第二个站点会
//! 撞上「跨 provider 同 id」保护而被拒绝 —— 而两地账号并存正是拆家的目的。
//!
//! 本文件全是「读-改-写」存储操作，**没有任何网络请求**（持锁不做网络）。
//! 绝不 unwrap/expect（release 是 panic=abort）。

use serde_json::{Map, Value};

use crate::server::core::providers::modelscope::Region;

use super::priority::next_free_priority;
use super::sql;
use super::state::{mark_name_custom, StoredAccount};
use super::store::{AccountStore, AccountStoreError};
use super::store_util::{token_tail_of, truncate_chars};
use crate::server::logging;

/// 备注名长度上限（与别家同一口径）
const MAX_NAME_LENGTH: usize = 100;

/// 令牌长度上限（超过这个长度的输入一定是粘错了东西）
const MAX_TOKEN_LENGTH: usize = 8192;

impl AccountStore {
    /// 取一条 ModelScope 账号记录（公开形态的原始 JSON）。
    ///
    /// `account_id` 非空 → 按 id 直查（**不属于本地区**返回 None —— 两个站点是
    /// 两家 provider，拿到另一家的 id 就该当作不存在）；
    /// 为空 → 本地区**启用中**账号里优先级最靠前的一条（转发默认用队首账号）。
    pub fn modelscope_account_record(&self, region: Region, account_id: &str) -> Option<Value> {
        let provider = region.provider_id();
        let guard = self.guard();
        if !account_id.is_empty() {
            let record = self.record_by_id(&guard, account_id)?;
            return (record.provider() == provider).then(|| record.to_value());
        }
        let mut candidates: Vec<StoredAccount> = self
            .records_for_provider(&guard, provider)
            .into_iter()
            .filter(StoredAccount::enabled)
            .collect();
        candidates.sort_by_key(StoredAccount::order_key);
        candidates.into_iter().next().map(|item| item.to_value())
    }

    /// 添加/更新一个 ModelScope 账号（粘贴令牌的唯一落账号入口）。
    ///
    /// 调用方**必须先探活**（`providers::modelscope::probe::identify`）：
    /// `token` 已是最新的、`user_id` 是上游给的 HavanaId。这里不再出网。
    ///
    /// 同一 `HavanaId` 的记录就地更新（保留优先级与启用状态、沿用用户改过的
    /// 备注名）；撞到**别的 provider** 的 id 时报错，绝不覆写。
    pub fn add_modelscope_account(
        &self,
        region: Region,
        token: &str,
        user_id: &str,
        display_name: &str,
        name: Option<&str>,
        source: &str,
    ) -> Result<Value, AccountStoreError> {
        let token = token.trim();
        if token.is_empty() {
            return Err(AccountStoreError::new("缺少 ModelScope 令牌", 400));
        }
        if token.chars().count() > MAX_TOKEN_LENGTH {
            return Err(AccountStoreError::new("ModelScope 令牌过长", 400));
        }
        let user_id = user_id.trim();
        if user_id.is_empty() {
            // 探活通过却读不到身份：不放行（否则 id 只能拿尾号猜，见模块头）
            return Err(AccountStoreError::new(
                "ModelScope 账号缺少稳定身份（HavanaId），无法落账号",
                400,
            ));
        }
        let provider = region.provider_id();
        let id = format!("{}{user_id}", region.account_id_prefix());

        let guard = self.guard();
        let existing = self.record_by_id(&guard, &id);
        if let Some(existing) = existing.as_ref() {
            let existing_provider = existing.provider();
            if existing_provider != provider {
                return Err(AccountStoreError::new(
                    format!(
                        "账号 id「{id}」已被{existing_provider}账号占用，无法添加同一个\
                         ModelScope 账号（请先处理那个账号）"
                    ),
                    400,
                ));
            }
        }
        // 备注名兜底：显式传入 → 既有记录里的备注名 → 上游昵称 → 「ModelScope <尾号>」
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
            .or_else(|| {
                let upstream = display_name.trim();
                (!upstream.is_empty()).then(|| truncate_chars(upstream, MAX_NAME_LENGTH))
            })
            .unwrap_or_else(|| {
                format!("ModelScope {} {}", region.label(), token_tail_of(token))
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
        record.insert("provider".to_string(), Value::String(provider.to_string()));
        record.insert("name".to_string(), Value::String(record_name.clone()));
        // 备注名标记：用户显式传了名（或既有记录已打标）时置位 —— 本家走自己的
        // 保存路径（不经过统一 upsert），必须显式调用
        mark_name_custom(
            &mut record,
            name.is_some_and(|value| !value.trim().is_empty()),
            existing.as_ref(),
        );
        record.insert("accessToken".to_string(), Value::String(token.to_string()));
        record.insert("tokenTail".to_string(), Value::String(token_tail_of(token)));
        // 身份键：账号行按它显示「是哪个魔搭账号」（两家 provider 的字段名统一）
        record.insert("userId".to_string(), Value::String(user_id.to_string()));
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
                "✅ ModelScope {}账号已保存: {record_name}（优先级 {priority}，令牌 …{}）",
                region.label(),
                token_tail_of(token)
            ),
        );
        Ok(self.public_account(&saved))
    }

    /// ModelScope 账号的**公开形态**：摘掉令牌本体，只留尾号。
    ///
    /// 与其余各家同一口径：公开形态进 HTTP 响应与局域网内的浏览器，
    /// 回显一次令牌就等于把它交给所有能看到账号列表的人。
    pub fn to_modelscope_public_account(&self, record: &StoredAccount) -> Value {
        let mut value = record.to_value();
        if let Some(object) = value.as_object_mut() {
            object.remove("accessToken");
            object.insert("available".to_string(), Value::Bool(record.enabled()));
            object.insert("hasToken".to_string(), Value::Bool(true));
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两个站点的 id 前缀必须不同，否则两地同 HavanaId 会撞车被拒
    #[test]
    fn region_prefixes_are_distinct() {
        let cn = format!("{}{}", Region::Cn.account_id_prefix(), "123");
        let intl = format!("{}{}", Region::Intl.account_id_prefix(), "123");
        assert_ne!(cn, intl);
        assert_eq!(cn, "modelscope-123");
        assert_eq!(intl, "modelscope-intl-123");
    }
}
