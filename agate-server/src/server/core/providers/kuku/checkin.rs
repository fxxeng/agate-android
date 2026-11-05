//! KukuAI 每日签到：免费领积分活动（freepoint）的每日任务领取。
//!
//! ── 上游是什么（2026-10-08 真实凭证实测 + kuku2api 同款）────────
//! ```text
//! GET  /api/genflowpro/freepoint/homenew?{WEB_QUERY}
//!      → {errno, data:{activities:[{activity_key, activity_name, period_no,
//!         tabs:[{tasks:[{task_type, task_name, task_status, single_reward_point,
//!         claimable_point}]}]}]}}
//! POST /api/genflowpro/freepoint/taskComplete?{WEB_QUERY}
//!      body: task_type=LOGIN|CHAT        （x-www-form-urlencoded）
//!      → {errno, data:{complete_status, reward_point}}
//! ```
//! 实测要点：
//!   - `homenew` 的任务状态 `FINISHED` / `UNFINISHED`，`claimable_point` 恒 0
//!     —— **不能拿它当领取门槛**（「完成一次对话」UNFINISHED 照样领到 +50）；
//!   - `taskComplete` **幂等**：重复领返回 `SUCCESS` + `reward_point=0`（或
//!     kuku2api 记录的 `BADGE_ALREADY_RECEIVED`），因此「已领过」不算失败；
//!   - 每日可自助完成的是 `LOGIN`（每日登录）与 `CHAT`（完成一次对话）；
//!     `INVITE_VISIT` / `INVITE_DOWNLOAD` 要真实受邀人，接口上强领只会失败，
//!     不进领取清单（kuku2api 同款取舍）。
//!
//! ── 会话前提 ────────────────────────────────────────────────
//! freepoint 是业务接口，与 userreport 同一道会话门：调 `with_business_stoken`
//! 先换发 genflowpro STOKEN（`engine.rs`），老账号 / 通行证级 STOKEN 也能签。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::credentials;
use super::http;
use super::{BASE_URL, WEB_QUERY};

/// 每日可自助完成的任务（类型，展示名）。展示名用于结果文案。
const DAILY_TASKS: [(&str, &str); 2] = [("LOGIN", "每日登录"), ("CHAT", "完成一次对话")];

/// KukuAI 账号的每日签到（自动签到框架的 claim）。
///
/// 返回 `{success, alreadyCompleted, outcome, msg}` 四态（见 [`summarize`]）：
/// `claimed` / `done` / `inactive` / `failed`。`billing::checkin` 的汇总认
/// `success` + `alreadyCompleted`，其余两个键是给排障与后续判据用的附加信息。
pub async fn claim_daily_checkin(
    store: &AccountStore,
    account_id: &str,
) -> Result<Value, GatewayError> {
    let credentials = credentials::snapshot_for(store, account_id)?;
    let prepared = credentials::with_business_stoken(&credentials).await;
    let headers = credentials::request_headers(&prepared);

    // 1) 任务面板：先探一次会话（freepoint 与 userreport 同一道门，-6 就是
    //    登录态问题），顺带确认活动面板可读。
    let panel_url = format!("{BASE_URL}/api/genflowpro/freepoint/homenew?{WEB_QUERY}");
    let panel = http::get_json_value(&panel_url, &headers, None, Some(30_000)).await?;
    let errno = panel.get("errno").and_then(Value::as_i64).unwrap_or(-1);
    if errno != 0 {
        let message = panel
            .get("show_msg")
            .or_else(|| panel.get("errmsg"))
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        return Err(GatewayError::with_status(
            401,
            format!("KukuAI 签到失败（freepoint errno={errno}：{message}）——请重新登录该账号"),
        ));
    }

    // 2) 逐个领取。单个任务失败只记日志继续（一个任务被风控不该拖垮另
    //    一个），**但失败必须被记住** —— 以前一律归成「今天已领过」并恒回
    //    success:true，自动签到会错误落 `checkinAt`，白丢一天。
    let mut results: Vec<TaskResult> = Vec::new();
    for (task_type, task_label) in DAILY_TASKS {
        let url = format!("{BASE_URL}/api/genflowpro/freepoint/taskComplete?{WEB_QUERY}");
        let value = match http::post_form_value(
            &url,
            &headers,
            &[("task_type", task_type)],
            None,
            Some(30_000),
        )
        .await
        {
            Ok(value) => value,
            Err(error) => {
                logging::log(
                    "[Checkin]",
                    &format!("⚠️ KukuAI「{task_label}」领取失败：{}", error.message),
                );
                results.push(TaskResult::Failure {
                    label: task_label.to_string(),
                    detail: error.message.clone(),
                });
                continue;
            }
        };
        let errno = value.get("errno").and_then(Value::as_i64).unwrap_or(-1);
        if errno != 0 {
            let message = value
                .get("show_msg")
                .or_else(|| value.get("errmsg"))
                .and_then(Value::as_str)
                .unwrap_or("未知错误");
            logging::log(
                "[Checkin]",
                &format!("⚠️ KukuAI「{task_label}」领取失败（errno={errno}：{message}）"),
            );
            results.push(TaskResult::Failure {
                label: task_label.to_string(),
                detail: format!("errno={errno}：{message}"),
            });
            continue;
        }
        let data = value.get("data").cloned().unwrap_or(Value::Null);
        let status = data
            .get("complete_status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // `reward_point` 缺失与 0 是两件事：前者说明上游没给结论，不能当成
        // 「重复领取」的幂等回执。保留 Option，判定交给 [`summarize`]。
        let reward = data.get("reward_point").and_then(Value::as_i64);
        if reward.is_some_and(|reward| reward > 0) && status.eq_ignore_ascii_case("SUCCESS") {
            results.push(TaskResult::Claimed {
                label: task_label.to_string(),
                reward: reward.unwrap_or(0),
            });
        } else if (status.eq_ignore_ascii_case("SUCCESS") && reward == Some(0))
            || status.eq_ignore_ascii_case("BADGE_ALREADY_RECEIVED")
        {
            results.push(TaskResult::Already {
                label: task_label.to_string(),
            });
        } else {
            results.push(TaskResult::Unresolved {
                label: task_label.to_string(),
                status,
            });
        }
    }
    Ok(summarize(&results))
}

/// 一个每日任务的结果。四类分开，避免把「失败 / 上游没给结论」压成
/// 「今天已经领过」。
enum TaskResult {
    Claimed { label: String, reward: i64 },
    Already { label: String },
    Unresolved { label: String, status: String },
    Failure { label: String, detail: String },
}

/// 把逐个任务的结果归并成签到结论（四态，对齐上游 3.4.1）。
///
/// ── 为什么 `success` 只在「真的领到」时为真 ──────────────────
/// `billing::checkin::checkin_completed_today` 见 `success:true` 或
/// `alreadyCompleted:true` 就落当天的 `checkinAt` 并把按钮置灰。恒回 true
/// 等于「什么都没发生也记成已完成」，定时链整天不再重试。
///
/// `inactive` 的文案故意带「暂无可领取活动」：`claim_pending` 靠这几个字
/// 判断「等窗口再试」，否则活动尚未下发会被记成终态失败。
fn summarize(results: &[TaskResult]) -> Value {
    let mut newly: Vec<String> = Vec::new();
    let mut total = 0i64;
    let mut already_labels: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();
    for result in results {
        match result {
            TaskResult::Claimed { label, reward } => {
                newly.push(format!("{label}+{reward}"));
                total = total.saturating_add(*reward);
            }
            // 标签要留着：结论里写「已领过」而说不出**哪几个**，用户看到
            // 一个灰掉的按钮却没有任何依据（与 `Unresolved` / `Failure` 同口径：
            // 只报结论、不报依据的消息在本项目里一律算没说清）
            TaskResult::Already { label } => already_labels.push(label.clone()),
            TaskResult::Unresolved { label, status } => {
                unresolved.push(format!("{label}：上游未确认完成（{status}）"));
            }
            TaskResult::Failure { label, detail } => {
                failures.push(format!("{label}（{detail}）"));
            }
        }
    }

    let already_claimed = !already_labels.is_empty();
    let success = !newly.is_empty();
    let done = !success && already_claimed;
    let mut msg = if success {
        format!("签到完成：{}（共 +{total}）", newly.join("、"))
    } else if done {
        format!(
            "今日已领过：{}（任务接口幂等，无新增）",
            already_labels.join("、")
        )
    } else if !failures.is_empty() {
        format!("每日任务领取失败：{}", failures.join("；"))
    } else {
        "暂无可领取活动：本次没有已确认完成的每日任务".to_string()
    };
    if (success || done) && !failures.is_empty() {
        msg.push_str(&format!("；部分任务失败：{}", failures.join("；")));
    }
    if !unresolved.is_empty() && !(success || done) {
        msg.push_str(&format!("；{}", unresolved.join("；")));
    }
    logging::log(
        "[Checkin]",
        &format!("{} {msg}", if success || done { "✅" } else { "⚠️" }),
    );
    json!({
        "success": success,
        "alreadyCompleted": done,
        "outcome": if success { "claimed" }
            else if done { "done" }
            else if failures.is_empty() { "inactive" }
            else { "failed" },
        "msg": msg,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claimed(reward: i64) -> TaskResult {
        TaskResult::Claimed { label: "每日登录".to_string(), reward }
    }

    #[test]
    fn a_real_claim_is_the_only_success() {
        let result = summarize(&[claimed(30)]);
        assert_eq!(result["success"], true);
        assert_eq!(result["outcome"], "claimed");
        assert!(result["msg"].as_str().unwrap_or("").contains("+30"));
    }

    /// 幂等回执（SUCCESS+0 / BADGE_ALREADY_RECEIVED）算「今天已领过」，
    /// 但不算本次领取成功 —— 两者都要让 `checkin_completed_today` 判已完成
    #[test]
    fn idempotent_replies_are_done_not_claimed() {
        let result = summarize(&[TaskResult::Already { label: "每日登录".to_string() }]);
        assert_eq!(result["success"], false);
        assert_eq!(result["alreadyCompleted"], true);
        assert_eq!(result["outcome"], "done");
        // 结论要说出**哪几个**任务已领过：只说「已领过」而不给依据，用户看到
        // 灰掉的按钮没法核实（`alreadyCompleted` 会让定时链整天不再重试）
        assert!(
            result["msg"].as_str().unwrap_or("").contains("每日登录"),
            "已领过的消息必须点名具体任务，拿到的是: {}",
            result["msg"]
        );
    }

    /// 核心缺陷回归：失败不再虚报成功，也不会被当成已领过
    #[test]
    fn failures_are_not_reported_as_completed() {
        let result = summarize(&[TaskResult::Failure {
            label: "每日登录".to_string(),
            detail: "errno=-6：登录态失效".to_string(),
        }]);
        assert_eq!(result["success"], false);
        assert_eq!(result["alreadyCompleted"], false);
        assert_eq!(result["outcome"], "failed");
        assert!(result["msg"].as_str().unwrap_or("").contains("登录态失效"));
    }

    /// 上游没给结论 / 活动未开始：不落已完成，且文案让定时链稍后重试
    #[test]
    fn inactive_keeps_the_retry_window_open() {
        let result = summarize(&[TaskResult::Unresolved {
            label: "每日登录".to_string(),
            status: "".to_string(),
        }]);
        assert_eq!(result["success"], false);
        assert_eq!(result["outcome"], "inactive");
        assert!(result["msg"].as_str().unwrap_or("").contains("暂无可领取活动"));
    }

    /// 部分成功仍是成功，但失败与未确认的明细不能丢
    #[test]
    fn partial_success_keeps_the_failures_visible() {
        let result = summarize(&[
            claimed(10),
            TaskResult::Failure {
                label: "完成一次对话".to_string(),
                detail: "errno=-1：风控".to_string(),
            },
        ]);
        assert_eq!(result["success"], true);
        assert!(result["msg"].as_str().unwrap_or("").contains("部分任务失败"));
    }

    /// 溢出不 panic（release 是 panic=abort）
    #[test]
    fn reward_sum_is_saturated() {
        let result = summarize(&[claimed(i64::MAX), claimed(1)]);
        assert_eq!(result["success"], true);
        assert!(result["msg"].as_str().unwrap_or("").contains(&format!("{}", i64::MAX)));
    }
}
