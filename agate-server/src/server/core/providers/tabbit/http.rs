//! Tabbit 出站：签名请求、sign-key 缓存、房间缓存、join + runs。
//!
//! ── 出站客户端复用 `core::egress`（不自己建）──────────────────
//! 上游参考实现为了 SSE 自己建了一个「只设连接超时」的客户端，因为它的通用
//! 客户端带 20 秒**总超时**、会掐断长回答。本项目的 `egress::client_for`
//! 本来就没有总超时（只有 connect_timeout + read_timeout 后备，见
//! `core::egress` 的模块头），因此直接复用 —— 少一份客户端、少一处超时口径。
//! 控制面请求各自 `.timeout()`，SSE 请求不设总超时。
//!
//! ── 两张进程级缓存 ──────────────────────────────────────────
//! ```text
//!   sign_keys  站点 → (key, 取到的时刻)     TTL 30 分钟
//!   sessions   站点|uid → (房间 id, 时刻)   TTL 30 分钟（每次命中续期）
//! ```
//! 房间缓存是**上游的产品设定**（一次房间可承载多轮、30 分钟量级），不是我们
//! 为了省事编的；期间被上游以 409「会话正在处理中」拒绝时，`drop_session`
//! 立刻把它扔掉，下一次自然新建（见 `chat.rs` 的重试）。
//!
//! ── 签名与 499 ──────────────────────────────────────────────
//! 只有 `join` 带签名（`runs` 不带，这是实测形态）。签名 key 由服务端轮换，
//! 过期/轮换后首个请求回 **499** —— 那既不是鉴权失败（不是 401）也不是限流，
//! 唯一正确的处置是「刷一次 key 重发」，见 `open_join`。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic；持锁不跨 await
//! （两处缓存都是「取锁 → 读/写 → 立刻放锁」，网络请求在锁外）。

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use futures::StreamExt;
use serde_json::{json, Value};

use crate::server::core::proxies::ResolvedProxy;
use crate::server::errors::GatewayError;

use super::credentials::Credentials;
use super::sign;
use super::{endpoints, Realm, SESSION_TTL_MS, SIGN_KEY_TTL_MS};

/// 控制面请求的超时（目录 / 额度 / 资料 / 签到）
const CONTROL_TIMEOUT: Duration = Duration::from_secs(20);
/// 投递一次对话的超时：上游排队时首个响应可能很慢，给 60 秒
/// （SSE 本身不设总超时 —— 那是 `egress` 的口径）
const RUN_TIMEOUT: Duration = Duration::from_secs(60);
/// 响应体大小上限（防上游返回一坨东西把内存吃满）
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// 出站客户端（复用 egress 的超时口径：连接超时 + 读后备，**无总超时**）
pub fn client(proxy: Option<&ResolvedProxy>) -> std::sync::Arc<reqwest::Client> {
    crate::server::core::egress::client_for(proxy)
}

// ─── sign-key 缓存 ──────────────────────────────────────────

fn sign_keys() -> &'static Mutex<BTreeMap<String, (String, i64)>> {
    static CACHE: OnceLock<Mutex<BTreeMap<String, (String, i64)>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// 取签名 key：缓存命中 → 拉取 → 兜底顺序（见模块头）
pub async fn sign_key(
    creds: &Credentials,
    realm: Realm,
    client: &reqwest::Client,
) -> String {
    let key = realm.id().to_string();
    let now = crate::server::logging::now_ms();
    {
        let guard = match sign_keys().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some((cached, at)) = guard.get(&key) {
            if now - *at < SIGN_KEY_TTL_MS {
                return cached.clone();
            }
        }
    }
    match fetch_sign_key(creds, realm, client).await {
        Ok(fresh) => {
            store_sign_key(realm, &fresh, now);
            fresh
        }
        Err(_) => {
            // 拉不到：先用上次那份（可能还轮换不了那么快），再退本站兜底 key
            let guard = match sign_keys().lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard
                .get(&key)
                .map(|(cached, _)| cached.clone())
                .unwrap_or_else(|| realm.fallback_sign_key().to_string())
        }
    }
}

fn store_sign_key(realm: Realm, key: &str, at: i64) {
    let mut guard = match sign_keys().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.insert(realm.id().to_string(), (key.to_string(), at));
}

/// 真打一次 `/chat/sign-key`
async fn fetch_sign_key(
    creds: &Credentials,
    realm: Realm,
    client: &reqwest::Client,
) -> Result<String, String> {
    let mut request = client
        .get(format!("{}{}", realm.base(), endpoints::SIGN_KEY_PATH))
        .timeout(CONTROL_TIMEOUT)
        .header("Accept", "text/plain")
        .header("Cookie", &creds.cookie);
    for (name, value) in endpoints::base_headers(realm, &creds.uid, creds.version(), creds.unlock_pro, "/") {
        request = request.header(name.as_str(), value.as_str());
    }
    let response = request.send().await.map_err(|_| "Tabbit 签名 key 请求失败")?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let key = text.trim();
    // 上游正常回一段纯 key；回了 HTML / JSON / 带空格的东西说明打到了别的地方
    if status != 200
        || key.is_empty()
        || key.len() > 512
        || key.contains(['<', '>', ' ', '\n', '\r', '{', '}', '"'])
    {
        return Err(format!("Tabbit 签名 key 无效（HTTP {status}）"));
    }
    Ok(key.to_string())
}

/// 强制刷一次 key（499 之后用）
async fn refresh_sign_key(creds: &Credentials, realm: Realm, client: &reqwest::Client) {
    if let Ok(fresh) = fetch_sign_key(creds, realm, client).await {
        store_sign_key(realm, &fresh, crate::server::logging::now_ms());
    }
}

// ─── 房间缓存 ───────────────────────────────────────────────

fn sessions() -> &'static Mutex<BTreeMap<String, (String, i64)>> {
    static CACHE: OnceLock<Mutex<BTreeMap<String, (String, i64)>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

fn session_key(realm: Realm, uid: &str) -> String {
    format!("{}|{uid}", realm.id())
}

/// 取房间 id（缓存命中就续期，30 分钟滑动窗口）
pub async fn session_id(
    creds: &Credentials,
    realm: Realm,
    client: &reqwest::Client,
) -> Result<String, GatewayError> {
    let cache_key = session_key(realm, &creds.uid);
    let now = crate::server::logging::now_ms();
    {
        let mut guard = match sessions().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some((id, at)) = guard.get_mut(&cache_key) {
            if now - *at < SESSION_TTL_MS {
                *at = now;
                return Ok(id.clone());
            }
        }
    }
    // 上游的 body 是**空串**（不是 `null`）—— 签名要签同一份字节，见 `signed_json`
    let value = signed_json(creds, realm, client, "POST", endpoints::SESSION_PATH, None, "/newtab").await?;
    let id = value
        .get("chat_session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| GatewayError::with_status(502, "Tabbit 没有返回会话 id"))?
        .to_string();
    {
        let mut guard = match sessions().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.insert(cache_key, (id.clone(), now));
    }
    Ok(id)
}

/// 扔掉房间缓存（409「正在处理中」之后用）
pub fn drop_session(creds: &Credentials, realm: Realm) {
    let cache_key = session_key(realm, &creds.uid);
    let mut guard = match sessions().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.remove(&cache_key);
}

// ─── 签名请求 ───────────────────────────────────────────────

/// 带签名的一次控制面请求（JSON 出、JSON 入）。
///
/// `body` 为 `None` 时按**空串**签名与发送 —— `/panel/session` 的上游约定就是
/// 空 body，签一个 `null` 上去签名就错了。
pub async fn signed_json(
    creds: &Credentials,
    realm: Realm,
    client: &reqwest::Client,
    method: &str,
    path: &str,
    body: Option<Value>,
    referer: &str,
) -> Result<Value, GatewayError> {
    let body_text = match body {
        Some(Value::Null) | None => String::new(),
        Some(value) => value.to_string(),
    };
    let key = sign_key(creds, realm, client).await;
    let mut headers = signed_headers(creds, realm, referer, &body_text, &key);
    headers.push(("Accept".to_string(), "application/json".to_string()));
    if method == "POST" {
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
    }
    let (status, bytes) = send(
        client,
        method,
        &format!("{}{}", realm.base(), path),
        &headers,
        (method == "POST").then_some(body_text.as_str()),
        CONTROL_TIMEOUT,
    )
    .await?;
    let value = parse_json(&bytes);
    if !(200..300).contains(&status) {
        return Err(classify(status, &value));
    }
    Ok(value)
}

/// 基础头 + trace-id + Cookie + Authorization + 签名三件套
fn signed_headers(
    creds: &Credentials,
    realm: Realm,
    referer_path: &str,
    body: &str,
    sign_key: &str,
) -> Vec<(String, String)> {
    let mut headers = endpoints::base_headers(realm, &creds.uid, creds.version(), creds.unlock_pro, referer_path);
    headers.push(("trace-id".to_string(), sign::random_uuid()));
    headers.push(("Cookie".to_string(), creds.cookie.clone()));
    if !creds.token.is_empty() {
        headers.push(("Authorization".to_string(), format!("Bearer {}", creds.token)));
    }
    headers.extend(sign::signature_headers(sign_key, body, crate::server::logging::now_ms()));
    headers
}

/// 发一次请求，返回 (状态码, 响应体字节)
async fn send(
    client: &reqwest::Client,
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    timeout: Duration,
) -> Result<(u16, Vec<u8>), GatewayError> {
    let mut request = match method {
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "DELETE" => client.delete(url),
        _ => client.get(url),
    }
    .timeout(timeout);
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    if let Some(body) = body {
        request = request.body(body.to_string());
    }
    let response = request.send().await.map_err(|error| {
        GatewayError::with_status(
            502,
            format!("Tabbit 请求失败：{}", crate::server::core::egress::describe_error_detail(&error)),
        )
    })?;
    let status = response.status().as_u16();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| GatewayError::with_status(502, "读取 Tabbit 响应失败"))?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err(GatewayError::with_status(502, "Tabbit 响应超过大小上限"));
    }
    Ok((status, bytes.to_vec()))
}

fn parse_json(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice::<Value>(bytes).unwrap_or(Value::Null)
}

/// 从错误响应里挖一句可读的原文（上游各接口的键名不统一）
pub fn error_text(value: &Value) -> String {
    const CANDIDATES: [&str; 6] = ["message", "msg", "detail", "error_message", "displayTips", "error"];
    for key in CANDIDATES {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            let text = text.trim();
            if !text.is_empty() {
                return text.to_string();
            }
        }
        // 有的接口是 `{"error": {"message": "…"}}`
        if let Some(text) = value
            .get(key)
            .and_then(|inner| inner.get("message"))
            .and_then(Value::as_str)
        {
            let text = text.trim();
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    let text = value.to_string();
    if text == "null" {
        return String::new();
    }
    if text.chars().count() > 300 {
        return text.chars().take(300).collect();
    }
    text
}

/// 上游状态码 → 网关错误。
///
/// 三处「不能当成普通失败」的映射（与 `adapter::classify_error` 同一口径，
/// 这里给出的是**转发路径**上要的形态）：
///   - 401 / 403 → 401（登录态失效：换账号，别重试）
///   - 429 / 492 → 429（限流 / 权益不足：冷却换号）
///   - 409       → 409（房间被占用：扔房间重来，不罚账号额度）
pub fn classify(status: u16, value: &Value) -> GatewayError {
    let message = {
        let text = error_text(value);
        if text.is_empty() {
            match status {
                401 | 403 => "Tabbit 登录态已失效，请重新导出 Cookie".to_string(),
                409 => "Tabbit 会话正在处理中，请稍后重试".to_string(),
                429 => "Tabbit 请求过于频繁，请稍后重试".to_string(),
                492 => "Tabbit 当前账号没有该模型的权限（需要 Pro 或今日额度已用完）".to_string(),
                other => format!("Tabbit 请求失败（HTTP {other}）"),
            }
        } else {
            text
        }
    };
    let status_code = match status {
        401 | 403 => 401,
        429 | 492 => 429,
        409 => 409,
        1..=399 => i32::from(status),
        400..=499 => 502,
        _ => 502,
    };
    GatewayError::with_status(status_code, message)
}

// ─── 账号资料 ───────────────────────────────────────────────

/// 探活并取 (uid, 昵称)。
///
/// 这一步同时是**添加账号时的校验**：粘贴进来的 Cookie 若属于另一个站点，
/// 这里会以 401/403 失败，调用方据此提示「改用另一张卡片」。
pub async fn user_info(
    creds: &Credentials,
    realm: Realm,
    client: &reqwest::Client,
) -> Result<(String, String), GatewayError> {
    let value = signed_json(creds, realm, client, "GET", endpoints::USER_INFO_PATH, None, "/").await?;
    let info = value.get("user_info").cloned().unwrap_or(Value::Null);
    let uid = super::credentials::pick(&info, &["user_id", "uid", "id"]);
    let mut nickname = super::credentials::pick(&info, &["display_name", "nickname", "name", "user_name"]);
    if nickname.is_empty() {
        nickname = super::credentials::nickname_from_jwt(&creds.token);
    }
    Ok((uid, nickname))
}

// ─── 对话：join + runs ──────────────────────────────────────

/// join 的请求体（`device_id_hash` 由 uid + 房间派生：同一房间同一设备）
pub fn build_join_payload(seed: &str) -> String {
    json!({
        "page_instance_id": sign::random_uuid(),
        "device_id_hash": sign::sha256_hex(format!("tabbit-page:{seed}").as_bytes()),
        "surface": "panel",
        "last_event_id": Value::Null,
    })
    .to_string()
}

/// runs 的请求体：一段纯文本 + 选中的模型（上游只认 display_name）
pub fn build_run_payload(content: &str, model: &str) -> String {
    let run_id = format!("tab_{}", sign::random_uuid());
    json!({
        "client_run_id": run_id,
        "client_message_id": run_id,
        "agent_plan_mode_enabled": false,
        "input_payload": {
            "task_name": "chat",
            "content": content,
            "parent_message_id": Value::Null,
            "selected_model": [model],
            "references": [],
            "metadatas": { "html_content": format!("<p>{content}</p>") },
            "page_info_list": [],
            "is_mobile": false,
            "agent_mode": false,
            "entity": { "key": sign::md5_empty_hex(), "extras": { "type": "tab", "url": "" } },
        },
    })
    .to_string()
}

/// 订阅房间事件流（**唯一带签名的对话请求**）。
///
/// 499 时刷新 sign-key 重试一次 —— 那是「key 被轮换」的唯一正确处置。
/// 注意这里**不设总超时**：SSE 是长连接，`egress` 只给了连接超时与读后备。
pub async fn open_join(
    creds: &Credentials,
    realm: Realm,
    room: &str,
    body: &str,
    client: &reqwest::Client,
) -> Result<reqwest::Response, GatewayError> {
    let target = format!("{}{}", realm.base(), endpoints::join_path(room));
    let referer = format!("/panel/{room}");
    for attempt in 0..2 {
        let mut headers =
            endpoints::base_headers(realm, &creds.uid, creds.version(), creds.unlock_pro, &referer);
        headers.push(("Accept".to_string(), "text/event-stream".to_string()));
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
        headers.push(("Cache-Control".to_string(), "no-cache".to_string()));
        headers.push(("trace-id".to_string(), sign::random_uuid()));
        headers.push(("Cookie".to_string(), creds.cookie.clone()));
        let key = sign_key(creds, realm, client).await;
        headers.extend(sign::signature_headers(&key, body, crate::server::logging::now_ms()));

        let mut request = client.post(&target);
        for (name, value) in &headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let response = request.body(body.to_string()).send().await.map_err(|error| {
            GatewayError::with_status(
                502,
                format!(
                    "无法连接 Tabbit：{}",
                    crate::server::core::egress::describe_error_detail(&error)
                ),
            )
        })?;
        let status = response.status().as_u16();
        if status == 499 && attempt == 0 {
            refresh_sign_key(creds, realm, client).await;
            continue;
        }
        if (200..300).contains(&status) {
            // 正常是 SSE；JSON 说明上游用 200 包了一个业务错误（权益 / 会话）
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            if content_type.contains("application/json") {
                let bytes = response.bytes().await.unwrap_or_default();
                let value = parse_json(&bytes);
                return Err(classify(status, &value));
            }
            return Ok(response);
        }
        let bytes = response.bytes().await.unwrap_or_default();
        let value = parse_json(&bytes);
        if status == 409 {
            drop_session(creds, realm);
        }
        return Err(classify(status, &value));
    }
    Err(GatewayError::with_status(
        502,
        "Tabbit 签名 key 刷新后仍返回 499",
    ))
}

/// 投递一次对话（**不带签名**），返回 run_id。
pub async fn post_run(
    creds: &Credentials,
    realm: Realm,
    room: &str,
    body: &str,
    client: &reqwest::Client,
) -> Result<String, GatewayError> {
    let target = format!("{}{}", realm.base(), endpoints::run_path(room));
    let referer = format!("/panel/{room}");
    let mut headers =
        endpoints::base_headers(realm, &creds.uid, creds.version(), creds.unlock_pro, &referer);
    headers.push(("Accept".to_string(), "application/json".to_string()));
    headers.push(("Content-Type".to_string(), "application/json".to_string()));
    headers.push(("trace-id".to_string(), sign::random_uuid()));
    headers.push(("Cookie".to_string(), creds.cookie.clone()));
    if !creds.token.is_empty() {
        headers.push(("Authorization".to_string(), format!("Bearer {}", creds.token)));
    }
    let (status, bytes) = send(client, "POST", &target, &headers, Some(body), RUN_TIMEOUT).await?;
    let value = parse_json(&bytes);
    if !(200..300).contains(&status) {
        if status == 409 {
            drop_session(creds, realm);
        }
        return Err(classify(status, &value));
    }
    let run_id = super::credentials::pick(&value, &["run_id", "runId"]);
    if run_id.is_empty() {
        return Err(GatewayError::with_status(502, "Tabbit 未返回 run 信息"));
    }
    Ok(run_id)
}

/// 上游的数值有时是字符串、有时是百分比串 —— 统一成 i64
pub fn as_i64(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    if let Some(number) = value.as_f64() {
        return Some(number as i64);
    }
    value.as_str().and_then(|text| text.trim().parse::<i64>().ok())
}

/// 同上，取 f64（额度是百分比，需要小数）
pub fn as_f64(value: &Value) -> Option<f64> {
    if let Some(number) = value.as_f64() {
        return Some(number);
    }
    value
        .as_str()
        .and_then(|text| text.trim().trim_end_matches('%').parse::<f64>().ok())
}

/// 读上游事件流的字节流（SSE），已接上空闲兜底
pub fn byte_stream(
    response: reqwest::Response,
    idle: Duration,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> {
    let source = response.bytes_stream().map(move |item| {
        item.map_err(|error| {
            std::io::Error::other(crate::server::core::egress::describe_error_detail(&error))
        })
    });
    crate::server::core::upstream::stall::idle_guard(Box::pin(source), idle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 749 / 492 这类业务码必须映射成「冷却换号」，不能当 502 原样透传
    #[test]
    fn status_mapping_keeps_the_rotation_semantics() {
        assert_eq!(classify(401, &Value::Null).status_code, 401);
        assert_eq!(classify(403, &Value::Null).status_code, 401);
        assert_eq!(classify(429, &Value::Null).status_code, 429);
        assert_eq!(classify(492, &Value::Null).status_code, 429);
        assert_eq!(classify(409, &Value::Null).status_code, 409);
        assert_eq!(classify(500, &Value::Null).status_code, 502);
        assert_eq!(classify(404, &Value::Null).status_code, 502);
    }

    /// 上游原文优先于我们的兜底文案（用户看到的应该是上游说的话）
    #[test]
    fn upstream_message_wins_over_the_fallback() {
        let error = classify(429, &json!({ "message": "今日额度已用完" }));
        assert_eq!(error.message, "今日额度已用完");
        let nested = classify(401, &json!({ "error": { "message": "token expired" } }));
        assert_eq!(nested.message, "token expired");
    }

    /// 空 body 的响应不能当错误（有的接口成功时回 200 + 空）
    #[test]
    fn empty_body_parses_to_null() {
        assert!(parse_json(b"").is_null());
        assert!(parse_json(b"not json").is_null());
    }

    /// join 的 payload 里 `last_event_id` 必须是 null（订阅从头开始）
    #[test]
    fn join_payload_subscribes_from_the_beginning() {
        let value: Value = serde_json::from_str(&build_join_payload("u1|room")).expect("payload 应是合法 JSON");
        assert!(value["last_event_id"].is_null());
        assert_eq!(value["surface"], "panel");
        // device_id_hash 是稳定派生：同一 seed 两次一致
        let again: Value = serde_json::from_str(&build_join_payload("u1|room")).expect("payload 应是合法 JSON");
        assert_eq!(value["device_id_hash"], again["device_id_hash"]);
        // page_instance_id 每次都换（它是「这一次页面实例」）
        assert_ne!(value["page_instance_id"], again["page_instance_id"]);
    }

    /// runs 的 payload：模型走 `selected_model` 数组，内容进 `content`
    #[test]
    fn run_payload_carries_the_content_and_the_model() {
        let value: Value =
            serde_json::from_str(&build_run_payload("你好", "GPT-5.5")).expect("payload 应是合法 JSON");
        assert_eq!(value["input_payload"]["content"], "你好");
        assert_eq!(value["input_payload"]["selected_model"][0], "GPT-5.5");
        assert_eq!(value["input_payload"]["task_name"], "chat");
        // run id 与 message id 同值（上游按它配对）
        assert_eq!(value["client_run_id"], value["client_message_id"]);
        // entity.key 是空内容的 md5
        assert_eq!(value["input_payload"]["entity"]["key"], sign::md5_empty_hex());
    }

    /// 缓存键必须按站点分开：两站的 uid 空间不同，混了会把 A 站的房间给 B 站用
    #[test]
    fn session_cache_keys_never_mix_realms() {
        assert_ne!(session_key(Realm::Cn, "u1"), session_key(Realm::Intl, "u1"));
        assert_eq!(session_key(Realm::Cn, "u1"), "tabbit|u1");
    }
}
