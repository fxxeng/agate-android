//! Kiro 凭证：三种来源的统一读取、续期判据与身份。
//!
//! ── 落盘字段 ────────────────────────────────────────────────
//! ```text
//!   method        apikey / social / idc / external-idp
//!   accessToken   访问令牌（API key 时是 ksk_…）
//!   refreshToken  续期令牌（API key 没有）
//!   expiresAt     毫秒时间戳（0 = 不知道，按「永不过期」处理）
//!   region        登录时的地区（profile 缺失时的兜底）
//!   profileArn    AWS 分配的 profile ARN（地区与账号都从它读）
//!   clientId/clientSecret  idc / external-idp 换码用
//!   tokenURL      external-idp 的 token 端点（idc 由 oidc(region) 派生）
//!   loginProvider google / github / builderid / enterprise
//!   accountId     邮箱或 AWS 账号（展示 + 去重兜底）
//!   plan          Free / Pro / Pro+ / Power（展示用）
//! ```
//!
//! ── 为什么 `method` 要有兜底推断 ────────────────────────────
//! 用户可能只粘一枚 `ksk_…`（官方 API key）或只粘 refresh token，`method`
//! 一片空白。此时按内容判：`ksk_` 前缀 → `apikey`，否则 → `social`。判错的
//! 代价很具体：`apikey` 会被当成可续期账号，每 2 分钟白刷一次续期接口。
//!
//! ── 为什么「不知道过期时间」算可用 ──────────────────────────
//! `expires == 0` 表示上游没告诉我们（API key 永远如此，部分 social 记录也
//! 没有 exp）。把它当过期会让账号每请求刷一次续期；当可用至少能试一次 ——
//! 真过期了上游会回 403，那条路径会触发一次续期重试。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{Map, Value};

use crate::server::errors::GatewayError;

use super::endpoints;
use super::DEFAULT_REGION;

/// 一份 Kiro 凭证。
#[derive(Clone, Debug, Default)]
pub struct KiroCredentials {
    /// `apikey` / `social` / `idc` / `external-idp`
    pub method: String,
    pub access: String,
    pub refresh: String,
    /// 毫秒时间戳；0 = 不知道
    pub expires: i64,
    /// 登录地区（profile 缺失时的兜底）
    pub region: String,
    /// profile ARN
    pub profile: String,
    pub client_id: String,
    pub client_secret: String,
    /// external-idp 的 token 端点
    pub token_url: String,
    /// google / github / builderid / enterprise
    pub login_provider: String,
    /// 邮箱或 AWS 账号
    pub account_id: String,
    /// Free / Pro / Pro+ / Power
    pub plan: String,
}

fn text(fields: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(value) = fields.get(*key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

fn number(fields: &Value, keys: &[&str]) -> i64 {
    for key in keys {
        if let Some(value) = fields.get(*key).and_then(Value::as_i64) {
            if value > 0 {
                return value;
            }
        }
        if let Some(value) = fields.get(*key).and_then(Value::as_str) {
            if let Ok(parsed) = value.trim().parse::<i64>() {
                if parsed > 0 {
                    return parsed;
                }
            }
        }
    }
    0
}

impl KiroCredentials {
    /// 从账号记录（或任意字段对象）读凭证。
    ///
    /// 字段名以本项目落盘名为首选，同时兼容上游插件的旧写法
    /// （`access` / `refresh` / `expires` / `profile`）—— 从旧备份导入的
    /// 账号不该因为键名不同而变成「缺凭证」。
    pub fn from_fields(fields: &Value) -> Result<Self, GatewayError> {
        let mut method = text(fields, &["method", "authMethod"]);
        let access = text(fields, &["accessToken", "access_token", "access", "apiKey", "key"]);
        if method.is_empty() {
            method = if access.starts_with("ksk_") {
                "apikey".to_string()
            } else {
                "social".to_string()
            };
        }
        let refresh = text(fields, &["refreshToken", "refresh_token", "refresh"]);
        let credentials = Self {
            method,
            access,
            refresh,
            expires: number(fields, &["expiresAt", "expires"]),
            region: {
                let region = text(fields, &["region"]);
                if region.is_empty() {
                    DEFAULT_REGION.to_string()
                } else {
                    region
                }
            },
            profile: text(fields, &["profileArn", "profile"]),
            client_id: text(fields, &["clientId"]),
            client_secret: text(fields, &["clientSecret"]),
            token_url: text(fields, &["tokenURL", "tokenUrl", "token_url"]),
            login_provider: text(fields, &["loginProvider", "uid"]),
            account_id: text(fields, &["accountId", "email"]),
            plan: text(fields, &["plan", "subscriptionTitle"]),
        };
        if credentials.access.is_empty() {
            return Err(GatewayError::with_status(
                404,
                "Kiro 账号没有可用凭证：请重新登录，或粘贴 ksk_ 开头的 API key",
            ));
        }
        Ok(credentials)
    }

    /// 写回记录（**只写自己认识的键**，调用方负责保留未知字段）
    pub fn write_into(&self, record: &mut Map<String, Value>) {
        record.insert("method".to_string(), Value::String(self.method.clone()));
        record.insert("accessToken".to_string(), Value::String(self.access.clone()));
        record.insert("refreshToken".to_string(), Value::String(self.refresh.clone()));
        record.insert("expiresAt".to_string(), Value::from(self.expires));
        record.insert("region".to_string(), Value::String(self.region.clone()));
        record.insert("profileArn".to_string(), Value::String(self.profile.clone()));
        record.insert("clientId".to_string(), Value::String(self.client_id.clone()));
        record.insert(
            "clientSecret".to_string(),
            Value::String(self.client_secret.clone()),
        );
        record.insert(
            "loginProvider".to_string(),
            Value::String(self.login_provider.clone()),
        );
        record.insert("accountId".to_string(), Value::String(self.account_id.clone()));
        // 空值不落：`tokenURL` / `plan` 缺省就是「没有」，写空串会让
        // `text()` 一族的「取第一个非空」逻辑白跑一遍，也让记录看起来更脏
        if !self.token_url.is_empty() {
            record.insert("tokenURL".to_string(), Value::String(self.token_url.clone()));
        }
        if !self.plan.is_empty() {
            record.insert("plan".to_string(), Value::String(self.plan.clone()));
        }
    }

    /// 是官方 API key（不续期、不写回任何地方）
    pub fn is_api_key(&self) -> bool {
        self.method == "apikey"
    }

    /// `tokentype` 请求头（只有 API key 与外部 IdP 才发）
    pub fn token_type(&self) -> Option<&'static str> {
        match self.method.as_str() {
            "apikey" => Some("API_KEY"),
            "external-idp" => Some("EXTERNAL_IDP"),
            _ => None,
        }
    }

    /// 实际使用的地区（profile 优先，登录地区兜底）
    pub fn region(&self) -> String {
        endpoints::region_of(&self.profile, &self.region)
    }

    /// 现在能用（离过期还早 / 永不过期）
    pub fn fresh(&self, early_ms: i64) -> bool {
        self.is_api_key() || self.expires == 0 || self.expires - now_ms() > early_ms
    }

    /// 能不能续期：API key 没有 refresh，其余要有 refresh 令牌
    pub fn can_refresh(&self) -> bool {
        !self.is_api_key() && !self.refresh.is_empty()
    }

    /// 日志用的脱敏形态（`ksk_…` 与 JWT 都只露尾巴）
    pub fn masked(&self) -> String {
        masked(&self.access)
    }
}

/// 当前毫秒时间戳（与 `logging::now_ms` 同一时钟，便于与日志对齐）
pub fn now_ms() -> i64 {
    crate::server::logging::now_ms()
}

/// 令牌的脱敏形态：官方 key 保留前缀，其余只留尾 4 位。
pub fn masked(token: &str) -> String {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return "（空）".to_string();
    }
    if trimmed.starts_with("ksk_") {
        return format!("ksk_…{}", tail_of(trimmed));
    }
    format!("…{}", tail_of(trimmed))
}

/// 令牌尾 4 个字符（按字符切，多字节安全）
pub fn tail_of(token: &str) -> String {
    let chars: Vec<char> = token.trim().chars().collect();
    let start = chars.len().saturating_sub(4);
    chars[start..].iter().collect()
}

/// 账号的稳定身份（去重判据）。
///
/// 优先级：邮箱 + 登录方式 > AWS 账号；API key 只能按摘要（它不带任何身份）。
/// 换一枚令牌仍是同一个账号 —— 这是「重登一次不会多出一条记录」的前提。
pub fn identity(credentials: &KiroCredentials) -> String {
    use sha2::{Digest, Sha256};
    if credentials.is_api_key() {
        let digest = Sha256::digest(credentials.access.as_bytes());
        let hex: String = digest.iter().take(8).map(|byte| format!("{byte:02x}")).collect();
        return format!("key:{hex}");
    }
    if !credentials.account_id.is_empty() {
        let way = if credentials.login_provider.is_empty() {
            "own"
        } else {
            credentials.login_provider.as_str()
        };
        return format!("{}:{}", credentials.account_id.to_lowercase(), way);
    }
    // 没有邮箱（idc 账号常常只有 profile ARN）→ 按 profile 的账号段
    let account = endpoints::account_of(&credentials.profile);
    if !account.is_empty() {
        return format!("aws:{account}:{}", credentials.region());
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 三种来源的形态差异：API key 不续期、社交登录有 refresh
    #[test]
    fn api_keys_short_circuit_every_refresh_path() {
        let key = KiroCredentials::from_fields(&json!({ "accessToken": "ksk_abcdefgh1234" }))
            .expect("API key 应能读出来");
        // `method` 空着时按 `ksk_` 前缀推断
        assert_eq!(key.method, "apikey");
        assert!(key.is_api_key());
        assert_eq!(key.token_type(), Some("API_KEY"));
        assert!(!key.can_refresh(), "API key 没有续期接口");
        // 永不过期（expires 未知也不刷）
        assert!(key.fresh(super::super::EARLY_REFRESH_MS));

        let social = KiroCredentials::from_fields(&json!({
            "accessToken": "jwt-1",
            "refreshToken": "refresh-1",
            "expiresAt": 1_700_000_000_000i64,
        }))
        .expect("社交登录应能读出来");
        assert_eq!(social.method, "social");
        assert!(social.can_refresh());
        assert_eq!(social.token_type(), None);
    }

    /// 过期判据：不知道过期 = 可用；真过期 = 不可用
    #[test]
    fn freshness_handles_the_unknown_expiry() {
        let unknown = KiroCredentials::from_fields(&json!({ "accessToken": "jwt" }))
            .expect("应能读出来");
        assert_eq!(unknown.expires, 0);
        assert!(unknown.fresh(60_000), "不知道过期时间时不该判「过期」");

        let expired = KiroCredentials::from_fields(&json!({
            "accessToken": "jwt",
            "expiresAt": now_ms() - 60_000,
        }))
        .expect("应能读出来");
        assert!(!expired.fresh(super::super::EARLY_REFRESH_MS));

        let soon = KiroCredentials::from_fields(&json!({
            "accessToken": "jwt",
            "expiresAt": now_ms() + 30_000,
        }))
        .expect("应能读出来");
        assert!(!soon.fresh(super::super::EARLY_REFRESH_MS), "30 秒后过期要先刷");
    }

    /// 地区：profile 里的优先；没有就按登录地区
    #[test]
    fn the_region_comes_from_the_profile_first() {
        let eu = KiroCredentials::from_fields(&json!({
            "accessToken": "jwt",
            "region": "us-east-1",
            "profileArn": "arn:aws:codewhisperer:eu-central-1:1:profile/A",
        }))
        .expect("应能读出来");
        assert_eq!(eu.region(), "eu-central-1");

        let fallback = KiroCredentials::from_fields(&json!({ "accessToken": "jwt" }))
            .expect("应能读出来");
        assert_eq!(fallback.region(), DEFAULT_REGION);
    }

    /// 旧键名（上游插件的 `access` / `expires` / `profile`）也要能读
    #[test]
    fn old_field_names_still_load() {
        let old = KiroCredentials::from_fields(&json!({
            "access": "jwt-old",
            "refresh": "refresh-old",
            "expires": "1700000000000",
            "profile": "arn:aws:codewhisperer:us-east-1:1:profile/A",
            "uid": "builderid",
        }))
        .expect("应能读出来");
        assert_eq!(old.access, "jwt-old");
        assert_eq!(old.refresh, "refresh-old");
        assert_eq!(old.expires, 1_700_000_000_000);
        assert_eq!(old.login_provider, "builderid");
        assert!(old.can_refresh());
    }

    /// 没有令牌 ⇒ 一条可读的 404（而不是空凭证继续往下飞）
    #[test]
    fn missing_token_is_a_readable_404() {
        let error = KiroCredentials::from_fields(&json!({ "accountId": "a@b.c" }))
            .expect_err("空凭证必须报错");
        assert_eq!(error.status_code, 404);
        assert!(error.message.contains("API key"), "{}", error.message);
    }

    /// 身份：换令牌不换身份；API key 按摘要且不泄漏明文
    #[test]
    fn identity_is_stable_across_tokens() {
        let first = KiroCredentials::from_fields(&json!({
            "accessToken": "jwt-a", "accountId": "User@Example.com", "loginProvider": "google",
        }))
        .expect("应能读出来");
        let second = KiroCredentials::from_fields(&json!({
            "accessToken": "jwt-b", "accountId": "user@example.com", "loginProvider": "google",
        }))
        .expect("应能读出来");
        assert_eq!(identity(&first), identity(&second), "同一账号换令牌不该换身份");

        let key = KiroCredentials::from_fields(&json!({ "accessToken": "ksk_secret-must-not-leak" }))
            .expect("应能读出来");
        let id = identity(&key);
        assert!(id.starts_with("key:"), "{id}");
        assert!(!id.contains("secret"), "{id}");

        // 没有邮箱时按 profile 的账号段兜底
        let arn_only = KiroCredentials::from_fields(&json!({
            "accessToken": "jwt",
            "profileArn": "arn:aws:codewhisperer:us-east-1:638616132270:profile/A",
        }))
        .expect("应能读出来");
        assert_eq!(identity(&arn_only), "aws:638616132270:us-east-1");

        // 什么都没有 → 空身份（调用方据此报错，不生成随机 id）
        let empty = KiroCredentials::from_fields(&json!({ "accessToken": "jwt" }))
            .expect("应能读出来");
        assert_eq!(identity(&empty), "");
    }

    /// 写回只写认识的键，空的可选键不落
    #[test]
    fn write_into_keeps_the_record_tidy() {
        let credentials = KiroCredentials::from_fields(&json!({
            "accessToken": "ksk_1",
            "accountId": "a@b.c",
        }))
        .expect("应能读出来");
        let mut record = Map::new();
        record.insert("unknownField".to_string(), json!("keep-me"));
        credentials.write_into(&mut record);
        assert_eq!(record.get("method").and_then(Value::as_str), Some("apikey"));
        assert_eq!(record.get("accessToken").and_then(Value::as_str), Some("ksk_1"));
        assert!(!record.contains_key("tokenURL"), "空值不该落盘");
        assert!(!record.contains_key("plan"), "空值不该落盘");
        assert_eq!(
            record.get("unknownField").and_then(Value::as_str),
            Some("keep-me"),
            "调用方的未知字段不该被写回逻辑碰掉"
        );
    }

    /// 日志脱敏：官方 key 留前缀、其余只留尾 4 位
    #[test]
    fn masking_never_prints_the_whole_token() {
        assert_eq!(masked("ksk_abcdefgh1234"), "ksk_…1234");
        assert_eq!(masked("eyJhbGciOi.payload.sig"), "….sig");
        assert_eq!(masked(""), "（空）");
        // 短令牌不 panic（多字节也安全；不足 4 位就整串露出来，反正是很短的东西）
        assert_eq!(masked("ab"), "…ab");
        assert_eq!(masked("令牌串"), "…令牌串");
    }
}
