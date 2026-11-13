//! 灵犀出站请求的薄封装 + 三请求编排。
//!
//! ⚠️ 端点与 body 字段来自公开参考实现（方案 §1），**尚未真机实测**；
//! 4 处来源冲突里 host 与 `execution_status` 路径就在这个文件。
//!
//! 硬约束：release 是 `panic=abort`，零 unwrap/expect/panic，不持锁跨 await。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::server::core::auth_http::send_raw;
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::{chat, classify, credentials, endpoints, usage};

const TIMEOUT_MS: u64 = 20_000;

/// 一条待结算的 run（`completed` 是结算触发点，见 `chat::report_lifecycle`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRun {
    pub run_id: String,
    pub wps_sid: String,
    pub device_id: String,
}

fn pending_table() -> &'static Mutex<HashMap<String, PendingRun>> {
    static TABLE: OnceLock<Mutex<HashMap<String, PendingRun>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_table() -> std::sync::MutexGuard<'static, HashMap<String, PendingRun>> {
    pending_table().lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 建会话的**并发单飞**锁。用 `tokio::sync::Mutex` 而不是 `std::sync::Mutex`：
/// 锁内要跨 await 发三次请求，std 锁会让 Future 失去 `Send`（整个转发链
/// 就编译不过），而且会把别的家一起堵住。
fn session_build_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// 按「上游真名」记一条待结算 run。会话 + run 由 `session.rs` 的
/// 账号×模型缓存 + 并发单飞保证不会同键并发；跨账号同模型并发时按最近一条
/// 结算（**边界**：真机多账号并发需复验，见方案 L7）。
pub fn remember_pending_run(model: &str, run: PendingRun) {
    lock_table().insert(model.trim().to_lowercase(), run);
}

/// 取走一条待结算 run（取走即删，避免重复上报）。
pub fn take_pending_run(model: &str) -> Option<PendingRun> {
    lock_table().remove(&model.trim().to_lowercase())
}

#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
}

/// 通用辅助请求（余额 / 目录 / 任务 / 探活）。请求头带 `wps_sid` 与设备头族。
pub async fn request(
    method: &str,
    url: &str,
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
    body: Option<&Value>,
) -> Result<Reply, GatewayError> {
    let headers = endpoints::headers(sid, device_id, cookie);
    let response = send_raw(method, url, body, &headers, None, Some(TIMEOUT_MS))
        .await
        .map_err(|error| {
            GatewayError::with_status(
                if error.is_timeout() { 504 } else { 502 },
                if error.is_timeout() {
                    "灵犀请求超时".to_string()
                } else {
                    "无法连接灵犀，请检查网络".to_string()
                },
            )
        })?;
    Ok(Reply {
        status: response.status,
        body: response.payload.unwrap_or(Value::Null),
    })
}

/// 一次对话的前置三步：建会话 → 领 run → 上报 running。
/// 结果放进 `session` 缓存与待结算表，供 `build_chat_request` 与流收尾使用。
pub async fn prepare_conversation(
    model: &str,
    account_id: &str,
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
    cached: Option<super::session::Session>,
) -> Result<super::session::Session, GatewayError> {
    if let Some(session) = cached {
        if chat::report_lifecycle() {
            remember_pending_run(
                model,
                PendingRun {
                    run_id: session.run_id.clone(),
                    wps_sid: sid.to_string(),
                    device_id: device_id.to_string(),
                },
            );
        }
        return Ok(session);
    }

    // 只有「缓存没命中」才进锁：命中路径（绝大多数请求）完全无锁。
    // 拿到锁再查一次（双检），别人刚建好的会话直接用，避免同账号同模型
    // 并发各建一个 run —— 上游按 run 计费，重复建会话就是重复扣智点。
    let key = super::session::cache_key(account_id, model);
    let _singleton = session_build_lock().lock().await;
    if let Some(session) = super::session::global().lookup(&key, logging::now_ms()) {
        if chat::report_lifecycle() {
            remember_pending_run(
                model,
                PendingRun {
                    run_id: session.run_id.clone(),
                    wps_sid: sid.to_string(),
                    device_id: device_id.to_string(),
                },
            );
        }
        return Ok(session);
    }

    let first = request("POST", &endpoints::sessions_url(), sid, device_id, cookie, Some(&json!({}))).await?;
    if !(200..300).contains(&first.status) {
        return Err(GatewayError::with_status(
            i32::from(first.status),
            format!("灵犀建会话失败（HTTP {}）", first.status),
        ));
    }
    let session_id = first
        .body
        .pointer("/data/session_id")
        .or_else(|| first.body.get("session_id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if session_id.is_empty() {
        return Err(GatewayError::with_status(502, "灵犀建会话响应里没有 session_id".to_string()));
    }

    let second = request(
        "POST",
        &endpoints::runs_url(),
        sid,
        device_id,
        cookie,
        Some(&json!({ "model": model })),
    )
    .await?;
    if !(200..300).contains(&second.status) {
        return Err(GatewayError::with_status(
            i32::from(second.status),
            format!("灵犀领 run 失败（HTTP {}）", second.status),
        ));
    }
    let run_id = second
        .body
        .pointer("/data/run_id")
        .or_else(|| second.body.get("run_id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if run_id.is_empty() {
        return Err(GatewayError::with_status(502, "灵犀领 run 响应里没有 run_id".to_string()));
    }

    if chat::report_lifecycle() {
        let running = request(
            "PUT",
            &endpoints::execution_status_url(&run_id),
            sid,
            device_id,
            cookie,
            Some(&json!({ "execution_status": "running", "lease_seconds": 90 })),
        )
        .await?;
        if !(200..300).contains(&running.status) {
            logging::log("[Models]", &format!("⚠️ 灵犀 run {run_id} running 上报失败（HTTP {}）", running.status));
        }
        remember_pending_run(
            model,
            PendingRun {
                run_id: run_id.clone(),
                wps_sid: sid.to_string(),
                device_id: device_id.to_string(),
            },
        );
    }

    Ok(super::session::Session {
        session_id,
        run_id,
        created_ms: logging::now_ms(),
    })
}

/// 流结束后的结算触发点。**fire-and-forget**：结算失败只记日志，不影响已经
/// 交给客户端的正文（参考实现的上游是延迟归账，失败也不该让答复消失）。
pub fn report_completed(run: PendingRun) {
    let url = endpoints::execution_status_url(&run.run_id);
    tokio::spawn(async move {
        let reply = request(
            "PUT",
            &url,
            &run.wps_sid,
            &run.device_id,
            None,
            Some(&json!({ "execution_status": "completed" })),
        )
        .await;
        match reply {
            Ok(res) if (200..300).contains(&res.status) => {
                logging::log("[Models]", &format!("灵犀 run {} 已上报 completed", run.run_id));
            }
            Ok(res) => logging::log(
                "[Models]",
                &format!("⚠️ 灵犀 run {} completed 上报失败（HTTP {}）", run.run_id, res.status),
            ),
            Err(error) => logging::log(
                "[Models]",
                &format!("⚠️ 灵犀 run {} completed 上报失败：{}", run.run_id, error.message),
            ),
        }
    });
}

/// 一次签到提交的结果。`status == 0` 表示「端点未验证，没发过请求」——
/// 与「上游 404」是两件事，后者不能算明确拒绝，否则台账会被清掉反复重试。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskReply {
    pub status: u16,
    pub explicit_reject: bool,
}

/// 任务中心状态回查。`status=claimed` 是公开资料唯一给出的「今日已签」
/// 判据；读不出返回 `None`（**不是 false**），由 `checkin::reconcile` 保留
/// 待确认。⚠️ 端点未验证时不发请求（见 `endpoints::CLAIM_ENDPOINT_VERIFIED`）。
pub async fn task_status(
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
) -> Option<bool> {
    if !endpoints::CLAIM_ENDPOINT_VERIFIED {
        return None;
    }
    let reply = request(
        "GET",
        &endpoints::task_url(endpoints::TASK_DAILY_CHECK_IN),
        sid,
        device_id,
        cookie,
        None,
    )
    .await
    .ok()?;
    if !(200..300).contains(&reply.status) {
        return None;
    }
    let status = reply
        .body
        .pointer("/data/status")
        .or_else(|| reply.body.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("");
    (!status.is_empty()).then(|| status.eq_ignore_ascii_case("claimed"))
}

/// 提交每日签到。上游对重复领取回 200 假成功（`result=ok` 而余额不动），
/// 所以这里**不判成功**，只把「明确业务拒绝」这一格交回去；到账由
/// `checkin::reconcile` 按领前/领后余额差或官方任务状态裁决。
pub async fn task_claim(
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
) -> Result<TaskReply, GatewayError> {
    if !endpoints::CLAIM_ENDPOINT_VERIFIED {
        return Ok(TaskReply { status: 0, explicit_reject: false });
    }
    let reply = request(
        "POST",
        &endpoints::task_url(endpoints::TASK_DAILY_CHECK_IN),
        sid,
        device_id,
        cookie,
        Some(&json!({ "task": endpoints::TASK_DAILY_CHECK_IN })),
    )
    .await?;
    let explicit_reject = reply.body.get("result").and_then(Value::as_str)
        == Some("error")
        || reply
            .body
            .get("code")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0);
    Ok(TaskReply { status: reply.status, explicit_reject })
}

/// 探活：只认上游 401 判死号（方案 L1，不按时间猜过期）。
pub async fn probe(sid: &str, device_id: &str, cookie: Option<&str>) -> bool {
    match request("GET", &endpoints::balance_url(), sid, device_id, cookie, None).await {
        Ok(reply) => reply.status != 401 && reply.status != 403,
        Err(_) => true, // 传输失败不是「凭证死了」，保留
    }
}

/// 额度。读不出时返回 None（界面写「暂不可用」，不拿 0 冒充）。
pub async fn balance(
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
) -> Option<usage::Balance> {
    let reply = request("GET", &endpoints::balance_url(), sid, device_id, cookie, None)
        .await
        .ok()?;
    if !(200..300).contains(&reply.status) {
        return None;
    }
    usage::parse_balance(&reply.body, logging::now_ms())
}

/// 目录拉取（两种来源形状都由 `models::parse_catalog` 归一）。
pub async fn catalog(
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
) -> Result<Vec<Value>, GatewayError> {
    let reply = request("GET", &endpoints::plans_url(), sid, device_id, cookie, None).await?;
    if !(200..300).contains(&reply.status) {
        return Err(GatewayError::with_status(
            i32::from(reply.status),
            format!("灵犀模型目录拉取失败（HTTP {}）", reply.status),
        ));
    }
    Ok(super::models::parse_catalog(&reply.body))
}

/// 出站前的本地判据：320KB 预检 + 错误分类入口（供适配器直接调用）。
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
    let uid = account
        .get("uid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    (sid, (!cookie.is_empty()).then_some(cookie), uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_run_is_taken_only_once() {
        let run = PendingRun {
            run_id: "r-1".to_string(),
            wps_sid: "V02S-abc".to_string(),
            device_id: "dev".to_string(),
        };
        remember_pending_run("GLM-5.3", run.clone());
        assert_eq!(take_pending_run("glm-5.3"), Some(run));
        assert_eq!(take_pending_run("GLM-5.3"), None);
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
}
