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
//! 用户可能只粘一枚 `ksk_…`（官方 API key）或整段 `kiro-auth-token.json`，
//! `method` 一片空白。此时按**手上有的字段**推断（`method_of`）：`ksk_` 前缀 →
//! `apikey`；有 `tokenURL` → `external-idp`；有 `clientId` + `clientSecret` →
//! `idc`；其余 → `social`。判错的代价很具体：`apikey` 被当成可续期账号时每
//! 2 分钟白刷一次续期接口；OAuth 被当成 API key 时 `can_refresh()` 直接 false、
//! 临期就报「请重新登录」—— 两种都是查库看不出原因的那种。
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

/// 从**已建好的字段表**里取第一个非空文本（`text` 的 Map 版本：粘贴归一
/// 手里是 `Map` 而不是 `Value`，两处判据必须是同一套键顺序）
fn first_text(fields: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = fields.get(*key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
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

/// 去掉粘贴时常带的外壳（`Bearer ` / 整行 `Authorization: Bearer …`）。
///
/// 用户从开发者工具复制的多半是整行请求头而不是裸令牌。在**唯一入口**剥一层，
/// 后面的签名、续期、请求头就都不必各自防一遍（各自防必然漏一处，漏的那处
/// 表现为「账号能加、一发请求就 401」）。
fn bare_token(value: &str) -> String {
    let mut current = value.trim();
    if let Some(rest) = current.strip_prefix("Authorization:") {
        current = rest.trim();
    }
    for prefix in ["Bearer ", "bearer ", "BEARER "] {
        if let Some(rest) = current.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    }
    current.to_string()
}

/// 判定凭证形态：`apikey` / `social` / `idc` / `external-idp`。
///
/// 显式写了 `method`（含上游旧名 `authMethod`）就按它；否则按「证据强弱」推
/// （见模块头）：`ksk_` 前缀 → 有 `tokenURL` → 有 `clientId` + `clientSecret`
/// → 兜底 `social`。
fn method_of(fields: &Value, access: &str) -> String {
    let explicit = text(fields, &["method", "authMethod"]);
    if !explicit.is_empty() {
        return explicit;
    }
    if access.starts_with("ksk_") {
        return "apikey".to_string();
    }
    if !text(fields, &["tokenURL", "tokenUrl", "token_url"]).is_empty() {
        return "external-idp".to_string();
    }
    if !text(fields, &["clientId"]).is_empty() && !text(fields, &["clientSecret"]).is_empty() {
        return "idc".to_string();
    }
    "social".to_string()
}

/// 令牌类字段的别名表（`from_fields` 与 `normalize_paste` 共用一份）
const ACCESS_KEYS: [&str; 6] = [
    "accessToken",
    "access_token",
    "access",
    "apiKey",
    "api_key",
    "key",
];

/// 把「用户随手粘的一大段」归一成可落库的字段对象（纯函数，无网络）。
///
/// ── 认三种粘法（按**内容**判，不按字段名）────────────────────
///   1. 整段 `kiro-auth-token.json`（IDE 的 token 文件：accessToken /
///      refreshToken / expiresAt / region / profileArn）；
///   2. 一枚裸 `ksk_…` API key（官方「API key」页面复制的那枚）；
///   3. 只有 access + refresh 的两行 JSON。
/// 三者都可能落在任意一个字符串字段里（客户端把输入框内容原样放进 payload），
/// 所以这里扫所有字符串字段看有没有一段 JSON，解出来补进字段表 —— 与 Tabbit
/// 那条「按内容归一、不认字段名」同一纪律：粘贴场景里字段名是最不可靠的东西。
///
/// 返回的是一份**新对象**：上传请求专用的键（`provider` / `name` /
/// `importDesktop` / `authFile*`）全部剥掉 —— `add_kiro_account` 会保留未知
/// 字段，留着它们只会让账号库里多一批与凭证无关的东西。
pub fn normalize_paste(payload: &Value) -> Result<Value, GatewayError> {
    let Some(object) = payload.as_object() else {
        return Err(GatewayError::with_status(400, "上传内容不是 JSON 对象"));
    };
    let mut fields = Map::new();
    for (key, value) in object {
        let transport = matches!(key.as_str(), "provider" | "name" | "importDesktop")
            || key.starts_with("authFile");
        if !transport {
            fields.insert(key.clone(), value.clone());
        }
    }
    // ① 先把「用户显式给的那枚令牌」搬到落盘名 `accessToken`。
    //    这一步必须在解 JSON 之前：后面解析出来的那段用 `or_insert` 补字段，
    //    字段已存在就不会被覆盖 —— 「用户亲手填的」自动赢过「粘进来的那份」，
    //    不需要额外的优先级字段。
    if let Some(value) = first_text(&fields, &ACCESS_KEYS) {
        fields.insert("accessToken".to_string(), Value::String(value));
    }
    // ② 整段 JSON 粘在某个字符串字段里 → 解出来补进去
    let mut consumed: Vec<String> = Vec::new();
    for (key, value) in fields.clone() {
        let Some(raw) = value.as_str() else { continue };
        let trimmed = raw.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        // 解不出来就原样留着：它可能就是一段普通文本，缺凭据时那条 400
        // 会如实说「没认出凭证」，比在这里猜它是什么更好。
        if let Ok(Value::Object(inner)) = serde_json::from_str::<Value>(trimmed) {
            for (inner_key, inner_value) in inner {
                fields.entry(inner_key).or_insert(inner_value);
            }
            consumed.push(key);
        }
    }
    for key in consumed {
        fields.remove(&key);
    }
    // ③ 还是没令牌 → 只剩「一枚裸令牌粘在通用字段里」这一种可能
    //    （输入框只有一个 textarea，客户端把它塞进 `text` / `value` / `content`）
    if !fields
        .get("accessToken")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
    {
        for key in ["text", "value", "content", "paste", "token", "secret"] {
            let candidate = fields
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            if let Some(candidate) = candidate {
                fields.insert("accessToken".to_string(), Value::String(candidate));
                fields.remove(key);
                break;
            }
        }
    }
    Ok(Value::Object(fields))
}

impl KiroCredentials {
    /// 从账号记录（或任意字段对象）读凭证。
    ///
    /// 字段名以本项目落盘名为首选，同时兼容上游插件的旧写法
    /// （`access` / `refresh` / `expires` / `profile`）—— 从旧备份导入的
    /// 账号不该因为键名不同而变成「缺凭证」。
    pub fn from_fields(fields: &Value) -> Result<Self, GatewayError> {
        // 先读齐再判形态：`method_of` 需要看到 `clientId` / `tokenURL` 才能
        // 区分 idc / external-idp / social（见模块头）。
        let access = bare_token(&text(fields, &ACCESS_KEYS));
        let credentials = Self {
            method: method_of(fields, &access),
            access,
            refresh: text(fields, &["refreshToken", "refresh_token", "refresh"]),
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

    /// 形态推断按「手上有的字段」，四档各走对一条
    #[test]
    fn the_credential_style_is_inferred_from_the_fields_that_are_there() {
        // ① 显式写了就按它（哪怕前缀像是别的一类）
        let explicit = KiroCredentials::from_fields(&json!({
            "method": "idc",
            "accessToken": "ksk_looks_like_a_key",
            "refreshToken": "r1",
            "clientId": "c1",
            "clientSecret": "s1",
        }))
        .expect("应能读出来");
        assert_eq!(explicit.method, "idc");
        // ② 外部 IdP 的判据是 token 端点（只有这一档需要）
        let external = KiroCredentials::from_fields(&json!({
            "accessToken": "jwt",
            "refreshToken": "r1",
            "tokenURL": "https://idp.example.com/token",
        }))
        .expect("应能读出来");
        assert_eq!(external.method, "external-idp");
        assert_eq!(external.token_type(), Some("EXTERNAL_IDP"));
        // ③ AWS SSO OIDC：clientId + clientSecret 两者都要
        let idc = KiroCredentials::from_fields(&json!({
            "accessToken": "jwt",
            "refreshToken": "r1",
            "clientId": "c1",
            "clientSecret": "s1",
        }))
        .expect("应能读出来");
        assert_eq!(idc.method, "idc");
        // 只给 clientId（缺 secret）不足以判 idc —— 退回社交登录
        let half = KiroCredentials::from_fields(&json!({
            "accessToken": "jwt",
            "refreshToken": "r1",
            "clientId": "c1",
        }))
        .expect("应能读出来");
        assert_eq!(half.method, "social");
    }

    /// 粘贴带进来的 `Bearer ` 外壳要剥掉（开发者工具复制的是整行请求头）
    #[test]
    fn the_authorization_shell_is_stripped_from_a_pasted_token() {
        let key = KiroCredentials::from_fields(&json!({
            "accessToken": "Authorization: Bearer ksk_abcdefgh1234",
        }))
        .expect("应能读出来");
        assert_eq!(key.access, "ksk_abcdefgh1234");
        assert_eq!(key.method, "apikey");
    }

    /// 整段 `kiro-auth-token.json` 粘进任意字段都能认出来
    #[test]
    fn a_pasted_token_file_becomes_credential_fields() {
        let file = json!({
            "accessToken": "jwt-1",
            "refreshToken": "refresh-1",
            "expiresAt": 1_700_000_000_000i64,
            "region": "eu-central-1",
            "profileArn": "arn:aws:codewhisperer:eu-central-1:1:profile/A",
        })
        .to_string();
        let normalized = normalize_paste(&json!({
            "provider": "kiro",
            "name": "我的 Kiro",
            // 客户端把输入框内容原样放进来：字段名是谁都无所谓
            "credentials": file,
        }))
        .expect("应能归一");
        assert_eq!(
            normalized.get("accessToken").and_then(Value::as_str),
            Some("jwt-1")
        );
        assert_eq!(
            normalized.get("region").and_then(Value::as_str),
            Some("eu-central-1")
        );
        // 上传专用的键不该跟着落库，解析用的那段原文也该被吃掉
        assert!(normalized.get("provider").is_none());
        assert!(normalized.get("name").is_none());
        assert!(normalized.get("credentials").is_none());
        // 归一后的字段能直接过 `from_fields`
        let parsed = KiroCredentials::from_fields(&normalized).expect("应能读出来");
        assert_eq!(parsed.method, "social");
        assert!(parsed.can_refresh());
        assert_eq!(parsed.region(), "eu-central-1");
    }

    /// 一枚裸 key 粘在通用字段里也算数（输入框只有一个 textarea）
    #[test]
    fn a_bare_key_in_a_generic_field_is_accepted() {
        let normalized = normalize_paste(&json!({
            "provider": "kiro",
            "text": "  ksk_abcdefgh1234  ",
        }))
        .expect("应能归一");
        assert_eq!(
            normalized.get("accessToken").and_then(Value::as_str),
            Some("ksk_abcdefgh1234")
        );
        // 被消费掉的通用键不留在记录里
        assert!(normalized.get("text").is_none());
    }

    /// 显式填的字段比粘进来的那段 JSON 可信：不被覆盖
    #[test]
    fn explicit_fields_win_over_a_pasted_blob() {
        let normalized = normalize_paste(&json!({
            "provider": "kiro",
            "apiKey": "ksk_explicit",
            "text": json!({ "accessToken": "from-file" }).to_string(),
        }))
        .expect("应能归一");
        assert_eq!(
            normalized.get("accessToken").and_then(Value::as_str),
            Some("ksk_explicit"),
            "外层显式给的 apiKey 不该被粘进来的那段覆盖"
        );
    }
}
