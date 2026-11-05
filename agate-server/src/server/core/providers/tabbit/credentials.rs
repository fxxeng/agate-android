//! Tabbit 凭证：**网页 Cookie（含 JWT）** 的归一化与读取。
//!
//! ── 上游认什么 ──────────────────────────────────────────────
//! 上游要的是**浏览器的整串 Cookie**，不是裸令牌：
//! ```text
//!   Cookie: token=<JWT>; user_id=<uid>; SAPISID=<uid>; managed=tab_browser; NEXT_LOCALE=zh
//! ```
//! 只带 `token=` 会被判 401「无权限」（参考实现实测，见 `login.rs` 里补全那一段）。
//! 所以本模块的核心不是「取 token」，而是**保住用户浏览器里那一整份登录态**。
//!
//! ── 用户会粘什么（都要认，否则一半人加不进来）──────────────────
//! ```text
//!   Cookie 头单行       token=…; user_id=…
//!   DevTools 的 cURL     curl 'https://web.tabbit.com/…' -b 'token=…' -H '…'
//!   裸 JWT              eyJhbGciOi…
//!   JSON                {"cookie":"…","uid":"…"}
//! ```
//! `extract_cookie` / `normalize_input_text` 就是这两级「随手一段东西 → 可用凭证」
//! 的归一化，纯函数、可离线测（见本文件底部的用例）。
//!
//! ── 没有续期（本家最需要说清的一件事）────────────────────────
//! 登录态是 RS256 JWT、`expires_in: 604800`（7 天），上游**没有 refresh 端点**。
//! 因此这里只提供 `expires_at_ms`（从 JWT 的 `exp` 读），供账号行如实显示；
//! 不假装能自动续期（`supports_refresh() == false`，见 `adapter.rs`）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::collections::BTreeMap;

use serde_json::Value;

use crate::server::errors::GatewayError;

use super::sign;
use super::DEFAULT_CLIENT_VERSION;

/// 上游真正需要的几个 Cookie 键（其余一律不带：多余的键会让 Cookie 串超过
/// 上游的长度阈值，而且里面常混着别的站点的键）
const WANTED_COOKIES: [&str; 5] = ["token", "user_id", "managed", "NEXT_LOCALE", "SAPISID"];

/// Cookie 串的长度上限（上游没给明文阈值；32KB 是浏览器单站点 Cookie 的常见上限）
const MAX_COOKIE_LENGTH: usize = 32_768;
/// 令牌长度上限
const MAX_TOKEN_LENGTH: usize = 32_768;

/// 一份可用的 Tabbit 凭证。
#[derive(Clone, Debug)]
pub struct Credentials {
    /// Cookie 里的 JWT（同时是 `Authorization: Bearer` 的值）
    pub token: String,
    /// 过滤后的 Cookie 请求头（**整串**，不是只有 token）
    pub cookie: String,
    /// 上游 user_id（额度接口必填、设备指纹要用）
    pub uid: String,
    /// 是否发送「已设为默认浏览器」标记（解锁 Premium 模型）
    pub unlock_pro: bool,
    /// 客户端版本号（`x-req-ctx` 的明文），空串表示用默认值
    pub client_version: String,
}

impl Credentials {
    /// 从账号记录（或任意字段对象）读凭证。
    ///
    /// 字段名以**本项目落盘名**为首选，同时兼容上游 / 用户粘贴的几种写法：
    /// `accessToken` / `access_token` / `token`、`uid` / `user_id` / `userId`。
    pub fn from_fields(fields: &Value) -> Result<Self, GatewayError> {
        let mut token = pick(fields, &["accessToken", "access_token", "token"]);
        let raw_cookie = pick(fields, &["cookie"]);
        let mut uid = pick(fields, &["uid", "user_id", "userId"]);
        let unlock_pro = bool_field(fields, "unlockPro", true);
        let client_version = pick(fields, &["clientVersion"]);

        let extracted = extract_cookie(&raw_cookie);
        let map = cookie_map(&extracted);
        if token.is_empty() {
            token = map.get("token").cloned().unwrap_or_default();
        }
        if uid.is_empty() {
            uid = map.get("user_id").cloned().unwrap_or_default();
        }
        if uid.is_empty() && !token.is_empty() {
            uid = uid_from_jwt(&token);
        }
        if token.is_empty() {
            return Err(GatewayError::with_status(
                400,
                "Tabbit 账号缺少登录令牌（Cookie 里要有 token=…）",
            ));
        }
        let cookie = if raw_cookie.trim().is_empty() {
            synthetic_cookie(&token, &uid)
        } else if map.contains_key("token") {
            cookie_header(&map)
        } else {
            // 用户只粘了额外 Cookie、令牌在 `token` 字段里：补进去再过滤
            let mut merged = map;
            merged.insert("token".to_string(), token.clone());
            cookie_header(&merged)
        };
        if cookie.is_empty() {
            return Err(GatewayError::with_status(
                400,
                "Tabbit Cookie 里没有可用的登录项（需要 token=…）",
            ));
        }
        if cookie.chars().count() > MAX_COOKIE_LENGTH || cookie.contains(['\r', '\n']) {
            return Err(GatewayError::with_status(400, "Tabbit Cookie 格式无效"));
        }
        Ok(Self {
            token,
            cookie,
            uid,
            unlock_pro,
            client_version,
        })
    }

    /// 从账号记录读凭证（记录不存在时给出「还没有账号」的可读错误）
    pub fn from_record(record: Option<&Value>) -> Result<Self, GatewayError> {
        let record = record.ok_or_else(|| {
            GatewayError::with_status(
                404,
                "还没有 Tabbit 账号：请先在「添加账号」里粘贴网页 Cookie",
            )
        })?;
        Self::from_fields(record)
    }

    /// 客户端版本号（空串回落默认值）
    pub fn version(&self) -> &str {
        if self.client_version.trim().is_empty() {
            DEFAULT_CLIENT_VERSION
        } else {
            self.client_version.trim()
        }
    }

    /// 账号展示用的脱敏尾巴
    pub fn tail(&self) -> String {
        sign::token_tail(&self.token)
    }
}

/// 从 JWT 的 `exp` 读过期时刻（毫秒）；不可解析时 None。
///
/// 只用来**显示**有效期：上游没有续期接口，这个值到期就是「该重新导出 Cookie」了。
pub fn expires_at_ms(token: &str) -> Option<i64> {
    let claims = crate::server::core::providers::raccoon::jwt::decode_jwt_claims(token)?;
    let exp = claims.get("exp").and_then(as_i64)?;
    if exp <= 0 {
        return None;
    }
    Some(exp * 1000)
}

/// JSON 数字 / 数字字符串 → i64（上游的 `exp` 有时是字符串）
fn as_i64(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    if let Some(number) = value.as_f64() {
        return Some(number as i64);
    }
    value.as_str().and_then(|text| text.trim().parse::<i64>().ok())
}

/// 从「随手复制的一段东西」里挖出 Cookie 串：DevTools 的 Copy as cURL、
/// `Cookie:` 单行、`-b '…'`、或直接粘 cookie 都能认。
pub fn extract_cookie(raw: &str) -> String {
    let text = raw.trim();
    if text.is_empty() {
        return String::new();
    }
    let lower = text.to_ascii_lowercase();
    // ① `Cookie: …` 单行（Copy as cURL 里也常带这一行）
    if let Some(index) = lower.find("cookie:") {
        let rest = &text[index + "cookie:".len()..];
        let end = rest
            .find(|ch| matches!(ch, '"' | '\'' | '\r' | '\n'))
            .unwrap_or(rest.len());
        let value = rest[..end].trim();
        if value.contains('=') {
            return value.to_string();
        }
    }
    // ② `-b 'cookie'` / `-b cookie`
    let marker = lower.find(" -b ").map(|index| index + 4).or_else(|| {
        if lower.starts_with("-b ") {
            Some(3)
        } else {
            None
        }
    });
    if let Some(start) = marker {
        let rest = text[start..].trim_start();
        let rest = rest.strip_prefix("-b ").unwrap_or(rest).trim_start();
        let value = if let Some(quote) = rest.chars().next().filter(|ch| *ch == '\'' || *ch == '"') {
            let body = &rest[quote.len_utf8()..];
            body.split(quote).next().unwrap_or(body)
        } else {
            rest.split(char::is_whitespace).next().unwrap_or(rest)
        };
        let value = value.trim();
        if value.contains('=') {
            return value.to_string();
        }
    }
    text.to_string()
}

/// Cookie 串 → 键值表（同名键后者覆盖前者）
fn cookie_map(cookie: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for part in cookie.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let mut pieces = part.splitn(2, '=');
        let name = pieces.next().unwrap_or("").trim();
        let value = pieces.next().unwrap_or("").trim();
        if !name.is_empty() && !value.is_empty() {
            map.insert(name.to_string(), value.to_string());
        }
    }
    map
}

/// 键值表 → 上游要的那几个键（顺序固定：`token` 在最前，与浏览器一致）
fn cookie_header(map: &BTreeMap<String, String>) -> String {
    let mut parts = Vec::new();
    for name in WANTED_COOKIES {
        if let Some(value) = map.get(name).filter(|value| !value.is_empty()) {
            parts.push(format!("{name}={value}"));
        }
    }
    parts.join("; ")
}

/// 只有令牌时**拼一份**浏览器形态的 Cookie。
///
/// 这几项不是装饰：参考实现实测「只带 token 会被判 401」。知道 uid 就把
/// `user_id` 与 `SAPISID` 一起补上，浏览器里那串就是这样。
pub fn synthetic_cookie(token: &str, uid: &str) -> String {
    let mut parts = vec![format!("token={token}")];
    if !uid.is_empty() {
        parts.push(format!("user_id={uid}"));
        parts.push(format!("SAPISID={uid}"));
    }
    parts.push("managed=tab_browser".to_string());
    parts.push("NEXT_LOCALE=zh".to_string());
    parts.join("; ")
}

/// Cookie 串里 `token=` 的值
pub fn cookie_token(cookie: &str) -> String {
    cookie_map(cookie).get("token").cloned().unwrap_or_default()
}

/// 把一段文本归一成「(令牌, Cookie, uid)」。
///
/// 返回的 `cookie` 可能是空串（用户只给了裸 JWT）—— 调用方随后要按
/// [`synthetic_cookie`] 补齐，那一步需要先探一次上游拿到 uid。
pub fn normalize_input_text(text: &str) -> (String, String) {
    let trimmed = text.trim();
    if let Some(value) = serde_json::from_str::<Value>(trimmed).ok() {
        if value.is_object() {
            let token = pick(&value, &["accessToken", "access_token", "token"]);
            let cookie = pick(&value, &["cookie"]);
            let token = if token.is_empty() {
                cookie_token(&extract_cookie(&cookie))
            } else {
                token
            };
            return (token, cookie);
        }
    }
    let cookie = extract_cookie(trimmed);
    let token = cookie_token(&cookie);
    if token.is_empty() && !trimmed.contains('=') {
        // 裸 JWT（可能带 `Bearer ` 前缀）。**Cookie 回空串**，不是把这段原文
        // 当 Cookie 带出去 —— 上游只认 `token=…` 形态的 Cookie 串，把 JWT 原文
        // 塞进 Cookie 头会被判 401；调用方（`from_fields` / `api::accounts`）
        // 拿到空 cookie 后用 `synthetic_cookie` 补齐，那一步需要先知道 uid，
        // 所以只能由它们来做。
        return (
            trimmed
                .trim_start_matches("Bearer ")
                .trim()
                .trim_start_matches("bearer ")
                .trim()
                .to_string(),
            String::new(),
        );
    }
    (token, cookie)
}

/// 从 JWT 的声明里读 uid（`user_id` / `uid` / `sub` / `userId`）
pub fn uid_from_jwt(token: &str) -> String {
    let Some(claims) = crate::server::core::providers::raccoon::jwt::decode_jwt_claims(token) else {
        return String::new();
    };
    ["user_id", "uid", "sub", "userId"]
        .iter()
        .find_map(|key| claims.get(*key).and_then(Value::as_str))
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 从 JWT 的声明里读昵称（账号备注名的兜底）
pub fn nickname_from_jwt(token: &str) -> String {
    let Some(claims) = crate::server::core::providers::raccoon::jwt::decode_jwt_claims(token) else {
        return String::new();
    };
    ["name", "display_name", "nickname", "user_name"]
        .iter()
        .find_map(|key| claims.get(*key).and_then(Value::as_str))
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 取第一个非空字符串（本项目落盘名优先）
pub fn pick(value: &Value, keys: &[&str]) -> String {
    for key in keys {
        let text = value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if !text.is_empty() {
            return text.to_string();
        }
    }
    String::new()
}

/// 宽松布尔：`false` / `0` / `off` / `no` / `关闭` / `否` 为假，其余按默认值
fn bool_field(value: &Value, key: &str, default: bool) -> bool {
    match value.get(key) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::String(text)) => {
            let text = text.trim().to_ascii_lowercase();
            if ["0", "false", "off", "no", "关闭", "否"].contains(&text.as_str()) {
                false
            } else if ["1", "true", "on", "yes", "开启", "是"].contains(&text.as_str()) {
                true
            } else {
                default
            }
        }
        Some(Value::Number(number)) => number.as_i64().unwrap_or(0) != 0,
        _ => default,
    }
}

/// 形态检查（添加账号时先用它挡一次明显粘错的东西）
pub fn shape_ok(token: &str) -> bool {
    let trimmed = token.trim();
    !trimmed.is_empty()
        && trimmed.chars().count() <= MAX_TOKEN_LENGTH
        && !trimmed.contains(['\r', '\n', ' '])
}

/// 日志里用的脱敏形态（只留尾 6 位）
pub fn masked(token: &str) -> String {
    let chars: Vec<char> = token.trim().chars().collect();
    if chars.len() <= 8 {
        return "JWT…".to_string();
    }
    let tail: String = chars[chars.len() - 6..].iter().collect();
    format!("JWT…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 直接粘一整串 Cookie：只保留上游要的 5 个键，顺序固定
    #[test]
    fn plain_cookie_is_filtered_to_the_keys_upstream_needs() {
        let creds = Credentials::from_fields(&json!({
            "cookie": "token=abc; user_id=u1; SAPISID=u1; managed=tab_browser; NEXT_LOCALE=zh; \
                        other=drop-me; ga=also-drop"
        }))
        .expect("完整 Cookie 应能解析");
        assert_eq!(creds.token, "abc");
        assert_eq!(creds.uid, "u1");
        assert_eq!(
            creds.cookie,
            "token=abc; user_id=u1; managed=tab_browser; NEXT_LOCALE=zh; SAPISID=u1"
        );
        assert!(!creds.cookie.contains("other"));
        assert!(!creds.cookie.contains("ga="));
    }

    /// cURL 整段里挖 Cookie（`-b '…'`）
    #[test]
    fn curl_command_yields_the_cookie() {
        let raw = "curl 'https://web.tabbit.com/api/v0/user/base-info' \\\n  \
                   -H 'accept: application/json' \\\n  -b 'token=jwt-token; user_id=42'";
        let cookie = extract_cookie(raw);
        assert_eq!(cookie, "token=jwt-token; user_id=42");
    }

    /// `Cookie:` 单行也能认（DevTools 里最常复制到的东西）
    #[test]
    fn cookie_header_line_yields_the_cookie() {
        let raw = "Cookie: token=jwt-token; user_id=42; managed=tab_browser";
        assert_eq!(
            extract_cookie(raw),
            "token=jwt-token; user_id=42; managed=tab_browser"
        );
    }

    /// 裸 JWT：Cookie 用合成的，uid 留空（后续探活补）
    #[test]
    fn bare_jwt_becomes_a_synthetic_cookie() {
        let creds = Credentials::from_fields(&json!({ "token": "eyJhbGciOi.abc.def" }))
            .expect("裸 JWT 应能解析");
        assert_eq!(creds.token, "eyJhbGciOi.abc.def");
        assert!(creds.cookie.starts_with("token=eyJhbGciOi.abc.def"));
        assert!(creds.cookie.contains("managed=tab_browser"));
        // 不知道 uid 时不编一个 user_id 出来
        assert!(!creds.cookie.contains("user_id="));
    }

    /// 只给令牌 + uid：合成的 Cookie 必须把 user_id / SAPISID 补上
    /// （参考实现实测：只带 token 会被判 401）
    #[test]
    fn synthetic_cookie_carries_uid_when_known() {
        let cookie = synthetic_cookie("jwt", "u-9");
        assert_eq!(
            cookie,
            "token=jwt; user_id=u-9; SAPISID=u-9; managed=tab_browser; NEXT_LOCALE=zh"
        );
    }

    /// 完全没令牌 → 400 且文案说清要什么（不是「未知错误」）
    #[test]
    fn missing_token_is_a_readable_400() {
        let error = Credentials::from_fields(&json!({ "cookie": "managed=tab_browser" }))
            .expect_err("没有 token 必须报错");
        assert_eq!(error.status_code, 400);
        assert!(error.message.contains("token="));
    }

    /// `unlockPro` 的宽松布尔：字符串 "0" / "false" 都算关
    #[test]
    fn unlock_pro_accepts_loose_booleans() {
        for value in [json!("0"), json!("false"), json!(0), json!(false)] {
            let creds = Credentials::from_fields(&json!({ "token": "jwt", "unlockPro": value }))
                .expect("令牌合法就应解析成功");
            assert!(!creds.unlock_pro, "unlockPro={value} 应判为关闭");
        }
        // 缺省开启（与导入路径的默认值一致）
        let creds = Credentials::from_fields(&json!({ "token": "jwt" })).expect("缺省应解析成功");
        assert!(creds.unlock_pro);
    }

    /// 客户端版本号缺省回落常量（`x-req-ctx` 不能是空串 —— 上游会判版本过低）
    #[test]
    fn client_version_falls_back_to_the_constant() {
        let creds = Credentials::from_fields(&json!({ "token": "jwt" })).expect("缺省应解析成功");
        assert_eq!(creds.version(), DEFAULT_CLIENT_VERSION);
        let custom = Credentials::from_fields(&json!({
            "token": "jwt",
            "clientVersion": "1.18.0(10118000)"
        }))
        .expect("自定义版本应解析成功");
        assert_eq!(custom.version(), "1.18.0(10118000)");
    }

    /// 含换行的 Cookie 一律拒绝（HTTP 头注入形态，不是「格式宽松点」能救的）
    #[test]
    fn cookie_with_newlines_is_rejected() {
        let error = Credentials::from_fields(&json!({ "cookie": "token=a\r\nX-Injected: 1" }))
            .expect_err("换行必须拒绝");
        assert_eq!(error.status_code, 400);
    }

    /// 归一化：JSON / Cookie 串 / 裸 JWT 三条路都要能出令牌
    #[test]
    fn normalize_accepts_json_cookie_and_bare_jwt() {
        let (token, cookie) = normalize_input_text(r#"{"cookie":"token=from-json; user_id=u1"}"#);
        assert_eq!(token, "from-json");
        assert_eq!(cookie, "token=from-json; user_id=u1");

        let (token, _) = normalize_input_text("token=plain; user_id=u2");
        assert_eq!(token, "plain");

        let (token, cookie) = normalize_input_text("Bearer eyJhbGciOi.payload.sig");
        assert_eq!(token, "eyJhbGciOi.payload.sig");
        assert_eq!(cookie, "");
    }

    /// `shape_ok` 挡掉空串 / 带空白 / 带换行的令牌
    #[test]
    fn shape_check_rejects_obvious_mistakes() {
        assert!(shape_ok("eyJhbGciOi.abc.def"));
        assert!(!shape_ok(""));
        assert!(!shape_ok("a b"));
        assert!(!shape_ok("a\nb"));
    }
}
