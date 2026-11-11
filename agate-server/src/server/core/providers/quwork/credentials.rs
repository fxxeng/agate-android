//! QuWork 凭证：粘贴解析、身份、公开视图。
//!
//! ── 去桌面化 ────────────────────────────────────────────────
//! 只搬上游 `accounts.rs::parse` / `parse_text` 的**协议逻辑**：JSON、纯
//! `llmToken` 文本、`.credentials.yaml` 里的 `QURIVS_LLM_API_KEY:` 行、
//! `secret/session/data` 与 `credentials` 嵌套。`desktop.rs` 的读盘、
//! `login.rs` 的设备码、`benefits.rs` 的设备标识一律不搬。
//!
//! ── 两套凭据各管一段（命名刻意与本仓区分）──────────────────────
//! ```text
//!   llmToken     模型凭证，必需；Authorization: Bearer {llmToken}
//!   loginToken   登录凭证（上游叫 accessToken），可选；账号服务刷新 / 积分 / 签到
//! ```
//! ⚠️ 上游把登录凭证叫 `accessToken`，而本仓 `account_store` 的 `accessToken`
//! 是**转发凭证**（`session_from_record` 把它放进 `auth.accessToken`）。若把
//! 上游字段原样落库，转发会拿登录凭证当模型凭证 → 稳定 401。因此模块内部把
//! 登录凭证统一叫 `loginToken`；落库时由 `quwork_accounts.rs` 把 `llmToken`
//! **镜像**到本仓的 `accessToken` 键供转发使用。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use base64::Engine as _;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::server::errors::GatewayError;

use super::client;
use super::endpoints;

/// 粘贴内容上限（上游 `MAX_CREDENTIAL_BYTES`）
pub const MAX_CREDENTIAL_BYTES: usize = 262_144;
/// 单枚令牌上限（上游 `token()`）
const MAX_TOKEN_BYTES: usize = 16_384;
/// 展示名上限
const MAX_NAME_BYTES: usize = 256;

/// 登录凭证别名（第一个是本仓落库名，其余是上游 / 桌面端形态）
const LOGIN_KEYS: [&str; 4] = ["loginToken", "accessToken", "access_token", "token"];
/// 模型凭证别名
const LLM_KEYS: [&str; 4] = ["llmToken", "llm_token", "apiKey", "key"];
/// 粘贴文本可能落在的通用键
const TEXT_KEYS: [&str; 6] = ["text", "credentials", "value", "content", "paste", "llmToken"];

/// 取第一个非空的字符串字段
pub fn text(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| {
            value
                .get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
        })
        .unwrap_or("")
        .to_string()
}

/// 用户信息的公开子集（只保留标识，不带任何凭据）
fn user_view(user: &Value) -> Value {
    let mut public = json!({});
    for (target, keys) in [
        ("uid", &["id", "uid", "userId"][..]),
        ("username", &["username"][..]),
        ("nickname", &["display_name", "displayName", "nickname"][..]),
        ("phone", &["phone"][..]),
    ] {
        if let Some(value) = keys.iter().find_map(|key| {
            user.get(*key)
                .filter(|value| value.is_string() || value.is_number())
        }) {
            public[target] = if value.is_string() {
                value.clone()
            } else {
                json!(value.to_string())
            };
        }
    }
    public
}

/// 令牌校验：去 `Bearer ` 外壳，要求 ASCII、无空白 / 控制字符、不超长。
///
/// 只剥**两侧普通空格**：换行 / 制表符是粘贴事故（`abc\n` 这种），
/// 静默 trim 掉就等于把一个「不知道从哪粘来的东西」当令牌收下 —— 报错让用户重贴。
pub fn token(value: &str, label: &str) -> Result<String, GatewayError> {
    let value = value.trim_matches(' ');
    let value = value.strip_prefix("Bearer ").unwrap_or(value).trim_matches(' ');
    if value.len() > MAX_TOKEN_BYTES
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(GatewayError::with_status(
            400,
            format!("QuWork {label}格式不正确"),
        ));
    }
    Ok(value.to_string())
}

/// 从账号记录 / 任意字段对象解析凭证（展开嵌套、校验 llmToken）。
pub fn parse(value: &Value) -> Result<Value, GatewayError> {
    parse_inner(value, 0)
}

fn parse_inner(value: &Value, depth: usize) -> Result<Value, GatewayError> {
    if depth > 6 {
        return Err(GatewayError::with_status(400, "QuWork 凭证嵌套过深"));
    }
    // 上游会话库把真正的 session 放在 secret/session/data 之一
    let mut input = value.clone();
    for key in ["secret", "session", "data"] {
        if let Some(nested) = input.get(key).filter(|value| value.is_object()) {
            input = nested.clone();
        }
    }
    // llmToken 本身可能是一段 JSON 字符串（上游 `nested_llm`）
    let nested_llm = input
        .get("llmToken")
        .and_then(Value::as_str)
        .filter(|text| text.trim().starts_with('{'));
    let mut parsed = if let Some(content) = input
        .get("credentials")
        .and_then(Value::as_str)
        .or(nested_llm)
    {
        parse_text_inner(content, depth + 1)?
    } else {
        json!({})
    };
    let login = token(&text(&input, &LOGIN_KEYS), "登录凭证")?;
    let llm = if nested_llm.is_some() {
        String::new()
    } else {
        token(&text(&input, &LLM_KEYS), "模型凭证")?
    };
    if !login.is_empty() {
        parsed["loginToken"] = json!(login);
    }
    if !llm.is_empty() && !llm.starts_with('{') {
        parsed["llmToken"] = json!(llm);
    }
    if let Some(user) = input.get("user").filter(|value| value.is_object()) {
        parsed["user"] = user_view(user);
    } else if input.get("uid").is_some() {
        parsed["user"] = user_view(&input);
    }
    let expires = client::timestamp(input.get("expiresAt"))
        .or_else(|| client::timestamp(input.get("expire")))
        .or_else(|| jwt_expiry(&text(&parsed, &LOGIN_KEYS)));
    if let Some(expires) = expires {
        parsed["expiresAt"] = json!(expires);
    }
    let name = text(&input, &["name"]);
    if !name.is_empty() {
        if name.len() > MAX_NAME_BYTES || name.chars().any(char::is_control) {
            return Err(GatewayError::with_status(400, "QuWork 账号字段格式不正确"));
        }
        parsed["name"] = json!(name);
    }
    if text(&parsed, &["llmToken"]).is_empty() {
        return Err(GatewayError::with_status(
            400,
            "缺少 QuWork 模型凭证 llmToken；登录凭证不能代替模型凭证，请粘贴完整凭证 JSON 或模型凭证",
        ));
    }
    Ok(parsed)
}

/// 把「用户随手粘的一大段」解析成凭证对象。
///
/// 认三种粘法（照上游 `parse_text`）：JSON 对象 / `.credentials.yaml` 里的
/// `QURIVS_LLM_API_KEY:` 行 / 纯 `llmToken` 文本。
pub fn parse_text(content: &str) -> Result<Value, GatewayError> {
    parse_text_inner(content, 0)
}

fn parse_text_inner(content: &str, depth: usize) -> Result<Value, GatewayError> {
    if depth > 6 {
        return Err(GatewayError::with_status(400, "QuWork 凭证嵌套过深"));
    }
    if content.len() > MAX_CREDENTIAL_BYTES {
        return Err(GatewayError::with_status(
            400,
            "QuWork 凭证内容超过大小限制",
        ));
    }
    let content = content.trim().trim_start_matches('\u{feff}').trim();
    if content.starts_with('{') {
        let value: Value = serde_json::from_str(content)
            .map_err(|_| GatewayError::with_status(400, "QuWork 凭证 JSON 格式不正确"))?;
        return parse_inner(&value, depth + 1);
    }
    if content
        .lines()
        .any(|line| line.trim_start().starts_with("QURIVS_LLM_API_KEY:"))
    {
        let value = content
            .lines()
            .find_map(|line| line.trim().strip_prefix("QURIVS_LLM_API_KEY:"))
            .unwrap_or("")
            .trim();
        let value = if value.starts_with('"') {
            serde_json::from_str::<String>(value)
                .map_err(|_| GatewayError::with_status(400, "QuWork 模型凭证格式不正确"))?
        } else {
            value.trim_matches('\'').to_string()
        };
        return parse_inner(&json!({ "llmToken": value }), depth + 1);
    }
    parse_inner(&json!({ "llmToken": content }), depth + 1)
}

/// 从 JWT 的 `exp` 兜底出过期时间（accessToken 没带 expiresAt 时）
pub fn jwt_expiry(token: &str) -> Option<i64> {
    let part = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(part.trim_end_matches('='))
        .ok()?;
    let body: Value = serde_json::from_slice(&bytes).ok()?;
    client::timestamp(body.get("exp"))
}

/// 稳定身份：`quwork:{uid}`，否则 `quwork:key:{llmToken 摘要}`。
///
/// 摘要只取前 16 位十六进制，绝不在 id 里带令牌明文（本仓没有上游
/// `flight::fingerprint`，用同一条 sha2 链自己算）。
pub fn identity(secret: &Value) -> String {
    let user = secret.get("user").unwrap_or(&Value::Null);
    let uid = text(user, &["uid", "id"]);
    if !uid.is_empty() {
        return format!("quwork:{uid}");
    }
    let key = text(secret, &["llmToken", "modelFingerprint"]);
    if key.is_empty() {
        return String::new();
    }
    if secret.get("modelFingerprint").is_some() && secret.get("llmToken").is_none() {
        format!("quwork:key:{key}")
    } else {
        format!("quwork:key:{}", fingerprint(&key))
    }
}

fn fingerprint(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 公开视图：**不带任何凭据**，只给 name / uid / phone / hasToken /
/// tokenTail / expiresAt（外加两个布尔能力位 canRefresh / hasAccessToken）。
pub fn public_view(secret: &Value) -> Value {
    let user = secret.get("user").unwrap_or(&Value::Null);
    let nickname = text(
        user,
        &[
            "nickname",
            "display_name",
            "displayName",
            "username",
            "phone",
            "uid",
        ],
    );
    let name = text(secret, &["name"]);
    let llm = text(secret, &["llmToken"]);
    let login = text(secret, &["loginToken"]);
    let tail: String = llm
        .chars()
        .rev()
        .take(6)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    let mut view = json!({
        "name": if name.is_empty() {
            if nickname.is_empty() { "QuWork" } else { nickname.as_str() }
        } else {
            name.as_str()
        },
        "uid": text(user, &["uid", "id"]),
        "phone": text(user, &["phone"]),
        "hasToken": !llm.is_empty(),
        "tokenTail": tail,
        "canRefresh": !login.is_empty(),
        "hasAccessToken": !login.is_empty(),
    });
    if let Some(expires) = secret.get("expiresAt").filter(|value| value.is_number()) {
        view["expiresAt"] = expires.clone();
    }
    view
}

/// 模型凭证（对话用），空则明确报错
pub fn token_of(secret: &Value) -> Result<String, GatewayError> {
    let token = text(secret, &["llmToken"]);
    if token.is_empty() {
        return Err(GatewayError::with_status(
            400,
            "QuWork 缺少模型凭证 llmToken，请重新粘贴账号",
        ));
    }
    Ok(token)
}

/// 登录凭证（账号服务用），可为空
pub fn login_token_of(secret: &Value) -> String {
    text(secret, &["loginToken"])
}

fn is_transport(key: &str) -> bool {
    matches!(key, "provider" | "name" | "importDesktop" | "inputs") || key.starts_with("authFile")
}

/// 本仓的粘贴入口：把 `POST /api/accounts` 的 payload 归一成内部 secret。
///
/// 客户端把整段凭证放在 `text`（JSON / 纯 token / yaml 行），可选的登录凭证
/// 放在 `accessToken`（见 `AccountOps.addAccountFor` 的 quwork 分支）。也认
/// `{ inputs: {…} }` 包装与顶层直接给字段的形态。
pub fn normalize_paste(payload: &Value) -> Result<Value, GatewayError> {
    let Some(object) = payload.as_object() else {
        return Err(GatewayError::with_status(400, "上传内容不是 JSON 对象"));
    };
    let mut fields = Map::new();
    for (key, value) in object {
        if !is_transport(key) {
            fields.insert(key.clone(), value.clone());
        }
    }
    // 动态登录的 `{ inputs: {…} }` 包装
    if let Some(Value::Object(inputs)) = object.get("inputs") {
        for (key, value) in inputs {
            fields.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    // 整段 JSON 粘在某个字符串字段里 → 解出来补进去（显式字段优先）
    let mut consumed: Vec<String> = Vec::new();
    for (key, value) in fields.clone() {
        if let Some(raw) = value.as_str() {
            if raw.trim().starts_with('{') {
                if let Ok(Value::Object(inner)) = serde_json::from_str::<Value>(raw.trim()) {
                    for (inner_key, inner_value) in inner {
                        fields.entry(inner_key).or_insert(inner_value);
                    }
                    consumed.push(key);
                }
            }
        }
    }
    for key in consumed {
        fields.remove(&key);
    }
    // 还没有 llmToken → 从文本字段按内容判（JSON / yaml 行 / 纯 token）
    let has_llm = fields
        .get("llmToken")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    if !has_llm {
        for key in TEXT_KEYS {
            let Some(raw) = fields.get(key).and_then(Value::as_str) else {
                continue;
            };
            let parsed = parse_text(raw)?;
            if let Some(inner) = parsed.as_object() {
                for (inner_key, inner_value) in inner {
                    fields
                        .entry(inner_key.clone())
                        .or_insert_with(|| inner_value.clone());
                }
            }
            fields.remove(key);
            break;
        }
    }
    // 独立填的登录凭证（secondary）→ 补进 loginToken，不覆盖已解析出的值
    if let Some(access) = object.get("accessToken").and_then(Value::as_str) {
        let access = token(access, "登录凭证")?;
        if !access.is_empty() {
            fields
                .entry("loginToken".to_string())
                .or_insert(Value::String(access));
        }
    }
    parse(&Value::Object(fields))
}

/// 续期窗口判据（纯函数，供 refresh / credentials_expiring / 单测共用）。
///
/// 上游 `refresh.rs::expiring`：只有登录凭证存在、且 expiresAt 进入 1 小时
/// 窗口才算临期；没有 accessToken 的「仅模型凭证」账号永不触发续期。
pub fn expiring(secret: &Value) -> bool {
    !login_token_of(secret).is_empty()
        && client::timestamp(secret.get("expiresAt"))
            .is_some_and(|expiry| expiry <= crate::server::logging::now_ms() + endpoints::REFRESH_WINDOW_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_nested_secret_and_credentials_json() {
        // secret/session/data 嵌套 + credentials 里再包一段 JSON（上游同款形态）
        let credentials = json!({
            "llmToken": "llm-abcdef",
            "accessToken": "login-xyz",
            "uid": "42"
        })
        .to_string();
        let input = json!({
            "secret": { "session": { "data": {
                "credentials": credentials,
                "expiresAt": 1_800_000_000_000_i64,
                "user": { "id": "42", "phone": "13800000000" }
            } } }
        });
        let parsed = parse(&input).unwrap_or(Value::Null);
        assert_eq!(
            parsed.get("llmToken").and_then(Value::as_str),
            Some("llm-abcdef")
        );
        assert_eq!(
            parsed.get("loginToken").and_then(Value::as_str),
            Some("login-xyz")
        );
        assert_eq!(
            parsed.pointer("/user/uid").and_then(Value::as_str),
            Some("42")
        );
        assert_eq!(
            parsed.pointer("/user/phone").and_then(Value::as_str),
            Some("13800000000")
        );
    }

    #[test]
    fn parse_rejects_credentials_without_llm_token() {
        // 只有登录凭证：上游明确「登录凭证不能代替模型凭证」
        let only_login = json!({ "accessToken": "login-xyz", "uid": "42" });
        let err = parse(&only_login).err().unwrap_or(GatewayError::new(""));
        assert_eq!(err.status_code, 400);
        assert!(err.message.contains("llmToken"));
    }

    #[test]
    fn parse_text_reads_qurivs_yaml_line_and_bare_token() {
        let yaml = "QURIVS_LLM_API_KEY: \"llm-from-yaml\"\nother: 1";
        let parsed = parse_text(yaml).unwrap_or(Value::Null);
        assert_eq!(
            parsed.get("llmToken").and_then(Value::as_str),
            Some("llm-from-yaml")
        );
        let bare = parse_text("llm-bare-token").unwrap_or(Value::Null);
        assert_eq!(
            bare.get("llmToken").and_then(Value::as_str),
            Some("llm-bare-token")
        );
    }

    #[test]
    fn normalize_paste_merges_text_and_secondary_access_token() {
        let payload = json!({
            "provider": "quwork",
            "name": "我的 QuWork",
            "text": "{\"llmToken\":\"llm-1\",\"user\":{\"id\":\"u-9\"}}",
            "accessToken": "login-1",
        });
        let parsed = normalize_paste(&payload).unwrap_or(Value::Null);
        assert_eq!(parsed.get("llmToken").and_then(Value::as_str), Some("llm-1"));
        assert_eq!(parsed.get("loginToken").and_then(Value::as_str), Some("login-1"));
        assert!(parsed.get("provider").is_none());
        assert!(parsed.get("name").is_none());
    }

    #[test]
    fn token_rejects_whitespace_control_and_non_ascii() {
        assert_eq!(token("  abc.def  ", "模型凭证").unwrap_or_default(), "abc.def");
        assert_eq!(token("Bearer abc.def", "模型凭证").unwrap_or_default(), "abc.def");
        assert!(token("abc def", "模型凭证").is_err());
        assert!(token("abc\n", "模型凭证").is_err());
        assert!(token("令牌", "模型凭证").is_err());
    }

    #[test]
    fn public_view_never_carries_a_credential() {
        let secret = json!({
            "llmToken": "llm-1234567890",
            "loginToken": "login-secret",
            "user": { "uid": "u-1", "phone": "139" },
            "expiresAt": 1_800_000_000_000_i64,
        });
        let view = public_view(&secret);
        assert!(view.get("llmToken").is_none());
        assert!(view.get("loginToken").is_none());
        assert_eq!(view.get("hasToken"), Some(&Value::Bool(true)));
        assert_eq!(view.get("tokenTail").and_then(Value::as_str), Some("567890"));
        assert_eq!(view.get("uid").and_then(Value::as_str), Some("u-1"));
        assert_eq!(view.get("phone").and_then(Value::as_str), Some("139"));
    }

    #[test]
    fn identity_prefers_uid_and_hides_the_token() {
        let with_uid = json!({ "llmToken": "llm-1", "user": { "uid": "u-1" } });
        assert_eq!(identity(&with_uid), "quwork:u-1");
        let key_only = json!({ "llmToken": "llm-abcdef" });
        let id = identity(&key_only);
        assert!(id.starts_with("quwork:key:"));
        assert!(!id.contains("llm-abcdef"));
    }
}
