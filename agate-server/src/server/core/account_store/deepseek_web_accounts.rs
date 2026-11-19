//! DeepSeek **网页版**账号：粘贴 `userToken` 落账号 + 公开形态。
//!
//! ── 账号字段（与 Command Code / LobsterAI 的对照）────────────────
//! ```text
//!   { id: "deepseek-web-<token 摘要>", provider: "deepseek-web", name,
//!     accessToken: "<userToken 明文>",      ← 键名与别家统一
//!     tokenTail,
//!     source, priority, enabled, addedAt, updatedAt }
//! ```
//! `accessToken` 落盘存的是 **userToken 本体**（键名与别家统一：`has_token()` /
//! `access_token()` 一族认这个键，转发时从 `auth.accessToken` 取，
//! `providers/deepseek/credentials.rs` 的读取链也以它为首选）。
//! **没有** `expiresAt` / `refreshToken`：上游没有 refresh 接口
//! （`supports_refresh = false`，见 `providers/deepseek/adapter.rs`）。
//!
//! ── 账号 id 的派生（为什么不是前缀 + 尾号）──────────────────────
//! 本家没有「userId」那种稳定身份可读（userToken 是 JWT 形态，但它的声明
//! 不该被当作身份契约 —— 上游换形态时会被追着改），而 id 必须**对同一个
//! token 稳定**（再粘一次要更新同一条记录，而不是新增一条）。
//! 因此取 `sha256("deepseek-web:account-id:v1\0" + token)` 的前 12 个 hex：
//! 确定性、不可反推 token、不同 token 撞 id 的概率可忽略。
//! （与 `commandcode_accounts.rs` 同一做法 —— 两家都是「单枚静态凭证」形态，
//! 因此复用同一个模式而不是另发明一套。）
//!
//! ── 为什么不用 token 本身当身份（对比 doubaowork 的教训）────────
//! doubaowork 用「解析出的 uid」当身份，好处是「上游续期换 Cookie 后仍认得
//! 同一账号」；代价是**判据必须与登录路径同一个函数**，两处分叉就会造重复记录。
//! 本家没有 uid 可读、也**没有续期**（token 不会在上游侧被替换），因此摘要式
//! 身份是更简单也唯一成立的选择：不存在「同一账号换凭据」的场景。
//!
//! 本文件全是「读-改-写」存储操作，**没有任何网络请求**（持锁不做网络）。
//! 绝不 unwrap/expect（release 是 panic=abort）。

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::server::core::providers::deepseek::credentials;

use super::priority::next_free_priority;
use super::sql;
use super::state::{mark_name_custom, StoredAccount};
use super::store::{AccountStore, AccountStoreError};
use super::store_util::{token_tail_of, truncate_chars};
use super::DEEPSEEK_WEB_PROVIDER_ID;
use crate::server::logging;

/// 备注名长度上限（与别家同一口径）
const MAX_NAME_LENGTH: usize = 100;

/// 输入长度上限（超过这个长度的输入一定是粘错了东西）
const MAX_TOKEN_LENGTH: usize = 16_384;

/// 从 payload 里取第一个非空字符串（候选键覆盖本项目落盘名与粘贴形态）
fn pick(payload: &Value, keys: &[&str]) -> String {
    for key in keys {
        let value = payload
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if !value.is_empty() {
            return value.to_string();
        }
    }
    String::new()
}

/// token → 稳定的账号 id（见模块头）
fn account_id_for(user_token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"deepseek-web:account-id:v1\0");
    hasher.update(user_token.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("deepseek-web-{}", &digest[..12])
}

impl AccountStore {
    /// 取一条 DeepSeek 网页版账号记录（公开形态的原始 JSON）。
    ///
    /// `account_id` 非空 → 按 id 直查（不属于本家返回 None）；
    /// 为空 → 本家**启用中**账号里优先级最靠前的一条（与
    /// `commandcode_account_record` 的语义一致：转发默认用队首账号）。
    pub fn deepseek_web_account_record(&self, account_id: &str) -> Option<Value> {
        let guard = self.guard();
        if !account_id.is_empty() {
            let record = self.record_by_id(&guard, account_id)?;
            return (record.provider() == DEEPSEEK_WEB_PROVIDER_ID).then(|| record.to_value());
        }
        let mut candidates: Vec<StoredAccount> = self
            .records_for_provider(&guard, DEEPSEEK_WEB_PROVIDER_ID)
            .into_iter()
            .filter(StoredAccount::enabled)
            .collect();
        candidates.sort_by_key(StoredAccount::order_key);
        candidates.into_iter().next().map(|item| item.to_value())
    }

    /// 添加/更新一个 DeepSeek 网页版账号（粘贴 userToken 的唯一落账号入口）。
    ///
    /// payload（`login::verify_token` 的返回，或界面直接传来的形状）：
    ///   - `accessToken` / `userToken` / `token`：userToken（必填）；
    ///
    /// 同一 token 的记录就地更新（保留优先级与启用状态、沿用用户改过的备注名）；
    /// 撞到**别的 provider** 的 id 时报错，绝不覆写（与另外几家同一策略）。
    pub fn add_deepseek_web_account(
        &self,
        payload: &Value,
        name: Option<&str>,
        source: &str,
    ) -> Result<Value, AccountStoreError> {
        let user_token = pick(payload, &["accessToken", "userToken", "token"]);
        if user_token.is_empty() {
            return Err(AccountStoreError::new(
                "缺少 userToken（accessToken / userToken / token）",
                400,
            ));
        }
        if user_token.chars().count() > MAX_TOKEN_LENGTH {
            return Err(AccountStoreError::new("userToken 过长", 400));
        }
        if !credentials::shape_ok(&user_token) {
            return Err(AccountStoreError::new(
                "这不是 DeepSeek 网页版的 userToken：应是不含空白的一整串（不要把 Cookie 或整行文本粘进来）",
                400,
            ));
        }
        let id = account_id_for(&user_token);

        let guard = self.guard();
        let existing = self.record_by_id(&guard, &id);
        if let Some(existing) = existing.as_ref() {
            let existing_provider = existing.provider();
            if existing_provider != DEEPSEEK_WEB_PROVIDER_ID {
                return Err(AccountStoreError::new(
                    format!(
                        "账号 id「{id}」已被{existing_provider}账号占用，无法添加同一个 userToken 的\
                         DeepSeek 网页版账号（请先处理那个账号）"
                    ),
                    400,
                ));
            }
        }
        // 备注名兜底：显式传入 → 既有记录里的备注名 → 「DeepSeek 网页 <尾号>」
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
            .unwrap_or_else(|| format!("DeepSeek 网页 {}", token_tail_of(&user_token)));
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
            Value::String(DEEPSEEK_WEB_PROVIDER_ID.to_string()),
        );
        record.insert("name".to_string(), Value::String(record_name.clone()));
        // 备注名标记：用户显式传了名（或既有记录已打标）时置位 —— 本家走自己的
        // 保存路径（不经过 `store_crud::upsert_account` 的统一打标），必须显式调用
        mark_name_custom(
            &mut record,
            name.is_some_and(|value| !value.trim().is_empty()),
            existing.as_ref(),
        );
        record.insert(
            "accessToken".to_string(),
            Value::String(user_token.clone()),
        );
        record.insert(
            "tokenTail".to_string(),
            Value::String(token_tail_of(&user_token)),
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
                "✅ DeepSeek 网页版账号已保存: {record_name}（优先级 {priority}，token {}）",
                credentials::masked(&user_token)
            ),
        );
        Ok(self.public_account(&saved))
    }

    /// DeepSeek 网页版账号的**公开形态**（进 HTTP 响应）：去掉 token 本体，
    /// 只留尾几位。
    pub fn to_deepseek_web_public_account(&self, record: &StoredAccount) -> Value {
        let mut value = record.to_value();
        if let Some(object) = value.as_object_mut() {
            object.remove("accessToken");
            object.insert("available".to_string(), Value::Bool(record.enabled()));
            // 有没有 token（等价于 has_credentials，但给出的是**本家语义**的
            // 字段名，界面按它显示「已配置 / 需重贴」）
            object.insert(
                "hasToken".to_string(),
                Value::Bool(
                    record
                        .get("accessToken")
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.trim().is_empty()),
                ),
            );
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// token → id 是确定性的（同一 token 必得同一 id，否则「再粘一次」会新增记录）
    #[test]
    fn account_id_is_deterministic() {
        let a = account_id_for("eyJ.abc.def");
        let b = account_id_for("eyJ.abc.def");
        assert_eq!(a, b);
        assert!(a.starts_with("deepseek-web-"));
        // 12 位十六进制摘要
        assert_eq!(a.len(), "deepseek-web-".len() + 12);
    }

    /// 不同 token 给不同 id
    #[test]
    fn account_id_differs_per_token() {
        assert_ne!(account_id_for("token-a"), account_id_for("token-b"));
    }

    /// id 不可反推 token（摘要里不含明文片段）
    #[test]
    fn account_id_does_not_leak_the_token() {
        let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.secret";
        let id = account_id_for(token);
        assert!(!id.contains("secret"));
        assert!(!id.contains("eyJhbGci"));
    }

    /// pick 的候选键顺序：本项目落盘名优先
    #[test]
    fn pick_prefers_project_field_name() {
        let payload = json!({
            "accessToken": "primary",
            "userToken": "secondary",
            "token": "tertiary"
        });
        assert_eq!(pick(&payload, &["accessToken", "userToken", "token"]), "primary");
        let only_secondary = json!({ "userToken": "secondary" });
        assert_eq!(
            pick(&only_secondary, &["accessToken", "userToken", "token"]),
            "secondary"
        );
    }

    /// pick 跳过空白值（不能把 "   " 当成有效 token）
    #[test]
    fn pick_skips_blank_values() {
        let payload = json!({ "accessToken": "   ", "userToken": "real" });
        assert_eq!(pick(&payload, &["accessToken", "userToken"]), "real");
    }
}