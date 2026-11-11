//! Qoder 凭证格式：兼容 Qoder-Proxy 的 access / refresh / expires 字段。

use serde_json::{json, Value};

use crate::server::core::account_store::MAX_TOKEN_LENGTH;
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::endpoints::Site;

pub const REFRESH_MARGIN_MS: i64 = 5 * 60 * 1000;

/// 临期判定窗口（千问办公）——**只对设备流那一族令牌成立**（`drt-`）：1 小时。
///
/// ── 为什么比 Qoder 宽这么多（这是账号报废级的差别）──────────────
/// 千问办公的设备刷新令牌**只在访问令牌还有效时可用**：访问令牌过期太久之后
/// 再去续期，上游直接回 `invalid_grant`（dumate2api 2026-10-29 实测：一个
/// 7 天有效期的账号因为到期那一刻恰好没有流量，23 小时后才第一次续期，
/// 额度与账号一起作废，只能重新登录）。
///
/// 所以窗口必须远大于「一次请求的耗时」，但又不能大到每次都刷（续期本身会
/// 轮换令牌）。1 小时是参考实现的选择，这里照抄并留档。
pub const QWENWORK_REFRESH_MARGIN_MS: i64 = 60 * 60 * 1000;

#[derive(Clone)]
pub struct Credentials {
    pub site: Site,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: Option<i64>,
    pub user_id: String,
    pub email: String,
    pub name: String,
    pub machine_id: String,
}

impl Credentials {
    pub fn from_payload(payload: &Value) -> Result<Self, GatewayError> {
        if !payload.is_object() {
            return Err(GatewayError::with_status(400, "Qoder 账号内容必须是 JSON 对象"));
        }
        // 账号记录的站点只认 provider id：千问办公的 `mode` 也是 "cn"，
        // 用 from_payload 会把它认成 Qoder 中国版（见 Site::from_record 的说明）。
        let site = Site::from_record(payload)?;
        let access_token = secret(payload, &["accessToken", "access", "token", "access_token"])?;
        if access_token.is_empty() {
            return Err(GatewayError::with_status(400, "缺少 Qoder access / accessToken"));
        }
        let refresh = secret(payload, &["refreshToken", "refresh", "refresh_token"])?;
        let parts: Vec<&str> = refresh.split('|').collect();
        let (packed_user, packed_machine) = match parts.as_slice() {
            ["pat", pat, _, user, machine] if !pat.is_empty() => (*user, *machine),
            [_, user, machine] => (*user, *machine),
            [_] => ("", ""),
            _ => return Err(GatewayError::with_status(400, "Qoder refresh 格式无效，请粘贴完整账号记录")),
        };
        let user_id = identity(payload, &["userId", "user_id", "uid"])?;
        if !user_id.is_empty() && !packed_user.is_empty() && user_id != packed_user {
            return Err(GatewayError::with_status(400, "Qoder userId 与刷新凭证中的用户不一致"));
        }
        let machine_id = identity(payload, &["machineId", "machine_id"])?;
        if !machine_id.is_empty() && !packed_machine.is_empty() && machine_id != packed_machine {
            return Err(GatewayError::with_status(400, "Qoder machineId 与刷新凭证中的设备不一致"));
        }
        let user_id = if user_id.is_empty() { packed_user.to_string() } else { user_id };
        let machine_id = if machine_id.is_empty() { packed_machine.to_string() } else { machine_id };
        validate_identity(&user_id)?;
        validate_identity(&machine_id)?;
        let expires_at = ["expiresAt", "expires", "expires_at", "tokenExpiresAt"]
            .iter()
            .find_map(|key| timestamp(payload.get(*key)));
        let mut credentials = Self {
            site,
            access_token,
            refresh_token: refresh.clone(),
            expires_at,
            user_id,
            email: text(payload, &["email"]).chars().take(320).collect(),
            name: text(payload, &["nickname", "name"]).chars().take(100).collect(),
            machine_id,
        };
        if parts.len() == 1 && !refresh.is_empty() {
            credentials.refresh_token = format!("{}|{}|{}", refresh, credentials.user_id, credentials.machine_id);
        }
        Ok(credentials)
    }

    /// 站点以 provider 身份为准（payload 里的 `mode` 只是兼容字段，见
    /// `refresh::snapshot`）。
    pub fn set_site(&mut self, site: Site) {
        self.site = site;
    }

    pub fn complete_identity(&mut self) -> Result<(), GatewayError> {
        if self.user_id.is_empty() {
            return Err(GatewayError::with_status(400, "缺少 Qoder userId，无法标识账号，请重新登录或粘贴完整账号记录"));
        }
        validate_identity(&self.user_id)?;
        if self.machine_id.is_empty() {
            self.machine_id = super::machine::machine_id()?;
        }
        validate_identity(&self.machine_id)?;
        // 刷新串的两段后缀固定是「用户 | 设备」（PAT 来源是「pat | PAT | 作业刷新
        // 令牌」+ 这两段，OAuth 来源是「刷新令牌」+ 这两段）。这里只重写后缀、
        // 保住前缀：PAT 与作业刷新令牌的形态由上游决定，按 `|` 切分再取下标会在
        // 段数不足时越界 panic（release 是 panic=abort），因此按**段数**分支、
        // 不足三段时整串当前缀（拼出的串上游会拒绝，但不会崩）。
        if !self.refresh_token.is_empty() {
            let parts: Vec<&str> = self.refresh_token.split('|').collect();
            let prefix = if parts.len() >= 3 {
                parts[..parts.len() - 2].join("|")
            } else {
                self.refresh_token.clone()
            };
            self.refresh_token = format!("{prefix}|{}|{}", self.user_id, self.machine_id);
        }
        if self.refresh_token.len() > MAX_TOKEN_LENGTH {
            return Err(GatewayError::with_status(400, "Qoder 刷新凭证过长"));
        }
        Ok(())
    }

    pub fn pat(&self) -> Option<&str> {
        self.refresh_token.strip_prefix("pat|")?.split('|').next().filter(|value| !value.is_empty())
    }

    pub fn oauth_refresh(&self) -> &str {
        if self.pat().is_some() {
            ""
        } else {
            self.refresh_token.split('|').next().unwrap_or("")
        }
    }

    pub fn can_refresh(&self) -> bool {
        self.pat().is_some() || !self.oauth_refresh().is_empty()
    }

    pub fn expiring(&self) -> bool {
        // 千问办公的窗口宽得多（见 [`QWENWORK_REFRESH_MARGIN_MS`]）：它的刷新
        // 令牌只在访问令牌仍有效时可用，晚一步就整号作废。
        let margin = if self.site.is_qwenwork() {
            QWENWORK_REFRESH_MARGIN_MS
        } else {
            REFRESH_MARGIN_MS
        };
        self.can_refresh() && self.expires_at.is_some_and(|expiry| expiry <= logging::now_ms() + margin)
    }

    pub fn to_value(&self) -> Value {
        // 空串按 null 落进记录：`add_qoder_account` 用「空值不覆盖」的合并规则，
        // 缺失的 refreshToken / expiresAt 不该把既有值洗成空串。
        let text_or_null = |value: &str| -> Value {
            if value.is_empty() { Value::Null } else { Value::String(value.to_string()) }
        };
        json!({
            "mode": self.site.id(),
            "accessToken": text_or_null(&self.access_token),
            "refreshToken": text_or_null(&self.refresh_token),
            "expiresAt": self.expires_at,
            "userId": text_or_null(&self.user_id),
            "email": text_or_null(&self.email),
            "nickname": text_or_null(&self.name),
            "machineId": text_or_null(&self.machine_id),
        })
    }
}

pub fn text(payload: &Value, keys: &[&str]) -> String {
    keys.iter().find_map(|key| match payload.get(*key) {
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.trim().to_string()),
        Some(Value::Number(value)) => Some(value.to_string()),
        _ => None,
    }).unwrap_or_default()
}

pub fn secret(payload: &Value, keys: &[&str]) -> Result<String, GatewayError> {
    let mut value = keys.iter().find_map(|key| payload.get(*key).and_then(Value::as_str)
        .map(str::trim).filter(|value| !value.is_empty())).unwrap_or("");
    if value.get(..7).is_some_and(|prefix| prefix.eq_ignore_ascii_case("Bearer ")) {
        value = value[7..].trim();
    }
    if value.len() > MAX_TOKEN_LENGTH || value.chars().any(char::is_control) {
        return Err(GatewayError::with_status(400, "Qoder 凭证过长或包含非法控制字符"));
    }
    Ok(value.to_string())
}

fn identity(payload: &Value, keys: &[&str]) -> Result<String, GatewayError> {
    let value = text(payload, keys);
    validate_identity(&value)?;
    Ok(value)
}

fn validate_identity(value: &str) -> Result<(), GatewayError> {
    if value.len() > 256 || value.contains('|') || value.chars().any(char::is_control) {
        return Err(GatewayError::with_status(400, "Qoder 用户或设备标识无效"));
    }
    Ok(())
}

pub fn timestamp(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    let number = value.as_i64().or_else(|| value.as_str()?.trim().parse::<i64>().ok());
    if let Some(number) = number.filter(|value| *value > 0) {
        return if number < 100_000_000_000 { number.checked_mul(1000) } else { Some(number) };
    }
    chrono::DateTime::parse_from_rfc3339(value.as_str()?).ok().map(|time| time.timestamp_millis())
}
