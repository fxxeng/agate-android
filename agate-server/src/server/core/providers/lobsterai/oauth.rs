//! LobsterAI 的网页登录（loopback 回调）与续期。
//!
//! ── 这条链属于哪一型 ────────────────────────────────────────
//! 与 Accio / AutoClaw 同族：**门户把浏览器 302 回本机 HTTP 端口**：
//! ```text
//!   授权页  {portal}/portal#/login?source=electron&redirect_uri=<本机>&state=<一次性>
//!   回调    GET http://127.0.0.1:<网关端口>/auth/callback?code=…&state=…
//!   换码    POST {api}/api/auth/exchange {authCode, firstKeyfrom, latestKeyfrom, uuid, version}
//! ```
//!
//! ── 为什么 state 必须自己保管 ────────────────────────────────
//! 回调落在本机端口上，任何本机进程都能伪造一次 GET。因此「这次回调属于哪一次
//! 登录」只能靠我们发起时生成的一次性 `state`：回调侧逐字比对进行中的那一轮，
//! **取走即失效**（`take_pending`）—— 「授权码只能兑一次」的闸门在这里。
//!
//! ── 回调路径为什么是裸 `/auth/callback` ─────────────────────────
//! 本仓其余几家自作主张的回调路径都带家名后缀（`/auth/callback-accio` 等），
//! 这一家**例外**：授权地址里的 `redirect_uri` 是我们拼给门户的，门户会按它回落
//! —— 参考实现用的就是裸 `/auth/callback`，而门户是否对回调路径有白名单**没有
//! 实测支撑**。所以跟随参考实现那条已验证的形状，不为了命名风格去赌一次未经验证
//! 的兼容性。该路径目前在本仓未被占用（`http.rs` 的回调路由表里没有它）。
//!
//! ── 续期：**必须原样回传载荷件，且 `latestKeyfrom` 永不更新** ─────
//! `POST /api/auth/refresh` 同样**不带 Authorization**，体是
//! `{ firstKeyfrom, latestKeyfrom, version, [uuid], [userId], refreshToken }`。
//! 响应带新 `refreshToken` 时才轮换；`latestKeyfrom` **不覆盖**（见
//! `credentials.rs` 的模块头）。
//!
//! ── 两种「成功状态下的失败」──────────────────────────────────
//!   1. 业务信封 `code != 0` → 按错误处理；
//!   2. 信封 `code == 0` 但 `data` 为 null → **令牌已失效**。这一条最容易漏：
//!      只看 HTTP 状态或只看 `code` 都会把它当成成功，于是上层拿空令牌去发请求。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic，不持锁跨 await。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::server::core::auth_http::send_raw;
use crate::server::errors::GatewayError;

use super::credentials::LobsterCredentials;
use super::endpoints::{
    api_url, portal_base, urlencode, CALLBACK_PATH, EXCHANGE_PATH, FALLBACK_CLIENT_VERSION,
    REFRESH_PATH, REQUEST_TIMEOUT_MS, USER_AGENT,
};

/// 回调路径（路由与授权 URL 共用这一处）
pub const LOOPBACK_CALLBACK_PATH: &str = CALLBACK_PATH;

/// 一轮 pending 登录的有效期（与本仓登录任务的 5 分钟超时同一量级）
const PENDING_TTL_MS: i64 = 6 * 60 * 1000;

/// 本机回调端口（bootstrap / JNI 桥写一次；0 = 未知）
static LOOPBACK_PORT: OnceLock<u16> = OnceLock::new();

/// 写入本机回调端口（`server/mod.rs` 的 bootstrap 与 `jni_bridge.rs` 各调一次）。
///
/// ⚠️ **漏了 = 登录必失败**：授权地址里的 `redirect_uri` 拼不出来，门户回不来。
pub fn set_loopback_port(port: u16) {
    let _ = LOOPBACK_PORT.set(port);
}

/// 本机回调基址（`http://127.0.0.1:<port>`）；端口未知时 None
pub fn loopback_base() -> Option<String> {
    let port = *LOOPBACK_PORT.get()?;
    if port == 0 {
        return None;
    }
    Some(format!("http://127.0.0.1:{port}"))
}

/// 交给门户的 `redirect_uri`（换码时要原样回传的那一个）
pub fn redirect_uri() -> Option<String> {
    loopback_base().map(|base| format!("{base}{LOOPBACK_CALLBACK_PATH}"))
}

/// 一轮进行中的登录（目前只需要「什么时候发起的」用于过期判定）
struct PendingLogin {
    created_at: i64,
}

fn pending_table() -> &'static Mutex<HashMap<String, PendingLogin>> {
    static TABLE: OnceLock<Mutex<HashMap<String, PendingLogin>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_pending() -> std::sync::MutexGuard<'static, HashMap<String, PendingLogin>> {
    match pending_table().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// 清掉过期条目（每次读写前顺手做，避免这张表随进程寿命增长）
fn sweep(table: &mut HashMap<String, PendingLogin>) {
    let now = crate::server::logging::now_ms();
    table.retain(|_, entry| now - entry.created_at < PENDING_TTL_MS);
}

/// 拼授权地址
pub fn build_authorize_url(return_url: &str, state: &str) -> String {
    format!(
        "{}/portal#/login?source=electron&redirect_uri={}&state={}",
        portal_base(),
        urlencode(return_url),
        urlencode(state)
    )
}

/// 发起一轮登录：生成 state、登记 pending、返回 `(auth_url, state)`
pub fn begin_login(return_url: &str) -> (String, String) {
    let state = super::ids::uuid_v4().replace('-', "");
    let auth_url = build_authorize_url(return_url, &state);
    let mut table = lock_pending();
    sweep(&mut table);
    table.insert(
        state.clone(),
        PendingLogin {
            created_at: crate::server::logging::now_ms(),
        },
    );
    (auth_url, state)
}

/// 取走一轮 pending（**取走即失效**，见模块头）；不属于本次 / 已过期 → false
pub fn take_pending(state: &str) -> bool {
    let mut table = lock_pending();
    sweep(&mut table);
    table.remove(state).is_some()
}

/// 换码 / 续期响应里的令牌包
#[derive(Clone, Debug, Default)]
pub struct TokenBundle {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
    pub uid: String,
    pub user_id: String,
    pub nickname: String,
}

/// 拆信封：`code == 0` 且 `data` **非 null** 才算成功；`data` 为空按登录态失效报。
pub fn unwrap_envelope(payload: &Value, what: &str) -> Result<Value, GatewayError> {
    let code = payload.get("code").and_then(Value::as_i64).unwrap_or(-1);
    let message = payload
        .get("msg")
        .or_else(|| payload.get("message"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if code != 0 {
        let detail = if message.is_empty() {
            format!("code={code}")
        } else {
            message.to_string()
        };
        return Err(GatewayError::with_status(
            502,
            format!("LobsterAI {what}失败：{detail}"),
        ));
    }
    match payload.get("data") {
        Some(Value::Null) | None => Err(GatewayError::with_status(
            401,
            format!("LobsterAI {what}未返回数据（登录态可能已失效），请重新登录"),
        )),
        Some(data) => Ok(data.clone()),
    }
}

/// 从 `data` 里读令牌包
pub fn parse_token_bundle(data: &Value) -> Result<TokenBundle, GatewayError> {
    let str_of = |value: &Value, key: &str| -> String {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let access_token = {
        let token = str_of(data, "accessToken");
        if token.is_empty() {
            str_of(data, "access_token")
        } else {
            token
        }
    };
    if access_token.is_empty() {
        return Err(GatewayError::with_status(
            502,
            "LobsterAI 换码响应缺少 accessToken",
        ));
    }
    let user = data.get("user").cloned().unwrap_or(Value::Null);
    let refresh_token = {
        let token = str_of(data, "refreshToken");
        if token.is_empty() {
            str_of(data, "refresh_token")
        } else {
            token
        }
    };
    let uid = {
        let id = str_of(&user, "id");
        if id.is_empty() {
            str_of(&user, "yid")
        } else {
            id
        }
    };
    Ok(TokenBundle {
        access_token,
        refresh_token,
        expires_in: data
            .get("expiresIn")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .unwrap_or(0),
        uid,
        user_id: str_of(&user, "userId"),
        nickname: str_of(&user, "nickname"),
    })
}

/// 用一次性 `code` 换令牌包（**不带 Authorization**）。
///
/// 体固定五个字段：`authCode` + 两个 keyfrom + `uuid` + `version`。
/// `uuid` 由调用方生成并**落盘**（续期要它）—— 它不是凭据，是这一台「客户端」的标识。
pub async fn exchange(
    auth_code: &str,
    uuid: &str,
    first_keyfrom: &str,
    latest_keyfrom: &str,
    client_version: &str,
) -> Result<TokenBundle, GatewayError> {
    let body = json!({
        "authCode": auth_code,
        "firstKeyfrom": first_keyfrom,
        "latestKeyfrom": latest_keyfrom,
        "uuid": uuid,
        "version": client_version,
    });
    let payload = post_anonymous(EXCHANGE_PATH, &body, "换码").await?;
    let data = unwrap_envelope(&payload, "换码")?;
    parse_token_bundle(&data)
}

/// 续期（**不带 Authorization**）：载荷件原样回传 + `refreshToken`。
pub async fn refresh(
    credentials: &LobsterCredentials,
    client_version: &str,
) -> Result<TokenBundle, GatewayError> {
    if !credentials.can_refresh() {
        return Err(GatewayError::with_status(
            401,
            "LobsterAI 账号缺少续期所需的载荷件（uuid / keyfrom 之一为空），请重新登录",
        ));
    }
    let mut body = serde_json::Map::new();
    for (key, value) in credentials.keyfrom_fields(client_version) {
        body.insert(key.to_string(), Value::String(value));
    }
    body.insert(
        "refreshToken".to_string(),
        Value::String(credentials.refresh_token.clone()),
    );
    let payload = post_anonymous(REFRESH_PATH, &Value::Object(body), "续期").await?;
    let data = unwrap_envelope(&payload, "续期")?;
    parse_token_bundle(&data)
}

/// 发一次匿名 POST（换码与续期都在登录前 / 凭据过期时，没有账号代理可用）
async fn post_anonymous(path: &str, body: &Value, what: &str) -> Result<Value, GatewayError> {
    let response = send_raw(
        "POST",
        &api_url(path),
        Some(body),
        &[
            ("accept".to_string(), "application/json".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
            ("user-agent".to_string(), USER_AGENT.to_string()),
        ],
        None,
        Some(REQUEST_TIMEOUT_MS),
    )
    .await
    .map_err(|error| GatewayError::with_status(502, format!("LobsterAI {what}请求失败：{error}")))?;
    let payload = response
        .payload
        .ok_or_else(|| GatewayError::with_status(502, format!("LobsterAI {what}响应不是 JSON")))?;
    // 401/403 直接归成登录态问题，别让信封里的文案把方向带偏
    if response.status == 401 || response.status == 403 {
        return Err(GatewayError::with_status(
            401,
            format!("LobsterAI {what}被拒（HTTP {}），请重新登录", response.status),
        ));
    }
    if !(200..300).contains(&response.status) {
        return Err(GatewayError::with_status(
            502,
            format!("LobsterAI {what}返回 HTTP {}", response.status),
        ));
    }
    Ok(payload)
}

/// 从回调查询串里取 `(code, state)`（两者缺一即 None）。
///
/// 门户可能带回额外参数（微信授权那条链会挂 `scope` 之类），所以按键找而不是按位置切。
pub fn parse_callback(query: &str) -> Option<(String, String)> {
    let mut code = String::new();
    let mut state = String::new();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let decoded = urldecode(value);
        match key {
            "code" => code = decoded,
            "state" => state = decoded,
            _ => {}
        }
    }
    if code.is_empty() || state.is_empty() {
        return None;
    }
    Some((code, state))
}

/// 最小 urldecode（`+` → 空格，`%XX` → 字节；非法转义原样保留）
fn urldecode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            other => {
                out.push(other);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 兜底客户端版本（授权 URL 里不强制带版本，这里给后续换码一份默认值）
pub fn default_client_version() -> &'static str {
    FALLBACK_CLIENT_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回调解析：两个参数缺一不可
    #[test]
    fn callback_requires_both_params() {
        assert_eq!(
            parse_callback("code=abc&state=s1"),
            Some(("abc".to_string(), "s1".to_string()))
        );
        assert_eq!(parse_callback("code=abc"), None);
        assert_eq!(parse_callback("state=s1"), None);
    }

    /// 回调里的转义要解码（门户把 code 当查询值编码过一次）
    #[test]
    fn callback_decodes_escapes() {
        assert_eq!(
            parse_callback("code=a%2Bb&state=s%20t"),
            Some(("a+b".to_string(), "s t".to_string()))
        );
    }

    /// 额外参数（微信链带的 scope）不影响解析
    #[test]
    fn callback_ignores_extra_params() {
        assert_eq!(
            parse_callback("scope=snsapi&code=c1&state=s2&foo=bar"),
            Some(("c1".to_string(), "s2".to_string()))
        );
    }

    /// 信封 `code != 0` 是失败；`data` 为空按登录态失效报 401
    #[test]
    fn envelope_rules() {
        use serde_json::json;
        assert!(unwrap_envelope(&json!({"code": 0, "data": {"a": 1}}), "换码").is_ok());
        assert_eq!(
            unwrap_envelope(&json!({"code": 7, "msg": "bad"}), "换码")
                .err()
                .map(|error| error.status_code),
            Some(502)
        );
        assert_eq!(
            unwrap_envelope(&json!({"code": 0, "data": null}), "换码")
                .err()
                .map(|error| error.status_code),
            Some(401)
        );
    }

    /// pending 取走即失效（同一个 state 兑不了两次）
    #[test]
    fn pending_is_single_use() {
        let (_url, state) = begin_login("http://127.0.0.1:1/auth/callback");
        assert!(take_pending(&state));
        assert!(!take_pending(&state));
    }
}