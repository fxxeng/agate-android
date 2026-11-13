//! 灵犀任务中心：4 个任务的状态读取、**通用**领取、以及「代跑」一次性任务。
//!
//! ── 上游只有 4 个任务（P2）─────────────────────────────────
//! ```text
//!   daily_check_in     每日签到（可领）
//!   try_expert_mode    专家模式体验（必须真发起一次对话）
//!   try_group_chat     群聊体验（必须真建一次群）
//!   use_lingxi_client  客户端体验（同样要真发起对话）
//! ```
//! 与 CodeBuddy 那套「接受 → 进度 → 领取」的成长任务链**完全不同**：
//!   · 没有 accept 步骤，`claim` 前只要求「真的做过动作」，由上游判 `status`；
//!   · `status` 三态：`incomplete`（没做）/ `claimable`（做了没领）/ `claimed`（领过）；
//!   · 自动化不能靠「点一下」，必须产生真实行为（真实对话 + `execution_status`），
//!     群聊任务还要把会话升级成群、判定后再解散。
//!
//! 依据（两个独立实现的源码逐字一致）：
//!   · `ketion/lingxi2api-panel` → `internal/panel/lxtasks.go` 看板语义 +
//!     `internal/upstream/lingxi.go` 的 `lxTaskRun` / `lxTaskChat` /
//!     `lxGroupTask`（任务对话文案、`max_tokens=4096`、`plan_pro`、WS 前置动作）；
//!   · `RobbsLuo/Coding2API` → `src/provider/lingxi/client.py` 的
//!     `fetch_tasks` / `claim_task`（`GET tasks` 与 `POST tasks/{key}/claim`）。
//!
//! ⚠️ `claim` 是**写请求**：与签到同一条纪律 —— 上游对重复领取回 200 假成功，
//! 所以这里**不判成功**，只回「上游怎么说」，到账由调用方回读 `status` 裁决。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic，不持锁跨 await。

use serde_json::{json, Value};

use crate::server::core::auth_http::send_raw;
use crate::server::core::beijing;
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::{chat, client, endpoints};

/// 专家模式体验。
pub const TASK_TRY_EXPERT: &str = "try_expert_mode";
/// 群聊体验（必须先在 completions WS 上收到过一次 `user.input`，见 `ws.rs`）。
pub const TASK_TRY_GROUP: &str = "try_group_chat";
/// 客户端体验。
pub const TASK_USE_CLIENT: &str = "use_lingxi_client";

/// 「代跑」用的对话文案。⚠️ 裸词（`hi`）会被 cowork 风控拒（`AuditFailed`），
/// 必须是自然语言；`max_tokens` 要给足 4096 —— 回答太短可能凑不够 1 智点、
/// 不产生计费记录，任务就不翻转（面板源码里的实测结论）。
pub const TASK_PROMPT: &str = "请用两三句话介绍一下你自己，包括你擅长的任务类型";

/// 群聊任务建群时上游要求的固定模型（网页端固定值，不是目录里的模型）。
pub const GROUP_PLAN_MODEL: &str = "plan_pro";

/// 一条任务在界面上的样子。
#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub key: String,
    pub title: String,
    /// 上游原样 `status`（`incomplete` / `claimable` / `claimed`，未知值也保留）。
    pub status: String,
    /// 任务声明的奖励（智点）。上游缺字段时为 None —— **不拿 0 冒充**。
    pub reward: Option<f64>,
}

impl Task {
    pub fn claimed(&self) -> bool {
        self.status.eq_ignore_ascii_case("claimed")
    }

    pub fn claimable(&self) -> bool {
        self.status.eq_ignore_ascii_case("claimable")
    }

    /// 能不能由网关代跑（4 个任务都可以，但各自的动作不同）。
    pub fn auto(&self) -> bool {
        auto_supported(&self.key)
    }
}

/// 是否属于灵犀任务中心的可自动项（面板 `LxTaskSupported` 同名单）。
pub fn auto_supported(key: &str) -> bool {
    matches!(
        key,
        endpoints::TASK_DAILY_CHECK_IN | TASK_TRY_EXPERT | TASK_TRY_GROUP | TASK_USE_CLIENT
    )
}

/// 不支持自动完成时的操作指引（当前 4 个都支持，留作扩展 —— 与面板同款）。
pub fn hint_of(key: &str) -> &'static str {
    match key {
        endpoints::TASK_DAILY_CHECK_IN => "每天可领一次，点「领取」或等定时签到",
        TASK_TRY_EXPERT | TASK_USE_CLIENT => "需真实发起一次对话（网关会代跑）",
        TASK_TRY_GROUP => "需创建一个群聊（网关会代建并解散）",
        _ => "",
    }
}

fn text_at(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("")
        .to_string()
}

fn number_at(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|node| {
            node.as_f64()
                .or_else(|| node.as_str().and_then(|text| text.parse::<f64>().ok()))
        })
    })
}

/// 顺序固定：签到 → 专家 → 群聊 → 客户端 → 其余（保持上游顺序）。
/// 面板同款：「避免每次刷新顺序跳动」。
fn order_of(key: &str) -> usize {
    match key {
        endpoints::TASK_DAILY_CHECK_IN => 0,
        TASK_TRY_EXPERT => 1,
        TASK_TRY_GROUP => 2,
        TASK_USE_CLIENT => 3,
        _ => 4,
    }
}

/// 从 `GET /api/public/v1/tasks` 的整包响应里解析**全部**任务（纯函数）。
///
/// 字段判据（两个实现一致）：`data.tasks[]`，每项 `task_key` + `status`；
/// 标题与奖励各实现口径不同，这里按「有就用、没有就留空/None」处理：
///   · 标题：`title` → `task_name` → 本地名单（`title_of`）
///   · 奖励：`reward_credit` → `credit` → `reward_amount` → `extra.today_reward`
pub fn parse_tasks(body: &Value) -> Vec<Task> {
    let tasks = body
        .pointer("/data/tasks")
        .or_else(|| body.get("tasks"))
        .and_then(Value::as_array);
    let Some(items) = tasks else {
        return Vec::new();
    };
    let mut out: Vec<Task> = Vec::with_capacity(items.len());
    for item in items {
        let key = text_at(item, &["task_key", "task_code", "code"]);
        if key.is_empty() {
            continue;
        }
        let title = {
            let upstream = text_at(item, &["title", "task_name"]);
            if upstream.is_empty() {
                title_of(&key).to_string()
            } else {
                upstream
            }
        };
        // `status` 是主口径；两个布尔是「上游哪天直接给了布尔」的容错。
        let status = {
            let raw = text_at(item, &["status"]);
            if !raw.is_empty() {
                raw
            } else if item.get("claimed").and_then(Value::as_bool) == Some(true) {
                "claimed".to_string()
            } else if item.get("claimable").and_then(Value::as_bool) == Some(true) {
                "claimable".to_string()
            } else {
                "incomplete".to_string()
            }
        };
        let extra = item.get("extra").cloned().unwrap_or(Value::Null);
        let reward = number_at(&extra, &["today_reward"])
            .or_else(|| number_at(item, &["reward_credit", "credit", "reward_amount"]))
            .or_else(|| {
                item.get("credit")
                    .and_then(|credit| credit.get("multiplier"))
                    .and_then(Value::as_f64)
            });
        out.push(Task { key, title, status, reward });
    }
    out.sort_by_key(|task| order_of(&task.key));
    out
}

/// 本地标题兜底（上游没给 `title` 时界面不至于只剩一个 key）。
pub fn title_of(key: &str) -> &'static str {
    match key {
        endpoints::TASK_DAILY_CHECK_IN => "每日签到",
        TASK_TRY_EXPERT => "体验专家模式",
        TASK_TRY_GROUP => "体验群聊",
        TASK_USE_CLIENT => "体验灵犀客户端",
        _ => "未知任务",
    }
}

/// 读任务中心（只读）。上游非 2xx 直接交调用方分类 —— 与签到回查
/// （`client::task_status` 读不出时返回 None）刻意不同：这里是用户主动点开
/// 看板，读不到就该说读不到，不能静默给一张空表。
pub async fn list(
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
) -> Result<Vec<Task>, GatewayError> {
    let reply = client::request("GET", endpoints::tasks_path(), sid, device_id, cookie, None).await?;
    if !(200..300).contains(&reply.status) {
        return Err(GatewayError::with_status(
            i32::from(reply.status),
            format!("读取灵犀任务中心失败（HTTP {}）", reply.status),
        ));
    }
    Ok(parse_tasks(&reply.body))
}

/// 通用领取结果。`explicit_reject` 只在「上游业务拒绝」时为真 ——
/// 404/5xx 是端点或上游问题，调用方不该据此清台账/改状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimOutcome {
    pub status: u16,
    pub result_ok: bool,
    pub explicit_reject: bool,
    /// 0 = 端点开关未打开，没发过请求（与「上游 404」是两件事）。
    pub sent: bool,
}

/// 领取任意任务：`POST /api/public/v1/tasks/{key}/claim`，body `{}`。
///
/// ⚠️ 上游对重复领取回 200 假成功，所以这里**不判成功**；调用方必须回读
/// `status == "claimed"` 才能说「已领取」。
pub async fn claim(
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
    key: &str,
    date: &str,
) -> Result<ClaimOutcome, GatewayError> {
    let key = key.trim();
    if key.is_empty() {
        return Err(GatewayError::with_status(400, "缺少任务 id"));
    }
    if !endpoints::CLAIM_ENDPOINT_VERIFIED {
        return Ok(ClaimOutcome { status: 0, result_ok: false, explicit_reject: false, sent: false });
    }
    // 只有签到认 `date`（签到窗口按北京日期换日），其余任务的领取不带 query。
    let date = if key == endpoints::TASK_DAILY_CHECK_IN {
        if date.trim().is_empty() { beijing::today_key() } else { date.trim().to_string() }
    } else {
        String::new()
    };
    let path = endpoints::claim_path(key, &date);
    let reply = client::request("POST", &path, sid, device_id, cookie, Some(&json!({}))).await?;
    let explicit_reject = !reply.result_ok
        || ((400..500).contains(&reply.status) && !endpoints::should_fallback(reply.status));
    Ok(ClaimOutcome {
        status: reply.status,
        result_ok: reply.result_ok,
        explicit_reject,
        sent: true,
    })
}

// ─── 代跑一次性任务 ─────────────────────────────────────────

/// 代跑结果摘要（给界面显示「做了什么」）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ran {
    pub key: String,
    pub message: String,
}

fn task_model() -> &'static str {
    // 目录里第一个可用模型由调用方给；这里给一个公开实现里出现过的兜底值
    // （面板 `lxTaskModel` 的返回值同款）。真机目录一般都会覆盖它。
    "deepseek-v4.1-flash"
}

/// 任务专用会话 + run：**不复用对话缓存的 run**（任务会话的
/// `execution_status` 生命周期独立管理，面板源码同款注释）。
async fn task_run(
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
    model: &str,
) -> Result<(String, String, String), GatewayError> {
    // ① 建会话（cowork_client + agent_version + env）
    let created = client::request(
        "POST",
        endpoints::sessions_path(),
        sid,
        device_id,
        cookie,
        Some(&chat::session_body(device_id)),
    )
    .await?;
    if !(200..300).contains(&created.status) || !created.result_ok {
        return Err(GatewayError::with_status(
            502,
            format!("灵犀建任务会话失败（HTTP {}）", created.status),
        ));
    }
    let session_id = created
        .body
        .pointer("/data/session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if session_id.is_empty() {
        return Err(GatewayError::with_status(502, "灵犀建任务会话响应里没有 session_id"));
    }
    // ② 领 run（`agent_input.text` 用任务文案，`expect_model` = model）
    let run_body = json!({
        "client_request_id": endpoints::request_id(),
        "session_id": session_id,
        "model_provider": "lingxi",
        "model_id": model,
        "agent_input": {
            "text": TASK_PROMPT,
            "files": [],
            "file_ids": [],
            "expect_model": model,
            "refs": [],
            "permission_mode": "default",
            "current_mode": "agent",
            "browser_plugin_enabled": false,
        },
    });
    let claimed = client::request(
        "POST",
        endpoints::runs_path(),
        sid,
        device_id,
        cookie,
        Some(&run_body),
    )
    .await?;
    if !(200..300).contains(&claimed.status) || !claimed.result_ok {
        return Err(GatewayError::with_status(
            502,
            format!("灵犀领任务 run 失败（HTTP {}）", claimed.status),
        ));
    }
    let data = claimed.body.get("data").unwrap_or(&claimed.body);
    let run_id = data.get("run_id").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if run_id.is_empty() {
        return Err(GatewayError::with_status(502, "灵犀领任务 run 响应里没有 run_id"));
    }
    let group_id = {
        let value = data.get("group_id").and_then(Value::as_str).unwrap_or("");
        if value.is_empty() { run_id.clone() } else { value.to_string() }
    };
    Ok((session_id, run_id, group_id))
}

async fn execution_status(
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
    session_id: &str,
    group_id: &str,
    status: &str,
) -> Result<(), GatewayError> {
    let path = endpoints::execution_status_path(session_id);
    let body = chat::execution_body(group_id, status);
    let reply = client::request("PUT", &path, sid, device_id, cookie, Some(&body)).await?;
    if (200..300).contains(&reply.status) && reply.result_ok {
        return Ok(());
    }
    Err(GatewayError::with_status(
        502,
        format!("灵犀上报 {status} 失败（HTTP {}）", reply.status),
    ))
}

/// 任务对话：发一次真实对话并读完整条流。
///
/// `stream: true` 是面板实测过的形状；这里用 `send_raw` 把响应整包读完
/// （等价于面板的 `io.Copy(io.Discard)`）—— 任务里不需要逐帧，只需要
/// 「这条 run 真的跑完了」，所以不做帧改写。
async fn task_chat(
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
    host: &str,
    session_id: &str,
    run_id: &str,
    model: &str,
) -> Result<(), GatewayError> {
    let body = json!({
        "stream": true,
        "model": model,
        "messages": [{ "role": "user", "content": TASK_PROMPT }],
        "base_llm_arguments": { "max_tokens": 4096 },
    });
    let url = endpoints::url_on(host, endpoints::chat_path());
    let headers = endpoints::chat_headers(
        host,
        sid,
        device_id,
        cookie,
        &endpoints::request_id(),
        session_id,
        run_id,
    );
    // 任务对话可能跑几十秒（沙箱/工具链），给 180s，比普通请求宽。
    match send_raw("POST", &url, Some(&body), &headers, None, Some(180_000)).await {
        Ok(response) if (200..300).contains(&response.status) => Ok(()),
        Ok(response) => Err(GatewayError::with_status(
            502,
            format!("灵犀任务对话失败（HTTP {}）", response.status),
        )),
        Err(error) => Err(GatewayError::with_status(
            502,
            format!("灵犀任务对话失败：{error}"),
        )),
    }
}

/// 代跑一个一次性任务。返回给人看得懂的摘要。
///
/// 两条动作链（面板源码同款）：
///   · `try_expert_mode` / `use_lingxi_client`：独立会话 + 真实对话 +
///     `execution_status` 生命周期（run 不报终态会话永远不 completed，任务不翻转）；
///   · `try_group_chat`：`cowork_web` 会话 → 在 completions WS 上发一次
///     `user.input` → 建群 → 解散（缺 WS 前置动作时建群必 400「参数错误」）。
pub async fn complete(
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
    key: &str,
) -> Result<Ran, GatewayError> {
    let key = key.trim();
    match key {
        TASK_TRY_EXPERT | TASK_USE_CLIENT => {
            let model = task_model();
            let (session_id, run_id, group_id) =
                task_run(sid, device_id, cookie, model).await?;
            // 会话是刚建在主域上的，任务对话也打主域。
            let host = endpoints::API_HOST;
            execution_status(sid, device_id, cookie, &session_id, &group_id, "running").await?;
            if let Err(error) =
                task_chat(sid, device_id, cookie, host, &session_id, &run_id, model).await
            {
                // 对话失败也要尽量收尾，别把 run 卡在 running（上游按 run 计费）。
                let _ = execution_status(sid, device_id, cookie, &session_id, &group_id, "completed")
                    .await;
                return Err(error);
            }
            execution_status(sid, device_id, cookie, &session_id, &group_id, "completed").await?;
            logging::log(
                "[Models]",
                &format!("灵犀任务 {key} 已代跑：真实对话 + run 生命周期上报完成"),
            );
            Ok(Ran {
                key: key.to_string(),
                message: "已发起一次真实对话（run 生命周期已上报）".to_string(),
            })
        }
        TASK_TRY_GROUP => {
            let message = super::ws::complete_group_task(sid, device_id, cookie).await?;
            logging::log("[Models]", &format!("灵犀任务 {key} 已代跑：{message}"));
            Ok(Ran { key: key.to_string(), message })
        }
        other => Err(GatewayError::with_status(
            400,
            format!("灵犀任务「{other}」不支持代跑"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> Value {
        json!({ "result": "ok", "data": { "tasks": [
            { "task_key": "use_lingxi_client", "status": "incomplete", "reward_credit": 5 },
            { "task_key": "daily_check_in", "status": "claimed",
              "extra": { "today_reward": 2.5, "claim_date": "2026-11-14" } },
            { "task_key": "try_group_chat", "status": "claimable", "title": "体验群聊" },
            { "task_key": "try_expert_mode", "status": "incomplete" },
            { "task_key": "", "status": "incomplete" },
            { "task_key": "unknown_future_task", "status": "incomplete" }
        ] } })
    }

    #[test]
    fn all_four_tasks_are_parsed_in_a_stable_order() {
        let tasks = parse_tasks(&body());
        let keys: Vec<&str> = tasks.iter().map(|task| task.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "daily_check_in",
                "try_expert_mode",
                "try_group_chat",
                "use_lingxi_client",
                "unknown_future_task"
            ]
        );
    }

    #[test]
    fn status_drives_claimed_and_claimable() {
        let tasks = parse_tasks(&body());
        assert!(tasks[0].claimed());
        assert!(!tasks[0].claimable());
        assert!(tasks[2].claimable());
        assert!(!tasks[2].claimed());
        assert!(!tasks[1].claimed() && !tasks[1].claimable());
    }

    #[test]
    fn reward_comes_from_whichever_field_upstream_sent() {
        let tasks = parse_tasks(&body());
        assert_eq!(tasks[0].reward, Some(2.5)); // extra.today_reward
        assert_eq!(tasks[3].reward, Some(5.0)); // reward_credit
        assert_eq!(tasks[1].reward, None); // 没给就是 None，不拿 0 冒充
    }

    #[test]
    fn missing_title_falls_back_to_a_local_label() {
        let tasks = parse_tasks(&body());
        assert_eq!(tasks[0].title, "每日签到");
        assert_eq!(tasks[2].title, "体验群聊"); // 上游给了就用上游的
        assert_eq!(tasks[4].title, "未知任务");
    }

    #[test]
    fn only_the_four_known_keys_are_auto_runnable() {
        for key in [
            endpoints::TASK_DAILY_CHECK_IN,
            TASK_TRY_EXPERT,
            TASK_TRY_GROUP,
            TASK_USE_CLIENT,
        ] {
            assert!(auto_supported(key), "{key}");
        }
        assert!(!auto_supported("unknown_future_task"));
        assert!(hint_of(TASK_TRY_GROUP).contains("群聊"));
    }

    #[test]
    fn empty_or_malformed_payload_yields_no_tasks() {
        assert!(parse_tasks(&json!({})).is_empty());
        assert!(parse_tasks(&json!({ "data": { "tasks": [] } })).is_empty());
        assert!(parse_tasks(&json!({ "data": { "tasks": "oops" } })).is_empty());
    }
}
