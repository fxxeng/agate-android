//! 灵犀出站请求的薄封装 + 三请求编排 + 账号面读取。
//!
//! ⚠️ **2026-11-13 校正**：第一批的路径/body 是按 README 语义猜的，这一批
//! 逐项对齐 `ketion/lingxi2api-panel::internal/upstream/lingxi.go` 与
//! `RobbsLuo/Coding2API::src/provider/lingxi/client.py` 的源码：
//!   · 两个域 `wps.cn`（主）/ `kdocs.cn`（回落），**401/403/404 也回落**；
//!   · 三请求 body 见 `chat.rs`；`session_id` 决定 `execution_status` 路径；
//!   · 响应是 `{result:"ok", data:{…}}` 包络，`result` 非 ok 是业务错；
//!   · 任务状态在 `data.tasks[]`，按 `task_key` 找，读 `status == "claimed"`。
//!
//! **仍未真机实测 2xx**：路径不再是猜的，能不能领到奖励仍要真机确认。
//!
//! 硬约束：release 是 `panic=abort`，零 unwrap/expect/panic，不持 std 锁跨 await。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::server::core::auth_http::send_raw;
use crate::server::core::beijing;
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::{chat, classify, credentials, endpoints, models, session, usage};

const TIMEOUT_MS: u64 = 20_000;

/// 一条待结算的 run（`completed` 是结算触发点，见 `chat::report_lifecycle`）。
/// ⚠️ `execution_status` 走的是 **session_id** 路径、body 用 **group_id**，
/// 所以两个 id 都要带；cookie 也要带，否则回落域上报时可能缺登录态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRun {
    pub session_id: String,
    pub group_id: String,
    pub wps_sid: String,
    pub device_id: String,
    pub cookie: Option<String>,
}

impl PendingRun {
    fn of(created: &session::Session, sid: &str, device: &str, cookie: Option<&str>) -> Self {
        Self {
            session_id: created.session_id.clone(),
            group_id: created.group_id.clone(),
            wps_sid: sid.to_string(),
            device_id: device.to_string(),
            cookie: cookie.map(str::to_string),
        }
    }
}

fn pending_table() -> &'static Mutex<HashMap<String, PendingRun>> {
    static TABLE: OnceLock<Mutex<HashMap<String, PendingRun>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_table() -> std::sync::MutexGuard<'static, HashMap<String, PendingRun>> {
    pending_table().lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 按「上游真名」记一条待结算 run。缓存 + 单飞保证同键不重复建会话；
/// 跨账号同模型并发时按最近一条结算（**边界**：真机多账号并发需复验）。
pub fn remember_pending_run(model: &str, run: PendingRun) {
    lock_table().insert(model.trim().to_lowercase(), run);
}

/// 取走一条待结算 run（取走即删，避免重复上报）。
pub fn take_pending_run(model: &str) -> Option<PendingRun> {
    lock_table().remove(&model.trim().to_lowercase())
}

/// 建会话的**并发单飞**锁。`tokio::sync::Mutex` 而不是 `std`：锁内要跨 await
/// 发三次请求，std 锁会让 Future 失去 `Send`。
fn session_build_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
    /// 2xx 且包络 `result` 为 ok/Success（或没带 result 字段）。
    pub result_ok: bool,
    /// 这次响应来自哪个域。⚠️ 对话请求由 `provider_loop` 直接发出、不走
    /// `request` 的双域回落，所以必须把「建 run 成功的那个域」一路带过去，
    /// 否则只在 `kdocs.cn` 有效的账号会拿主域去发聊天，稳定 401。
    pub host: String,
}

/// 路径 → 主域 / 回落域各打一次。**401 也回落**：kdocs 账号的 cookie 只在
/// kdocs 集群有效，主域 401 不代表凭证死了（两个参考实现同一判据）。
pub async fn request(
    method: &str,
    path: &str,
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
    body: Option<&Value>,
) -> Result<Reply, GatewayError> {
    let request_id = endpoints::request_id();
    let mut last: Option<Reply> = None;
    for host in [endpoints::API_HOST, endpoints::FALLBACK_HOST] {
        let url = endpoints::url_on(host, path);
        let headers = endpoints::account_headers(host, sid, device_id, cookie, &request_id);
        match send_raw(method, &url, body, &headers, None, Some(TIMEOUT_MS)).await {
            Ok(response) => {
                let payload = response.payload.unwrap_or(Value::Null);
                let reply = Reply {
                    status: response.status,
                    result_ok: endpoints::envelope_ok(&payload),
                    body: payload,
                    host: host.to_string(),
                };
                if (200..300).contains(&response.status) && reply.result_ok {
                    return Ok(reply);
                }
                // 非回落类错误（402/429/4xx）直接交调用方分类，别浪费第二次请求
                if !endpoints::should_fallback(response.status) && response.status < 500 {
                    return Ok(reply);
                }
                last = Some(reply);
            }
            Err(_) => {
                // 传输失败：换域再试一次
            }
        }
    }
    match last {
        Some(reply) => Ok(reply),
        None => Err(GatewayError::with_status(502, "无法连接灵犀，请检查网络")),
    }
}

fn data_of(reply: &Reply) -> &Value {
    reply.body.get("data").unwrap_or(&reply.body)
}

fn text_of(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("")
        .to_string()
}

fn require_ok(reply: &Reply, what: &str) -> Result<(), GatewayError> {
    if (200..300).contains(&reply.status) && reply.result_ok {
        return Ok(());
    }
    let status = if (200..300).contains(&reply.status) {
        400
    } else {
        i32::from(reply.status)
    };
    let hint = text_of(&reply.body, &["hint", "msg", "message"]);
    let mut message = format!("灵犀{what}失败（HTTP {}）", reply.status);
    if !hint.is_empty() {
        message.push_str("：");
        message.push_str(&hint.chars().take(200).collect::<String>());
    }
    Err(GatewayError::with_status(status, message))
}

/// 一次对话的前置三步：建会话 → 领 run → 上报 running。
/// 结果落进 `session` 缓存与待结算表，供 `build_chat_request` 与流收尾使用。
pub async fn prepare_conversation(
    model: &str,
    account_id: &str,
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
    messages: &Value,
    cached: Option<session::Session>,
) -> Result<session::Session, GatewayError> {
    if let Some(existing) = cached {
        if chat::report_lifecycle() {
            remember_pending_run(model, PendingRun::of(&existing, sid, device_id, cookie));
        }
        return Ok(existing);
    }

    // 只有「缓存没命中」才进锁；命中路径完全无锁。锁内双检避免同账号同模型
    // 并发各建一个 run —— 上游按 run 计费，重复建会话就是重复扣智点。
    let key = session::cache_key(account_id, model);
    let _singleton = session_build_lock().lock().await;
    if let Some(existing) = session::global().lookup(&key, logging::now_ms()) {
        if chat::report_lifecycle() {
            remember_pending_run(model, PendingRun::of(&existing, sid, device_id, cookie));
        }
        return Ok(existing);
    }

    // ① 建会话
    let first = request(
        "POST",
        endpoints::sessions_path(),
        sid,
        device_id,
        cookie,
        Some(&chat::session_body(device_id)),
    )
    .await?;
    require_ok(&first, "建会话")?;
    let session_id = text_of(data_of(&first), &["session_id"]);
    if session_id.is_empty() {
        return Err(GatewayError::with_status(502, "灵犀建会话响应里没有 session_id"));
    }

    // ② 领 run
    let second = request(
        "POST",
        endpoints::runs_path(),
        sid,
        device_id,
        cookie,
        Some(&chat::run_body(&session_id, model, messages, &endpoints::request_id())),
    )
    .await?;
    require_ok(&second, "领 run")?;
    let data = data_of(&second);
    let run_id = text_of(data, &["run_id"]);
    if run_id.is_empty() {
        return Err(GatewayError::with_status(502, "灵犀领 run 响应里没有 run_id"));
    }
    let group_id = {
        let value = text_of(data, &["group_id"]);
        if value.is_empty() { run_id.clone() } else { value }
    };

    let created = session::Session {
        session_id: session_id.clone(),
        run_id,
        group_id: group_id.clone(),
        base: second.host.clone(),
        created_ms: logging::now_ms(),
    };

    // ③ running 上报（结算触发链的第一环，失败只记日志不掐断正文）
    if chat::report_lifecycle() {
        let path = endpoints::execution_status_path(&session_id);
        let body = chat::execution_body(&group_id, "running");
        match request("PUT", &path, sid, device_id, cookie, Some(&body)).await {
            Ok(reply) if (200..300).contains(&reply.status) && reply.result_ok => {}
            Ok(reply) => logging::log(
                "[Models]",
                &format!("⚠️ 灵犀 run {group_id} running 上报失败（HTTP {}）", reply.status),
            ),
            Err(error) => logging::log(
                "[Models]",
                &format!("⚠️ 灵犀 run {group_id} running 上报失败：{}", error.message),
            ),
        }
        remember_pending_run(model, PendingRun::of(&created, sid, device_id, cookie));
    }
    Ok(created)
}

/// 流结束后的结算触发点。**fire-and-forget**：结算失败只记日志，不影响已经
/// 交给客户端的正文（上游延迟归账，失败也不该让答复消失）。
pub fn report_completed(run: PendingRun) {
    let path = endpoints::execution_status_path(&run.session_id);
    let body = chat::execution_body(&run.group_id, "completed");
    tokio::spawn(async move {
        let result = request(
            "PUT",
            &path,
            &run.wps_sid,
            &run.device_id,
            run.cookie.as_deref(),
            Some(&body),
        )
        .await;
        match result {
            Ok(reply) if (200..300).contains(&reply.status) && reply.result_ok => {
                logging::log("[Models]", &format!("灵犀 run {} 已上报 completed", run.group_id));
            }
            Ok(reply) => logging::log(
                "[Models]",
                &format!("⚠️ 灵犀 run {} completed 上报失败（HTTP {}）", run.group_id, reply.status),
            ),
            Err(error) => logging::log(
                "[Models]",
                &format!("⚠️ 灵犀 run {} completed 上报失败：{}", run.group_id, error.message),
            ),
        }
    });
}

/// 任务中心里「每日签到」那一条的状态。
#[derive(Clone, Debug, PartialEq)]
pub struct TaskState {
    pub claimed: bool,
    /// 上游签到窗口日期 `YYYY-MM-DD`（领取时带在 query 上）。
    pub date: String,
    /// 任务里声明的当日奖励（余额差读不出时的回落口径）。
    pub reward: Option<f64>,
}

fn number_at(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(|node| node.as_f64().or_else(|| node.as_str().and_then(|s| s.parse::<f64>().ok())))
    })
}

/// 从 `GET /api/public/v1/tasks` 的整包响应里解析签到任务（**纯函数**）。
/// 字段名来自两个参考实现：`data.tasks[].task_key` / `status` / `extra`。
pub fn parse_task_state(body: &Value) -> Option<TaskState> {
    let tasks = body
        .pointer("/data/tasks")
        .or_else(|| body.get("tasks"))
        .and_then(Value::as_array)?;
    let daily = tasks.iter().find(|item| {
        item.get("task_key").and_then(Value::as_str) == Some(endpoints::TASK_DAILY_CHECK_IN)
    })?;
    let extra = daily.get("extra").cloned().unwrap_or(Value::Null);
    let date = text_of(&extra, &["claim_date", "date", "today"]);
    let date = if date.is_empty() { beijing::today_key() } else { date };
    Some(TaskState {
        claimed: daily.get("status").and_then(Value::as_str) == Some("claimed"),
        date,
        reward: number_at(&extra, &["today_reward"])
            .or_else(|| number_at(daily, &["reward_amount"]))
            .or_else(|| {
                number_at(
                    body.pointer("/data/extra").unwrap_or(&Value::Null),
                    &["today_reward"],
                )
            }),
    })
}

/// 任务中心状态回查（**只读**，不受签到写开关影响）。读不出返回 None ——
/// **不是 false**，由 `checkin::reconcile` 保留待确认。
pub async fn task_status(sid: &str, device_id: &str, cookie: Option<&str>) -> Option<TaskState> {
    let reply = request("GET", endpoints::tasks_path(), sid, device_id, cookie, None)
        .await
        .ok()?;
    if !(200..300).contains(&reply.status) || !reply.result_ok {
        return None;
    }
    parse_task_state(&reply.body)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskReply {
    /// 0 = 端点开关未打开、没发过请求（与「上游 404」是两件事）。
    pub status: u16,
    pub explicit_reject: bool,
}

/// 提交每日签到：`POST /api/public/v1/tasks/daily_check_in/claim?date=…`，body `{}`。
/// 上游对重复领取回 200 假成功，所以这里**不判成功**；到账由
/// `checkin::reconcile` 按领前/领后余额差或官方任务状态裁决。
pub async fn task_claim(
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
    date: &str,
) -> Result<TaskReply, GatewayError> {
    if !endpoints::CLAIM_ENDPOINT_VERIFIED {
        return Ok(TaskReply { status: 0, explicit_reject: false });
    }
    let path = endpoints::claim_path(endpoints::TASK_DAILY_CHECK_IN, date);
    let reply = request("POST", &path, sid, device_id, cookie, Some(&json!({}))).await?;
    // 「明确拒绝」只认上游业务拒绝：result 非 ok / 4xx 业务码。
    // 404/5xx 是端点或上游问题，清台账会导致反复重打。
    let explicit_reject = !reply.result_ok
        || ((400..500).contains(&reply.status) && !endpoints::should_fallback(reply.status));
    Ok(TaskReply { status: reply.status, explicit_reject })
}

/// 探活：只认「两个域都 401/403」判死号（方案 L1，不按时间猜过期）。
pub async fn probe(sid: &str, device_id: &str, cookie: Option<&str>) -> bool {
    match request("GET", endpoints::balance_path(), sid, device_id, cookie, None).await {
        Ok(reply) => reply.status != 401 && reply.status != 403,
        Err(_) => true, // 传输失败不是「凭证死了」，保留
    }
}

/// 额度。读不出返回 None（界面写「暂不可用」，不拿 0 冒充）。
pub async fn balance(sid: &str, device_id: &str, cookie: Option<&str>) -> Option<usage::Balance> {
    let reply = request("GET", endpoints::balance_path(), sid, device_id, cookie, None)
        .await
        .ok()?;
    if !(200..300).contains(&reply.status) || !reply.result_ok {
        return None;
    }
    usage::parse_balance(&reply.body, logging::now_ms())
}

/// 目录拉取（`data.plans` + `data.models` 两组都由 `models::parse_catalog` 归一）。
pub async fn catalog(
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
) -> Result<Vec<Value>, GatewayError> {
    let reply = request("GET", endpoints::plans_path(), sid, device_id, cookie, None).await?;
    require_ok(&reply, "模型目录拉取")?;
    Ok(models::parse_catalog(&reply.body))
}

/// 出站前的本地判据：320KB 预检。
pub fn precheck(body: &Value) -> Option<GatewayError> {
    if classify::oversized(body) {
        return Some(GatewayError::with_status(400, classify::oversized_message()));
    }
    None
}

/// 从账号记录里取凭证（会话形态：`accessToken` 存的是 Cookie 串）。
pub fn credentials_of(account: &Value) -> (String, Option<String>, String) {
    let cookie = account
        .get("auth")
        .and_then(|auth| auth.get("accessToken"))
        .or_else(|| account.get("accessToken"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let sid = credentials::cookie_sid(&cookie).unwrap_or_default();
    let uid = account.get("uid").and_then(Value::as_str).unwrap_or("").to_string();
    (sid, (!cookie.is_empty()).then_some(cookie), uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> PendingRun {
        PendingRun {
            session_id: "s1".to_string(),
            group_id: "g1".to_string(),
            wps_sid: "V02S-abc".to_string(),
            device_id: "dev".to_string(),
            cookie: None,
        }
    }

    #[test]
    fn pending_run_is_taken_only_once() {
        remember_pending_run("GLM-5.3", run());
        assert_eq!(take_pending_run("glm-5.3"), Some(run()));
        assert_eq!(take_pending_run("GLM-5.3"), None);
    }

    #[test]
    fn pending_run_carries_session_and_group_ids_for_settlement() {
        let r = run();
        assert_eq!(
            endpoints::execution_status_path(&r.session_id),
            "/api/public/v1/sessions/s1/execution_status"
        );
        assert_eq!(chat::execution_body(&r.group_id, "completed")["run_token"], json!("g1"));
    }

    #[test]
    fn normal_body_passes_precheck() {
        assert!(precheck(&json!({ "model": "qwen3.8_flash", "messages": [] })).is_none());
    }

    #[test]
    fn oversized_body_is_rejected_before_any_network_call() {
        let big = json!({ "messages": [{ "content": "x".repeat(330_000) }] });
        let error = precheck(&big).unwrap_or(GatewayError::with_status(500, ""));
        assert_eq!(error.status_code, 400);
    }

    #[test]
    fn task_state_reads_the_daily_check_in_entry() {
        let body = json!({ "result": "ok", "data": { "tasks": [
            { "task_key": "try_expert_mode", "status": "pending" },
            { "task_key": "daily_check_in", "status": "claimed",
              "reward_amount": 3, "extra": { "claim_date": "2026-11-13", "today_reward": 2.5 } }
        ] } });
        let fallback = TaskState { claimed: false, date: String::new(), reward: None };
        let state = parse_task_state(&body).unwrap_or(fallback);
        assert!(state.claimed);
        assert_eq!(state.date, "2026-11-13");
        assert_eq!(state.reward, Some(2.5));
    }

    #[test]
    fn missing_or_unclaimed_task_is_not_read_as_failure() {
        assert!(parse_task_state(&json!({ "data": { "tasks": [] } })).is_none());
        let fallback = TaskState { claimed: true, date: String::new(), reward: None };
        let unclaimed = parse_task_state(&json!({ "data": { "tasks": [
            { "task_key": "daily_check_in", "status": "pending" }
        ] } })).unwrap_or(fallback);
        assert!(!unclaimed.claimed);
        assert_eq!(unclaimed.date.len(), 10); // 回落到北京日期键
    }
}
