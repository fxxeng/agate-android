//! WPS 灵犀上游端点与出站头族。
//!
//! ⚠️ **来源变了（2026-11-13 第二轮）**：此前只按三份 README 的语义字段猜
//! 路径，后来逐项复核两个互相独立的公开实现**源码**，得到下面这张常量表：
//!   · `ketion/lingxi2api-panel` → `internal/upstream/lingxi.go` 的 `LX*` 常量
//!   · `RobbsLuo/Coding2API`      → `src/provider/lingxi/client.py` 的 `EP_*` 常量
//! 两者逐字一致（`LXSessions/LXRuns/LXChat/LXPlans/LXBalance/LXTasks/LXClaim`）。
//! 三请求 body、`execution_status` 路径与字段、任务状态字段名也由这两处给全。
//! 之前那批占位路径已全部替换，见 `docs/WPS灵犀-移植方案.md` 的校正记录。
//!
//! **仍未真机实测 2xx**：路径不再是猜的，能不能领到奖励仍要真机确认。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic。

/// provider id（与注册表字符串契约一致）。
pub const ID: &str = "lingxi";

/// 登录页 host（用户在这里拿 `wps_sid`）：**WPS 通行证的 SSR 登录页**。
///
/// ⚠️ 2026-11-14 实测推翻「登录页就是 lingxi.wps.cn」这个假设：
///   · `lingxi.wps.cn` 302 到 `lingxi.kdocs.cn`（多一跳跨域）；
///   · `lingxi.kdocs.cn` 对**移动 UA** 返回的是 **WPS 客户端容器页**
///     （`<title>WPS AI</title>`、`onclick="window.wps.closeWeb()"`）——
///     它假定自己跑在 WPS App 的 WebView 里，普通 Android WebView 没有
///     `window.wps` 这个 JSBridge，页面就停在 `#app-loading` 一片空白。
///     真机表现正是「登录页已就绪，但整片空白」。
///   · 桌面 UA 下 kdocs 会出完整 SPA，可首屏要拉 `qn.cache.wpscdn.cn` 上
///     十几个带 SRI 的 ES module，移动 WebView 里又慢又脆（CDN 一抖就白屏）。
///
/// `account.wps.cn` 是**服务端直出的手机号登录表单**（自带移动视口、7 个
/// input、无 SPA、无 CDN 依赖），登录后把 `wps_sid` 下发到 `.wps.cn` 域 ——
/// 这正是转发层要的那条凭证。各家网页登录本来也都加载该家的登录页而不是
/// 应用首页，这里没有特例。
pub const LOGIN_HOST: &str = "https://account.wps.cn";
/// 主域：两个参考实现的 `bases()` / `lxBases` 都把它排第一。
pub const API_HOST: &str = "https://lingxi.wps.cn";
/// 回落域：kdocs 账号的 cookie 只在 `lingxi.kdocs.cn` 集群有效 ——
/// **主域回 401 不代表死号**，必须换域再试一次才判失效。
pub const FALLBACK_HOST: &str = "https://lingxi.kdocs.cn";

/// 桌面客户端身份（两个实现一致给出 1.3.36 / lingxi_claw）。
pub const APP_VERSION: &str = "1.3.36";
pub const APP_NAME: &str = "win-lingxi-desktop";
pub const CLIENT_TYPE: &str = "lingxi_claw";
pub const CLIENT_LANG: &str = "zh-CN";
pub const AGENT_VERSION: u8 = 2;
pub const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) ",
    "Lingxi Claw/1.3.36 Chrome/144.0.7559.236 Electron/40.8.2 Safari/537.36",
);

/// 提示词超过约 320KB 上游直接 400「AI服务异常」。本地预检留 4KB 余量。
pub const MAX_PROMPT_BYTES: usize = 316 * 1024;

/// 会话 + run 按「账号 × 模型」缓存 1 小时，**不滑动续期**。
pub const SESSION_TTL_MS: i64 = 3_600_000;

/// 任务中心签到任务名（两个实现都按 `task_key == "daily_check_in"` 找）。
pub const TASK_DAILY_CHECK_IN: &str = "daily_check_in";

/// 上游统一包络 `{result, data, hint, msg}`；`result` 非 ok/Success 即业务错。
pub fn envelope_ok(body: &serde_json::Value) -> bool {
    match body.get("result").and_then(serde_json::Value::as_str) {
        None => true, // 没带 result 字段的不据此判错（SSE / 老形状）
        Some(value) => value.eq_ignore_ascii_case("ok") || value.eq_ignore_ascii_case("success"),
    }
}

/// 会不会因为这次响应而换域重试：401/403/404 在两个实现里都是换域信号。
pub fn should_fallback(status: u16) -> bool {
    matches!(status, 401 | 403 | 404)
}

fn url(path: &str) -> String {
    format!("{API_HOST}{path}")
}

/// 指定 host 拼 URL（回落域用）。
pub fn url_on(host: &str, path: &str) -> String {
    format!("{host}{path}")
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

/// run 终态上报：⚠️ 用 **session_id**（不是 run_id），两个实现的源码都是
/// `PUT /api/public/v1/sessions/{session_id}/execution_status`。
pub fn execution_status_url(session_id: &str) -> String {
    url(&format!("/api/public/v1/sessions/{session_id}/execution_status"))
}

/// 额度（智点）。两个实现都是这个路径，**没有** `?credit_type=total`。
pub fn balance_url() -> String {
    url("/api/public/v1/credits/balance")
}

/// 模型目录：响应 `data` 下有 `plans` 与 `models` 两组。
pub fn plans_url() -> String {
    url("/api/aioffice/v1/sessions/plans")
}

/// 任务中心状态（只读）：`data.tasks[]` 里按 `task_key` 找，读 `status`。
pub fn tasks_url() -> String {
    url("/api/public/v1/tasks")
}

/// 领取任务：`POST /api/public/v1/tasks/{task}/claim?date=YYYY-MM-DD`，body `{}`。
pub fn claim_url(task: &str, date: &str) -> String {
    let mut path = format!("/api/public/v1/tasks/{task}/claim");
    if !date.trim().is_empty() {
        path.push_str("?date=");
        path.push_str(date.trim());
    }
    url(&path)
}

/// 路径段（供 `url_on` 做回落时复用，避免两处各拼一份）。
pub fn sessions_path() -> &'static str {
    "/api/aioffice/v1/cowork/sessions"
}
pub fn runs_path() -> &'static str {
    "/api/public/v1/credits/runs"
}
pub fn chat_path() -> &'static str {
    "/api/public/v1/agent-runtime/chat/completions"
}
pub fn execution_status_path(session_id: &str) -> String {
    format!("/api/public/v1/sessions/{session_id}/execution_status")
}
pub fn balance_path() -> &'static str {
    "/api/public/v1/credits/balance"
}
pub fn plans_path() -> &'static str {
    "/api/aioffice/v1/sessions/plans"
}
pub fn tasks_path() -> &'static str {
    "/api/public/v1/tasks"
}
pub fn claim_path(task: &str, date: &str) -> String {
    let mut path = format!("/api/public/v1/tasks/{task}/claim");
    if !date.trim().is_empty() {
        path.push_str("?date=");
        path.push_str(date.trim());
    }
    path
}

/// 智点明细流水（`GET`）。参数口径见 `ledger::list_query`。
pub fn transaction_list_path() -> &'static str {
    "/api/public/v1/credits/transaction/list"
}

/// 某个会话的消耗明细（`GET session_id&cursor&limit`）。
pub fn transaction_expense_path() -> &'static str {
    "/api/public/v1/credits/transaction/expense"
}

/// 群聊：建群 / 解散（`POST /api/imchat/v1/channels`、`DELETE …/{id}`）。
pub fn imchat_channels_path() -> &'static str {
    "/api/imchat/v1/channels"
}

pub fn imchat_channel_path(channel_id: &str) -> String {
    format!("/api/imchat/v1/channels/{channel_id}")
}

/// ── WebSocket 两条通道（P3 + 群聊任务）──────────────────────
///
/// ⚠️ host 是 **`lingxi.kdocs.cn`**：插件作者实测「同一个 cookie 两边都认」，
/// 且握手 `Origin` 必须是 `https://lingxi.kdocs.cn`（不是 wps.cn）——
/// 换域会连不上或被拒。这里把 `https` 换成 `wss`，路径保持原样。
///
/// 两条不是同一个东西：
///   · `cowork_ws_url`：`/api/aioffice/v1/cowork/sessions/{id}/sandbox/completions/ws`
///     —— 对话主通道（思考 + 工具调用 + 真沙箱执行），会话由
///     `POST /api/aioffice/v1/cowork/sessions` 创建；
///   · `group_ws_url`：`/api/aioffice/v1/sessions/{id}/completions/ws`
///     —— **群聊任务**的前置动作（建群前必须让该会话收到过一次 `user.input`），
///     会话由 `POST /api/aioffice/v1/cowork/sessions` 以 `cowork_web` 类型创建。
pub fn ws_host() -> &'static str {
    FALLBACK_HOST
}

/// 握手 `Origin`（`wss` 与 `Origin` 都必须是 kdocs 域）。
pub fn ws_origin() -> &'static str {
    FALLBACK_HOST
}

pub fn cowork_ws_url(session_id: &str) -> String {
    format!(
        "wss://{}/api/aioffice/v1/cowork/sessions/{session_id}/sandbox/completions/ws",
        host_of(FALLBACK_HOST)
    )
}

pub fn group_ws_url(session_id: &str) -> String {
    format!(
        "wss://{}/api/aioffice/v1/sessions/{session_id}/completions/ws",
        host_of(FALLBACK_HOST)
    )
}

/// `https://host` → `host`（拼 `wss://` 时用；不引第二套字符串替换）。
fn host_of(origin: &str) -> &str {
    origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .unwrap_or(origin)
}

/// 群聊任务用的会话类型（与对话缓存里的 `cowork_client` 不同）。
pub const SESSION_TYPE_GROUP: &str = "cowork_web";

/// 签到写请求开关。**2026-11-13 第二轮打开**：端点不再是猜的 ——
/// `ketion/lingxi2api-panel` 的 `LXClaim` + `autotask.go`（真实在跑的每日
/// 任务链）与 `RobbsLuo/Coding2API` 的 `EP_CLAIM` + `checkin()` 逐字一致。
/// 保留它的理由：万一上游改了协议，把这里改回 `false` 就能一键停发写请求，
/// 而**不必删链路**。失效时 `checkin::reconcile` 仍会保留「待确认」、
/// 不虚报已领取，所以打开后最坏情况是留一条待确认记录，不是假成功。
/// ⚠️ 真机抓到 2xx 仍是必须补的验收项（见方案 §6），只是不阻塞代码落地。
pub const CLAIM_ENDPOINT_VERIFIED: bool = true;

/// 每请求一个 id（两个实现分别是 uuid4 / uuid4().hex）。这里不引新依赖：
/// 毫秒时间戳 + 进程内序号，32 位 hex，形状与 uuid4().hex 一致。
pub fn request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{:016x}{:016x}", crate::server::logging::now_ms() as u64, seq)
}

fn base_headers(
    host: &str,
    wps_sid: &str,
    device_id: &str,
    cookie: Option<&str>,
    request_id: &str,
    accept: &str,
) -> Vec<(String, String)> {
    let cookie_value = match cookie {
        Some(full) if !full.trim().is_empty() => full.trim().to_string(),
        _ => format!("wps_sid={wps_sid}"),
    };
    let rid = if request_id.is_empty() {
        self::request_id()
    } else {
        request_id.to_string()
    };
    vec![
        ("Cookie".to_string(), cookie_value),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), accept.to_string()),
        ("User-Agent".to_string(), USER_AGENT.to_string()),
        ("X-Client-Type".to_string(), CLIENT_TYPE.to_string()),
        ("X-Client-Lang".to_string(), CLIENT_LANG.to_string()),
        ("X-Cc-Agent-Type".to_string(), CLIENT_TYPE.to_string()),
        ("X-Request-Id".to_string(), rid),
        ("x-kso-app-name".to_string(), APP_NAME.to_string()),
        ("x-kso-app-version".to_string(), APP_VERSION.to_string()),
        ("x-kso-device-id".to_string(), device_id.to_string()),
        ("Referer".to_string(), format!("{host}/")),
    ]
}

/// 账号面 / 三请求头（默认主域，Accept: application/json）。
pub fn headers(wps_sid: &str, device_id: &str, cookie: Option<&str>) -> Vec<(String, String)> {
    account_headers(API_HOST, wps_sid, device_id, cookie, "")
}

pub fn account_headers(
    host: &str,
    wps_sid: &str,
    device_id: &str,
    cookie: Option<&str>,
    request_id: &str,
) -> Vec<(String, String)> {
    base_headers(host, wps_sid, device_id, cookie, request_id, "application/json")
}

/// 对话请求头：⚠️ `session_id`/`run_id` 只在**头**里（`x-cc-session-id` /
/// `x-cc-run-id`），不进 body —— 两个实现的源码都这么写。
pub fn chat_headers(
    host: &str,
    wps_sid: &str,
    device_id: &str,
    cookie: Option<&str>,
    request_id: &str,
    session_id: &str,
    run_id: &str,
) -> Vec<(String, String)> {
    let mut out = base_headers(host, wps_sid, device_id, cookie, request_id, "text/event-stream");
    out.push(("x-cc-session-id".to_string(), session_id.to_string()));
    out.push(("x-cc-run-id".to_string(), run_id.to_string()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_step_urls_match_the_two_public_implementations() {
        assert_eq!(sessions_path(), "/api/aioffice/v1/cowork/sessions");
        assert_eq!(runs_path(), "/api/public/v1/credits/runs");
        assert_eq!(chat_path(), "/api/public/v1/agent-runtime/chat/completions");
        assert_eq!(balance_path(), "/api/public/v1/credits/balance");
        assert_eq!(plans_path(), "/api/aioffice/v1/sessions/plans");
        assert_eq!(tasks_path(), "/api/public/v1/tasks");
        for u in [sessions_url(), runs_url(), chat_url()] {
            assert!(u.starts_with(API_HOST), "{u}");
        }
    }

    #[test]
    fn execution_status_uses_session_id_not_run_id() {
        let u = execution_status_url("sess-1");
        assert!(u.contains("/api/public/v1/sessions/sess-1/execution_status"), "{u}");
        assert!(!u.contains("credits/runs"));
    }

    #[test]
    fn claim_url_carries_the_signin_window_date() {
        assert_eq!(
            claim_path("daily_check_in", "2026-11-13"),
            "/api/public/v1/tasks/daily_check_in/claim?date=2026-11-13"
        );
        assert_eq!(claim_path("daily_check_in", ""), "/api/public/v1/tasks/daily_check_in/claim");
    }

    #[test]
    fn login_page_is_the_wps_sso_form_not_the_kdocs_app_shell() {
        // 回归钉子：`lingxi.kdocs.cn` 对移动 UA 返回的是 WPS 客户端容器页
        // （依赖 `window.wps` JSBridge），在普通 WebView 里是空白页。
        // 登录窗必须加载 SSR 的 WPS 通行证登录页。
        assert_eq!(LOGIN_HOST, "https://account.wps.cn");
        assert!(!LOGIN_HOST.contains("kdocs"));
    }

    #[test]
    fn primary_host_is_wps_cn_and_kdocs_is_the_fallback() {
        assert_eq!(API_HOST, "https://lingxi.wps.cn");
        assert_eq!(FALLBACK_HOST, "https://lingxi.kdocs.cn");
        assert!(should_fallback(401) && should_fallback(403) && should_fallback(404));
        assert!(!should_fallback(429));
    }

    #[test]
    fn envelope_result_gates_business_errors() {
        use serde_json::json;
        assert!(envelope_ok(&json!({ "result": "ok", "data": {} })));
        assert!(envelope_ok(&json!({ "result": "Success" })));
        assert!(envelope_ok(&json!({ "data": {} })));
        assert!(!envelope_ok(&json!({ "result": "error", "hint": "今日已领取" })));
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

    fn header<'a>(list: &'a [(String, String)], key: &str) -> Option<&'a str> {
        list.iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn account_and_chat_headers_differ_exactly_where_they_should() {
        let a = account_headers(API_HOST, "V02S-a", "dev-1", None, "rid-1");
        let c = chat_headers(API_HOST, "V02S-a", "dev-1", None, "rid-1", "sess", "run");
        assert_eq!(header(&a, "Accept"), Some("application/json"));
        assert_eq!(header(&c, "Accept"), Some("text/event-stream"));
        assert_eq!(header(&c, "x-cc-session-id"), Some("sess"));
        assert_eq!(header(&c, "x-cc-run-id"), Some("run"));
        assert_eq!(header(&a, "x-cc-session-id"), None);
        assert_eq!(header(&c, "X-Client-Lang"), Some("zh-CN"));
        assert_eq!(header(&c, "X-Request-Id"), Some("rid-1"));
    }
}
