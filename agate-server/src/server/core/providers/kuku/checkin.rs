//! KukuAI 每日签到：**主链路是「库库 30 天签到」**，旧 freepoint 任务仅作兜底。
//!
//! ── 30 天签到（2026-10-21 起，上游 3.4.2 移植）────────────────
//! ```text
//! GET  /act/api/conf?conf_key=kukuSignConf
//!      → data[].conf_value.{activityTid, signPoint}
//! GET  /act/v2/kukuai30/homepage?tid=…
//!      → {errno:0, data:{signed_today, continuous_days, total_days}}
//! GET  /operation/activitys/api/getJsToken
//!      → data.jsToken：URL 编码的 JS 片段，取最后一对引号里的 HEX
//! POST /act/v2/kukuai30/signin?tid=…&jsToken=…&aid=15459
//!      → {errno:0, data:{continuous, signed_today:true}}；已签 errno=10152
//! ```
//! 缺 `aid` 回 8001、缺 `jsToken` 回 8002。这一路只认**网页 Cookie**，
//! 不需要换发业务 STOKEN，所以放在 freepoint 之前；活动配置读不到 / 面板
//! 报「无活动」时才回落旧链路（部分账号的 freepoint 面板已是
//! `{activities: []}`，再打 taskComplete 只会拿到一组 FAIL 误报失败）。
//!
//! ── 旧 freepoint 兜底（2026-10-08 真实凭证实测 + kuku2api 同款）──
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
use super::{BASE_URL, USER_AGENT, WEB_QUERY};

/// 每日可自助完成的任务（类型，展示名）。展示名用于结果文案。
const DAILY_TASKS: [(&str, &str); 2] = [("LOGIN", "每日登录"), ("CHAT", "完成一次对话")];

// ── 30 天签到活动 ────────────────────────────────────────────
const ACTIVITY_CONF_URL: &str = "/act/api/conf?conf_key=kukuSignConf";
/// 风控接口按来源页校验，Referer 不能省。
const ACTIVITY_REFERER: &str = "https://kuku.baidu.com/operation/activitys/kukuPcSignAct/main";
/// 活动配置 `dataApp.app_key`；签到接口缺它回 8001。
const ACTIVITY_AID: i64 = 15459;
/// 配置里 `signPoint` 读不到时的兜底积分。
const ACTIVITY_SIGN_POINT: u32 = 10;

/// 30 天签到链路的四种结论。`Unavailable` 才交给旧 freepoint。
enum ThirtyDaySignin {
    Signed { continuous: u32, total: u32, point: u32 },
    Already { continuous: u32, total: u32 },
    Unavailable,
    Failed(String),
}

/// 30 天签到用的是**网页会话**，不是 freepoint 的业务 STOKEN。
fn activity_headers(credentials: &credentials::KukuCredentials) -> Vec<(String, String)> {
    vec![
        ("Cookie".to_string(), credentials.cookie_header()),
        ("User-Agent".to_string(), USER_AGENT.to_string()),
        ("Referer".to_string(), ACTIVITY_REFERER.to_string()),
        ("Origin".to_string(), BASE_URL.to_string()),
        ("Accept-Language".to_string(), "zh-CN,zh;q=0.9".to_string()),
    ]
}

/// 读活动配置 → 看今日状态 → 取风控 token → 调签到（上游 3.4.2 同款四步）。
async fn signin_thirty_days(credentials: &credentials::KukuCredentials) -> ThirtyDaySignin {
    let headers = activity_headers(credentials);
    let conf_url = format!("{BASE_URL}{ACTIVITY_CONF_URL}");
    let conf = match http::get_json_value(&conf_url, &headers, None, Some(30_000)).await {
        Ok(value) => value,
        Err(error) => {
            logging::log(
                "[Checkin]",
                &format!("KukuAI 30 天签到：活动配置读取失败（{}）", error.message),
            );
            return ThirtyDaySignin::Unavailable;
        }
    };
    let Some((tid, point)) = activity_config(&conf) else {
        return ThirtyDaySignin::Unavailable;
    };

    let home_url = format!("{BASE_URL}/act/v2/kukuai30/homepage?tid={tid}");
    let home = match http::get_json_value(&home_url, &headers, None, Some(30_000)).await {
        Ok(value) => value,
        Err(error) => {
            logging::log(
                "[Checkin]",
                &format!("KukuAI 30 天签到：活动面板读取失败（{}）", error.message),
            );
            return ThirtyDaySignin::Unavailable;
        }
    };
    if home.get("errno").and_then(Value::as_i64).unwrap_or(-1) != 0 {
        // 151004 / -6 都是上游的「无活动」；其它错误也按不可用交给旧链路。
        return ThirtyDaySignin::Unavailable;
    }
    let data = home.get("data").cloned().unwrap_or(Value::Null);
    let continuous = data
        .get("continuous_days")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let total = data
        .get("total_days")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    if data
        .get("signed_today")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return ThirtyDaySignin::Already { continuous, total };
    }

    let token_url = format!("{BASE_URL}/operation/activitys/api/getJsToken");
    let token_value = match http::get_json_value(&token_url, &headers, None, Some(30_000)).await {
        Ok(value) => value,
        Err(error) => {
            return ThirtyDaySignin::Failed(format!("签到凭证获取失败：{}", error.message))
        }
    };
    let Some(token) = token_value
        .get("data")
        .and_then(|data| data.get("jsToken"))
        .and_then(Value::as_str)
        .and_then(extract_js_token)
    else {
        return ThirtyDaySignin::Failed("签到凭证为空（jsToken 解析失败）".to_string());
    };

    let sign_url =
        format!("{BASE_URL}/act/v2/kukuai30/signin?tid={tid}&jsToken={token}&aid={ACTIVITY_AID}");
    let signed = match http::post_form_value(&sign_url, &headers, &[], None, Some(30_000)).await {
        Ok(value) => value,
        Err(error) => return ThirtyDaySignin::Failed(format!("签到请求失败：{}", error.message)),
    };
    let errno = signed.get("errno").and_then(Value::as_i64).unwrap_or(-1);
    if errno == 0 {
        let signed_data = signed.get("data").cloned().unwrap_or(Value::Null);
        let continuous = signed_data
            .get("continuous")
            .and_then(Value::as_u64)
            .unwrap_or(continuous as u64) as u32;
        let total = total.max(continuous);
        if signed_data
            .get("signed_today")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            return ThirtyDaySignin::Signed { continuous, total, point };
        }
        return ThirtyDaySignin::Failed("上游未确认签到完成".to_string());
    }
    if errno == 10152 {
        return ThirtyDaySignin::Already { continuous, total };
    }
    let message = signed
        .get("show_msg")
        .or_else(|| signed.get("errmsg"))
        .and_then(Value::as_str)
        .unwrap_or("未知错误");
    if errno == 8001 || errno == 8002 {
        return ThirtyDaySignin::Failed(format!(
            "上游风控未通过（errno={errno}：{message}），请重新登录该账号后再试"
        ));
    }
    ThirtyDaySignin::Failed(format!("errno={errno}：{message}"))
}

/// 从活动配置里取 `activityTid` 与每日积分（两种数字/字符串形态都认）。
fn activity_config(value: &Value) -> Option<(String, u32)> {
    let rows = value.get("data")?.as_array()?;
    for row in rows {
        let Some(config) = row.get("conf_value") else { continue };
        let Some(tid) = config.get("activityTid").and_then(|value| {
            value
                .as_str()
                .map(str::to_string)
                .or_else(|| value.as_i64().map(|number| number.to_string()))
        }) else {
            continue;
        };
        if tid.is_empty() {
            continue;
        }
        let point = config
            .get("signPoint")
            .and_then(|value| {
                value
                    .as_str()
                    .and_then(|text| text.parse::<u32>().ok())
                    .or_else(|| value.as_u64().map(|number| number as u32))
            })
            .unwrap_or(ACTIVITY_SIGN_POINT);
        return Some((tid, point));
    }
    None
}

/// 风控 token 是 URL 编码的 JS 片段：`function fn(a){...};fn("HEX")`。
/// 取最后一对引号里的值，**不执行任何 JS**。
fn extract_js_token(raw: &str) -> Option<String> {
    let decoded = percent_decode(raw);
    let end = decoded.rfind('"')?;
    let start = decoded[..end].rfind('"')?;
    let token = &decoded[start + 1..end];
    (!token.is_empty()).then(|| token.to_string())
}

/// 只解码 `%XX`（风控 token 片段只用到这一层编码）。
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                out.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

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

    // 0) 30 天签到（主链路）：只用网页 Cookie，不必先换发业务 STOKEN。
    //    只有「活动不可用」才继续走旧的 freepoint 任务。
    match signin_thirty_days(&credentials).await {
        ThirtyDaySignin::Signed { continuous, total, point } => {
            let msg = format!(
                "库库 30 天签到成功：+{point} 积分（连续 {continuous} 天，累计 {total} 天）"
            );
            logging::log("[Checkin]", &msg);
            return Ok(json!({
                "success": true,
                "alreadyCompleted": false,
                "outcome": "claimed",
                "credit": format!("+{point} 积分"),
                "streak": continuous,
                "msg": msg,
            }));
        }
        ThirtyDaySignin::Already { continuous, total } => {
            let msg = format!("今日已签到（连续 {continuous} 天，累计 {total} 天）");
            logging::log("[Checkin]", &format!("KukuAI {msg}"));
            return Ok(json!({
                "success": false,
                "alreadyCompleted": true,
                "outcome": "done",
                "streak": continuous,
                "msg": msg,
            }));
        }
        ThirtyDaySignin::Failed(message) => {
            logging::log(
                "[Checkin]",
                &format!("⚠️ KukuAI 30 天签到失败：{message}"),
            );
            return Ok(json!({
                "success": false,
                "alreadyCompleted": false,
                "outcome": "failed",
                "msg": format!("库库 30 天签到失败：{message}"),
            }));
        }
        ThirtyDaySignin::Unavailable => {}
    }

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
    // 旧活动已结束 / 该账号没有活动时面板是 `{activities: []}`。这时再打
    // taskComplete 只会拿到一组 FAIL，把「没活动」误报成「签到失败」。
    // 文案沿用 `summarize` 的「暂无可领取活动」，`claim_pending` 靠这几个字
    // 留着重试窗口，否则活动尚未下发会被记成终态失败。
    let activity_count = panel
        .get("data")
        .and_then(|data| data.get("activities"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    if activity_count == 0 {
        let msg = "暂无可领取活动：当前没有可参加的「免费领积分」活动（活动可能已结束）";
        logging::log("[Checkin]", &format!("KukuAI {msg}"));
        return Ok(json!({
            "success": false,
            "alreadyCompleted": false,
            "outcome": "inactive",
            "msg": msg,
        }));
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

    /// 活动配置的 tid / 积分两种形态都要认；没有配置项时回落旧链路。
    #[test]
    fn activity_config_reads_tid_and_point_in_both_shapes() {
        let value = json!({"data":[
            {"conf_value":{"foo":"bar"}},
            {"conf_value":{"activityTid":"tid-9","signPoint":"25"}}
        ]});
        assert_eq!(activity_config(&value), Some(("tid-9".to_string(), 25)));
        let numeric = json!({"data":[{"conf_value":{"activityTid":15459,"signPoint":10}}]});
        assert_eq!(activity_config(&numeric), Some(("15459".to_string(), 10)));
        assert_eq!(activity_config(&json!({"data":[]})), None);
        assert_eq!(activity_config(&json!({})), None);
    }

    /// 风控 token 是 JS 片段，只取引号里的 HEX，绝不执行脚本。
    #[test]
    fn js_token_is_read_without_executing_js() {
        assert_eq!(
            extract_js_token("function%20fn(a)%7B%7D%3Bfn(%22A1B2C3%22)"),
            Some("A1B2C3".to_string())
        );
        assert_eq!(extract_js_token("fn(\"\")"), None);
        assert_eq!(extract_js_token("no quotes"), None);
    }

    #[test]
    fn percent_decode_only_handles_hex_escapes() {
        assert_eq!(percent_decode("%41%42%7a"), "ABz");
        assert_eq!(percent_decode("a%2G%"), "a%2G%");
        assert_eq!(percent_decode("plain"), "plain");
    }

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
