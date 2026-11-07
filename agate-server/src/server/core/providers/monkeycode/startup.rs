//! MonkeyCode 任务环境就绪等待（上游 3.4.2 `mode=attach` 的前置）。
//!
//! 创建任务时请求体已经带了首轮 `content`，上游会在环境就绪后自动执行它。
//! 旧链路连上 `mode=new` 后再发一次 `user-input`，等于追加了新一轮 ——
//! 要么重复跑一遍，要么在环境还没起来时把消息丢掉。正确顺序是：
//! 建任务 → 轮询任务详情直到 `processing` / `finished` → 连 `mode=attach`
//! 回放首轮输出并接实时结果，连接后**只**设置 auto-approve。
//!
//! ── 为什么必须等环境就绪 ────────────────────────────────────
//! `attach` 会回放连接前的输出，但环境还没创建完成时上游回放不到任何东西；
//! 轮询同时负责把「镜像准备失败」这种上游错误尽早变成可读的网关错误，
//! 而不是让客户端对着一条空流等到超时。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::time::Duration;

use serde_json::Value;

use crate::server::config;
use crate::server::core::auth_http;
use crate::server::core::egress;
use crate::server::core::proxies::ResolvedProxy;
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::client;
use super::credentials::MonkeyCodeCredentials;
use super::endpoints;
use super::region::Region;

/// 单次状态查询的超时上限（总时长由 headers 超时决定）。
const QUERY_TIMEOUT_MS: u64 = 20_000;
/// 轮询间隔。状态有变化时才打 verbose，避免刷屏。
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// 任务详情 → 就绪判定（**纯函数**，不碰网络，可离线测）。
enum Readiness {
    /// `processing` / `finished`：可以连 attach 回放首轮。
    Ready,
    Pending { progress: String },
    /// 环境准备失败 / 状态无法识别：尽早变成可读错误，不要空等到超时。
    Broken(String),
}

fn readiness(task: &Value) -> Readiness {
    let status = task.get("status").and_then(Value::as_str).unwrap_or("");
    let condition = task
        .pointer("/virtualmachine/conditions")
        .and_then(Value::as_array)
        .and_then(|items| items.last());
    let condition_type = condition
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let detail: String = condition
        .and_then(|item| item.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .take(300)
        .collect();
    if status == "error" || condition_type.eq_ignore_ascii_case("failed") {
        return Readiness::Broken(if detail.is_empty() {
            "请在官网查看任务详情".to_string()
        } else {
            detail
        });
    }
    match status {
        // 已完成的极短任务仍要 attach 回放结果，不能再次发起一轮。
        "processing" | "finished" => Readiness::Ready,
        "pending" => Readiness::Pending {
            progress: format!("{status}/{condition_type}"),
        },
        _ => Readiness::Broken("MonkeyCode 返回了无法识别的任务状态，已停止等待".to_string()),
    }
}

/// 轮询任务详情，直到环境就绪可以 attach。
pub(super) async fn wait_ready(
    region: Region,
    credentials: &MonkeyCodeCredentials,
    task_id: &str,
    proxy: Option<&ResolvedProxy>,
) -> Result<(), GatewayError> {
    let total_ms = config::timeout_settings()
        .headers_ms()
        .min(60 * 60 * 1000);
    let timeout = Duration::from_millis(total_ms);
    let deadline = tokio::time::Instant::now() + timeout;
    let url = format!("{}/{}", endpoints::task_create_url(region), task_id);
    let headers = endpoints::authed_headers(region, &credentials.session);
    let mut previous = String::new();
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(GatewayError::with_status(
                504,
                format!(
                    "MonkeyCode 任务环境在 {} 秒内未准备完成，已停止等待；请在官网确认镜像和环境状态",
                    timeout.as_secs()
                ),
            ));
        }
        let remaining = deadline
            .saturating_duration_since(now)
            .as_millis()
            .min(QUERY_TIMEOUT_MS as u128) as u64;
        let response = auth_http::send_raw("GET", &url, None, &headers, proxy, Some(remaining))
            .await
            .map_err(|error| {
                GatewayError::with_status(
                    if error.is_timeout() { 504 } else { 502 },
                    format!(
                        "MonkeyCode 查询任务环境失败：{}",
                        egress::describe_error_detail(&error)
                    ),
                )
            })?;
        let payload = response.payload.unwrap_or(Value::Null);
        if client::is_auth_failure(response.status, &payload) {
            return Err(GatewayError::with_status(
                401,
                "MonkeyCode 登录态失效，请重新登录",
            ));
        }
        if !response.ok || client::business_code(&payload) != Some(client::SUCCESS_CODE) {
            let detail = client::upstream_message(&payload);
            return Err(GatewayError::with_status(
                502,
                format!(
                    "MonkeyCode 查询任务环境失败（HTTP {}）：{}",
                    response.status,
                    if detail.is_empty() {
                        "上游未返回有效任务状态"
                    } else {
                        detail.as_str()
                    }
                ),
            ));
        }
        let task = payload.get("data").unwrap_or(&payload);
        match readiness(task) {
            Readiness::Ready => return Ok(()),
            Readiness::Pending { progress } => {
                if progress != previous {
                    logging::verbose(
                        "[MonkeyCode]",
                        &format!("任务 {task_id} 等待环境就绪（{progress}）"),
                    );
                    previous = progress;
                }
            }
            Readiness::Broken(message) => {
                return Err(GatewayError::with_status(
                    502,
                    format!("MonkeyCode 任务环境准备失败：{message}"),
                ))
            }
        }
        tokio::time::sleep(POLL_INTERVAL.min(
            deadline.saturating_duration_since(tokio::time::Instant::now()),
        ))
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn processing_and_finished_are_both_attachable() {
        assert!(matches!(readiness(&json!({"status": "processing"})), Readiness::Ready));
        assert!(matches!(readiness(&json!({"status": "finished"})), Readiness::Ready));
    }

    #[test]
    fn pending_keeps_waiting() {
        let state = readiness(&json!({
            "status": "pending",
            "virtualmachine": {"conditions": [{"type": "Initializing"}]}
        }));
        assert!(matches!(state, Readiness::Pending { .. }));
    }

    #[test]
    fn environment_failures_are_surfaced_instead_of_hanging() {
        let broken = readiness(&json!({
            "status": "error",
            "virtualmachine": {"conditions": [{"type": "Failed", "message": "镜像不可用"}]}
        }));
        assert!(matches!(broken, Readiness::Broken(ref m) if m.contains("镜像不可用")));
        assert!(matches!(readiness(&json!({"status": "weird"})), Readiness::Broken(_)));
        assert!(matches!(
            readiness(&json!({
                "status": "pending",
                "virtualmachine": {"conditions": [{"type": "Failed"}]}
            })),
            Readiness::Broken(_)
        ));
    }
}
