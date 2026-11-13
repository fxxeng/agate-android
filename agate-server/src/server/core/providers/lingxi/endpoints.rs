//! WPS 灵犀上游端点与出站头族。
//!
//! ⚠️ **本文件的字段来自三个公开参考实现（见 `docs/WPS灵犀-移植方案.md` §1），
//! 尚未真机实测**。方案里标出的 4 处来源冲突（host / 额度字段 / 目录来源 /
//! 通道）在 P0 探针端点验证前不要当既成事实改；每处冲突都在下面就近标注。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic。

/// provider id（与注册表中的字符串契约一致；本批尚未注册 `ProviderKind`，
/// 先在这里定死，接线时直接引用，避免又抄一份字面量）。
pub const ID: &str = "lingxi";

/// 登录页 host（用户在这里拿 `wps_sid`）。
pub const LOGIN_HOST: &str = "https://lingxi.wps.cn";
/// API host。`dsh-router-wpslingxi` 实测「同一 cookie 两边都认」，与
/// `lingxi2api-panel` 用 `lingxi.wps.cn` 的说法冲突 → 待 P0 探针实测。
pub const API_HOST: &str = "https://lingxi.kdocs.cn";

/// 桌面客户端身份（三个参考实现一致给出 1.3.36）。
pub const APP_VERSION: &str = "1.3.36";
pub const APP_NAME: &str = "win-lingxi-desktop";
pub const CLIENT_TYPE: &str = "lingxi_claw";
pub const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) ",
    "Lingxi Claw/1.3.36 Chrome/144.0.7559.236 Electron/40.8.2 Safari/537.36",
);

/// 提示词超过约 320KB 上游直接 400「AI服务异常」，不提示「太长」。
/// 本地预检按序列化字节数算，留 4KB 余量给头族与包装字段。
pub const MAX_PROMPT_BYTES: usize = 316 * 1024;

/// 会话 + run 按「账号 × 模型」缓存 1 小时，**不滑动续期**。
pub const SESSION_TTL_MS: i64 = 3_600_000;

/// 任务中心签到任务名。
pub const TASK_DAILY_CHECK_IN: &str = "daily_check_in";

fn url(path: &str) -> String {
    format!("{API_HOST}{path}")
}

/// ① 建 cowork 会话。
pub fn sessions_url() -> String {
    url("/api/aioffice/v1/cowork/sessions")
}

/// ② 领 credits run（计费单元）。
pub fn runs_url() -> String {
    url("/api/public/v1/credits/runs")
}

/// ③ 私有 SSE 对话。
pub fn chat_url() -> String {
    url("/api/public/v1/agent-runtime/chat/completions")
}

/// run 终态上报：**结算触发点**。参考实现只给了 `PUT execution_status` 的
/// 动作与 body，没给完整路径 —— 这一条是推断，P0 必须实测校正。
pub fn execution_status_url(run_id: &str) -> String {
    url(&format!("/api/public/v1/credits/runs/{run_id}/execution_status"))
}

/// 额度（智点）。dsh 插件实测带 `?credit_type=total`。
pub fn balance_url() -> String {
    url("/api/public/v1/credits/balance?credit_type=total")
}

/// 模型目录：与 `parse_catalog` 的两种形状对应（`key/multiplier` 来自网关项目，
/// `data.models` 来自 dsh 插件）。
pub fn plans_url() -> String {
    url("/api/aioffice/v1/sessions/plans")
}

/// 任务中心状态 / 领取。路径未在任何参考实现里给全，标注为待实测；
/// P0 只做状态机（见 `checkin.rs`），不依赖这条常量发请求。
pub fn task_url(task: &str) -> String {
    url(&format!("/api/aioffice/v1/tasks/{task}"))
}

/// 出站头族。`wps_sid` 是 HttpOnly Cookie，只能整串放进 Cookie 头；
/// `device_id` 由 `credentials::device_id(uid)` 按账号稳定派生。
pub fn headers(wps_sid: &str, device_id: &str, cookie: Option<&str>) -> Vec<(String, String)> {
    let cookie_value = match cookie {
        Some(full) if !full.trim().is_empty() => full.trim().to_string(),
        _ => format!("wps_sid={wps_sid}"),
    };
    vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "text/event-stream".to_string()),
        ("Cookie".to_string(), cookie_value),
        ("User-Agent".to_string(), USER_AGENT.to_string()),
        ("X-Client-Type".to_string(), CLIENT_TYPE.to_string()),
        ("x-kso-app-name".to_string(), APP_NAME.to_string()),
        ("x-kso-app-version".to_string(), APP_VERSION.to_string()),
        ("x-kso-device-id".to_string(), device_id.to_string()),
        ("Origin".to_string(), API_HOST.to_string()),
        ("Referer".to_string(), format!("{API_HOST}/")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_step_urls_are_on_the_same_host() {
        for u in [sessions_url(), runs_url(), chat_url()] {
            assert!(u.starts_with(API_HOST), "{u}");
        }
    }

    #[test]
    fn cookie_header_prefers_the_full_cookie_when_present() {
        let h = headers("V02S-abc", "dev-1", Some("uid=1; wps_sid=V02S-abc; wpsua=x"));
        let cookie = h.iter().find(|(k, _)| k == "Cookie").map(|(_, v)| v.as_str());
        assert_eq!(cookie, Some("uid=1; wps_sid=V02S-abc; wpsua=x"));
    }

    #[test]
    fn cookie_header_builds_a_minimal_cookie_from_bare_sid() {
        let h = headers("V02S-abc", "dev-1", None);
        let cookie = h.iter().find(|(k, _)| k == "Cookie").map(|(_, v)| v.as_str());
        assert_eq!(cookie, Some("wps_sid=V02S-abc"));
    }

    #[test]
    fn device_headers_are_always_present() {
        let h = headers("V02S-abc", "dev-1", None);
        for key in ["x-kso-app-name", "x-kso-app-version", "x-kso-device-id", "X-Client-Type"] {
            assert!(h.iter().any(|(k, _)| k == key), "缺头 {key}");
        }
    }
}
