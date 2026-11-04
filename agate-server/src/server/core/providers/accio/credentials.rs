//! Accio 凭证格式：`accessToken` / `refreshToken` / `expiresAt` + 账号标识。
//!
//! ── 上游给的东西（桌面端 `/api/oauth/token` 与 `/api/auth/refresh_token`
//!    的返回）──────────────────────────────────────────────────
//! ```jsonc
//! { "accessToken": "…", "refreshToken": "…", "expiresAt": 1789…, "sid": "…", "sgcookie": "…" }
//! ```
//! `sid` / `sgcookie` 是浏览器侧会话的伴随标识，桌面端只用于「账号切换」
//! （`/api/auth/safe/refresh_token` 那条多账号链路）。本网关的续期走普通那条，
//! 因此**不落**这两个字段 —— 少存两个用不上的敏感值。
//!
//! ── 身份用什么标识 ──────────────────────────────────────────
//! Accio 的 token 是**不透明串**（不是 JWT，解不出 userId），因此账号身份由
//! `/api/auth/userinfo` 查回的资料里的 id 决定（见 `auth::fetch_profile`）。
//! 「粘贴凭证」这条路查一次资料拿到 id；查不到时退回按凭证指纹生成 id ——
//! 至少能保证同一串凭证重复添加时命中同一条记录，不会堆出一串重复账号。
//!
//! ── `deviceId`（utdid）是干嘛的 ─────────────────────────────
//! 推理请求要带 `utdid`（设备指纹）。桌面端每台机器一个（装机时生成、落盘）。
//! 本网关按**账号**生成并落进记录：多账号池化时每个账号带自己的指纹，
//! 与「一个账号一台设备」的真实形态一致（同一指纹挂十个账号才是可疑形态）。
//!
//! ⚠️ **必带，不是可选**（对齐上游 3.4.1）：上游把 utdid 当风控基线，缺了会
//! 回 **HTTP 200 + HTML 拦截页**（bixi-intl 的 punish 脚本），症状是「空响应 /
//! 模型不回复」—— 不报错、不换号，最难查。所以请求头一律走 [`Credentials::utdid`]：
//! 记录里没有 `deviceId`（手改的、旧版本迁来的）也要按账号身份派生一个稳定值。
//! 旧注释里「缺省也能跑」是错的，已废弃。

use serde_json::{json, Value};

use crate::server::core::account_store::MAX_TOKEN_LENGTH;
use crate::server::errors::GatewayError;
use crate::server::logging;
use crate::server::core::upstream::request::new_request_id;

use super::endpoints::Region;

/// 提前多久认为 token 该续期（与另外几家同一量级）
pub const REFRESH_MARGIN_MS: i64 = 5 * 60 * 1000;

#[derive(Clone)]
pub struct Credentials {
    pub region: Region,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: Option<i64>,
    /// 上游账号标识（userinfo 的 id / userId）
    pub user_id: String,
    pub email: String,
    pub name: String,
    /// 推理请求的设备指纹（本网关按账号生成）
    pub device_id: String,
}

impl Credentials {
    /// 从账号记录 / 添加请求解析凭证。
    ///
    /// 接受的键名与其它家保持一致的宽口径（`accessToken` / `access_token` /
    /// `token`），前端「填写凭证」与前端的导入路径都可能给不同形态。
    pub fn from_payload(payload: &Value) -> Result<Self, GatewayError> {
        if !payload.is_object() {
            return Err(GatewayError::with_status(400, "Accio 账号内容必须是 JSON 对象"));
        }
        let region = Region::from_payload(payload)?;
        let access_token = secret(payload, &["accessToken", "access_token", "token"])?;
        if access_token.is_empty() {
            return Err(GatewayError::with_status(400, "缺少 Accio accessToken"));
        }
        let refresh_token = secret(payload, &["refreshToken", "refresh_token"])?;
        let expires_at = ["expiresAt", "expires_at", "expires"]
            .iter()
            .find_map(|key| timestamp(payload.get(*key)));
        Ok(Self {
            region,
            access_token,
            refresh_token,
            expires_at,
            user_id: text(payload, &["userId", "user_id", "uid"]),
            email: text(payload, &["email"]).chars().take(320).collect(),
            name: text(payload, &["nickname", "name"]).chars().take(100).collect(),
            device_id: text(payload, &["deviceId", "device_id"]),
        })
    }

    /// 新账号的兜底标识：没有 userId 时按凭证指纹生成一个稳定串。
    ///
    /// 用 `new_request_id()`（项目里唯一的不可预测随机源，同 `raccoon::oauth`
    /// 的手法）而不是凭证本身的摘要：id 会落进账号 id 与日志，**不该可反推
    /// 凭证**。代价是同一串凭证重复添加会得到不同的兜底 id —— 由
    /// `add_accio_account` 的「同 provider 内按 userId 去重」兜住（userId 缺失
    /// 时它按 accessToken 相等去重，见那边的实现）。
    pub fn ensure_identity(&mut self) {
        if self.device_id.is_empty() {
            self.device_id = format!("desktop-{}", new_request_id().replace('-', ""));
        }
    }

    /// 推理请求必带的设备指纹。
    ///
    /// 正常路径上 `ensure_identity`（添加账号 / 登录 / 续期）已经补过值，这里
    /// 兜的是「记录是手改的 / 从旧版本迁过来的」那一档：**缺字段也不能让头消失**。
    /// 另外还要挡脏值 —— reqwest 遇到非法头值会让**整条请求**失败（不是这一个
    /// 头失效），手改记录与上游回填的 deviceId 都可能出现非 ASCII。
    ///
    /// 兜底值按账号身份派生，**绝不共用**：`user_id` 优先（它最稳）；拿不到身份
    /// 时退回 `access_token` 的摘要 —— 至少「一个账号一个指纹」，代价是 token
    /// 轮换后这个值会变（那种账号本来就没有稳定的身份字段）。
    pub fn utdid(&self) -> String {
        if header_safe(&self.device_id) {
            return self.device_id.clone();
        }
        let seed = if !self.user_id.is_empty() {
            self.user_id.as_str()
        } else {
            self.access_token.as_str()
        };
        format!("desktop-{}", super::protocol::md5_hex(seed))
    }

    /// 有刷新凭证 + 有到期时间，且快到点了
    pub fn expiring(&self) -> bool {
        self.can_refresh()
            && self
                .expires_at
                .is_some_and(|expiry| expiry <= logging::now_ms() + REFRESH_MARGIN_MS)
    }

    pub fn can_refresh(&self) -> bool {
        !self.refresh_token.is_empty()
    }

    /// 落进账号记录的形态。空串按 null 落（`add_accio_account` 的合并规则是
    /// 「空值不覆盖」，缺项不该把既有值洗成空）。
    pub fn to_value(&self) -> Value {
        let text_or_null = |value: &str| -> Value {
            if value.is_empty() { Value::Null } else { Value::String(value.to_string()) }
        };
        json!({
            "mode": self.region.edition(),
            "accessToken": text_or_null(&self.access_token),
            "refreshToken": text_or_null(&self.refresh_token),
            "expiresAt": self.expires_at,
            "userId": text_or_null(&self.user_id),
            "email": text_or_null(&self.email),
            "nickname": text_or_null(&self.name),
            "deviceId": text_or_null(&self.device_id),
        })
    }
}

/// 能不能安全放进 HTTP 头：只接受**可见 ASCII**（含空格），空串按不安全处理。
fn header_safe(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

/// 取一个文本字段（空串按缺失处理）
pub fn text(payload: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| match payload.get(*key) {
            Some(Value::String(value)) if !value.trim().is_empty() => {
                Some(value.trim().to_string())
            }
            Some(Value::Number(value)) => Some(value.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

/// 取一个密钥字段：去 `Bearer ` 前缀、限长、拒绝控制字符。
pub fn secret(payload: &Value, keys: &[&str]) -> Result<String, GatewayError> {
    let mut value = keys
        .iter()
        .find_map(|key| {
            payload
                .get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("");
    if value.get(..7).is_some_and(|prefix| prefix.eq_ignore_ascii_case("Bearer ")) {
        value = value[7..].trim();
    }
    if value.len() > MAX_TOKEN_LENGTH || value.chars().any(char::is_control) {
        return Err(GatewayError::with_status(400, "Accio 凭证过长或包含非法控制字符"));
    }
    Ok(value.to_string())
}

/// 时间戳（毫秒）；接受秒级数字与 RFC3339 字符串。0 / 负数 / 解析不出 → None。
pub fn timestamp(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    let number = value
        .as_i64()
        .or_else(|| value.as_str()?.trim().parse::<i64>().ok());
    if let Some(number) = number.filter(|value| *value > 0) {
        return if number < 100_000_000_000 {
            number.checked_mul(1000)
        } else {
            Some(number)
        };
    }
    chrono::DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|time| time.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(user_id: &str, access_token: &str, device_id: &str) -> Credentials {
        Credentials {
            region: Region::Global,
            access_token: access_token.to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: None,
            user_id: user_id.to_string(),
            email: String::new(),
            name: String::new(),
            device_id: device_id.to_string(),
        }
    }

    /// 记录里有的指纹照用
    #[test]
    fn utdid_uses_the_saved_device_id_when_usable() {
        assert_eq!(creds("u1", "tok", "desktop-abc").utdid(), "desktop-abc");
    }

    /// 缺字段**不能让头消失**：这是「HTTP 200 + 空响应」那类静默失败的防线
    #[test]
    fn utdid_is_never_empty_when_the_record_lacks_device_id() {
        let value = creds("u1", "tok", "").utdid();
        assert!(header_safe(&value), "兜底值必须能安全进 HTTP 头: {value:?}");
        assert!(value.starts_with("desktop-"));
    }

    /// 脏值（非 ASCII / 控制字符）也不能原样进头 —— reqwest 会让整条请求失败
    #[test]
    fn utdid_replaces_unsafe_values() {
        assert!(header_safe(&creds("u1", "tok", "设备\n指纹").utdid()));
        assert!(header_safe(&creds("", "tok", "bad\u{7f}value").utdid()));
    }

    /// 派生值稳定且按账号分开：同一账号每次一样，不同账号绝不共用一个指纹
    #[test]
    fn derived_utdid_is_stable_and_per_account() {
        let a = creds("u1", "tok-a", "");
        let b = creds("u2", "tok-a", "");
        assert_eq!(a.utdid(), a.utdid());
        assert_ne!(a.utdid(), b.utdid());
        // 没有 userId 时退回 token 摘要
        let c = creds("", "tok-a", "");
        let d = creds("", "tok-b", "");
        assert_ne!(c.utdid(), d.utdid());
    }

    /// md5_hex 是唯一一份摘要实现：sg_k 与设备指纹共用
    #[test]
    fn md5_hex_is_stable() {
        assert_eq!(super::super::protocol::md5_hex("abc").len(), 32);
        assert_eq!(
            super::super::protocol::md5_hex("abc"),
            super::super::protocol::md5_hex("abc")
        );
        assert_ne!(
            super::super::protocol::md5_hex("abc"),
            super::super::protocol::md5_hex("abd")
        );
    }
}
