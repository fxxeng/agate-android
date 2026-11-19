//! LobsterAI（网易有道「有道龙虾」）的域名、路径与客户端版本。
//!
//! ── 上游长什么样（一手来源：插件仓库 `crates/providers/src/providers/lobsterai/endpoints.rs`）──
//! ```text
//!   登录门户   https://lobsterai.youdao.com        浏览器授权页（手机号 / 微信）
//!   API        https://lobsterai-server.youdao.com 换码 / 续期 / 目录 / 对话 / 积分 / 活动
//! ```
//! 对话端点是 **OpenAI 兼容原生协议**，但**只收 SSE**（`stream:false` 回 HTTP 500）
//! —— 本网关的转发入口恒以 `stream:true` 请求上游，所以这条要求天然满足。
//!
//! ── 两个 `X-LobsterAI-Client-*` 头是功能性的，不是装饰 ──────────
//! 服务端**按 `ClientCapabilities` 过滤**目录：
//! ```text
//!   不带该头               -> 目录里 25 个模型（没有 kimi-k3）
//!   带 kimi-k3-agentic-v1 -> 26 个（kimi-k3 才出现）
//! ```
//! 另一个能力位 `thinking-level-control-v1` 是 `reasoning_effort: "off"` 的前提
//! —— 缺它时发那个值会得到 HTTP 500。两个都实测过。
//!
//! ── 客户端版本要动态解析，但**故意不跟真实版本** ────────────────
//! `X-LobsterAI-Client-Version` 要的是**日期式版本**（形如 `2026.9.4`），由上游
//! 更新接口给出；而 `User-Agent` 照抄参考实现里那个固定的 `LobsterAI/0.1.0` ——
//! 它**故意不跟**：动态的那份只由版本头承载。
//!
//! ── 没有任何签名 ────────────────────────────────────────────
//! 纯 `Bearer`。没有 HMAC / DPoP / PKCE，也没有腾讯系的归属头。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

/// API 基址（换码 / 续期 / 目录 / 对话 / 积分 / 活动）
pub const API_BASE: &str = "https://lobsterai-server.youdao.com";

/// 登录门户基址（浏览器授权页）
pub const PORTAL_BASE: &str = "https://lobsterai.youdao.com";

/// 客户端版本查询接口。**第三方主机，响应不是业务信封**：版本号在
/// `data.value.version`（对象的两层，不是 `data.value` 标量）。
pub const CLIENT_VERSION_API: &str =
    "https://api-overmind.youdao.com/openapi/get/luna/hardware/lobsterai/prod/update";

/// 兜底客户端版本（版本接口不可达 / 关掉发现时用；空版本头会被按「不认识的客户端」对待）
pub const FALLBACK_CLIENT_VERSION: &str = "2026.9.4";

/// `User-Agent`：照抄参考实现，**故意固定**不跟随真实版本
pub const USER_AGENT: &str = "LobsterAI/0.1.0";

/// 声明的客户端能力。两个能力位都是功能性的：
/// `kimi-k3-agentic-v1` 是模型列表的准入门（缺它少一个模型）、
/// `thinking-level-control-v1` 是 `reasoning_effort: "off"` 的前提（缺它 HTTP 500）。
pub const CLIENT_CAPABILITIES: &str = "kimi-k3-agentic-v1,thinking-level-control-v1";

/// 换码（一次性 code → 令牌对；**不带 Authorization**）
pub const EXCHANGE_PATH: &str = "/api/auth/exchange";
/// 续期（**不带 Authorization**）
pub const REFRESH_PATH: &str = "/api/auth/refresh";
/// 模型目录
pub const MODELS_PATH: &str = "/api/models/available";
/// 对话（OpenAI 兼容，SSE only）
pub const CHAT_PATH: &str = "/api/proxy/v1/chat/completions";
/// **回调路径**（门户 302 回本机时用的那条，本仓路由也按它挂）
pub const CALLBACK_PATH: &str = "/auth/callback";
/// 积分读数（**不是** `/api/user/quota`，那个只报 300 免费额度）
pub const PROFILE_SUMMARY_PATH: &str = "/api/user/profile-summary";
/// 活动位（签到第一步）
pub const ACTIVITY_SLOT_PATH: &str = "/api/client-activities/slot";
/// 活动上下文（签到第二步 + 签到动作，活动码拼在路径后面）
pub const ACTIVITY_CONTEXT_PATH: &str = "/api/client-activities";

/// 活动位查询的三个固定参数（照抄真实客户端；缺一个上游就可能不给签到入口）。
/// `platform=win32` 是**按桌面客户端的形态标识**，与运行本网关的宿主系统无关。
pub const SLOT_PLACEMENT: &str = "desktop_sidebar";
pub const SLOT_CONTAINER_API_VERSION: &str = "2";
pub const SLOT_PLATFORM: &str = "win32";

/// 控制面请求超时（毫秒）
pub const REQUEST_TIMEOUT_MS: u64 = 30_000;
/// 目录请求超时（毫秒）
pub const CATALOG_TIMEOUT_MS: u64 = 20_000;
/// 已解析的客户端版本缓存时长（毫秒）——12 小时，版本不常变
pub const VERSION_CACHE_TTL_MS: i64 = 12 * 60 * 60 * 1000;

/// 读环境变量覆盖（空值视为未设置），去掉尾部斜杠
fn env_override(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

/// API 基址：`LOBSTERAI_API_BASE` 可覆盖
pub fn api_base() -> String {
    env_override("LOBSTERAI_API_BASE").unwrap_or_else(|| API_BASE.to_string())
}

/// 门户基址：`LOBSTERAI_PORTAL_BASE` 可覆盖
pub fn portal_base() -> String {
    env_override("LOBSTERAI_PORTAL_BASE").unwrap_or_else(|| PORTAL_BASE.to_string())
}

/// 拼 API URL
pub fn api_url(path: &str) -> String {
    format!("{}{}", api_base(), path)
}

/// 对话地址
pub fn chat_url() -> String {
    api_url(CHAT_PATH)
}

/// 目录地址（`keyfrom` 走 query —— 上游用它认客户端来源）。
/// query 为空时退回不带 `?` 的形态（与参考实现 `buildModelsURL` 同口径）。
pub fn models_url(keyfrom_query: &str) -> String {
    let trimmed = keyfrom_query.trim();
    if trimmed.is_empty() {
        api_url(MODELS_PATH)
    } else {
        format!("{}{MODELS_PATH}?{trimmed}", api_base())
    }
}

/// 积分地址
pub fn profile_summary_url() -> String {
    api_url(PROFILE_SUMMARY_PATH)
}

/// 活动位地址（三个固定参数；版本的参数名是 `clientVersion`，不是 `version`）
pub fn activity_slot_url(client_version: &str) -> String {
    format!(
        "{}{ACTIVITY_SLOT_PATH}?placement={SLOT_PLACEMENT}&clientVersion={}&containerApiVersion={SLOT_CONTAINER_API_VERSION}&platform={SLOT_PLATFORM}",
        api_base(),
        urlencode(client_version)
    )
}

/// 某个活动的上下文 URL。
///
/// ⚠️ 末尾那个 `/context` 段不能省：上游的路径是三段式
/// `{code}/context?configRevision=…`，少了它整条请求落到 `/api/client-activities/{code}`
/// 上、直接被回 **HTTP 404**（签到因此稳定失败）。
pub fn activity_context_url(code: &str, config_revision: &str) -> String {
    format!(
        "{}{ACTIVITY_CONTEXT_PATH}/{}/context?configRevision={}",
        api_base(),
        urlencode(code),
        urlencode(config_revision)
    )
}

/// 某个活动的 check_in URL（三段式：`{code}/actions/check_in`）
pub fn activity_checkin_url(code: &str) -> String {
    format!(
        "{}{ACTIVITY_CONTEXT_PATH}/{}/actions/check_in",
        api_base(),
        urlencode(code)
    )
}

/// 最小 urlencode（只保证 `%XX` + 空格转 `%20`；用于路径段与查询值）。
///
/// 不引依赖：这些值都可能来自上游响应（活动码、configRevision），一旦含
/// `&` / `=` / `?` 就会截断 query，所以必须转义而不是直接拼。
pub fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 查询串里的中文 / 特殊字符必须转义（否则上游按畸形参数拒绝）
    #[test]
    fn urlencode_escapes_non_ascii() {
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("中文"), "%E4%B8%AD%E6%96%87");
    }

    /// 三个活动 URL 的形状（参数名 / 三段式路径都是上游硬要求）
    #[test]
    fn activity_urls_match_upstream_shape() {
        let slot = activity_slot_url("2026.9.4");
        assert!(slot.contains("placement=desktop_sidebar"));
        // ⚠️ 参数名是 clientVersion，曾经写成 version
        assert!(slot.contains("clientVersion=2026.9.4"));
        assert!(slot.contains("containerApiVersion=2"));
        assert!(slot.contains("platform=win32"));
        let context = activity_context_url("daily", "7");
        assert!(context.ends_with("/api/client-activities/daily/context?configRevision=7"));
        // ⚠️ 签到是多一段 actions/check_in，曾经写成 /action
        let checkin = activity_checkin_url("daily");
        assert!(checkin.ends_with("/api/client-activities/daily/actions/check_in"));
    }

    /// query 为空时目录地址退回不带 `?` 的形态
    #[test]
    fn models_url_omits_empty_query() {
        assert!(models_url("").ends_with("/api/models/available"));
        assert!(models_url("  ").ends_with("/api/models/available"));
        assert!(models_url("a=1&b=2").ends_with("/api/models/available?a=1&b=2"));
    }
}