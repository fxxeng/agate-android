//! DeepSeek 网页版对话链路：**领题 → 求解 → 建会话 → 发问**。
//!
//! ── 四步链路（每一步都不可省）──────────────────────────────────
//! ```text
//!   1. POST /api/v0/chat/create_pow_challenge  {"target_path":"/api/v0/chat/completion"}
//!      → data.biz_data.challenge = { algorithm, challenge, salt, difficulty,
//!                                    expire_at, signature, target_path }
//!   2. 本地求解（同步，见 pow.rs）→ answer
//!   3. POST /api/v0/chat_session/create       {"character_id": null}
//!      → data.biz_data.id（**注意是 `.id`，不是 `.chat_session.id`**）
//!   4. POST /api/v0/chat/completion
//!      headers 里带 `x-ds-pow-response: <base64>`，
//!      body { chat_session_id, parent_message_id, prompt, thinking_enabled,
//!             search_enabled, stream: true, client_stream_id, model_type, … }
//!      → SSE 流（path-value 结构，见 protocol::deepseek_stream）
//! ```
//!
//! ── 三个容易踩的坑（逐字记下，都是「看不出错在哪」的那类）──────
//! ```text
//!   1. 会话 id 在 data.biz_data.id —— 不是 data.biz_data.chat_session.id。
//!      官方文档与抓包都点明了这一点（多层信封长得像，读错就恒拿到 null）。
//!   2. `expire_at` 在 PoW 前缀里**按原样（毫秒）拼**，不做单位换算：
//!      prefix = salt + "_" + expire_at + "_"。换算成秒 = 哈希永远对不上。
//!   3. 每次 completion 都要**新领一道题**。挑战带随机 salt、一次性消费，
//!      不能复用、不能预取缓存 —— 因此本函数每一步都真发请求。
//! ```
//!
//! ── 为什么 PoW 求解在**转发路径上同步**做 ─────────────────────
//! 上游要求每次 completion 都带一份新鲜题解（见上一段）。求解实测约
//! 100–500ms（`difficulty` 十几万到二十几万），必须在发请求前完成 ——
//! 没有「预取一批备着」的可能。这是本家相对别家**固定多出来的一段延迟**，
//! 如实写在模块头而不是假装它不存在。
//!
//! ── 上游不给 token 计数（如实声明）──────────────────────────
//! 网页版 SSE 不返回 usage（`protocol::deepseek_stream` 的收尾帧恒回 0）。
//! 这是上游事实，不是本实现的缺口 —— 官方文档的能力表里 usage 也是
//! `undeclared`。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::auth_http::send_raw;
use crate::server::errors::GatewayError;

use super::{endpoints, pow};

/// 各步超时（领题 / 建会话是控制面，给短；求解是本地计算不占网络）
const CONTROL_TIMEOUT_MS: u64 = 15_000;

/// 一次转发的三个产物（交给 `build_chat_request` 后面的转发层）
#[derive(Clone, Debug)]
pub struct PreparedRequest {
    /// 对话 URL
    pub url: String,
    /// 完整请求头（含 Authorization 与 `x-ds-pow-response`）
    pub headers: Vec<(String, String)>,
    /// 请求体
    pub body: Value,
}

/// 领一道 PoW 题并**本地求解**。
///
/// 返回求解结果；任一步失败返回可读的 `GatewayError`。
pub async fn obtain_and_solve_pow(
    user_token: &str,
) -> Result<pow::Solved, GatewayError> {
    let mut headers = endpoints::base_headers();
    headers.extend(endpoints::auth_headers(user_token));
    let body = json!({ "target_path": endpoints::COMPLETION_PATH });
    let response = send_raw(
        "POST",
        &endpoints::url(endpoints::POW_CHALLENGE_PATH),
        Some(&body),
        &headers,
        None,
        Some(CONTROL_TIMEOUT_MS),
    )
    .await
    .map_err(|error| {
        GatewayError::with_status(502, format!("DeepSeek 领取 PoW 题失败：{error}"))
    })?;

    if response.status == 401 || response.status == 403 {
        return Err(GatewayError::with_status(
            401,
            format!(
                "DeepSeek 登录态无效（HTTP {}）：请重新登录 chat.deepseek.com 并取新的 userToken",
                response.status
            ),
        ));
    }
    let payload = response.payload.unwrap_or(Value::Null);
    if !response.ok {
        // 题面可能带业务码（如 40002 缺 token）
        let detail = readable_error(&payload);
        return Err(GatewayError::with_status(
            if response.status == 0 { 502 } else { i32::from(response.status) },
            format!("DeepSeek 领取 PoW 题失败：{detail}"),
        ));
    }

    let challenge = pow::parse_challenge(&payload).map_err(|error| {
        GatewayError::with_status(502, format!("DeepSeek PoW 题面解析失败：{error}"))
    })?;
    challenge
        .solve()
        .map_err(|error| GatewayError::with_status(502, format!("DeepSeek {error}")))
}

/// 建一个新会话，取 `data.biz_data.id`（**不是** `.chat_session.id`，见模块头）。
pub async fn create_session(user_token: &str) -> Result<String, GatewayError> {
    let mut headers = endpoints::base_headers();
    headers.extend(endpoints::auth_headers(user_token));
    let body = json!({ "character_id": Value::Null });
    let response = send_raw(
        "POST",
        &endpoints::url(endpoints::CREATE_SESSION_PATH),
        Some(&body),
        &headers,
        None,
        Some(CONTROL_TIMEOUT_MS),
    )
    .await
    .map_err(|error| {
        GatewayError::with_status(502, format!("DeepSeek 建会话失败：{error}"))
    })?;
    let payload = response.payload.unwrap_or(Value::Null);
    if !response.ok {
        return Err(GatewayError::with_status(
            if response.status == 0 { 502 } else { i32::from(response.status) },
            format!("DeepSeek 建会话失败：{}", readable_error(&payload)),
        ));
    }
    // 严格只读 `data.biz_data.id`；读不到就报错（不猜别的层级 ——
    // 猜错的后果是往 completion 里发一个 null 会话 id，报错在更远的地方）
    payload
        .pointer("/data/biz_data/id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            GatewayError::with_status(502, "DeepSeek 建会话响应里没有 data.biz_data.id")
        })
}

/// 组装 `chat/completion` 的完整请求（含新鲜 PoW）。
///
/// `prompt` 是拍平后的单条提问（本家网页版一次只收一条 prompt，见 `plan.rs`）。
pub async fn prepare_completion(
    user_token: &str,
    prompt: &str,
    session_id: &str,
    parent_message_id: Option<&str>,
    model: &str,
    thinking: bool,
    search: bool,
) -> Result<PreparedRequest, GatewayError> {
    // 1. 新领一道题并求解（每次 completion 都要新鲜的，见模块头）
    let solved = obtain_and_solve_pow(user_token).await?;
    // 2. 组装请求头（含题解）
    let mut headers = endpoints::base_headers();
    headers.extend(endpoints::auth_headers(user_token));
    headers.extend(endpoints::sse_headers());
    headers.push((
        "x-ds-pow-response".to_string(),
        pow::response_header(&solved),
    ));
    // 3. 请求体：字段名逐字对齐二进制里的那一串
    let mut body = serde_json::Map::new();
    body.insert(
        "chat_session_id".to_string(),
        Value::String(session_id.to_string()),
    );
    body.insert(
        "parent_message_id".to_string(),
        match parent_message_id {
            Some(value) if !value.is_empty() => Value::String(value.to_string()),
            _ => Value::Null,
        },
    );
    body.insert("prompt".to_string(), Value::String(prompt.to_string()));
    body.insert("model_type".to_string(), Value::String(model_type_of(model)));
    body.insert("thinking_enabled".to_string(), Value::Bool(thinking));
    body.insert("search_enabled".to_string(), Value::Bool(search));
    body.insert("preempt".to_string(), Value::Bool(false));
    body.insert("ref_file_ids".to_string(), Value::Array(Vec::new()));
    body.insert("stream".to_string(), Value::Bool(true));
    // client_stream_id：上游要求一个「本次流」的标识（实测格式 YYYYMMDD-<随机>）
    body.insert(
        "client_stream_id".to_string(),
        Value::String(client_stream_id()),
    );

    Ok(PreparedRequest {
        url: endpoints::url(endpoints::COMPLETION_PATH),
        headers,
        body: Value::Object(body),
    })
}

/// 网页模式 → 上游的 `model_type`。
///
/// 二进制里只有两个模型名（`deepseek-web-flash` / `deepseek-web-pro`），
/// `model_type` 字段承载「快速 / 专家」这一档；未知名字回落 `flash`
/// （宁可给一个能用的档，也不要把未知串当模型名发上去换一个 400）。
pub fn model_type_of(model: &str) -> String {
    match model.trim().to_ascii_lowercase().as_str() {
        "deepseek-web-pro" | "pro" | "expert" => "expert".to_string(),
        _ => "default".to_string(),
    }
}

/// `client_stream_id`：`YYYYMMDD-<随机十六进制>`（二进制里该字段的形态）
fn client_stream_id() -> String {
    let now = crate::server::logging::now_ms();
    let date = date_part(now / 1000);
    format!("{date}-{}", random_hex(16))
}

/// 把秒级时间戳折成 `YYYYMMDD`（UTC，够用即可 —— 它是流的标识不是日期语义）。
///
/// 手写公历年月日换算（不引依赖）：从 1970-01-01 起逐段进位。
fn date_part(seconds: i64) -> String {
    let mut days = seconds.max(0) / 86_400;
    let mut year = 1970i64;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let length = if leap { 366 } else { 365 };
        if days < length {
            break;
        }
        days -= length;
        year += 1;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0usize;
    while month < months.len() && days >= months[month] {
        days -= months[month];
        month += 1;
    }
    format!("{year:04}{:02}{:02}", month + 1, days + 1)
}

/// 随机十六进制串（不引 rand：getrandom 已是既有依赖）
fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    if getrandom::getrandom(&mut buffer).is_err() {
        return format!("{:x}", crate::server::logging::now_ms());
    }
    let mut out = String::with_capacity(bytes * 2);
    for byte in buffer {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 从上游响应里读一条可读的错误文案（业务码 + 文案 / message / 裸 JSON）
pub fn readable_error(payload: &Value) -> String {
    let code = payload
        .get("code")
        .and_then(Value::as_i64)
        .or_else(|| payload.pointer("/data/biz_code").and_then(Value::as_i64));
    let text = payload
        .get("msg")
        .and_then(Value::as_str)
        .or_else(|| payload.get("message").and_then(Value::as_str))
        .or_else(|| payload.pointer("/data/biz_msg").and_then(Value::as_str))
        .unwrap_or("");
    match (code, text) {
        (Some(code), "") => format!("上游返回业务码 {code}"),
        (Some(code), text) => format!("上游返回 {code}: {text}"),
        (None, "") => {
            let flat = payload.to_string();
            if flat.len() > 200 {
                format!("上游返回：{}…", &flat[..200])
            } else {
                format!("上游返回：{flat}")
            }
        }
        (None, text) => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_type_maps_two_web_modes() {
        assert_eq!(model_type_of("deepseek-web-flash"), "default");
        assert_eq!(model_type_of("deepseek-web-pro"), "expert");
        // 未知名字回落可用档，而不是把未知串发上去换一个 400
        assert_eq!(model_type_of("gpt-5"), "default");
    }

    #[test]
    fn client_stream_id_has_date_prefix() {
        let id = client_stream_id();
        let (date, rest) = id.split_once('-').expect("应含 - 分隔");
        assert_eq!(date.len(), 8, "日期应是 YYYYMMDD");
        assert!(date.chars().all(|ch| ch.is_ascii_digit()));
        assert!(rest.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn date_part_matches_known_dates() {
        // 1970-01-01 = 0
        assert_eq!(date_part(0), "19700101");
        // 2026-11-01 00:00:00 UTC = 1793491200
        assert_eq!(date_part(1_793_491_200), "20261101");
        // 闰年边界：2024-02-29
        assert_eq!(date_part(1_709_164_800), "20240229");
    }

    #[test]
    fn readable_error_prefers_code_and_message() {
        let payload = json!({"code": 40002, "msg": "Missing Token"});
        assert_eq!(readable_error(&payload), "上游返回 40002: Missing Token");
        let nested = json!({"data": {"biz_code": 40003, "biz_msg": "权限不足"}});
        assert_eq!(readable_error(&nested), "上游返回 40003: 权限不足");
        let plain = json!({"msg": "普通错误"});
        assert_eq!(readable_error(&plain), "普通错误");
    }

    #[test]
    fn readable_error_falls_back_to_raw_json() {
        let payload = json!({"unexpected": true});
        let text = readable_error(&payload);
        assert!(text.starts_with("上游返回："));
        assert!(text.contains("unexpected"));
    }
}