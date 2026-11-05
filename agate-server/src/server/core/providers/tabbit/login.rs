//! Tabbit 手机号 + 短信验证码登录（**国内站专用**）。
//!
//! ── 链路（两步，没有窗口、没有轮询）──────────────────────────
//! ```text
//! POST /proxy/v0/oauth/send-verification-code
//!      {uuid, platform:"1", version:"", app:"1000", mobile}
//!   → {code:0}（uuid 由我们自己生成，随登录请求带回）
//! POST /proxy/v0/oauth/login
//!      同上 + {smsCode, device_info}
//!   → {success:true, code, expires_in, user_id, access_token}
//!      **Set-Cookie 里才是真正的登录态**（约 3.4KB）
//! ```
//! 这两个端点**不需要签名**：登录发生在拿到账号之前，没有可签的 Cookie；
//! 上游只认 `x-req-ctx` / `unique-uuid` 与设备信息（见 `login_headers`）。
//!
//! ── 登录态为什么必须读响应头 ────────────────────────────────
//! body 的 `access_token` 只是 Cookie 里那枚 JWT 的副本，而后续所有请求要的是
//! **整串 Cookie**（`token` / `user_id` / `SAPISID` / `managed` / `NEXT_LOCALE`）。
//! 所以 `post_login` 必须把 `Set-Cookie` 留下来，不能像控制面请求那样只读 body。
//!
//! ── 中间态（uuid）为什么不存服务端 ──────────────────────────
//! 发码与登录是两次 HTTP 请求，中间可能被用户放弃；把 uuid 回给前端、
//! 登录时带回来，服务端就不必维护一份要清理的登录会话。与 Loomy 的 `msgid`、
//! AutoClaw 的 `deviceId` 同一处置。
//!
//! ── 国际站为什么不给这条链 ──────────────────────────────────
//! 上游注释写得很直白：「国际站灰度里被 403，本适配只开国内」。同一份代码
//! 打过去只会稳定失败，因此 [`login_with_code`] 对 `Realm::Intl` **明确拒绝**
//! 并给出可行替代（粘贴 Cookie），而不是让用户填完号码才收到一句上游报错。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};

use crate::server::core::hostinfo::hostname;
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::credentials::{self, pick};
use super::http;
use super::sign;
use super::{Realm, DEFAULT_CLIENT_VERSION, SEC_CH_UA, USER_AGENT};

/// 发码路径
const SEND_CODE_PATH: &str = "/proxy/v0/oauth/send-verification-code";
/// 验证码登录路径
const LOGIN_PATH: &str = "/proxy/v0/oauth/login";

/// 上游登录端点的 `app` 与 `platform`（网页端固定值）
const LOGIN_APP_ID: &str = "1000";
const LOGIN_PLATFORM: &str = "1";
/// 登录请求超时（上游给的窗口很宽；这里只防挂死）
const LOGIN_TIMEOUT: Duration = Duration::from_secs(30);
/// 真正要留下的几个 Cookie 项（顺序即拼出来的顺序）。
///
/// 与 `credentials::WANTED_COOKIES` 是同一组，但那里的常量是私有的 ——
/// 这里**不另写一份语义**：最终仍交给 `Credentials::from_fields` 归一，
/// 这份只用于「从 Set-Cookie 里先挑出这几项」。
const WANTED_COOKIES: [&str; 5] = ["token", "user_id", "SAPISID", "managed", "NEXT_LOCALE"];

/// 64 位 `[A-Za-z0-9]` 的登录会话标识（上游 `login_uuid`）。
///
/// 形状是上游定的：`send-verification-code` 与 `login` 必须带**同一个** uuid，
/// 上游据此把「刚发的这个码」对应起来。
fn login_uuid() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut raw = [0u8; 64];
    if getrandom::getrandom(&mut raw).is_err() {
        // 拿不到系统熵时用随机 uuid 的字节循环铺满（只是退化，不失败）
        let seed = sign::random_uuid().replace('-', "");
        let bytes = seed.as_bytes();
        for (index, slot) in raw.iter_mut().enumerate() {
            *slot = bytes[index % bytes.len()];
        }
    }
    raw.iter()
        .map(|byte| ALPHABET[usize::from(*byte) % ALPHABET.len()] as char)
        .collect()
}

/// 设备信息（上游 `device_info`）。
///
/// ── 为什么要造一台「Windows 浏览器」而不是如实报 Android ────────
/// 本模块转发的是**网页端**会话（UA / `sec-ch-ua` 都是 Tabbit Browser），
/// 登录时如实报一个 Android 设备会与后续每个请求的浏览器身份自相矛盾。
/// 这里与 `USER_AGENT` / `SEC_CH_UA` 保持同一套身份，不是隐瞒平台。
///
/// `device_id_hash` 用一个**进程内稳定**的派生值而不是真实设备指纹：
/// 上游拿它做同设备判定，跨账号共用一个常量会让多账号被认成同一台机器。
fn device_info() -> Value {
    let host = hostname();
    json!({
        // 上游只要求「同一次登录内一致」，不要求跨进程稳定
        "device_id_hash": sign::sha256_hex(format!("tabbit-login-device:{host}").as_bytes()),
        "device_name": host,
        "platform": "win",
        "os_version": "10.0",
        "browser_version": "1.17.16.0",
        "tabbit_version": DEFAULT_CLIENT_VERSION,
    })
}

/// 登录请求的头（上游 `login_headers`）。
///
/// 与 `endpoints::base_headers` 的差别：登录时**没有账号**，所以
/// `unique-uuid` 的 Pro 位按「非默认浏览器」如实填 false，Cookie 只有
/// `NEXT_LOCALE=zh`，也没有 `Authorization` 与签名三件套。
fn login_headers(realm: Realm) -> Vec<(String, String)> {
    vec![
        ("User-Agent".to_string(), USER_AGENT.to_string()),
        ("sec-ch-ua".to_string(), SEC_CH_UA.to_string()),
        ("sec-ch-ua-platform".to_string(), "\"Windows\"".to_string()),
        ("sec-ch-ua-mobile".to_string(), "?0".to_string()),
        ("Accept".to_string(), "*/*".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Origin".to_string(), realm.base().to_string()),
        (
            "Referer".to_string(),
            format!("{}/login?callback=close", realm.base()),
        ),
        (
            "x-req-ctx".to_string(),
            sign::base64_text(DEFAULT_CLIENT_VERSION),
        ),
        (
            "unique-uuid".to_string(),
            sign::unique_uuid(false, crate::server::logging::now_ms()),
        ),
        ("trace-id".to_string(), sign::random_uuid()),
        ("Cookie".to_string(), "NEXT_LOCALE=zh".to_string()),
    ]
}

/// 一次登录请求：返回 `(HTTP 状态, 解析后的 body, 原始文本, Set-Cookie 列表)`。
///
/// 四个返回值都要：状态判成败、body 判业务码、原文做错误兜底、Cookie 是凭证本体。
async fn post_login(
    realm: Realm,
    path: &str,
    body: &Value,
) -> Result<(u16, Value, String, Vec<String>), GatewayError> {
    // 登录发生在「还没有账号」的时候，因此没有账号级代理可解析 ——
    // 与另外八家登录链同一处置（都走直连）。
    let client = http::client(None);
    let mut request = client
        .post(format!("{}{}", realm.base(), path))
        .timeout(LOGIN_TIMEOUT)
        .body(body.to_string());
    for (name, value) in login_headers(realm) {
        request = request.header(name.as_str(), value.as_str());
    }
    let response = request.send().await.map_err(|error| {
        GatewayError::with_status(502, format!("无法连接 Tabbit 登录服务：{error}"))
    })?;
    let status = response.status().as_u16();
    // Set-Cookie 必须先读：`headers()` 在 `bytes()` 消费完响应后仍可用，
    // 但一次读齐更不容易漏。
    let cookies = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok().map(str::to_string))
        .collect::<Vec<_>>();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| GatewayError::with_status(502, "读取 Tabbit 登录响应失败"))?;
    let text = String::from_utf8_lossy(&bytes).to_string();
    let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    Ok((status, value, text, cookies))
}

/// 去掉上游文案里的 pydantic 前缀（`Value error, ` 之类），对用户没有意义。
fn tidy_upstream(message: &str) -> String {
    let trimmed = message.trim();
    for prefix in ["Value error, ", "value error, ", "Validation error, "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    }
    trimmed.to_string()
}

/// 从响应里找一句能读的错误原因（候选键按上游出现频次排）。
fn login_error_text(status: u16, value: &Value, raw: &str) -> String {
    if let Some(text) = ["error", "message", "msg", "detail_text"]
        .iter()
        .filter_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|text| !text.is_empty())
    {
        return tidy_upstream(text);
    }
    if let Some(text) = value
        .get("detail")
        .and_then(Value::as_array)
        .and_then(|detail| detail.first())
        .and_then(|entry| entry.get("msg").or_else(|| entry.get("message")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return tidy_upstream(text);
    }
    if let Some(text) = value
        .get("detail")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return tidy_upstream(text);
    }
    let raw = raw.trim();
    if !raw.is_empty() && raw.chars().count() < 300 {
        raw.to_string()
    } else {
        format!("登录接口返回 HTTP {status}")
    }
}

/// 候选键里取第一个非空字符串（上游同一个字段有多种写法）。
fn text_of(value: &Value, keys: &[&str]) -> String {
    pick(value, keys)
}

/// 从 `Set-Cookie` 列表拼出 Cookie 请求头（上游 `cookie_from_headers`）。
///
/// 只留 [`WANTED_COOKIES`] 里的几项，且保持常量顺序 —— 上游的 Cookie 顺序
/// 在浏览器里是固定的，换序没有功能影响，但排障时对不上包会多花时间。
fn cookie_from_headers(headers: &[String]) -> String {
    let mut values: BTreeMap<String, String> = BTreeMap::new();
    for line in headers {
        let first = line.split(';').next().unwrap_or("");
        let mut pieces = first.splitn(2, '=');
        let name = pieces.next().unwrap_or("").trim();
        let value = pieces
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches('"')
            .to_string();
        if !name.is_empty() && !value.is_empty() {
            values.insert(name.to_string(), value);
        }
    }
    let mut parts = Vec::new();
    for name in WANTED_COOKIES {
        if let Some(value) = values.get(name).filter(|value| !value.is_empty()) {
            parts.push(format!("{name}={value}"));
        }
    }
    parts.join("; ")
}

/// Cookie 串里有没有某一项（按 `name=` 前缀判，容忍前后空格）。
fn has_cookie(cookie: &str, name: &str) -> bool {
    let prefix = format!("{name}=");
    cookie
        .split(';')
        .any(|part| part.trim().starts_with(&prefix))
}

/// 发送短信验证码。返回 `{uuid, deviceId}`（前端随登录请求带回）。
///
/// 国内站才有这条链；国际站与上游一致地**明确拒绝**。
pub async fn send_code(realm: Realm, phone: &str) -> Result<Value, GatewayError> {
    if realm != Realm::Cn {
        return Err(GatewayError::with_status(
            400,
            "Tabbit 国际站暂不支持短信登录，请改用粘贴 Cookie",
        ));
    }
    let phone = phone.trim();
    if phone.is_empty() {
        return Err(GatewayError::with_status(400, "请填写手机号"));
    }
    let uuid = login_uuid();
    let body = json!({
        "uuid": uuid,
        "platform": LOGIN_PLATFORM,
        "version": "",
        "app": LOGIN_APP_ID,
        "mobile": phone,
    });
    let (status, value, raw, _) = post_login(realm, SEND_CODE_PATH, &body).await?;
    if !(200..300).contains(&status) {
        // 400 而不是 502：这是「用户填的号码/频率」类问题，不是上游坏了
        return Err(GatewayError::with_status(
            400,
            login_error_text(status, &value, &raw),
        ));
    }
    if let Some(code) = value.get("code").and_then(http::as_i64).filter(|code| *code != 0) {
        let message = login_error_text(status, &value, &raw);
        return Err(GatewayError::with_status(
            400,
            format!("发送验证码失败（错误码 {code}）：{message}"),
        ));
    }
    // 不打手机号（个人信息）：只留可核对的痕迹
    logging::log(
        "[Login]",
        &format!("📱 Tabbit 验证码已发送（尾号 {}）", tail_of(phone)),
    );
    Ok(json!({ "uuid": uuid, "deviceId": uuid }))
}

/// 手机号尾号（日志用；不做脱敏改造，只取后四位）
fn tail_of(phone: &str) -> String {
    let digits: Vec<char> = phone.chars().filter(char::is_ascii_digit).collect();
    digits
        .iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// 用手机号 + 验证码登录，返回可直接交给 `add_tabbit_account` 的凭证对象。
///
/// 返回 `{accessToken, cookie, uid, unlockPro, clientVersion, source, phoneTail}`
/// —— 字段名与粘贴路径**同一套**（`Credentials::from_fields` 是唯一判据），
/// 因此落库、命名、去重全部复用既有的添加路径。
pub async fn login_with_code(
    realm: Realm,
    phone: &str,
    code: &str,
    uuid: Option<&str>,
    name: Option<&str>,
) -> Result<Value, GatewayError> {
    if realm != Realm::Cn {
        return Err(GatewayError::with_status(
            400,
            "Tabbit 国际站暂不支持短信登录，请改用粘贴 Cookie",
        ));
    }
    let phone = phone.trim();
    let code = code.trim();
    if phone.is_empty() || code.is_empty() {
        return Err(GatewayError::with_status(400, "请填写手机号和验证码"));
    }
    let Some(uuid) = uuid.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(GatewayError::with_status(
            400,
            "登录会话已失效：请先点「发送验证码」，用本次返回的 uuid 登录",
        ));
    };
    let body = json!({
        "uuid": uuid,
        "platform": LOGIN_PLATFORM,
        "version": "",
        "app": LOGIN_APP_ID,
        "mobile": phone,
        "smsCode": code,
        "device_info": device_info(),
    });
    let (status, value, raw, cookies) = post_login(realm, LOGIN_PATH, &body).await?;
    if !(200..300).contains(&status) {
        return Err(GatewayError::with_status(
            400,
            login_error_text(status, &value, &raw),
        ));
    }
    if value.get("success").and_then(Value::as_bool) == Some(false) {
        return Err(GatewayError::with_status(
            400,
            login_error_text(status, &value, &raw),
        ));
    }

    let access_token = text_of(&value, &["access_token", "accessToken"]);
    let mut uid = text_of(&value, &["user_id", "userId", "id"]);
    let mut cookie = cookie_from_headers(&cookies);
    // 上游有些响应只给 token，不给 `user_id` / `SAPISID`；后续请求要整串
    // Cookie（实测只带 token 会被判 401「无权限」）。知道 uid 就补齐。
    if !cookie.contains("token=") && !access_token.is_empty() {
        cookie = credentials::synthetic_cookie(&access_token, &uid);
    }
    if !cookie.contains("token=") {
        return Err(GatewayError::with_status(
            400,
            "登录成功但没拿到登录态 Cookie，请改用「填写凭证」粘贴 Cookie",
        ));
    }
    let token = credentials::cookie_token(&cookie);
    let token = if token.is_empty() {
        access_token.clone()
    } else {
        token
    };
    if token.is_empty() {
        return Err(GatewayError::with_status(
            400,
            "登录成功但没有拿到访问令牌，请改用粘贴 Cookie",
        ));
    }
    if uid.is_empty() {
        uid = credentials::uid_from_jwt(&token);
    }
    if !uid.is_empty() {
        for name in ["user_id", "SAPISID"] {
            if !has_cookie(&cookie, name) {
                cookie.push_str(&format!("; {name}={uid}"));
            }
        }
    }
    for (name, value) in [("managed", "tab_browser"), ("NEXT_LOCALE", "zh")] {
        if !has_cookie(&cookie, name) {
            cookie.push_str(&format!("; {name}={value}"));
        }
    }

    // 交给唯一判据归一（长度闸、Cookie 过滤、uid 补全都在那里）：
    // 登录成功后仍然过一遍 `from_fields`，才能保证「登录取到的凭证」
    // 与「粘贴进来的凭证」走的是同一套校验。
    let fields = json!({
        "accessToken": token,
        "cookie": cookie,
        "uid": uid,
        "unlockPro": true,
        "clientVersion": DEFAULT_CLIENT_VERSION,
        "source": "sms",
        "phoneTail": tail_of(phone),
    });
    let normalized = credentials::Credentials::from_fields(&fields)?;

    let mut secret = fields;
    if let Some(object) = secret.as_object_mut() {
        // 用归一后的值覆盖：`from_fields` 可能补出了 uid / 重建了 Cookie
        object.insert(
            "accessToken".to_string(),
            Value::String(normalized.token.clone()),
        );
        object.insert(
            "cookie".to_string(),
            Value::String(normalized.cookie.clone()),
        );
        object.insert("uid".to_string(), Value::String(normalized.uid.clone()));
        let nickname = credentials::nickname_from_jwt(&normalized.token);
        if !nickname.is_empty() {
            object.insert("nickname".to_string(), Value::String(nickname));
        }
        if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) {
            object.insert("name".to_string(), Value::String(name.chars().take(100).collect()));
        }
    }
    logging::log(
        "[Login]",
        &format!(
            "✅ Tabbit 登录成功（尾号 {}，uid {}）",
            tail_of(phone),
            if normalized.uid.is_empty() {
                "缺失"
            } else {
                &normalized.uid
            }
        ),
    );
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 登录会话标识必须是 64 位字母数字（上游按长度校验）
    #[test]
    fn the_login_uuid_has_the_upstream_shape() {
        let uuid = login_uuid();
        assert_eq!(uuid.chars().count(), 64);
        assert!(uuid.chars().all(|ch| ch.is_ascii_alphanumeric()));
        // 两次必须不同（同一个 uuid 复用会把两次发码串成一次）
        assert_ne!(uuid, login_uuid());
    }

    /// `Set-Cookie` 只留需要的几项，且按 `WANTED_COOKIES` 的顺序拼
    #[test]
    fn only_the_wanted_cookies_survive() {
        let headers = vec![
            "token=jwt-value; Path=/; HttpOnly".to_string(),
            "user_id=42; Path=/".to_string(),
            "SAPISID=42; Path=/".to_string(),
            "noise=drop-me; Path=/".to_string(),
            "managed=tab_browser".to_string(),
        ];
        let cookie = cookie_from_headers(&headers);
        assert_eq!(
            cookie,
            "token=jwt-value; user_id=42; SAPISID=42; managed=tab_browser"
        );
        assert!(!cookie.contains("noise"));
        assert!(!cookie.contains("Path"));
    }

    /// 空值不写进 Cookie（上游出现过 `managed=` 空串）
    #[test]
    fn empty_cookie_values_are_skipped() {
        let headers = vec!["token=abc".to_string(), "managed=".to_string()];
        assert_eq!(cookie_from_headers(&headers), "token=abc");
    }

    /// 国际站必须在校验参数**之前**就被拒：否则用户填完号码才看到「不支持」
    #[tokio::test]
    async fn the_international_site_refuses_before_any_request() {
        let sent = send_code(Realm::Intl, "").await;
        assert_eq!(sent.as_ref().err().map(|e| e.status_code), Some(400));
        let login = login_with_code(Realm::Intl, "", "", None, None).await;
        assert_eq!(login.as_ref().err().map(|e| e.status_code), Some(400));
        let message = login.err().map(|e| e.message).unwrap_or_default();
        assert!(message.contains("粘贴 Cookie"), "文案要给可行替代：{message}");
    }

    /// 缺 uuid 时如实让用户重发验证码，而不是打一次注定失败的请求
    #[tokio::test]
    async fn a_missing_uuid_asks_for_a_new_code() {
        let result = login_with_code(Realm::Cn, "13800138000", "123456", Some("  "), None).await;
        let message = result.err().map(|e| e.message).unwrap_or_default();
        assert!(message.contains("发送验证码"), "文案要指回发码那一步：{message}");
    }

    /// 上游的 pydantic 前缀要剥掉，否则用户看到「Value error, 验证码错误」
    #[test]
    fn the_pydantic_prefix_is_stripped() {
        assert_eq!(tidy_upstream("Value error, 验证码错误"), "验证码错误");
        assert_eq!(tidy_upstream("  验证码错误  "), "验证码错误");
    }

    /// 错误文案的候选顺序：先 body 里的几种键，再看 detail 数组，最后看原文
    #[test]
    fn the_error_text_is_found_in_every_shape() {
        let raw = "{\"x\":1}";
        assert_eq!(
            login_error_text(400, &json!({"message": "验证码已过期"}), raw),
            "验证码已过期"
        );
        assert_eq!(
            login_error_text(400, &json!({"detail": [{"msg": "号码格式不对"}]}), raw),
            "号码格式不对"
        );
        assert_eq!(
            login_error_text(400, &json!({"detail": "太频繁"}), raw),
            "太频繁"
        );
        // 一个都认不出时回落原文（不足 300 字）
        assert_eq!(login_error_text(400, &json!({}), "  plain text  "), "plain text");
    }

    /// 尾号只取四位（日志里不出现完整手机号）
    #[test]
    fn the_logged_tail_is_four_digits() {
        assert_eq!(tail_of("+86 138-0013-8000"), "8000");
        assert_eq!(tail_of("123"), "123");
    }
}
