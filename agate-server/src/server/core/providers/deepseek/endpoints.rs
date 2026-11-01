//! DeepSeek 网页版（`chat.deepseek.com`）的基址、路径与请求头。
//!
//! ── 上游长什么样（来源：web 插件包二进制 + 公开逆向的交叉印证）────
//! 本家是**网页版对话站**（不是 API Key 那条 `api.deepseek.com`）：
//! ```text
//!   POST /api/v0/chat/create_pow_challenge   领 PoW 题（每次 completion 前）
//!   POST /api/v0/chat_session/create         建会话（拿 chat_session_id）
//!   POST /api/v0/chat/completion             对话（SSE 流式；带 x-ds-pow-response）
//!   POST /api/v0/file/upload_file            图片上传（二进制里未见此路径，
//!                                            按「不做」处理，见下）
//! ```
//! 单一域名、**没有国内 / 国际两套**（与 Command Code 同一处境）。
//!
//! ── 认证：`Authorization: Bearer <userToken>`，不是 Cookie ────────
//! 网页版认的是浏览器 `localStorage` 里 `userToken` 的 `.value` 那一串，
//! 不是 Cookie —— 这条是本家最容易走偏的地方（把 Cookie 整串粘进来会稳定
//! 401）。因此粘贴通道的提示语必须点明「userToken」而不是「Cookie」。
//!
//! ── 为什么本家没有「伪装客户端」那一整套头 ─────────────────────
//! 与 Command Code / OrcaTerm 那种「伪装官方 CLI」不同，本家要伪装的是
//! **网页版自己**：头集合以浏览器常规头为主（`Origin` / `Referer` /
//! 常见 UA），而风控的真正屏障是 PoW（见 `pow.rs`）而不是头指纹。
//! 因此本文件只给最必要的几项，不堆砌 UA 常量 —— 堆了也无据可依。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

/// 默认基址（网页版站点本体）
pub const DEFAULT_BASE_URL: &str = "https://chat.deepseek.com";

/// 领 PoW 题的路径
pub const POW_CHALLENGE_PATH: &str = "/api/v0/chat/create_pow_challenge";
/// 建会话的路径
pub const CREATE_SESSION_PATH: &str = "/api/v0/chat_session/create";
/// 对话的路径（PoW 题面的 `target_path` 就是这个值）
pub const COMPLETION_PATH: &str = "/api/v0/chat/completion";

/// 两个网页模式（题面与二进制里都只有这两支）
pub const MODEL_FLASH: &str = "deepseek-web-flash";
pub const MODEL_PRO: &str = "deepseek-web-pro";

/// 默认模型（网页版默认档；`pro` 是「深度思考」档）
pub const DEFAULT_MODEL: &str = MODEL_FLASH;

/// 读环境变量覆盖（空值视为未设置，去掉尾部斜杠）
fn env_override(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

/// 基址：`DEEPSEEK_WEB_BASE` 可覆盖（便于指向测试站 / 反向代理）
pub fn base_url() -> String {
    env_override("DEEPSEEK_WEB_BASE").unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
}

/// 全 URL（含路径）
pub fn url(path: &str) -> String {
    format!("{}{path}", base_url())
}

/// 浏览器常规 UA（网页版自身用的那一支）
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
                          AppleWebKit/537.36 (KHTML, like Gecko) \
                          Chrome/131.0.0.0 Safari/537.36";

/// 一家共用的基础头（`Authorization` 之外的那些）。
///
/// 逐字给出而不是省略：`send_raw` 默认会带 `Accept: application/json`，
/// 而本家的对话是 SSE —— 因此 `Accept` 必须在调用点覆盖成 `text/event-stream`
/// （见 `chat.rs`），这条注释是提醒别在两处写成互相打架的值。
pub fn base_headers() -> Vec<(String, String)> {
    vec![
        ("User-Agent".to_string(), USER_AGENT.to_string()),
        ("Origin".to_string(), base_url()),
        ("Referer".to_string(), format!("{}/", base_url())),
        ("x-app-version".to_string(), "20241129.1".to_string()),
    ]
}

/// 鉴权头（`Authorization: Bearer <userToken>`）
pub fn auth_headers(user_token: &str) -> Vec<(String, String)> {
    vec![(
        "Authorization".to_string(),
        format!("Bearer {}", user_token.trim()),
    )]
}

/// 对话所需的 SSE 接受头（`Accept` 与 `Content-Type` 都按流式给）
pub fn sse_headers() -> Vec<(String, String)> {
    vec![
        ("Accept".to_string(), "text/event-stream".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_join_without_double_slash() {
        assert_eq!(
            url(COMPLETION_PATH),
            "https://chat.deepseek.com/api/v0/chat/completion"
        );
        assert!(url(POW_CHALLENGE_PATH).ends_with("/create_pow_challenge"));
        assert!(!url("").contains("//api"));
    }

    #[test]
    fn auth_header_is_bearer() {
        let headers = auth_headers("  tok123  ");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].1, "Bearer tok123");
    }

    #[test]
    fn default_model_is_one_of_the_two_web_modes() {
        assert!(DEFAULT_MODEL == MODEL_FLASH || DEFAULT_MODEL == MODEL_PRO);
    }
}