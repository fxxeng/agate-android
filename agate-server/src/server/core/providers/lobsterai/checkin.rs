//! LobsterAI 的每日活动签到：**三步、幂等靠客户端**。
//!
//! ── 三步协议（一手来源：插件仓库 `lobsterai/checkin.rs`）──────────
//! ```text
//!   1. GET  /api/client-activities/slot?placement=…&clientVersion=…&…
//!        -> 找到「每日签到」那个活动位：slotState / activity.activityCode /
//!           activity.configRevision
//!   2. GET  /api/client-activities/{code}/context?configRevision=…
//!        -> 活动状态：state.claimedToday 与 actions[]（含不含 check_in）
//!   3. POST /api/client-activities/{code}/actions/check_in
//!        体 {configRevision, idempotencyKey:<uuid4>, payload:{}}
//! ```
//!
//! ── 幂等是**客户端**的事，所以两道前置检查都必须做 ──────────────
//! 上游没有「今天签过了就返回已签」的幂等语义（否则不需要 idempotencyKey）：
//! 幂等靠 ① 请求里那个 uuid4 的 `idempotencyKey`，② 第二步的两道预检
//! （`state.claimedToday` 与 `actions` 里有没有 `check_in`）。**两道都要**：
//! 只看 `claimedToday` 会漏掉「活动在、但今天不可签」那一格。预检命中时如实回
//! 一句中性的「今天已签到」，**不**再去打签到接口 —— 那会在上游不幂等的情况下
//! 把一天的奖励签重。
//!
//! ── 发放额字段有**三段回退** ─────────────────────────────────
//! `data.result` 里的奖励名随活动 / 版本变化：`creditsGranted` → `rewardCredits`
//! → `credits`，取第一个能读到的。
//!
//! ── 返回形状 ────────────────────────────────────────────────
//! `{success, msg, …}` —— 交给 `billing::checkin::claim_result` 翻成签到结果行。
//! 「今天已签到」那句必须带「已签到」三字：`billing::checkin::checkin_completed_today`
//! 是按 msg 子串判「当日已办」的（那样调度与台账才不会把一次中性结果当成失败去反复重打）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::core::auth_http::send_raw;
use crate::server::errors::GatewayError;

use super::credentials::{from_record, LobsterCredentials};
use super::endpoints::{
    activity_checkin_url, activity_context_url, activity_slot_url, REQUEST_TIMEOUT_MS, USER_AGENT,
};
use super::oauth::unwrap_envelope;

/// 活动上下文里表示「今天可签」的动作名
pub const CHECK_IN_ACTION: &str = "check_in";

/// 活动位（第一步 `data` 里我们关心的三个字段）
#[derive(Clone, Debug, Default)]
struct ActivitySlot {
    /// `slotState`（`available` 才可能可签）
    slot_state: String,
    /// `activity.activityCode`
    activity_code: String,
    /// `activity.configRevision`（原样留着，写进第二步的 query 与第三步的 body）
    config_revision: Value,
}

/// 从活动位 `data` 里取三个字段
fn parse_slot(data: &Value) -> ActivitySlot {
    let activity = data.get("activity");
    ActivitySlot {
        slot_state: data
            .get("slotState")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string(),
        activity_code: activity
            .and_then(|value| value.get("activityCode"))
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string(),
        config_revision: activity
            .and_then(|value| value.get("configRevision"))
            .cloned()
            .unwrap_or(Value::Null),
    }
}

/// `configRevision` 渲染成 query 里那个数（整数不带小数尾巴；读不出数时给 `0`，不猜）
fn config_revision_string(value: &Value) -> String {
    if let Some(number) = value.as_i64() {
        return number.to_string();
    }
    if let Some(number) = value.as_f64() {
        if number.fract() == 0.0 {
            return (number as i64).to_string();
        }
        return format!("{number}");
    }
    if let Some(text) = value.as_str() {
        if let Ok(number) = text.trim().parse::<f64>() {
            if number.fract() == 0.0 {
                return (number as i64).to_string();
            }
            return format!("{number}");
        }
    }
    "0".to_string()
}

/// `configRevision` 作为 **JSON 数字**放进签到体（原样透传数字形态；字符串数字也
/// 折成数字，非数字给 0）
fn config_revision_json(value: &Value) -> Value {
    if value.as_f64().is_some() || value.as_i64().is_some() {
        return value.clone();
    }
    if let Some(text) = value.as_str() {
        if let Ok(number) = text.trim().parse::<f64>() {
            return Value::from(number);
        }
    }
    Value::from(0)
}

/// 今天是否已经签过（第一道预检）
fn claimed_today(context_data: &Value) -> bool {
    context_data
        .get("state")
        .and_then(|state| state.get("claimedToday"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// 活动上下文里的 `actions[]` 是否包含 `check_in`（第二道预检）
fn has_check_in_action(context_data: &Value) -> bool {
    context_data
        .get("actions")
        .and_then(Value::as_array)
        .map(|actions| {
            actions
                .iter()
                .filter_map(Value::as_str)
                .any(|action| action.trim() == CHECK_IN_ACTION)
        })
        .unwrap_or(false)
}

/// 从签到结果的 `data.result` 里取发放额（`creditsGranted` → `rewardCredits`
/// → `credits`，见模块头）。返回 `(amount, found)`。
fn credit_from_result(result: &Value) -> (f64, bool) {
    for key in ["creditsGranted", "rewardCredits", "credits"] {
        let Some(value) = result.get(key) else {
            continue;
        };
        let number = value
            .as_f64()
            .or_else(|| value.as_str().and_then(|text| text.trim().parse::<f64>().ok()));
        if let Some(number) = number {
            return (number, true);
        }
    }
    (0.0, false)
}

/// 带鉴权的 JSON 请求（非 JSON 响应给「凭据已失效」这类可读原因，而不是解析错误）
async fn request_json(
    method: &str,
    url: &str,
    credentials: &LobsterCredentials,
    body: Option<&Value>,
    what: &str,
) -> Result<Value, GatewayError> {
    let headers: Vec<(String, String)> = vec![
        (
            "authorization".to_string(),
            format!("Bearer {}", credentials.access_token),
        ),
        ("accept".to_string(), "application/json".to_string()),
        ("content-type".to_string(), "application/json".to_string()),
        ("user-agent".to_string(), USER_AGENT.to_string()),
    ];
    let response = send_raw(method, url, body, &headers, None, Some(REQUEST_TIMEOUT_MS))
        .await
        .map_err(|error| GatewayError::with_status(502, format!("LobsterAI {what}请求失败: {error}")))?;
    if response.status == 401 || response.status == 403 {
        return Err(GatewayError::with_status(
            401,
            format!("LobsterAI 凭据已失效（HTTP {}），请重新登录该账号", response.status),
        ));
    }
    if !(200..300).contains(&response.status) {
        return Err(GatewayError::with_status(
            502,
            format!("LobsterAI {what}返回 HTTP {}", response.status),
        ));
    }
    response
        .payload
        .ok_or_else(|| GatewayError::with_status(502, format!("LobsterAI {what}响应不是 JSON")))
}

/// 中性结果：活动不可签（**不带「已签到」字样** —— 那会被当成当日已办）
fn inactive_result(message: &str, activity_code: &str) -> Value {
    json!({
        "success": false,
        "msg": message,
        "activityCode": activity_code,
        "needsAction": true,
    })
}

/// 中性结果：今天已签到（带「已签到」字样 + `alreadyCompleted`：当日完成）
fn already_claimed_result(activity_code: &str) -> Value {
    json!({
        "success": false,
        "msg": "今天已签到",
        "activityCode": activity_code,
        "alreadyCompleted": true,
        "needsAction": false,
    })
}

/// 拿一个账号的凭证（读不到就报 401）
fn credentials_for(store: &AccountStore, account_id: &str) -> Result<LobsterCredentials, GatewayError> {
    from_record(store.lobsterai_account_record(account_id).as_ref())
}

/// 每日签到（三步）。返回 `{success, msg, …}`，交给 `billing::checkin::claim_result`。
///
/// ── 三种结局对应用户看到的三种话 ─────────────────────────────
///   · **今天已签**：`success:false` + `alreadyCompleted:true` + 「今天已签到」
///     （中性、不重签）；
///   · **不可签**：`success:false` + 中性文案（活动未下发 / `check_in` 不在
///     `actions` 里）；
///   · **真签上**：`success:true` + 发放积分（字段三段回退）。
/// 传输 / 信封层面的失败才返回 `Err`（由 `claim_result` 放进结果的 `error`）。
pub async fn claim_for_account(
    store: &AccountStore,
    account_id: &str,
) -> Result<Value, GatewayError> {
    let credentials = credentials_for(store, account_id)?;
    // 版本号是活动位 query 的必填参数（`clientVersion`）；`prime_client_version`
    // 解析一次并顺手把转发路径的同步版本头快照填上（解析失败回落兜底常量）。
    let version = super::chat::prime_client_version().await;

    // ── 第一步：活动位 ─────────────────────────────────────────
    let slot_payload = request_json(
        "GET",
        &activity_slot_url(&version),
        &credentials,
        None,
        "活动槽位查询",
    )
    .await?;
    let slot_data = unwrap_envelope(&slot_payload, "活动槽位查询")?;
    let slot = parse_slot(&slot_data);
    if slot.slot_state != "available" || slot.activity_code.is_empty() {
        let message = format!("今天没有可签到的活动（slotState={}）", slot.slot_state);
        crate::server::logging::verbose("[Checkin]", &format!("LobsterAI {account_id}: {message}"));
        return Ok(inactive_result(&message, &slot.activity_code));
    }

    // ── 第二步：两道预检（幂等靠客户端，缺一不可，见模块头）─────────
    let revision = config_revision_string(&slot.config_revision);
    let context_payload = request_json(
        "GET",
        &activity_context_url(&slot.activity_code, &revision),
        &credentials,
        None,
        "活动上下文查询",
    )
    .await?;
    let context_data = unwrap_envelope(&context_payload, "活动上下文查询")?;
    if claimed_today(&context_data) {
        crate::server::logging::verbose(
            "[Checkin]",
            &format!("LobsterAI {account_id}: 今天已签到（claimedToday）"),
        );
        return Ok(already_claimed_result(&slot.activity_code));
    }
    if !has_check_in_action(&context_data) {
        let message = "当前不可签到（活动未开放 check_in）".to_string();
        crate::server::logging::verbose("[Checkin]", &format!("LobsterAI {account_id}: {message}"));
        return Ok(inactive_result(&message, &slot.activity_code));
    }

    // ── 第三步：真签（幂等键是 uuid4，客户端生成）───────────────────
    let body = json!({
        "configRevision": config_revision_json(&slot.config_revision),
        "idempotencyKey": super::ids::uuid_v4(),
        "payload": {},
    });
    let checkin_payload = request_json(
        "POST",
        &activity_checkin_url(&slot.activity_code),
        &credentials,
        Some(&body),
        "签到",
    )
    .await?;
    let data = unwrap_envelope(&checkin_payload, "签到")?;
    let result = data.get("result").cloned().unwrap_or(Value::Null);
    let (credit, granted) = credit_from_result(&result);
    let delayed = result
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let message = if granted {
        format!("签到成功，获得 {credit} 积分")
    } else {
        "签到成功".to_string()
    };
    crate::server::logging::log(
        "[Checkin]",
        &format!("LobsterAI {account_id}: {message}（活动 {}）", slot.activity_code),
    );
    let mut outcome = json!({
        "success": true,
        "msg": message,
        "activityCode": slot.activity_code,
        "credit": credit,
        "creditGranted": granted,
        "needsAction": false,
    });
    if !delayed.is_empty() {
        if let Some(object) = outcome.as_object_mut() {
            object.insert("delayedMessage".to_string(), Value::String(delayed));
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 活动位解析：三个字段都取得到
    #[test]
    fn slot_parsed_from_data() {
        let slot = parse_slot(&json!({
            "slotState": "available",
            "activity": { "activityCode": "daily", "configRevision": 7 }
        }));
        assert_eq!(slot.slot_state, "available");
        assert_eq!(slot.activity_code, "daily");
        assert_eq!(config_revision_string(&slot.config_revision), "7");
    }

    /// 两道预检各自独立：claimedToday 与 actions 都要看
    #[test]
    fn both_prechecks_are_checked() {
        assert!(claimed_today(&json!({"state": {"claimedToday": true}})));
        assert!(!claimed_today(&json!({"state": {"claimedToday": false}})));
        assert!(has_check_in_action(&json!({"actions": ["sign_in", "check_in"]})));
        assert!(!has_check_in_action(&json!({"actions": ["sign_in"]})));
        // 活动在、但今天不可签：两支的结论必须不同（只看 claimedToday 会漏这格）
        let inactive = json!({"state": {"claimedToday": false}, "actions": ["sign_in"]});
        assert!(!claimed_today(&inactive) && !has_check_in_action(&inactive));
    }

    /// 发放额三段回退：creditsGranted → rewardCredits → credits
    #[test]
    fn credit_falls_back_through_three_keys() {
        assert_eq!(credit_from_result(&json!({"creditsGranted": 100})), (100.0, true));
        assert_eq!(credit_from_result(&json!({"rewardCredits": "50"})), (50.0, true));
        assert_eq!(credit_from_result(&json!({"credits": 3.5})), (3.5, true));
        assert_eq!(credit_from_result(&json!({})), (0.0, false));
    }

    /// 元数的渲染：整数不带小数尾巴，非数字给 0（不猜）
    #[test]
    fn config_revision_rendering() {
        assert_eq!(config_revision_string(&json!(7)), "7");
        assert_eq!(config_revision_string(&json!(7.0)), "7");
        assert_eq!(config_revision_string(&json!(7.5)), "7.5");
        assert_eq!(config_revision_string(&json!("9")), "9");
        assert_eq!(config_revision_string(&json!("junk")), "0");
        assert_eq!(config_revision_json(&json!(7)), json!(7));
        assert_eq!(config_revision_json(&json!("8")), json!(8.0));
    }

    /// 中性措辞：只有「已签到」那句带当日完成标记（会被台账判为「今日已办」）
    #[test]
    fn neutral_messages_are_distinct() {
        let claimed = already_claimed_result("daily");
        assert_eq!(claimed.get("msg").and_then(Value::as_str), Some("今天已签到"));
        assert_eq!(
            claimed.get("alreadyCompleted").and_then(Value::as_bool),
            Some(true)
        );
        let inactive = inactive_result("今天没有可签到的活动（slotState=empty）", "daily");
        assert_eq!(inactive.get("alreadyCompleted"), None);
        assert!(!inactive
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("已签到"));
    }

    /// 幂等键是 uuid4 形态（上游要求）
    #[test]
    fn idempotency_key_is_uuid4() {
        let key = super::super::ids::uuid_v4();
        assert_eq!(key.len(), 36);
        assert_eq!(key.split('-').nth(2).and_then(|part| part.chars().next()), Some('4'));
    }
}