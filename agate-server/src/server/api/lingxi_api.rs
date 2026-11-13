//! WPS 灵犀的管理端点：任务中心（P2）、智点明细（P2）、通道设置（P3）。
//!
//! ```text
//!   GET  /api/accounts/{id}/lingxi/tasks          任务看板（只读）
//!   POST /api/accounts/{id}/lingxi/tasks/claim    领取某个任务 { taskKey }
//!   POST /api/accounts/{id}/lingxi/tasks/run      代跑某个任务 { taskKey }
//!   GET  /api/accounts/{id}/lingxi/ledger         智点明细一页
//!   GET  /api/lingxi/settings                     通道设置（sse / ws）
//!   POST /api/lingxi/settings                     保存通道设置 { channel }
//! ```
//!
//! ── 三条纪律 ────────────────────────────────────────────────
//!   1. **查询与写分开**：`GET` 只读，动状态一律 `POST`（与 growth / onboarding
//!      同一口径：页面展开不给「看一眼就改状态」留机会）；
//!   2. **不虚报结果**：领取后回读 `status == "claimed"` 才说「已领取」——
//!      上游对重复领取回 200 假成功（方案 L11），HTTP 200 不是判据；
//!   3. **读不出就说读不出**：明细 / 任务读失败时带 `upstreamStatus` 返回，
//!      界面写「暂不可用」，不拿 0 或空表冒充。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic。

use axum::body::Bytes;
use axum::response::Response;
use serde_json::{json, Value};

use crate::server::core::providers::lingxi::{channel, client, credentials, ledger, tasks};
use crate::server::errors::{management_error, GatewayError};
use crate::server::http::ok_json;
use crate::server::ServerState;

/// 灵犀账号的凭证三元组（sid / 整段 Cookie / 派生设备 id）。
struct Credential {
    sid: String,
    cookie: Option<String>,
    device: String,
}

fn credential_of(state: &ServerState, account_id: &str) -> Result<Credential, Response> {
    let record = state
        .store()
        .lingxi_account_record(account_id)
        .ok_or_else(|| management_error(404, "没有这个 WPS 灵犀账号"))?;
    let (sid, cookie, uid) = client::credentials_of(&record);
    if sid.is_empty() {
        return Err(management_error(
            401,
            "WPS 灵犀账号缺少 wps_sid，请重新登录或粘贴 Cookie",
        ));
    }
    let identity = if uid.is_empty() { sid.clone() } else { uid };
    Ok(Credential { sid, cookie, device: credentials::device_id(&identity) })
}

fn fail(error: GatewayError) -> Response {
    management_error(error.status_code, error.message)
}

/// 任务行 → 界面契约（判据全在内核，界面只读不算）。
fn task_row(task: &tasks::Task) -> Value {
    json!({
        "key": task.key,
        "title": task.title,
        "status": task.status,
        "claimable": task.claimable(),
        "claimed": task.claimed(),
        "reward": task.reward,
        "auto": task.auto(),
        "hint": tasks::hint_of(&task.key),
    })
}

/// 汇总：待办 / 可领 / 已领 / 待领智点 / 已领智点（面板同款口径）。
fn summarize(list: &[tasks::Task]) -> Value {
    let mut incomplete = 0u32;
    let mut claimable = 0u32;
    let mut claimed = 0u32;
    let mut pending = 0.0f64;
    let mut claimed_credit = 0.0f64;
    for task in list {
        if task.claimed() {
            claimed += 1;
            claimed_credit += task.reward.unwrap_or(0.0);
        } else if task.claimable() {
            claimable += 1;
            pending += task.reward.unwrap_or(0.0);
        } else {
            incomplete += 1;
        }
    }
    json!({
        "incomplete": incomplete,
        "claimable": claimable,
        "claimed": claimed,
        "pendingCredit": pending,
        "claimedCredit": claimed_credit,
    })
}

fn board(list: &[tasks::Task]) -> Value {
    let checked_in_today = list
        .iter()
        .find(|task| task.key == crate::server::core::providers::lingxi::endpoints::TASK_DAILY_CHECK_IN)
        .map(tasks::Task::claimed)
        .unwrap_or(false);
    json!({
        "tasks": list.iter().map(task_row).collect::<Vec<Value>>(),
        "summary": summarize(list),
        "checkedInToday": checked_in_today,
    })
}

/// `GET /api/accounts/{id}/lingxi/tasks` —— 任务看板（只读，不发写请求）。
pub async fn get_tasks(state: &ServerState, account_id: &str) -> Response {
    let credential = match credential_of(state, account_id) {
        Ok(credential) => credential,
        Err(response) => return response,
    };
    match tasks::list(&credential.sid, &credential.device, credential.cookie.as_deref()).await {
        Ok(list) => {
            let mut body = board(&list);
            if let Some(object) = body.as_object_mut() {
                object.insert("accountId".to_string(), Value::String(account_id.to_string()));
                object.insert("upstreamStatus".to_string(), Value::from(200));
            }
            ok_json(body)
        }
        // 读不出就如实说读不出：界面按 `error` 写「暂不可用」，不拿空表冒充
        Err(error) => ok_json(json!({
            "accountId": account_id,
            "tasks": [],
            "summary": Value::Null,
            "checkedInToday": Value::Null,
            "upstreamStatus": error.status_code,
            "error": error.message,
        })),
    }
}

/// `POST /api/accounts/{id}/lingxi/tasks/claim` —— 领取。
///
/// 领取后**回读**任务状态：只有 `status == "claimed"` 才在 `message` 里说
/// 「已领取」；200 假成功时如实说「上游没确认」。
pub async fn claim_task(state: &ServerState, account_id: &str, body: &Bytes) -> Response {
    let credential = match credential_of(state, account_id) {
        Ok(credential) => credential,
        Err(response) => return response,
    };
    let payload: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let key = payload
        .get("taskKey")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if key.is_empty() {
        return management_error(400, "缺少 taskKey");
    }
    let outcome = match tasks::claim(
        &credential.sid,
        &credential.device,
        credential.cookie.as_deref(),
        &key,
        "",
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => return fail(error),
    };
    // 回读：上游对重复领取回 200 假成功，唯一判据是任务状态
    let list = tasks::list(&credential.sid, &credential.device, credential.cookie.as_deref())
        .await
        .unwrap_or_default();
    let now_claimed = list
        .iter()
        .find(|task| task.key == key)
        .map(tasks::Task::claimed)
        .unwrap_or(false);
    let message = if now_claimed {
        "已领取"
    } else if outcome.explicit_reject {
        "上游拒绝了这次领取"
    } else if !outcome.sent {
        "领取端点未启用（本地开关关闭），没有发出请求"
    } else {
        "上游未确认到账（可能已领过，稍后看明细）"
    };
    let mut view = board(&list);
    if let Some(object) = view.as_object_mut() {
        object.insert("accountId".to_string(), Value::String(account_id.to_string()));
        object.insert("taskKey".to_string(), Value::String(key));
        object.insert("claimed".to_string(), Value::Bool(now_claimed));
        object.insert("message".to_string(), Value::String(message.to_string()));
        object.insert("upstreamStatus".to_string(), Value::from(outcome.status));
        object.insert("explicitReject".to_string(), Value::Bool(outcome.explicit_reject));
    }
    ok_json(view)
}

/// `POST /api/accounts/{id}/lingxi/tasks/run` —— 代跑一次性任务。
///
/// 慢（真实对话 / 建群，几十秒到两分钟），失败一律带原因返回；成功后同样
/// 回读任务状态 —— 任务翻转由上游判定，网关**不自行宣布成功**。
pub async fn run_task(state: &ServerState, account_id: &str, body: &Bytes) -> Response {
    let credential = match credential_of(state, account_id) {
        Ok(credential) => credential,
        Err(response) => return response,
    };
    let payload: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let key = payload
        .get("taskKey")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if key.is_empty() {
        return management_error(400, "缺少 taskKey");
    }
    if key == crate::server::core::providers::lingxi::endpoints::TASK_DAILY_CHECK_IN {
        return management_error(400, "每日签到不需要代跑：请用领取接口");
    }
    let ran = match tasks::complete(
        &credential.sid,
        &credential.device,
        credential.cookie.as_deref(),
        &key,
    )
    .await
    {
        Ok(ran) => ran,
        Err(error) => {
            return ok_json(json!({
                "accountId": account_id,
                "taskKey": key,
                "ok": false,
                "message": error.message,
                "upstreamStatus": error.status_code,
            }));
        }
    };
    // 上游判定有延迟（面板实测 2 秒后才回读），这里同款等一拍再读
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let list = tasks::list(&credential.sid, &credential.device, credential.cookie.as_deref())
        .await
        .unwrap_or_default();
    let task = list.iter().find(|task| task.key == key);
    let mut view = board(&list);
    if let Some(object) = view.as_object_mut() {
        object.insert("accountId".to_string(), Value::String(account_id.to_string()));
        object.insert("taskKey".to_string(), Value::String(key));
        object.insert("ok".to_string(), Value::Bool(true));
        object.insert("message".to_string(), Value::String(ran.message));
        object.insert(
            "nowClaimable".to_string(),
            Value::Bool(task.map(tasks::Task::claimable).unwrap_or(false)),
        );
        object.insert(
            "nowClaimed".to_string(),
            Value::Bool(task.map(tasks::Task::claimed).unwrap_or(false)),
        );
    }
    ok_json(view)
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let mut parts = pair.splitn(2, '=');
        let name = parts.next().unwrap_or("");
        if name != key {
            return None;
        }
        let value = parts.next().unwrap_or("");
        Some(
            value
                .replace("%3D", "=")
                .replace("%26", "&")
                .replace("%20", " ")
                .trim()
                .to_string(),
        )
    })
}

fn number_query(query: &str, key: &str) -> i64 {
    query_param(query, key)
        .and_then(|text| text.parse::<i64>().ok())
        .unwrap_or(0)
}

/// `GET /api/accounts/{id}/lingxi/ledger` —— 智点明细一页。
///
/// 参数：`type=all|income|expense`、`incomeCursor`、`expenseCursor`、`limit`。
/// 读不出时带 `upstreamStatus` 与 `error` 返回空表 —— 界面写「暂不可用」。
pub async fn get_ledger(state: &ServerState, account_id: &str, query: &str) -> Response {
    let credential = match credential_of(state, account_id) {
        Ok(credential) => credential,
        Err(response) => return response,
    };
    let typ = query_param(query, "type").unwrap_or_else(|| "all".to_string());
    if !matches!(typ.as_str(), "all" | "income" | "expense") {
        return management_error(400, "type 只支持 all / income / expense");
    }
    let income_cursor = number_query(query, "incomeCursor");
    let expense_cursor = number_query(query, "expenseCursor");
    let limit = number_query(query, "limit").clamp(0, i64::from(ledger::PAGE_LIMIT_MAX)) as u32;
    match ledger::page(
        &credential.sid,
        &credential.device,
        credential.cookie.as_deref(),
        &typ,
        income_cursor,
        expense_cursor,
        limit,
    )
    .await
    {
        Ok(page) => ok_json(json!({
            "accountId": account_id,
            "type": typ,
            "items": page.items.iter().map(|txn| json!({
                "id": txn.id,
                "kind": txn.kind,
                "title": txn.title,
                "source": txn.source,
                "amount": txn.amount,
                "time": txn.time,
                "expireTime": txn.expire_time,
                "sessionId": txn.session_id,
            })).collect::<Vec<Value>>(),
            "incomeCursor": page.income_cursor,
            "expenseCursor": page.expense_cursor,
            "hasMore": page.has_more,
            "upstreamStatus": 200,
        })),
        Err(status) => ok_json(json!({
            "accountId": account_id,
            "type": typ,
            "items": [],
            "incomeCursor": 0,
            "expenseCursor": 0,
            "hasMore": false,
            "upstreamStatus": status,
            "error": "灵犀明细读不出来（上游没有返回可用流水）",
        })),
    }
}

// ─── 通道设置 ───────────────────────────────────────────────

/// `GET /api/lingxi/settings`
pub async fn get_settings() -> Response {
    let current = channel::current();
    let mut view = channel::view(current);
    if let Some(object) = view.as_object_mut() {
        // 文案由内核给：两条通道各自拿到什么，界面不该自己编
        object.insert(
            "options".to_string(),
            json!([
                {
                    "id": "sse",
                    "label": "SSE（默认）",
                    "detail": "多轮历史原样透出、有 token 用量；拿不到工具执行",
                },
                {
                    "id": "ws",
                    "label": "cowork WebSocket",
                    "detail": "上游真执行工具与沙箱代码；多轮压平成一段文本、没有 token 用量",
                }
            ]),
        );
    }
    ok_json(view)
}

/// `POST /api/lingxi/settings` —— `{ channel: "sse"|"ws" }`
pub async fn post_settings(body: Bytes) -> Response {
    let payload: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let raw = payload
        .get("channel")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let Some(next) = channel::Channel::parse(&raw) else {
        return management_error(400, "channel 只支持 sse / ws");
    };
    match channel::set(next) {
        Ok(saved) => ok_json(channel::view(saved)),
        Err(message) => management_error(500, message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::core::providers::lingxi::tasks::Task;

    fn task(key: &str, status: &str, reward: Option<f64>) -> Task {
        Task {
            key: key.to_string(),
            title: tasks::title_of(key).to_string(),
            status: status.to_string(),
            reward,
        }
    }

    #[test]
    fn summary_counts_match_the_panel_semantics() {
        let list = vec![
            task("daily_check_in", "claimed", Some(2.5)),
            task("try_expert_mode", "claimable", Some(5.0)),
            task("try_group_chat", "incomplete", Some(5.0)),
            task("use_lingxi_client", "incomplete", None),
        ];
        let summary = summarize(&list);
        assert_eq!(summary["claimed"], json!(1));
        assert_eq!(summary["claimable"], json!(1));
        assert_eq!(summary["incomplete"], json!(2));
        assert_eq!(summary["claimedCredit"].as_f64(), Some(2.5));
        assert_eq!(summary["pendingCredit"].as_f64(), Some(5.0));
    }

    #[test]
    fn board_reports_checked_in_today_from_the_daily_task() {
        let list = vec![task("daily_check_in", "claimed", Some(1.0))];
        assert_eq!(board(&list)["checkedInToday"], json!(true));
        let list = vec![task("daily_check_in", "incomplete", Some(1.0))];
        assert_eq!(board(&list)["checkedInToday"], json!(false));
        assert!(board(&[])["tasks"].as_array().map(Vec::is_empty).unwrap_or(false));
    }

    #[test]
    fn task_row_exposes_capabilities_without_recomputing_them() {
        let row = task_row(&task("try_group_chat", "claimable", Some(5.0)));
        assert_eq!(row["key"], json!("try_group_chat"));
        assert_eq!(row["claimable"], json!(true));
        assert_eq!(row["claimed"], json!(false));
        assert_eq!(row["auto"], json!(true));
        assert!(row["hint"].as_str().unwrap_or("").contains("群聊"));
    }

    #[test]
    fn query_params_are_parsed_with_defaults() {
        assert_eq!(query_param("type=income&limit=20", "type").as_deref(), Some("income"));
        assert_eq!(number_query("incomeCursor=-5", "incomeCursor"), -5);
        assert_eq!(number_query("", "limit"), 0);
        assert_eq!(query_param("", "type"), None);
    }
}
