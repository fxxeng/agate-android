//! Tabbit 的接口路径与浏览器常规头（一个站点一份常量，集中在 `Realm` 之外）。
//!
//! ── 为什么路径也放这里 ──────────────────────────────────────
//! 上游把「账号域 / 对话域 / 商务域」分在同一个主机的三组前缀下：
//! ```text
//!   /panel/session                       房间（一次对话一个）
//!   /api/v3/chat/rooms/{id}/join         订阅事件流（**带签名**）
//!   /api/v3/chat/rooms/{id}/runs         投递消息（**不带签名**）
//!   /api/v0/user/base-info               账号资料
//!   /proxy/v1/model_config/models        模型目录
//!   /api/commerce/quota/v1/usage         额度（百分比制）
//!   /api/commerce/activity/v1/sign-in    每日签到
//!   /chat/sign-key                       签名 key（会轮换）
//! ```
//! 路径写字面量散在业务代码里，改一处就得全仓搜 —— 与 qoder / zcode 的
//! `endpoints` 同一处置：**路径是本站点的知识，只在这里出现一次**。
//!
//! ── 头里的那两件「看起来多余」的东西 ────────────────────────
//!   - `x-req-ctx`：base64 的客户端版本号。上游按 `{version}({build})` 解析，
//!     格式不对判 493「版本过低」—— 所以它必须与 UA 的版本号同源；
//!   - `unique-uuid`：第 5 位是「默认浏览器」标记（解锁 Premium 模型），
//!     另 8 位编码时间（防重放）。见 `sign::unique_uuid`。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use super::sign;
use super::{Realm, DEFAULT_CLIENT_VERSION, SEC_CH_UA, USER_AGENT};

/// 房间（一次对话一个，30 分钟复用）
pub const SESSION_PATH: &str = "/panel/session";
/// 订阅房间事件流（唯一带签名的对话请求）
pub fn join_path(room: &str) -> String {
    format!("/api/v3/chat/rooms/{}/join", path_escape(room))
}
/// 投递一次对话（不带签名）
pub fn run_path(room: &str) -> String {
    format!("/api/v3/chat/rooms/{}/runs", path_escape(room))
}
/// 账号资料
pub const USER_INFO_PATH: &str = "/api/v0/user/base-info";
/// 模型目录（`scene=chat` 是网页对话那一档）
pub const MODELS_PATH: &str = "/proxy/v1/model_config/models?a=0&scene=chat";
/// 签名 key（纯文本响应）
pub const SIGN_KEY_PATH: &str = "/chat/sign-key";
/// 额度查询（百分比制，`user_id` 必填）
pub fn usage_path(uid: &str, timezone: &str) -> String {
    format!(
        "/api/commerce/quota/v1/usage?user_id={}&timezone={}",
        path_escape(uid),
        path_escape(timezone)
    )
}
/// 签到状态（两个场景一起查，只关心 `daily_sign_in`）
pub const CHECKIN_STATUS_PATH: &str =
    "/api/commerce/activity/v1/sign-in/status?scene_codes=daily_sign_in&scene_codes=desktop_pet";
/// 签到
pub const CHECKIN_PATH: &str = "/api/commerce/activity/v1/sign-in";

/// 浏览器常规头（每个请求都带；签名三件套由 `http` 追加）。
///
/// `referer_path` 是上游要的「页面来源」：控制面请求填 `/`，对话请求填
/// `/panel/{room}`（照着浏览器当时在哪个页发请求来填，不做统一值）。
///
/// `unlock_pro` 决定 `unique-uuid` 第 5 位（是否「已设为默认浏览器」）——
/// 它**不是**装饰位：命中才解锁 Premium 模型。所以这个开关来自账号
/// （`unlockPro`），不能在这里写死 true。
pub fn base_headers(
    realm: Realm,
    uid: &str,
    client_version: &str,
    unlock_pro: bool,
    referer_path: &str,
) -> Vec<(String, String)> {
    let base = realm.base();
    let version = if client_version.trim().is_empty() {
        DEFAULT_CLIENT_VERSION
    } else {
        client_version.trim()
    };
    vec![
        ("User-Agent".to_string(), USER_AGENT.to_string()),
        ("sec-ch-ua".to_string(), SEC_CH_UA.to_string()),
        ("sec-ch-ua-platform".to_string(), "\"Windows\"".to_string()),
        ("sec-ch-ua-mobile".to_string(), "?0".to_string()),
        (
            "x-chrome-id-consistency-request".to_string(),
            sign::chrome_consistency(uid),
        ),
        ("referer".to_string(), format!("{base}{referer_path}")),
        ("Origin".to_string(), base.to_string()),
        ("x-req-ctx".to_string(), sign::base64_text(version)),
        (
            "unique-uuid".to_string(),
            sign::unique_uuid(unlock_pro, crate::server::logging::now_ms()),
        ),
    ]
}

/// 路径段转义（房间 id 是上游给的串，仍按 RFC 3986 的 unreserved 集处理）
fn path_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 房间 id 里的特殊字符必须转义（不转义会把路径拼成别的东西）
    #[test]
    fn room_ids_are_escaped_in_paths() {
        assert_eq!(join_path("abc"), "/api/v3/chat/rooms/abc/join");
        assert_eq!(join_path("a b"), "/api/v3/chat/rooms/a%20b/join");
        assert_eq!(join_path("a/b"), "/api/v3/chat/rooms/a%2Fb/join");
        assert_eq!(run_path("a?b"), "/api/v3/chat/rooms/a%3Fb/runs");
    }

    /// 额度查询的两个参数都要转义（时区名里有 `/`，不转义会被当成路径分隔符）
    #[test]
    fn usage_path_encodes_the_timezone_slash() {
        let path = usage_path("u-1", Realm::Cn.timezone());
        assert!(path.starts_with("/api/commerce/quota/v1/usage?user_id=u-1&timezone="));
        assert!(path.contains("Asia%2FShanghai"), "{path}");
    }

    /// 基础头必须带齐上游判客户端形态的那几件；版本号为空时回落到默认值
    #[test]
    fn base_headers_carry_the_fingerprint() {
        let headers = base_headers(Realm::Cn, "u1", "", true, "/");
        let map: std::collections::BTreeMap<_, _> = headers.into_iter().collect();
        assert_eq!(
            map.get("x-req-ctx").map(String::as_str),
            Some(sign::base64_text(DEFAULT_CLIENT_VERSION).as_str())
        );
        assert_eq!(map.get("Origin").map(String::as_str), Some("https://web.tabbit.com"));
        assert_eq!(map.get("referer").map(String::as_str), Some("https://web.tabbit.com/"));
        assert!(map.contains_key("unique-uuid"));
        assert!(map.contains_key("x-chrome-id-consistency-request"));
        assert_eq!(map.get("sec-ch-ua-mobile").map(String::as_str), Some("?0"));
    }

    /// 两个站点的 Origin / referer 必须跟着站点走（混了就是跨站请求）
    #[test]
    fn origin_follows_the_realm() {
        for realm in Realm::ALL {
            let headers = base_headers(realm, "u1", "", true, "/newtab");
            let map: std::collections::BTreeMap<_, _> = headers.into_iter().collect();
            assert_eq!(map.get("Origin").map(String::as_str), Some(realm.base()));
            assert_eq!(
                map.get("referer").map(String::as_str),
                Some(format!("{}/newtab", realm.base()).as_str())
            );
        }
    }
}
