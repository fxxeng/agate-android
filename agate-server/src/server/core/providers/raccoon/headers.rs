//! 小浣熊对话链路的身份头：box_agent（官方 Python 后端）的流量形态。
//!
//! ── 为什么整套换掉（2026-11「很少流控」基线）────────────────
//! 上一版对话身份是「1.1.5 桌面端 JS 三头 + 每请求随机的 `X-Request-Id`」。
//! 上游对 LLM 网关的请求按「像不像官方客户端」打分，两个已实锤的风控点
//! （AgentHub 项目 `docs/raccoon-反代` 会话 1~5 的逆向记录，与插件
//! 3.4.3-beta.3 二进制常量区互相印证）：
//!
//!   · 官方 box_agent 链路带 `x-client-*` 六头 + `X-RACCOON-*` 会话关联头，
//!     缺失的请求被当作裸 API 客户端静默降级；
//!   · **每请求随机 Session-ID 配上多轮完整历史**，上游看到「全新会话却带全部
//!     上下文」的逻辑矛盾，触发风控静默丢弃 —— 表现是真实请求 10s 首字节超时
//!     而测试连接能过。这就是「很少流控版本」修的主问题。
//!
//! 取值逐项对齐两份事实源：官方 `Raccoon-Office/Box-Agent` 公开源码
//! （`client_info.py` / `llm/base.py`）与 AgentHub `rules.cjs` 的 raccoon 配置
//! （asar 解包实测）。`User-Agent` + `X-Stainless-*` 在插件二进制常量区出现，
//! 是 openai Python SDK 2.8.0 的身份层（box_agent 就是一个 openai SDK 客户端）。
//!
//! ── 与浏览器域身份头互不影响 ────────────────────────────────
//! 余额 / 签到 / 新手任务走的是浏览器域「`X-Client-*` 大写三头」（无架构后缀），
//! 见 `balance::desktop_client_headers()`，本文件不动它。

use sha2::{Digest, Sha256};
use serde_json::Value;

use crate::server::errors::GatewayError;

use super::jwt;

/// box_agent 链路六头的固定取值（AgentHub `rules.cjs` raccoon 配置的实测值）。
const CLIENT_NAME: &str = "raccoon-ai";
const CLIENT_PLATFORM: &str = "desktop-windows-x64";
const CLIENT_VERSION: &str = "v1.1.5";
const CLIENT_OS_VERSION: &str = "10.0.26200";
const CLIENT_CHANNEL: &str = "official";

/// openai Python SDK 2.8.0 的默认身份层（官方 box_agent 就是一个 openai SDK 客户端）。
const SDK_USER_AGENT: &str = "OpenAI/Python 2.8.0";
const SDK_PACKAGE_VERSION: &str = "2.8.0";
const SDK_RUNTIME: &str = "CPython";
/// Runtime-Version 官方每台机器随其 Python 补丁号而不同，取常量即可 —— 上游能
/// 校验的只是「CPython 形态在不在」，不可能钉死某个补丁号。
const SDK_RUNTIME_VERSION: &str = "3.13.5";
/// SDK 每请求的固定形态：重试计数从 0 起、读超时 600s（SDK 默认值）。
const SDK_RETRY_COUNT: &str = "0";
const SDK_READ_TIMEOUT: &str = "600";

/// 组织身份在账号记录里的键（与 `oauth.rs` 落库、`add_raccoon_account`
/// 的取值链同一套；兼容上游返回的驼峰写法）。
fn org_code(account: &Value) -> String {
    let pick = |value: &Value| -> Option<String> {
        value
            .get("office_identity")
            .or_else(|| value.get("officeIdentity"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    pick(account)
        .or_else(|| account.get("auth").and_then(pick))
        .unwrap_or_default()
}

/// 组织身份头：个人账号不发；值必须是可见 ASCII（脏值会让整条请求失败，
/// 而不是只丢这一个头，所以要在拼头之前挡掉）。
fn append_org(headers: &mut Vec<(String, String)>, account: &Value) -> Result<(), GatewayError> {
    let code = org_code(account);
    if code.is_empty() || code.eq_ignore_ascii_case("personal") {
        return Ok(());
    }
    if !code.is_ascii() || code.bytes().any(|byte| byte < 32 || byte == 127) {
        return Err(GatewayError::with_status(
            400,
            "小浣熊组织身份无效，请重新导入登录态",
        ));
    }
    headers.push(("X-Org-Code".to_string(), code));
    Ok(())
}

/// 账号稳定键：JWT 里的平台用户 id（`sid`/`iss`/`sub` 链，见 `jwt::extract_user_id`）。
/// 同一账号刷新令牌后不变，是设备指纹与会话键的种子；没有登录态信息时
/// 回落账号记录的 `id`。取不到就返回空串 —— 相关头省发，不编值。
fn account_key(account: &Value) -> String {
    let token = account
        .get("auth")
        .and_then(|auth| auth.get("accessToken"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !token.is_empty() {
        if let Some(claims) = jwt::decode_jwt_claims(token) {
            let id = jwt::extract_user_id(&claims);
            if !id.is_empty() {
                return id;
            }
        }
    }
    account
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("")
        .to_string()
}

/// 各部分用单元分隔符连接后取 SHA-256 hex —— 部件之间不会互相粘连出歧义。
fn sha256_hex(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0x1f]);
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 64 位 hex 指纹 → UUID v4 形态（8-4-4-4-12，版本位 4、变体位 a）。
fn uuid_from_hex(hex: &str) -> String {
    format!(
        "{}-{}-4{}-a{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

/// 设备指纹：每号一个稳定 UUID（`x-client-device-id`）。官方客户端的受信
/// 设备绑定就是这个语义；顺序号或时间戳会成为风控特征，这里只用散列。
pub(super) fn device_id(account: &Value) -> String {
    let key = account_key(account);
    if key.is_empty() {
        return String::new();
    }
    uuid_from_hex(&sha256_hex(&["agate:raccoon:device", &key]))
}

/// 会话键：优先信客户端自带的会话标记（网关透传的 `x-session-id`）；没有就
/// 落在「账号 + 模型 + 首条用户消息开头」上 —— 多轮对话里历史逐轮变化，
/// 只有对话起点是稳定的锚点。
pub(super) fn conversation_key(
    account: &Value,
    model: &str,
    body: &Value,
    inbound_session: &str,
) -> String {
    let inbound = inbound_session.trim();
    if !inbound.is_empty()
        && inbound.len() <= 256
        && inbound.bytes().any(|byte| !byte.is_ascii_whitespace())
        && inbound.bytes().all(|byte| (32..127).contains(&byte))
    {
        return format!("inbound:{inbound}");
    }
    let key = account_key(account);
    let first_user = first_user_text(body).unwrap_or_default();
    let anchor: String = first_user.chars().take(256).collect();
    sha256_hex(&["agate:raccoon:conv", &key, model, &anchor])
}

/// 首条有字符串 content 的用户消息文本（没有则 None）。
fn first_user_text(body: &Value) -> Option<String> {
    body.get("messages")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|message| {
            if message.get("role").and_then(Value::as_str) != Some("user") {
                return None;
            }
            match message.get("content") {
                Some(Value::String(text)) => Some(text.clone()),
                _ => None,
            }
        })
}

/// 会话与轮次 id（`X-RACCOON-Session-ID` / `X-RACCOON-Turn-ID`）。
///
/// 同一会话内 Session-ID 必须恒定、Turn-ID 随轮次推进 —— 这是官方客户端的
/// 行为，也是上一版「每请求随机」被风控静默丢弃的根因。轮次号取消息条数
/// （第几轮对话），与官方客户端的推进方式一致。
pub(super) fn session_and_turn(account: &Value, conv: &str, turn: usize) -> (String, String) {
    let key = account_key(account);
    let seed = sha256_hex(&["agate:raccoon:sess", &key, conv]);
    let session = uuid_from_hex(&seed);
    let turn_seed = sha256_hex(&[&seed, "turn", &turn.to_string()]);
    (session, uuid_from_hex(&turn_seed))
}

/// `X-RACCOON-Title`：官方客户端取首条用户消息压缩空白后的前 20 个字符；
/// 空消息留空（上游把空标题当异常信号，但不编内容）。HTTP 头值只能是
/// latin-1：非 ASCII 码点逐字符做 UTF-8 百分号编码，中文标题才进得了头。
pub(super) fn title_header(body: &Value) -> String {
    let raw = first_user_text(body).unwrap_or_default();
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for ch in collapsed.chars().take(20) {
        if ('\x20'..='\x7E').contains(&ch) {
            out.push(ch);
        } else {
            let mut buf = [0u8; 4];
            for byte in ch.encode_utf8(&mut buf).as_bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

/// `x-client-*` 六头 + openai SDK 身份层（对话与生图共用，值见模块头常量）。
fn push_client_and_sdk(headers: &mut Vec<(String, String)>, account: &Value) {
    headers.push(("x-client-name".to_string(), CLIENT_NAME.to_string()));
    headers.push(("x-client-platform".to_string(), CLIENT_PLATFORM.to_string()));
    headers.push(("x-client-version".to_string(), CLIENT_VERSION.to_string()));
    headers.push((
        "x-client-os-version".to_string(),
        CLIENT_OS_VERSION.to_string(),
    ));
    headers.push(("x-client-channel".to_string(), CLIENT_CHANNEL.to_string()));
    let device = device_id(account);
    if !device.is_empty() {
        headers.push(("x-client-device-id".to_string(), device));
    }

    headers.push(("User-Agent".to_string(), SDK_USER_AGENT.to_string()));
    headers.push((
        "X-Stainless-Package-Version".to_string(),
        SDK_PACKAGE_VERSION.to_string(),
    ));
    headers.push(("X-Stainless-Runtime".to_string(), SDK_RUNTIME.to_string()));
    headers.push((
        "X-Stainless-Runtime-Version".to_string(),
        SDK_RUNTIME_VERSION.to_string(),
    ));
    headers.push((
        "X-Stainless-Retry-Count".to_string(),
        SDK_RETRY_COUNT.to_string(),
    ));
    headers.push((
        "X-Stainless-Read-Timeout".to_string(),
        SDK_READ_TIMEOUT.to_string(),
    ));
}

/// 给**对话请求**补齐 box_agent 链路的完整身份头
/// （调用于 `mod.rs::build_chat_request`）。
///
/// 组成：组织身份（团队账号）→ `x-client-*` 六头 → openai SDK 身份层 →
/// `X-RACCOON-*` 会话关联头。全部头名与官方客户端同形（六头小写、
/// 关联头大写驼峰），值见模块头常量。
pub(super) fn chat_identity(
    account: &Value,
    body: &Value,
    model: &str,
    inbound_session: &str,
) -> Result<Vec<(String, String)>, GatewayError> {
    let mut headers: Vec<(String, String)> = Vec::with_capacity(17);
    append_org(&mut headers, account)?;
    push_client_and_sdk(&mut headers, account);

    let turn = body
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| messages.len())
        .unwrap_or(1);
    let conv = conversation_key(account, model, body, inbound_session);
    let (session, turn_id) = session_and_turn(account, &conv, turn);
    headers.push(("X-RACCOON-Session-ID".to_string(), session));
    headers.push(("X-RACCOON-Turn-ID".to_string(), turn_id));
    headers.push(("X-RACCOON-Title".to_string(), title_header(body)));
    headers.push(("X-RACCOON-Call-Kind".to_string(), "chat".to_string()));
    Ok(headers)
}

/// 生图请求的身份头（`image_generation.rs::identity_headers` 调用）。
///
/// 与对话同一套 box_agent 身份层，会话关联头换生图语义 —— 插件
/// 3.4.3-beta.3 常量区带 `raccoon-image` / `Raccoon Image`：Call-Kind 为
/// `raccoon-image`、Title 固定 `Raccoon Image`。生图没有多轮会话，
/// Session-ID 按「账号 + image」取稳定值。
pub(super) fn image_identity(account: &Value) -> Result<Vec<(String, String)>, GatewayError> {
    let mut headers: Vec<(String, String)> = Vec::with_capacity(16);
    append_org(&mut headers, account)?;
    push_client_and_sdk(&mut headers, account);

    let (session, _) = session_and_turn(account, "image", 1);
    headers.push(("X-RACCOON-Session-ID".to_string(), session));
    headers.push(("X-RACCOON-Title".to_string(), "Raccoon Image".to_string()));
    headers.push(("X-RACCOON-Call-Kind".to_string(), "raccoon-image".to_string()));
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn has(headers: &[(String, String)], name: &str) -> Option<String> {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    }

    /// 账号身份键：优先 JWT 里的平台用户 id，回落账号记录的 `id`
    fn account_with(token: &str) -> Value {
        json!({ "auth": { "accessToken": token } })
    }

    const TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1c2VyLTQyIn0.sig";

    #[test]
    fn chat_identity_has_the_full_box_agent_set() {
        let body = json!({ "model": "raccoon-8c4485",
            "messages": [{ "role": "user", "content": "你好" }] });
        let headers =
            chat_identity(&account_with(TOKEN), &body, "raccoon-8c4485", "").expect("个人账号");
        assert_eq!(has(&headers, "x-client-name").as_deref(), Some("raccoon-ai"));
        assert_eq!(
            has(&headers, "x-client-platform").as_deref(),
            Some("desktop-windows-x64")
        );
        assert_eq!(has(&headers, "x-client-version").as_deref(), Some("v1.1.5"));
        assert_eq!(
            has(&headers, "x-client-os-version").as_deref(),
            Some("10.0.26200")
        );
        assert_eq!(has(&headers, "x-client-channel").as_deref(), Some("official"));
        assert!(has(&headers, "x-client-device-id").is_some());
        assert_eq!(has(&headers, "User-Agent").as_deref(), Some("OpenAI/Python 2.8.0"));
        assert_eq!(
            has(&headers, "X-Stainless-Retry-Count").as_deref(),
            Some("0")
        );
        assert_eq!(has(&headers, "X-RACCOON-Call-Kind").as_deref(), Some("chat"));
        assert!(has(&headers, "X-Org-Code").is_none());
    }

    /// 设备指纹每号一个、号内稳定，形态是 UUID v4
    #[test]
    fn device_id_is_stable_uuid_per_account() {
        let first = device_id(&account_with(TOKEN));
        let again = device_id(&account_with(TOKEN));
        assert_eq!(first, again, "同一账号必须稳定");
        assert_eq!(first.len(), 36, "{first}");
        assert_eq!(&first[14..15], "4", "版本位：{first}");
        assert_eq!(&first[19..20], "a", "变体位：{first}");

        let other_token = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1c2VyLTc3In0.sig";
        assert_ne!(first, device_id(&account_with(other_token)), "不同账号必须互异");
    }

    /// 同一会话内 Session-ID 恒定；轮次推进只换 Turn-ID；跨账号不共享
    #[test]
    fn session_is_stable_and_turn_advances() {
        let body = json!({ "model": "m",
            "messages": [{ "role": "user", "content": "hi" }] });
        let (s1, t1) = session_and_turn(&account_with(TOKEN), "conv-1", 1);
        let (s2, t2) = session_and_turn(&account_with(TOKEN), "conv-1", 1);
        assert_eq!(s1, s2);
        assert_eq!(t1, t2);

        let (_, t3) = session_and_turn(&account_with(TOKEN), "conv-1", 2);
        assert_eq!(s1, session_and_turn(&account_with(TOKEN), "conv-1", 2).0);
        assert_ne!(t1, t3, "轮次推进要换 Turn-ID");

        let (s4, _) = session_and_turn(&account_with(TOKEN), "conv-2", 1);
        assert_ne!(s1, s4, "不同会话不得复用 Session-ID");
    }

    /// 入站会话标记有效时直接作为会话键；脏值（超长/控制字符）落回散列键
    #[test]
    fn conversation_key_prefers_clean_inbound_marker() {
        let body = json!({ "model": "m", "messages": [] });
        let account = account_with(TOKEN);
        let via_inbound = conversation_key(&account, "m", &body, "client-sess-1");
        let again = conversation_key(&account, "m", &body, "client-sess-1");
        assert_eq!(via_inbound, again);
        assert!(via_inbound.starts_with("inbound:client-sess-1"));

        let other_body = json!({ "model": "m",
            "messages": [{ "role": "user", "content": "别的对话" }] });
        assert_ne!(
            via_inbound,
            conversation_key(&account, "m", &other_body, ""),
            "无标记时按对话起点散列"
        );
    }

    /// 标题：压缩空白、截 20 字符、非 ASCII 百分号编码
    #[test]
    fn title_is_collapsed_truncated_and_latin1_safe() {
        let body = json!({ "messages": [
            { "role": "system", "content": "系统提示" },
            { "role": "user", "content": "  帮我  写一段\n代码  " }
        ] });
        let title = title_header(&body);
        assert_eq!(title, "%E5%B8%AE%E6%88%91 %E5%86%99%E4%B8%80%E6%AE%B5 %E4%BB%A3%E7%A0%81");

        let long = json!({ "messages": [{ "role": "user", "content": "0123456789abcdefghijKLMNOP" }] });
        assert_eq!(title_header(&long), "0123456789abcdefghij", "只取前 20 字符");

        let empty = json!({ "messages": [] });
        assert_eq!(title_header(&empty), "");
    }

    /// 组织账号把 X-Org-Code 带上；驼峰与 auth 子对象两种存法都认；个人不发
    #[test]
    fn org_code_is_read_from_either_shape() {
        let body = json!({ "messages": [] });
        let headers = chat_identity(
            &json!({ "auth": { "accessToken": TOKEN }, "office_identity": "org-42" }),
            &body,
            "m",
            "",
        )
        .expect("组织账号");
        assert_eq!(has(&headers, "X-Org-Code").as_deref(), Some("org-42"));

        let personal = chat_identity(
            &json!({ "auth": { "accessToken": TOKEN }, "officeIdentity": "personal" }),
            &body,
            "m",
            "",
        )
        .expect("个人身份");
        assert!(has(&personal, "X-Org-Code").is_none());
    }

    /// 脏组织身份在拼头之前挡掉：让请求整体失败比发一条非法头更安全
    #[test]
    fn unsafe_org_code_is_rejected() {
        let body = json!({ "messages": [] });
        let err = chat_identity(
            &json!({ "auth": { "accessToken": TOKEN }, "office_identity": "组织\n42" }),
            &body,
            "m",
            "",
        );
        assert!(err.is_err());
    }
}
