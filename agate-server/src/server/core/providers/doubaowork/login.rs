//! 豆包工作的「登录」：**网页登录交回 Cookie** 与 **凭证文件导入**，共用一套归一。
//!
//! ── 本家三种可行的接入方式（对照上游 beta 的 `add-doubaowork-config.ts`）──
//! ```text
//!   网页登录（webLogin）   内嵌窗口打开 www.doubao.com，登录完成后由**壳侧**
//!                         读出完整 Cookie 交回 → 本模块收尾落账号
//!   填写凭证（manual）     粘贴 Cookie 串 / Cookie JSON 数组或对象（见 normalize_import）
//!   导入凭证文件(fileImport) 选一个 .json（含 cookies 的对象 / 数组 / 单账号导出）
//!   ── 桌面端登录态（desktop）**不在本文件范围**：它要读本机豆包客户端的
//!      Cookie（且需客户端带 `--remote-debugging-port=9342` 启动），
//!      Android 上没有这回事，客户端注册表里那条通道本就被过滤掉
//!      （见 `PresetRegistry` 的 IMPORT_DESKTOP 说明）。
//! ```
//!
//! ── 为什么「网页登录」不需要新的 HTTP 端点 ─────────────────────
//! 上游 beta 的做法是：壳侧读到 Cookie 后把 **Cookie 的 JSON 字符串**塞进
//! `POST /api/session/login/callback` 的 `callbackUrl` 字段交回
//! （`desktop-tauri/src-tauri/src/login.rs` 的 `submit_callback`）。本仓沿用同一
//! 契约：`core::login::submit_login_callback` 里有一条 doubaowork 分支，
//! 把 `callbackUrl` 当**载荷**而不是 URL 交给 [`normalize_import`]。
//! 这样不必新增路由、不必新增端点，两条入口（壳侧交回 / 手工粘贴）走同一段归一。
//!
//! ── 归一必须认三种形态（用户/壳侧送来的东西形态不一致）────────────
//! ```text
//!   1. Cookie HTTP 头串      sessionid=…; sessionid_ss=…; ttwid=…
//!   2. Cookie 行数组          [{"name":"sessionid","value":"…","domain":".doubao.com",…}]
//!      或包一层的对象         {"cookies":[ … ]}      ← 壳侧登录窗口导出的就是这个
//!   3. 扁平键值对象           {"sessionid":"…","ttwid":"…"}
//! ```
//! 形态 2/3 走 `credentials::normalize_exported_cookies`（有域信息，按 doubao 域
//! 过滤，与上游 beta 逐条一致）；形态 1 走 `credentials::normalize_paste`。
//! **两条路最终都汇到同一个 seed 形状**（cookies / fingerprint / sessionTail /
//! accessToken），落账号只走 `add_doubaowork_account` 一处 —— 否则「网页登录」
//! 与「手动添加」会在 id 派生、备注名、优先级上慢慢分叉。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::Value;

use crate::server::errors::GatewayError;
use crate::server::logging;

use super::credentials;

/// 网页登录打开的页面（上游 beta 的壳侧校验：**必须**是 `www.doubao.com`）。
pub const LOGIN_PAGE: &str = "https://www.doubao.com";

/// 输入长度上限（整份 Cookie JSON 可能不小；超过这个长度一定是选错了文件）
const MAX_INPUT_LENGTH: usize = 512 * 1024;

/// 一次性 state（网页登录任务归属凭据）。
///
/// 与 `orcaterm::login::new_login_state` 同款：32 个十六进制字符，
/// `getrandom` 不可用时退回时间戳（宁可弱一点也不要拿不到 state）。
pub fn new_login_state() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::getrandom(&mut bytes).is_err() {
        return format!("{:x}", logging::now_ms());
    }
    let mut out = String::with_capacity(32);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 网页登录地址（恒为官方站点首页 —— 登录态写在 `www.doubao.com` 的 Cookie 上）。
pub fn login_page_url() -> String {
    LOGIN_PAGE.to_string()
}

/// 把「任意形态的 Cookie 输入」归一成落账号用的 seed。
///
/// 三种形态见模块头。**失败一律给可执行的文案**：这条路径的用户
/// （或壳侧）拿到的错误必须说清「该粘什么 / 该选什么文件」。
pub fn normalize_import(raw: &str) -> Result<Value, GatewayError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(GatewayError::with_status(
            400,
            "Cookie 为空：请登录 www.doubao.com 后复制完整 Cookie（必须含 sessionid）",
        ));
    }
    if trimmed.chars().count() > MAX_INPUT_LENGTH {
        return Err(GatewayError::with_status(
            400,
            "输入过长：请确认粘贴的是 Cookie 或 Cookie JSON，而不是整页内容/别的大文件",
        ));
    }

    // ── 形态 2/3：JSON（数组 / 含 cookies 的对象 / 扁平键值对象）──
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => seed_from_json(&value),
            Err(error) => Err(GatewayError::with_status(
                400,
                format!(
                    "Cookie JSON 解析失败（{error}）：请选上游导出的 Cookie JSON，\
                     或改用「填写凭证」粘贴 `sessionid=…; …` 形态的 Cookie 串"
                ),
            )),
        };
    }

    // ── 形态 1：Cookie HTTP 头串 ──
    let seed = credentials::normalize_paste(trimmed, None, None, None)?;
    ensure_has_session(&seed)?;
    Ok(seed)
}

/// JSON 形态 → seed。
fn seed_from_json(value: &Value) -> Result<Value, GatewayError> {
    // 取出「行数组」：`{"cookies":[…]}` / 直接 `[…]`
    let rows: Vec<Value> = match value {
        Value::Array(items) => items.clone(),
        Value::Object(object) => match object.get("cookies") {
            Some(Value::Array(items)) => items.clone(),
            // 扁平键值对象：{"sessionid":"…","ttwid":"…"} → 合成行（无域信息，
            // 因此不做域过滤 —— 与粘贴形态同一口径，见 `cookie_pairs_from_rows`
            // 的说明）
            _ => object
                .iter()
                .filter_map(|(name, raw)| {
                    raw.as_str()
                        .map(|text| serde_json::json!({ "name": name, "value": text }))
                })
                .collect(),
        },
        _ => Vec::new(),
    };
    if rows.is_empty() {
        return Err(GatewayError::with_status(
            400,
            "这份 JSON 里没有可识别的 Cookie：需要 `cookies` 数组，或 {\"sessionid\":\"…\"} 形态的对象",
        ));
    }

    // 有 `domain` 的行才做域过滤（登录窗口导出的那种）；没有域的走扁平路径。
    let has_domain = rows
        .iter()
        .any(|row| row.get("domain").and_then(Value::as_str).is_some_and(|text| !text.trim().is_empty()));
    let now = logging::now_ms() / 1000;
    let pairs: Vec<(String, String)> = if has_domain {
        credentials::normalize_exported_cookies(&rows, now)
            .iter()
            .map(|row| {
                let cell = row.to_value();
                (
                    cell.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                    cell.get("value").and_then(Value::as_str).unwrap_or("").to_string(),
                )
            })
            .filter(|(name, value)| !name.is_empty() && !value.is_empty())
            .collect()
    } else {
        credentials::cookie_pairs_from_rows(&rows)
    };
    if pairs.is_empty() {
        return Err(GatewayError::with_status(
            400,
            "这份 Cookie 里没有属于 doubao.com 且未过期的条目：请确认登录的是 www.doubao.com \
             （其它站点的 Cookie 不能用来转发）",
        ));
    }

    // 拼回 Cookie 头再走 `normalize_paste`：**只有一处**构造 seed 的逻辑
    // （顺带保留它算出来的 fingerprint / sessionTail / accessToken）。
    let header = pairs
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<String>>()
        .join("; ");
    let seed = credentials::normalize_paste(&header, None, None, None)?;
    ensure_has_session(&seed)?;
    Ok(seed)
}

/// 把调用方额外提供的风控参数补进 seed 的 `fingerprint`。
///
/// 「填写凭证 / 文件导入」两条路拿不到风控参数（它们在登录窗口里才有），
/// 因此值是**可选**的：读不到就不写，交给上游按 Cookie 自行派生 —— 编一个
/// 随机的 deviceId 反而更糟（同一个账号每次换设备号 = 每次换身份，与
/// `chatglm` 那边踩过的坑同源）。
pub fn attach_fingerprint(
    mut seed: Value,
    device_id: Option<&str>,
    web_id: Option<&str>,
    fp: Option<&str>,
) -> Value {
    let entries: [(&str, Option<&str>); 3] =
        [("deviceId", device_id), ("webId", web_id), ("fp", fp)];
    let extras: Vec<(String, String)> = entries
        .iter()
        .filter_map(|(key, value)| {
            value
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(|text| ((*key).to_string(), text.to_string()))
        })
        .collect();
    if extras.is_empty() {
        return seed;
    }
    let Some(object) = seed.as_object_mut() else {
        return seed;
    };
    let fingerprint = object
        .entry("fingerprint".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Some(map) = fingerprint.as_object_mut() {
        for (key, value) in extras {
            map.insert(key, Value::String(value));
        }
    }
    seed
}

/// 登录态必须存在（`sessionid` / `sessionid_ss` 之一）。
///
/// `normalize_paste` 已经校验过一次；这里再判一次是**刻意的重复**：
/// 本函数是「网页登录 / 文件导入」两条路的唯一收尾口，错在这里的后果是
/// **加进一个永远 401 的账号**（用户会以为加成功了，直到发消息才失败）。
fn ensure_has_session(seed: &Value) -> Result<(), GatewayError> {
    let has_session = seed
        .get("cookies")
        .and_then(Value::as_array)
        .is_some_and(|rows| {
            rows.iter().any(|row| {
                let name = row.get("name").and_then(Value::as_str).unwrap_or("");
                let value = row.get("value").and_then(Value::as_str).unwrap_or("");
                credentials::SESSION_COOKIE_NAMES.contains(&name) && !value.trim().is_empty()
            })
        });
    if has_session {
        Ok(())
    } else {
        Err(GatewayError::with_status(
            400,
            format!(
                "Cookie 里缺少登录态（需要 {} 之一）：请先登录 www.doubao.com 再复制",
                credentials::SESSION_COOKIE_NAMES.join(" / ")
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 32 位十六进制、两次不同（一次性 state）
    #[test]
    fn login_state_is_hex_and_unique() {
        let first = new_login_state();
        let second = new_login_state();
        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    /// 登录页必须是官方站点（上游 beta 的壳侧会校验这一点）
    #[test]
    fn login_page_is_official() {
        assert_eq!(login_page_url(), "https://www.doubao.com");
    }

    /// 形态 1：Cookie 头串
    ///
    /// 断言落在 `cookies` 而不是 `accessToken`：seed 只装 Cookie 行，
    /// **拼好的 Cookie 头由账号层在落盘时生成**（见 `doubaowork_accounts.rs`
    /// 模块头里「accessToken 为什么必须冗余存一份」那段），这里是刻意的分工。
    #[test]
    fn accepts_cookie_header() {
        let seed = normalize_import("sessionid=abc123; ttwid=xyz").expect("应能归一");
        let rows = seed
            .get("cookies")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            rows.iter()
                .any(|row| row.get("name").and_then(Value::as_str) == Some("sessionid")),
            "sessionid 应被收进 cookies"
        );
        assert!(rows
            .iter()
            .any(|row| row.get("value").and_then(Value::as_str) == Some("abc123")));
    }

    /// 形态 2：`{"cookies":[…]}`（壳侧登录窗口导出的就是这种）
    #[test]
    fn accepts_cookies_envelope() {
        let raw = r#"{"cookies":[
            {"name":"sessionid","value":"abc123","domain":".doubao.com","path":"/"},
            {"name":"ttwid","value":"xyz","domain":".doubao.com","path":"/"}
        ]}"#;
        let seed = normalize_import(raw).expect("应能归一");
        let rows = seed.get("cookies").and_then(Value::as_array).cloned().unwrap_or_default();
        assert!(rows.iter().any(|row| row.get("name").and_then(Value::as_str) == Some("sessionid")));
    }

    /// 形态 2b：裸数组
    #[test]
    fn accepts_bare_array() {
        let raw = r#"[{"name":"sessionid","value":"abc","domain":"www.doubao.com","path":"/"}]"#;
        assert!(normalize_import(raw).is_ok());
    }

    /// 形态 3：扁平键值对象
    #[test]
    fn accepts_flat_object() {
        let raw = r#"{"sessionid":"abc123","ttwid":"xyz"}"#;
        let seed = normalize_import(raw).expect("应能归一");
        assert!(seed
            .get("cookies")
            .and_then(Value::as_array)
            .is_some_and(|rows| !rows.is_empty()));
    }

    /// 缺登录态（只有无关 Cookie）→ 明确报错，不许加进一个永远 401 的账号
    #[test]
    fn rejects_without_session_cookie() {
        let raw = r#"{"cookies":[{"name":"ttwid","value":"x","domain":".doubao.com","path":"/"}]}"#;
        let error = normalize_import(raw).expect_err("缺 sessionid 应报错");
        assert_eq!(error.status_code, 400);
        assert!(error.message.contains("sessionid"));
    }

    /// 别家域的 Cookie 不能当凭证（域过滤生效）
    #[test]
    fn rejects_foreign_domain() {
        let raw = r#"{"cookies":[{"name":"sessionid","value":"x","domain":".example.com","path":"/"}]}"#;
        let error = normalize_import(raw).expect_err("外域应被拒");
        assert_eq!(error.status_code, 400);
    }

    /// 过期 Cookie 被丢掉（上游 beta 同口径）
    #[test]
    fn drops_expired_rows() {
        let raw = r#"{"cookies":[{"name":"sessionid","value":"x","domain":".doubao.com","path":"/","expires":1}]}"#;
        assert!(normalize_import(raw).is_err(), "已过期应判无登录态");
    }

    /// 空输入 / 超长输入 / 坏 JSON 都给人话
    #[test]
    fn error_messages_are_actionable() {
        assert!(normalize_import("   ").expect_err("空").message.contains("Cookie"));
        let broken = normalize_import("{not json").expect_err("坏 JSON");
        assert!(broken.message.contains("JSON"));
        let long = "x".repeat(MAX_INPUT_LENGTH + 1);
        assert!(normalize_import(&long).expect_err("超长").message.contains("过长"));
    }

    /// 没有 cookies 字段又没有扁平值的 JSON 对象
    #[test]
    fn rejects_json_without_cookies() {
        let error = normalize_import(r#"{"foo":123}"#).expect_err("无 Cookie");
        assert!(error.message.contains("cookies"));
    }
}