//! QuWork 权益：月度赠送列表（只读）+ 两种兑换（订单号 / 邀请码）。
//!
//! ── 三个端点（2026-11-14 用真实账号实测，不是照 README 猜的）──────
//! ```text
//!   GET  /api/user/benefit/monthly-gifts   → { data: { list: [ … ] } }
//!   POST /api/user/benefit/redeem          body { "order_id": <int64> }
//!   POST /api/user/activate                body { "invite_code": "…" }
//! ```
//! 实测回执（同一台机器、同一个账号）：
//! ```text
//!   redeem {}                        → Field validation for 'OrderId' failed on the 'required' tag
//!   redeem { order_id: "…" }         → cannot unmarshal string into … field .order_id of type int64
//!   redeem { order_id: 1234… }       → 订单不存在或不可兑换      ← 字段名与类型都对了
//!   activate {}                      → 邀请码不能为空
//!   activate { invite_code: "ZZZ" }  → 邀请码不存在              ← 字段名对了
//! ```
//! **订单号是数字（int64），不是字符串** —— 上游是 Go gin + validator，`order_id`
//! 走 `int64` 反序列化，粘成带引号的 JSON 会直接被拒（`cannot unmarshal string`）。
//!
//! ── 两条兑换路径的语义差别 ────────────────────────────────────
//!   · `order_id`：官网下的订单（16 位数字）换权益 → `/api/user/benefit/redeem`；
//!   · `invite_code`：邀请码（6 位左右字母数字）→ `/api/user/activate`。
//! 上游桌面端把两者放在同一张「权益兑换」表单里，靠输入形态分派；本仓照做，
//! 但**一次只发一条**（两个字段都填就 400，不猜用户想兑哪个）。
//!
//! ── 设备标识：读得到才带，实测不带也通 ─────────────────────────
//! 上游 `benefits.rs` 会读本机 `~/.qurwork-desktop-dsh/.user-device/device-identity.json`
//! 里的 64 位十六进制标识（读不到就提示「请到官方客户端完成权益兑换」）。
//! 实测这两个端点**带不带 `device_identifier` 结果完全一致**，所以本仓按
//! 「凭证里有就带上、没有就不带」处理 —— 将来上游对某些权益加严校验时不用改协议。
//!
//! ── 到账判据 ────────────────────────────────────────────────
//! 只认上游 `success` 字段（实测失败时确实回 `success:false` + 可读 `message`）。
//! 成功后再查一次 `/api/user/self` 把**当前**积分读回来（`pointsAfter`）给界面
//! 展示；读不到就留空，**不编造「已到账」**。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::errors::GatewayError;

use super::{client, endpoints, refresh, usage};

/// 月度赠送的一行（上游字段名逐个对上，见模块头）。
#[derive(Clone, Debug, PartialEq)]
pub struct MonthlyGift {
    /// 赠送周期（上游 `period`，如 `2026-11`；也有账号回 `periods` 数组，取字符串项）
    pub period: String,
    /// 赠送状态（上游 `gift_status`，如 `granted` / `pending`）
    pub status: String,
    /// 应到账时刻（上游 `expected_gift_date`，毫秒）
    pub expected_at: Option<i64>,
    /// 实际到账时刻（上游 `actual_gift_date`，毫秒）
    pub actual_at: Option<i64>,
    /// 其余标量字段（键值对，保序透传给界面 —— 上游加字段时这里自动跟上）
    pub extra: Vec<(String, Value)>,
}

/// 已知字段名 → 不再重复塞进 `extra`
const KNOWN_FIELDS: [&str; 8] = [
    "period",
    "periods",
    "gift_status",
    "status",
    "expected_gift_date",
    "actual_gift_date",
    "name",
    "plan_name",
];

/// 取第一个非空字符串（也认数字，转成字符串）
fn text_of(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| match value.get(*key) {
            Some(Value::String(text)) if !text.trim().is_empty() => Some(text.trim().to_string()),
            Some(Value::Number(number)) => Some(number.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

/// 解析一行月度赠送（**纯函数**，生产路径与单测共用）。
///
/// 宽容策略：字段缺了就是空 / None，不因此拒收整份列表 —— 上游对不同账号
/// 下发的字段并不完全一致（实测该账号 `list` 为空，字段形状取自上游实现）。
pub fn parse_gift(item: &Value) -> MonthlyGift {
    let mut extra = Vec::new();
    if let Some(object) = item.as_object() {
        for (key, value) in object {
            if KNOWN_FIELDS.contains(&key.as_str()) || value.is_object() || value.is_array() {
                continue;
            }
            extra.push((key.clone(), value.clone()));
        }
    }
    MonthlyGift {
        period: text_of(item, &["period", "periods"]),
        status: text_of(item, &["gift_status", "status"]),
        expected_at: client::timestamp(item.get("expected_gift_date")),
        actual_at: client::timestamp(item.get("actual_gift_date")),
        extra,
    }
}

/// 列表行 → 界面契约（`period` / `status` / `expectedAt` / `actualAt` / `extra`）。
fn gift_row(gift: &MonthlyGift) -> Value {
    json!({
        "period": gift.period,
        "status": gift.status,
        "expectedAt": gift.expected_at,
        "actualAt": gift.actual_at,
        "extra": gift.extra.iter()
            .map(|(key, value)| json!({ "key": key, "value": value }))
            .collect::<Vec<Value>>(),
    })
}

/// 月度赠送列表。读失败原样上抛（界面写「暂不可用」，不拿空表冒充）。
pub async fn monthly(store: &AccountStore, account_id: &str) -> Result<Value, GatewayError> {
    let data = refresh::protected(
        store,
        account_id,
        "GET",
        endpoints::MONTHLY_GIFTS_PATH,
        None,
    )
    .await?
    .data("月度赠送查询")?;
    // 实测是 `{ list: [] }`；`campaigns` 是上游另一处的字段名，这里一起认（宽容）
    let list = data
        .get("list")
        .or_else(|| data.get("campaigns"))
        .and_then(Value::as_array);
    let items: Vec<Value> = list
        .map(|list| list.iter().map(parse_gift).map(|gift| gift_row(&gift)).collect())
        .unwrap_or_default();
    Ok(json!({
        "available": true,
        "count": items.len(),
        "items": items,
    }))
}

/// 兑换入参（一次只允许一种）
#[derive(Clone, Debug, PartialEq)]
pub enum RedeemInput {
    /// 16 位订单号（上游 `order_id`，int64）
    Order(i64),
    /// 邀请码（上游 `invite_code`）
    Invite(String),
}

impl RedeemInput {
    /// 给界面看的种类名
    pub fn kind(&self) -> &'static str {
        match self {
            RedeemInput::Order(_) => "order",
            RedeemInput::Invite(_) => "invite",
        }
    }

    /// 打哪个端点
    fn path(&self) -> &'static str {
        match self {
            RedeemInput::Order(_) => endpoints::REDEEM_ORDER_PATH,
            RedeemInput::Invite(_) => endpoints::ACTIVATE_INVITE_PATH,
        }
    }

    /// 请求体（`device_identifier` 由调用方按凭证有无决定要不要塞）
    fn body(&self) -> Value {
        match self {
            RedeemInput::Order(order) => json!({ "order_id": order }),
            RedeemInput::Invite(code) => json!({ "invite_code": code }),
        }
    }
}

/// 订单号上限：16 位数字（上游提示「兑换码或 16 位订单号」）
const ORDER_MAX_DIGITS: usize = 16;
/// 邀请码上限（字符）
const INVITE_MAX_CHARS: usize = 64;

/// 解析订单号：数字或纯数字字符串，≤16 位，必须落在 i64 里。
fn parse_order(value: &Value) -> Result<Option<i64>, GatewayError> {
    let raw = match value {
        Value::Number(number) => {
            // 数字形态：先按 i64 取；取不到（小数 / 超界）就是格式错
            let Some(order) = number.as_i64() else {
                return Err(GatewayError::with_status(
                    400,
                    "QuWork 订单号应为整数，请检查是否粘成了小数或超出范围",
                ));
            };
            return Ok(Some(order));
        }
        Value::String(text) => text.trim().to_string(),
        Value::Null => return Ok(None),
        _ => {
            return Err(GatewayError::with_status(
                400,
                "QuWork 订单号应为数字或数字字符串",
            ))
        }
    };
    if raw.is_empty() {
        return Ok(None);
    }
    if raw.len() > ORDER_MAX_DIGITS || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(GatewayError::with_status(
            400,
            format!("QuWork 订单号应为不超过 {ORDER_MAX_DIGITS} 位的数字"),
        ));
    }
    raw.parse::<i64>().map(Some).map_err(|_| {
        GatewayError::with_status(400, "QuWork 订单号超出可表示范围，请核对后重试")
    })
}

/// 解析邀请码：非空 ASCII，无空白 / 控制字符。
fn parse_invite(value: &Value) -> Result<Option<String>, GatewayError> {
    let raw = match value {
        Value::String(text) => text.trim().to_string(),
        Value::Null => return Ok(None),
        _ => {
            return Err(GatewayError::with_status(
                400,
                "QuWork 邀请码应为字符串",
            ))
        }
    };
    if raw.is_empty() {
        return Ok(None);
    }
    if raw.chars().count() > INVITE_MAX_CHARS
        || !raw.is_ascii()
        || raw.bytes().any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(GatewayError::with_status(
            400,
            "QuWork 邀请码格式不正确（应为不含空白的字母数字）",
        ));
    }
    Ok(Some(raw))
}

/// 解析兑换请求体（**纯函数**）：`orderId`/`order_id` 与 `inviteCode`/`invite_code`/`code`。
///
/// 两个字段都给 → 400：不猜用户想兑哪一个（猜错就是替用户花掉权益）。
pub fn parse_redeem_input(body: &Value) -> Result<RedeemInput, GatewayError> {
    let order_value = body
        .get("orderId")
        .or_else(|| body.get("order_id"))
        .unwrap_or(&Value::Null);
    let invite_value = body
        .get("inviteCode")
        .or_else(|| body.get("invite_code"))
        .or_else(|| body.get("code"))
        .unwrap_or(&Value::Null);
    let order = parse_order(order_value)?;
    let invite = parse_invite(invite_value)?;
    match (order, invite) {
        (Some(_), Some(_)) => Err(GatewayError::with_status(
            400,
            "订单号与邀请码只能填一项：兑换码请只填邀请码，官网订单请只填订单号",
        )),
        (Some(order), None) => Ok(RedeemInput::Order(order)),
        (None, Some(invite)) => Ok(RedeemInput::Invite(invite)),
        (None, None) => Err(GatewayError::with_status(
            400,
            "请填写订单号（16 位数字）或邀请码",
        )),
    }
}

/// 兑换一次。成功只认上游 `success` 字段；成功后再读一次积分作为展示值。
pub async fn redeem(
    store: &AccountStore,
    account_id: &str,
    input: &RedeemInput,
) -> Result<Value, GatewayError> {
    // 设备标识：凭证里有就带上（实测可选，见模块头）
    let secret = refresh::snapshot(store, account_id).await?;
    let device = super::credentials::text(&secret, &["deviceIdentifier"]);
    let mut body = input.body();
    if !device.is_empty() {
        if let Some(object) = body.as_object_mut() {
            object.insert("device_identifier".to_string(), json!(device));
        }
    }
    let reply = refresh::protected(
        store,
        account_id,
        "POST",
        input.path(),
        Some(&body),
    )
    .await?;
    if !reply.ok() {
        let detail = client::message(&reply.body);
        let status = if reply.unauthorized() {
            401
        } else if (400..500).contains(&reply.status) {
            i32::from(reply.status)
        } else {
            502
        };
        return Err(GatewayError::with_status(
            status,
            if detail.is_empty() {
                "QuWork 兑换未成功".to_string()
            } else {
                format!("QuWork 兑换未成功：{detail}")
            },
        ));
    }
    // 上游确认成功：把当前积分读回来给界面（读不到就留空，不编造到账）
    let points_after = usage::query(store, account_id)
        .await
        .ok()
        .and_then(|usage| usage.get("available").cloned())
        .unwrap_or(Value::Null);
    let data = reply.body.get("data").cloned().unwrap_or(Value::Null);
    let message = match input {
        RedeemInput::Order(_) => "QuWork 已确认订单兑换成功",
        RedeemInput::Invite(_) => "QuWork 已确认邀请码激活成功",
    };
    Ok(json!({
        "ok": true,
        "kind": input.kind(),
        "message": message,
        "pointsAfter": points_after,
        "data": data,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gift_reads_upstream_fields_and_keeps_the_rest() {
        let gift = parse_gift(&json!({
            "period": "2026-11",
            "gift_status": "granted",
            "expected_gift_date": "2026-11-01T00:00:00Z",
            "actual_gift_date": 1_800_000_000_000_i64,
            "plan_name": "专业版包月",
            "amount": 1200,
        }));
        assert_eq!(gift.period, "2026-11");
        assert_eq!(gift.status, "granted");
        assert!(gift.expected_at.is_some());
        assert_eq!(gift.actual_at, Some(1_800_000_000_000));
        // plan_name 是已知字段（不进 extra），amount 进 extra
        assert!(gift.extra.iter().any(|(key, _)| key == "amount"));
        assert!(!gift.extra.iter().any(|(key, _)| key == "plan_name"));
    }

    #[test]
    fn parse_gift_tolerates_missing_fields() {
        let gift = parse_gift(&json!({}));
        assert!(gift.period.is_empty());
        assert!(gift.status.is_empty());
        assert_eq!(gift.expected_at, None);
        assert!(gift.extra.is_empty());
    }

    #[test]
    fn order_accepts_number_and_digits_but_rejects_the_rest() {
        assert_eq!(
            parse_redeem_input(&json!({ "orderId": 1234567890123456 }))
                .ok()
                .map(|input| input.kind()),
            Some("order")
        );
        assert_eq!(
            parse_redeem_input(&json!({ "order_id": "1234567890123456" }))
                .ok()
                .map(|input| input.kind()),
            Some("order")
        );
        // 带引号不是问题（本仓自己转成 int64 发），但超过 16 位 / 非数字要拒
        assert!(parse_redeem_input(&json!({ "order_id": "12345678901234567" })).is_err());
        assert!(parse_redeem_input(&json!({ "order_id": "12ab" })).is_err());
        assert!(parse_redeem_input(&json!({ "order_id": 1.5 })).is_err());
    }

    #[test]
    fn invite_accepts_text_and_rejects_whitespace() {
        assert_eq!(
            parse_redeem_input(&json!({ "inviteCode": "AB12CD" }))
                .ok()
                .map(|input| input.kind()),
            Some("invite")
        );
        assert!(parse_redeem_input(&json!({ "invite_code": "AB 12" })).is_err());
    }

    #[test]
    fn both_fields_at_once_is_refused() {
        let error = parse_redeem_input(&json!({
            "orderId": 1234567890123456,
            "inviteCode": "AB12CD",
        }))
        .err()
        .map(|error| error.message)
        .unwrap_or_default();
        assert!(error.contains("只能填一项"), "实际文案：{error}");
    }

    #[test]
    fn empty_body_says_what_to_fill() {
        assert!(parse_redeem_input(&json!({})).is_err());
    }

    #[test]
    fn request_bodies_match_the_measured_upstream_contract() {
        // 实测：order_id 必须是 JSON 数字（字符串会被上游 int64 反序列化拒掉）
        assert_eq!(
            RedeemInput::Order(1234567890123456).body(),
            json!({ "order_id": 1234567890123456_i64 })
        );
        assert_eq!(
            RedeemInput::Invite("AB12CD".to_string()).body(),
            json!({ "invite_code": "AB12CD" })
        );
        assert_eq!(RedeemInput::Order(1).path(), endpoints::REDEEM_ORDER_PATH);
        assert_eq!(
            RedeemInput::Invite("x".to_string()).path(),
            endpoints::ACTIVATE_INVITE_PATH
        );
    }
}
