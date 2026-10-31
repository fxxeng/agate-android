//! LobsterAI 凭证：**uuid 与两个 keyfrom 是载荷件，丢了账号就永远续不了期**。
//!
//! ── 为什么这一家的凭证字段比别家多 ──────────────────────────────
//! 上游的续期接口不认「只给 refresh token」：它要的是一组身份字段再加 refresh
//! token（一手来源：插件仓库 `lobsterai/credentials.rs` 的模块头）：
//!
//! ```text
//!   { firstKeyfrom, latestKeyfrom, version, [uuid], [userId], refreshToken }
//! ```
//!
//! 而 `uuid` 与那两个 keyfrom 只在**换码那一刻**下发，此后再也拿不到。所以落
//! 账号时**必须把它们一起存下来** —— 只存两个 token 的账号，看起来能用到 access
//! 过期，之后既续不了期也修不回来（只能重新登录）。
//!
//! `latest_keyfrom` 还额外有个「故意不更新」的口径：续期响应里即使带了新的
//! keyfrom 也不覆盖它，永远保持登录时那一份 —— 这是唯一经过生产验证的形态。
//!
//! ── 过期时刻 ────────────────────────────────────────────────
//! 上游给 `expiresIn`（秒），落盘时算成 `expiresAt`（毫秒）。access token 是
//! HS512 的 JWT（约 30 天），所以判不出 `expiresAt` 时可以退回读 JWT 的 `exp`；
//! 两条都没有就返回 0 = 判不出来（不猜）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::Value;

use crate::server::errors::GatewayError;

/// 临期窗口（续期在这之内就动手：到期前 1 小时）
const EXPIRING_WINDOW_MS: i64 = 60 * 60 * 1000;

/// 一条 LobsterAI 账号的凭证
#[derive(Clone, Debug, Default)]
pub struct LobsterCredentials {
    /// 账号记录 id（`lobsterai-<身份>`）
    pub id: String,
    /// 访问令牌（HS512 JWT，约 30 天）
    pub access_token: String,
    /// 续期令牌（可能为空 —— 那种账号只能重新登录）
    pub refresh_token: String,
    /// 到期时刻（毫秒；0 = 判不出）
    pub expires_at: i64,
    /// 门户侧的用户 id（记录里的 `uid`）
    pub uid: String,
    /// 上游 `user.userId`（续期载荷件之一，也是身份首选）
    pub user_id: String,
    /// 展示名
    pub nickname: String,
    /// **载荷件**：换码时下发，续期必须原样回传
    pub uuid: String,
    /// **载荷件**：登录时那一份，续期时原样回传且**永不更新**
    pub first_keyfrom: String,
    /// **载荷件**：同 [`Self::first_keyfrom`]
    pub latest_keyfrom: String,
}

impl LobsterCredentials {
    /// 还能不能续期：有 refresh token **且** 载荷件齐全。
    ///
    /// 缺任一载荷件时上游会拒（或更糟：给一个不认识的错误），所以这里先判死 ——
    /// 让「这个账号续不了期」在界面上是一句可读的话，而不是一次玄学失败。
    pub fn can_refresh(&self) -> bool {
        !self.refresh_token.trim().is_empty()
            && !self.first_keyfrom.trim().is_empty()
            && !self.latest_keyfrom.trim().is_empty()
    }

    /// 是否已过期或进入提前续期窗口（到期前 1 小时）。
    /// 判不出到期时刻（0）时返回 false —— 那种账号只能等 401 的懒刷新。
    pub fn expiring(&self) -> bool {
        self.expires_at > 0
            && self.expires_at - EXPIRING_WINDOW_MS <= crate::server::logging::now_ms()
    }

    /// keyfrom 载荷（`firstKeyfrom` / `latestKeyfrom` / `version` 恒在，
    /// `uuid` 与 `userId` 非空才带）。
    ///
    /// 两个消费点共用这一份：目录端点把它拼成 **query**（特意不带 refresh token
    /// —— query 会进服务端访问日志），续期端点把它当 **body** 再补一个
    /// `refreshToken`。**按键名排序**：上游对顺序敏感，照抄官方客户端 / Go 的
    /// `url.Values.Encode()` 口径，让同样的账号每次拼出同样的串。
    pub fn keyfrom_fields(&self, client_version: &str) -> Vec<(&'static str, String)> {
        let mut fields: Vec<(&'static str, String)> = vec![
            ("firstKeyfrom", self.first_keyfrom.clone()),
            ("latestKeyfrom", self.latest_keyfrom.clone()),
            ("version", client_version.to_string()),
        ];
        if !self.uuid.trim().is_empty() {
            fields.push(("uuid", self.uuid.clone()));
        }
        if !self.user_id.trim().is_empty() {
            fields.push(("userId", self.user_id.clone()));
        }
        fields.sort_by(|a, b| a.0.cmp(b.0));
        fields
    }

    /// 目录请求的查询串（`k=v&k2=v2`，值 urlencoded，**不含 refresh token**）
    pub fn keyfrom_query(&self, client_version: &str) -> String {
        self.keyfrom_fields(client_version)
            .into_iter()
            .map(|(key, value)| format!("{key}={}", super::endpoints::urlencode(&value)))
            .collect::<Vec<String>>()
            .join("&")
    }
}

/// 从取值链里挑第一个非空字符串（记录可能被手工编辑过，宽容读取）
fn pick(record: &Value, keys: &[&str]) -> String {
    for key in keys {
        let value = record
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

/// 读一个毫秒时刻（数字或数字字符串都认）
fn pick_ms(record: &Value, keys: &[&str]) -> i64 {
    for key in keys {
        if let Some(value) = record.get(*key) {
            if let Some(number) = value.as_i64() {
                if number > 0 {
                    return number;
                }
            }
            if let Some(text) = value.as_str() {
                if let Ok(number) = text.trim().parse::<i64>() {
                    if number > 0 {
                        return number;
                    }
                }
            }
        }
    }
    0
}

/// 读一个秒数（`expiresIn`）
fn pick_secs(record: &Value, keys: &[&str]) -> i64 {
    for key in keys {
        if let Some(number) = record.get(*key).and_then(Value::as_i64) {
            if number > 0 {
                return number;
            }
        }
    }
    0
}

/// 读 JWT 载荷里的 `exp`（**不解签**，只读；解不出返回 None）。
///
/// access token 是 HS512 的三段点分 JWT，`exp` 在第二段的 claims 里。
/// **只解码不验签**：这是本地判过期用的，签名由上游校验。
pub fn jwt_exp_ms(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64_url_decode(payload)?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    let exp = claims.get("exp").and_then(Value::as_f64)?;
    if !exp.is_finite() || exp <= 0.0 {
        return None;
    }
    Some((exp * 1000.0) as i64)
}

/// base64url（无填充）解码 —— JWT 载荷段的形态
fn base64_url_decode(text: &str) -> Option<Vec<u8>> {
    let mut buffer: Vec<u8> = Vec::with_capacity(text.len());
    let mut accumulator: u32 = 0;
    let mut bits: u32 = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => continue,
            _ => return None,
        };
        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            buffer.push(((accumulator >> bits) & 0xff) as u8);
        }
    }
    Some(buffer)
}

/// 由 `createdAt + expiresIn` 或 JWT `exp` 推出过期时刻（毫秒），都没有则 0。
pub fn expires_at_from(created_at_ms: i64, expires_in_secs: i64, access_token: &str) -> i64 {
    if created_at_ms > 0 && expires_in_secs > 0 {
        return created_at_ms + expires_in_secs * 1000;
    }
    jwt_exp_ms(access_token).unwrap_or(0)
}

/// 从账号记录解析凭证（**唯一入口**；`record` 为 None 时报「没有可用账号」）。
///
/// 字段兼容：本项目的驼峰名（`accessToken` / `refreshToken` / `firstKeyfrom`…）
/// 与参考实现里的下划线名（`access_token` / `first_keyfrom`…）都认。
pub fn from_record(record: Option<&Value>) -> Result<LobsterCredentials, GatewayError> {
    let Some(record) = record else {
        return Err(GatewayError::with_status(
            401,
            "没有可用的 LobsterAI 账号：请在账号页用浏览器登录添加",
        ));
    };
    let access_token = pick(record, &["accessToken", "access_token"]);
    if access_token.is_empty() {
        return Err(GatewayError::with_status(
            401,
            "LobsterAI 账号缺少访问令牌，请重新登录",
        ));
    }
    let created_at = pick_ms(record, &["createdAt", "created_at"]);
    let expires_in = pick_secs(record, &["expiresIn", "expires_in"]);
    let explicit_expiry = pick_ms(record, &["expiresAt", "expires_at"]);
    let expires_at = if explicit_expiry > 0 {
        explicit_expiry
    } else {
        expires_at_from(created_at, expires_in, &access_token)
    };
    Ok(LobsterCredentials {
        id: pick(record, &["id"]),
        refresh_token: pick(record, &["refreshToken", "refresh_token"]),
        access_token,
        expires_at,
        uid: pick(record, &["uid", "userId", "user_id"]),
        user_id: pick(record, &["userId", "user_id"]),
        nickname: pick(record, &["name", "nickname"]),
        uuid: pick(record, &["uuid", "UUID"]),
        first_keyfrom: pick(record, &["firstKeyfrom", "first_keyfrom"]),
        latest_keyfrom: pick(record, &["latestKeyfrom", "latest_keyfrom"]),
    })
}

/// 账号展示名（昵称优先，其次 uid，最后账号 id）
pub fn display_name(nickname: &str, uid: &str, id: &str) -> String {
    for candidate in [nickname, uid, id] {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "LobsterAI 账号".to_string()
}

/// 账号的稳定身份（落盘 id 与跨 provider 去重用）：`userId → uid → 令牌尾段`。
///
/// ⚠️ 与落账号路径（`account_store::lobsterai_accounts`）的口径**必须逐字一致**
/// —— 两处分叉会让「同一个账号重新登录」变成「新增一条记录」。
pub fn identity(user_id: &str, uid: &str, access_token: &str) -> String {
    let user_id = user_id.trim();
    if !user_id.is_empty() {
        return user_id.to_string();
    }
    let uid = uid.trim();
    if !uid.is_empty() {
        return uid.to_string();
    }
    // 都读不到时用令牌尾号兜底（会退化成按凭证去重，界面能看出来）
    let chars: Vec<char> = access_token.chars().collect();
    let start = chars.len().saturating_sub(12);
    format!("tok{}", chars[start..].iter().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 三件载荷件缺一件就不能续期（只存两个 token 的账号续不了，见模块头）
    #[test]
    fn refresh_needs_all_keyfrom_pieces() {
        let mut credentials = LobsterCredentials {
            access_token: "a".into(),
            refresh_token: "r".into(),
            first_keyfrom: "f".into(),
            latest_keyfrom: "l".into(),
            ..Default::default()
        };
        assert!(credentials.can_refresh());
        credentials.uuid = "u".into();
        assert!(credentials.can_refresh());
        credentials.latest_keyfrom.clear();
        assert!(!credentials.can_refresh());
    }

    /// 续期字段按键名排序（上游对顺序敏感）
    #[test]
    fn keyfrom_fields_are_sorted() {
        let credentials = LobsterCredentials {
            refresh_token: "r".into(),
            first_keyfrom: "f".into(),
            latest_keyfrom: "l".into(),
            uuid: "u".into(),
            user_id: "x".into(),
            ..Default::default()
        };
        let keys: Vec<&str> = credentials
            .keyfrom_fields("2026.9.4")
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(
            keys,
            vec!["firstKeyfrom", "latestKeyfrom", "userId", "uuid", "version"]
        );
    }

    /// 目录 query 里**不能**出现 refreshToken（会进服务端访问日志）
    #[test]
    fn keyfrom_query_never_carries_refresh_token() {
        let credentials = LobsterCredentials {
            refresh_token: "secret-refresh".into(),
            first_keyfrom: "f".into(),
            latest_keyfrom: "l".into(),
            ..Default::default()
        };
        let query = credentials.keyfrom_query("2026.9.4");
        assert!(query.contains("firstKeyfrom=f"));
        assert!(!query.contains("secret-refresh"));
        assert!(!query.contains("refreshToken"));
    }

    /// 身份优先级：userId → uid → 令牌尾 12 位
    #[test]
    fn identity_prefers_user_id() {
        assert_eq!(identity("7", "42", "tok"), "7");
        assert_eq!(identity("", "42", "tok"), "42");
        assert_eq!(identity("", "", "abcdefghijklmn"), "tokabcdefghijklmn");
    }

    /// JWT 的 exp 读得出来（`{"exp":1e9}` 的 base64url），非 JWT 解不出
    #[test]
    fn jwt_exp_is_readable() {
        // {"exp":1000000000}
        let token = "h.eyJleHAiOjEwMDAwMDAwMDB9.s";
        assert_eq!(jwt_exp_ms(token), Some(1_000_000_000_000));
        assert_eq!(jwt_exp_ms("not-a-jwt"), None);
    }

    /// 记录读取：顶层 accessToken 是唯一真源；载荷件齐全时 can_refresh 为真
    #[test]
    fn record_reads_top_level_token_and_payload() {
        let record = json!({
            "id": "lobsterai-1",
            "accessToken": "at",
            "refreshToken": "rt",
            "uuid": "u",
            "firstKeyfrom": "f",
            "latestKeyfrom": "l",
        });
        let credentials = from_record(Some(&record)).ok();
        assert!(credentials.as_ref().is_some_and(|c| c.access_token == "at"));
        assert!(credentials.as_ref().is_some_and(|c| c.can_refresh()));
        assert!(from_record(None).is_err());
    }
}