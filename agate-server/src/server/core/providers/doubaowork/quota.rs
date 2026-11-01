//! 豆包工作的额度读数：**订阅窗口 + 奖励额度**（`subscription/overview` + `entry/config`）。
//!
//! ── 上游的额度形态（[二进制] 字段名逐字）─────────────────────────
//! 两条接口各出一半，合起来才是完整读数：
//! ```text
//!   GET /alice/commerce/sale/subscription/overview/
//!        membership_display_name
//!        /current_subscription/display/short_name
//!        /current_subscription/display/product_name
//!        /subscription/display/short_name
//!        window_limit_groups   usage_exhausted
//!        total_amount  used_amount
//!   GET /alice/commerce/sale/subscription/entry/config
//!        window_limit_section  enterprise_window_limit_section
//!        member_info  hasActiveSubscription  hasEnterpriseSubscription
//!        quota_package_section  incentive_quota_section
//!        feature_group_name  unlimited  exemption  active
//!        bonus  view  remainingPercent  availableView  windows  planName
//!        window  resetsAt  end_time
//! ```
//! 读法（本模块最重要的一段）：
//!   - **窗口**（`windows` / `window_limit_groups`）是主读数：本家的额度是
//!     「每窗口多少、已用多少」，`total_amount` / `used_amount` 是它的原始形态，
//!     `remainingPercent` / `availableView` 是上游**自己算好给界面看的**；
//!   - `unlimited` / `exemption` 是**无限档**标记（会员里有一类是「不限量但限速」）；
//!   - `bonus` / `incentive_quota_section` 是**奖励额度**（与签到链相关，
//!     见 [`super::rewards`]）—— 它与订阅窗口是两笔账，所以单列一份 wallet；
//!   - `hasActiveSubscription` / `hasEnterpriseSubscription` 说明有没有订阅
//!     （`false` 时窗口读数可能是「赠送额度」）。
//!
//! ── 归一形状（本仓的统一契约，见 `ProviderAdapter::query_usage` 的文档）──
//! ```jsonc
//! {
//!   "available": 1234.5,          // 可用总量（判不出就**不写**这个键）
//!   "unit": "次",                  // 展示单位
//!   "availableView": "…",          // 上游自己的紧凑展示串（有就带上，前端按形状显示）
//!   "wallets": [ {...}, {...} ],   // 逐窗口 / 逐奖励的明细
//!   "subscription": { planName, status, … },
//!   "raw": { overview: …, config: … }
//! }
//! ```
//!
//! ⚠️ **一条必须遵守的契约：读不到额度就不要写 `available`** ────────
//! 限额判定走 `core::usage_records::extract_remaining`，它的规则是
//! 「`unlimited` → 判不出；否则看 `totalLeft`；否则看 `available`」——
//! 也就是说 **`available` 一旦出现就会被当成真实读数**。把「这次我没读懂」
//! 写成 `available: 0`，账号会当场被标成「余额不足 / 已跳过」（用户看到的现象
//! 是「我刚充了钱怎么还是余额不足」）。**没有窗口 ≠ 余额为 0**：前者是
//! 「这份响应我没读懂」，后者是「确实用完了」。因此本模块只在**确实解析出窗口
//! 或奖励**时才写 `available`（这一条与 AtomCode 的 `shape_usage` 同款，
//! 那家在这上面真的踩过一次）。
//!
//! ── 额度接口对 Cookie 无效的处置（本家特有）──────────────────────
//! 实测口径（写下来是因为它会误导排查）：额度接口在 Cookie 失效时**不一定**
//! 回 401 —— 它可能回 `HTTP 200` + 空读数（那时 [`shape_usage`] 会正常返回
//! 「读不到额度」，而 `available` 键缺席，符合上面那条契约）。因此：
//!   - `401` / `403` **原样透出**（调用方据此走「刷新后重试一次」的既有处置）；
//!   - `200 + 空读数` **不算失败**，但也**不写 `available`** ——
//!     如实回「这次读不出来」，而不是「余额 0」。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Map, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::core::auth_http::send_raw;
use crate::server::errors::GatewayError;

use super::credentials::from_record;
use super::endpoints;

/// 额度查询的超时（毫秒）。
///
/// ⚠️ 与别家同一条纪律：**必须自己设总超时**。`egress` 的默认 read_timeout 是
/// 600 秒（那是给 SSE 长连接用的），不设总超时会让一个挂住的额度查询把前端的
/// 转圈转到天荒地老。
const QUOTA_TIMEOUT_MS: u64 = 20_000;

/// 一次额度查询的原始两段响应
#[derive(Clone, Debug, Default)]
pub struct RawQuota {
    /// `subscription/overview` 的响应（失败时为 `Null`）
    pub overview: Value,
    /// `entry/config` 的响应（失败时为 `Null`）
    pub config: Value,
}

/// 一个窗口读数
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    /// 窗口名（上游的 `feature_group_name` / `planName`，可能为空）
    pub name: String,
    /// 上限（`total_amount` / `window.limit`；判不出 = 0）
    pub total: f64,
    /// 已用（`used_amount` / `window.used`；判不出 = 0）
    pub used: f64,
    /// 重置时刻（上游给的 `resetsAt` / `end_time`，原文形态；可能为空）
    pub resets_at: String,
    /// 本窗口是否已耗尽（`usage_exhausted`）
    pub exhausted: bool,
    /// 是否**不限量**（`unlimited` / `exemption`）—— 这类窗口没有「剩余」可算
    pub unlimited: bool,
}

/// 从一份响应里读一个实数（数字 / 数字字符串都认；读不出给 `None`）
fn number_of(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    if let Some(number) = value.as_f64() {
        return Some(number);
    }
    value
        .as_str()
        .and_then(|text| text.trim().parse::<f64>().ok())
}

/// 从对象的多个候选键里读第一个能解析成数字的值
fn number_by_keys(object: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| number_of(object.get(*key)))
}

/// 从对象的多个候选键里读第一个非空字符串
fn text_by_keys(object: &Value, keys: &[&str]) -> String {
    for key in keys {
        let text = object
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if !text.is_empty() {
            return text.to_string();
        }
    }
    String::new()
}

/// 从一个窗口对象里读一个读数（键名链覆盖两套接口的形态）
fn window_from_row(row: &Value) -> Option<Window> {
    if !row.is_object() {
        return None;
    }
    // `unlimited` / `exemption` 的取值可能是布尔也可能是 1/0（两套接口都见过）
    let flag = |keys: &[&str]| -> bool {
        keys.iter().any(|key| {
            row.get(*key).is_some_and(|value| match value {
                Value::Bool(flag) => *flag,
                Value::Number(number) => number.as_i64().unwrap_or(0) != 0,
                Value::String(text) => {
                    let lowered = text.trim().to_ascii_lowercase();
                    lowered == "1" || lowered == "true" || lowered == "unlimited"
                }
                _ => false,
            })
        })
    };
    let window = row.get("window").filter(|value| value.is_object()).unwrap_or(row);
    let total = number_by_keys(window, &["total_amount", "totalAmount", "limit", "total", "quota"])
        .or_else(|| number_by_keys(row, &["total_amount", "totalAmount", "limit", "total", "quota"]));
    let used = number_by_keys(window, &["used_amount", "usedAmount", "used", "usage"])
        .or_else(|| number_by_keys(row, &["used_amount", "usedAmount", "used", "usage"]));
    let unlimited = flag(&["unlimited", "exemption", "is_unlimited", "isUnlimited"]);
    let exhausted = flag(&["usage_exhausted", "usageExhausted", "exhausted"]);
    // 一个数都读不出、也不是「不限量」标记 → 这不是一个可用的窗口
    if total.is_none() && used.is_none() && !unlimited {
        // 但若这行本身带了 `remainingPercent` / `availableView`，上游已经算过了，
        // 仍算一个可用读数（分母缺失时由展示串表达）
        let has_view = !text_by_keys(row, &["availableView", "remainingPercent", "view"]).is_empty();
        if !has_view {
            return None;
        }
    }
    Some(Window {
        name: text_by_keys(row, &["feature_group_name", "featureGroupName", "planName", "name", "title"]),
        total: total.unwrap_or(0.0),
        used: used.unwrap_or(0.0),
        resets_at: text_by_keys(row, &["resetsAt", "resets_at", "resetAt", "end_time", "endTime"]),
        exhausted,
        unlimited,
    })
}

/// 从一份响应里抽出全部窗口（三处载体：顶层数组 + 嵌套在 section / groups 里）
pub fn windows_of(payload: &Value) -> Vec<Window> {
    let mut out: Vec<Window> = Vec::new();
    // 直接数组载体
    for pointer in [
        "/windows",
        "/data/windows",
        "/data/window_limit_groups",
        "/window_limit_groups",
        "/data/enterprise_window_limit_groups",
        "/enterprise_window_limit_groups",
    ] {
        if let Some(items) = payload.pointer(pointer).and_then(Value::as_array) {
            for row in items {
                if let Some(window) = window_from_row(row) {
                    out.push(window);
                }
            }
        }
    }
    // 嵌在 section 里的载体（`window_limit_section.windows` 一族）
    for pointer in [
        "/data/window_limit_section",
        "/window_limit_section",
        "/data/enterprise_window_limit_section",
        "/enterprise_window_limit_section",
        "/data/quota_package_section",
        "/quota_package_section",
        "/data/incentive_quota_section",
        "/incentive_quota_section",
    ] {
        if let Some(section) = payload.pointer(pointer) {
            if let Some(items) = section.get("windows").and_then(Value::as_array) {
                for row in items {
                    if let Some(window) = window_from_row(row) {
                        out.push(window);
                    }
                }
            } else if section.is_object() {
                if let Some(window) = window_from_row(section) {
                    out.push(window);
                }
            }
        }
    }
    // 单窗口形态：顶层直接有 `total_amount` / `used_amount`
    if out.is_empty() {
        if let Some(window) = window_from_row(payload.get("data").unwrap_or(payload)) {
            out.push(window);
        }
    }
    // 去重（同一窗口可能同时出现在两处）
    let mut deduped: Vec<Window> = Vec::new();
    for window in out {
        if deduped
            .iter()
            .any(|item| item.name == window.name && item.total == window.total && item.used == window.used)
        {
            continue;
        }
        deduped.push(window);
    }
    deduped
}

/// 会员名（跨两套接口的键名链）
pub fn plan_name_of(payload: &Value) -> String {
    for pointer in [
        "/data/membership_display_name",
        "/membership_display_name",
        "/data/current_subscription/display/short_name",
        "/data/current_subscription/display/product_name",
        "/data/subscription/display/short_name",
        "/data/subscription/display/product_name",
        "/data/member_info/name",
        "/data/planName",
        "/planName",
    ] {
        let text = payload
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if !text.is_empty() {
            return text.to_string();
        }
    }
    String::new()
}

/// 上游自己的紧凑展示串（`availableView` / `remainingPercent` / `view`）
pub fn available_view_of(payload: &Value) -> String {
    for pointer in [
        "/data/availableView",
        "/availableView",
        "/data/view",
        "/view",
    ] {
        let text = payload
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if !text.is_empty() {
            return text.to_string();
        }
    }
    // `remainingPercent` 是一条单独的读数 → 拼一句「剩余 x%」
    for pointer in ["/data/remainingPercent", "/remainingPercent"] {
        if let Some(value) = payload.pointer(pointer) {
            if let Some(number) = number_of(Some(value)) {
                // 上游可能给 0~1 也可能给 0~100；≥1 之外都按百分比直读
                let percent = if number <= 1.0 {
                    number * 100.0
                } else {
                    number
                };
                return format!("剩余 {:.0}%", percent);
            }
            if let Some(text) = value.as_str() {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    String::new()
}

/// 有没有订阅（`hasActiveSubscription` / `hasEnterpriseSubscription`）
pub fn has_subscription(payload: &Value) -> bool {
    for pointer in [
        "/data/hasActiveSubscription",
        "/data/hasEnterpriseSubscription",
        "/hasActiveSubscription",
        "/hasEnterpriseSubscription",
    ] {
        if payload.pointer(pointer).and_then(Value::as_bool) == Some(true) {
            return true;
        }
    }
    false
}

/// 把两段原始响应归成界面认的额度形状（**纯函数**，便于测试）。
///
/// ⚠️ 见模块头那条契约：**读不到额度就不要写 `available`**。
pub fn shape_usage(overview: &Value, config: &Value) -> Value {
    // 两段各出窗口（有的账号只有一段有读数）
    let mut windows = windows_of(overview);
    windows.extend(windows_of(config));

    // 奖励额度（`bonus` / `incentive_quota_section`）单列一笔账
    let bonus = bonus_of(overview).or_else(|| bonus_of(config));

    let plan_name = {
        let from_overview = plan_name_of(overview);
        if from_overview.is_empty() {
            plan_name_of(config)
        } else {
            from_overview
        }
    };
    let available_view = {
        let from_overview = available_view_of(overview);
        if from_overview.is_empty() {
            available_view_of(config)
        } else {
            from_overview
        }
    };
    let active = has_subscription(overview) || has_subscription(config);

    // ── wallets：逐窗口 ──
    let mut wallets: Vec<Value> = Vec::new();
    let mut first_available: Option<f64> = None;
    for window in &windows {
        let mut wallet = Map::new();
        wallet.insert(
            "type".to_string(),
            Value::String("window".to_string()),
        );
        wallet.insert(
            "displayName".to_string(),
            Value::String(if window.name.is_empty() {
                "订阅窗口".to_string()
            } else {
                window.name.clone()
            }),
        );
        if window.unlimited {
            // 不限量窗口没有「剩余」可算 ⇒ 给 unlimited 标记而不是一个假数字
            wallet.insert("unlimited".to_string(), Value::Bool(true));
        } else {
            let remaining = (window.total - window.used).max(0.0);
            wallet.insert("balance".to_string(), Value::from(remaining));
            wallet.insert("remaining".to_string(), Value::from(remaining));
            if window.total > 0.0 {
                wallet.insert("limit".to_string(), Value::from(window.total));
                wallet.insert("used".to_string(), Value::from(window.used));
            }
            if first_available.is_none() && window.total > 0.0 {
                first_available = Some(remaining);
            }
        }
        if window.exhausted {
            wallet.insert("exhausted".to_string(), Value::Bool(true));
        }
        if !window.resets_at.is_empty() {
            wallet.insert(
                "resetAt".to_string(),
                Value::String(window.resets_at.clone()),
            );
        }
        wallets.push(Value::Object(wallet));
    }
    if let Some(bonus) = bonus {
        let mut wallet = Map::new();
        wallet.insert(
            "type".to_string(),
            Value::String("bonus".to_string()),
        );
        wallet.insert(
            "displayName".to_string(),
            Value::String("奖励额度".to_string()),
        );
        let remaining = (bonus.0 - bonus.1).max(0.0);
        wallet.insert("balance".to_string(), Value::from(remaining));
        wallet.insert("remaining".to_string(), Value::from(remaining));
        wallet.insert("limit".to_string(), Value::from(bonus.0));
        wallet.insert("used".to_string(), Value::from(bonus.1));
        if first_available.is_none() {
            first_available = Some(remaining);
        }
        wallets.push(Value::Object(wallet));
    }

    // ── 主读数：只在**确实解析出东西**时才写 available ──
    let available = if !windows.is_empty() {
        first_available
    } else {
        None
    };

    let mut subscription = json!({
        "status": if plan_name.is_empty() {
            if active { "active" } else { "unknown" }
        } else {
            "active"
        },
    });
    if let Some(object) = subscription.as_object_mut() {
        if !plan_name.is_empty() {
            object.insert("planName".to_string(), Value::String(plan_name));
        }
        if let Some(first) = windows.iter().find(|window| window.total > 0.0) {
            object.insert(
                "remainQuota".to_string(),
                Value::from((first.total - first.used).max(0.0)),
            );
            object.insert("totalQuota".to_string(), Value::from(first.total));
        }
    }

    let mut shaped = json!({
        "unit": "次",
        "wallets": wallets,
        "subscription": subscription,
        "raw": { "overview": overview, "config": config },
    });
    if let Some(object) = shaped.as_object_mut() {
        if !available_view.is_empty() {
            // 上游自己给的展示串（前端按形状探测，有就优先显示）
            object.insert("availableView".to_string(), Value::String(available_view));
        }
        if let Some(available) = available {
            object.insert("available".to_string(), Value::from(available));
        }
    }
    shaped
}

/// 奖励额度 (总量, 已用)
fn bonus_of(payload: &Value) -> Option<(f64, f64)> {
    for pointer in ["/data/bonus", "/bonus"] {
        if let Some(bonus) = payload.pointer(pointer) {
            if bonus.is_object() {
                let total = number_by_keys(bonus, &["total_amount", "totalAmount", "total", "limit"])?;
                let used = number_by_keys(bonus, &["used_amount", "usedAmount", "used"]).unwrap_or(0.0);
                return Some((total, used));
            }
            if let Some(number) = number_of(Some(bonus)) {
                // 裸数字：那是「还剩多少」而不是总量 ⇒ 总量即余额、已用为 0
                return Some((number, 0.0));
            }
        }
    }
    None
}

/// 查额度：读记录 → 打两条接口 → 归一。
///
/// ⚠️ 两条接口都失败才报错；**一条成功就用那一条**（本家两段各有各的读数，
/// 只挂其中一条不该让整次查询失败）。
pub async fn query(store: &AccountStore, account_id: &str) -> Result<Value, GatewayError> {
    let record = store.doubaowork_account_record(account_id);
    if !account_id.is_empty() && record.is_none() {
        return Err(GatewayError::with_status(
            401,
            format!("豆包工作账号 {account_id} 不存在（请重新添加）"),
        ));
    }
    let credentials = from_record(record.as_ref())?;
    let raw = fetch_raw(&credentials).await?;
    Ok(shape_usage(&raw.overview, &raw.config))
}

/// 打两条额度接口（**一条失败不影响另一条**，见 [`query`] 的说明）。
///
/// `401` / `403` 会**原样透出**：那是登录态失效，调用方要走既有的
/// 「刷新后重试一次」处置 —— 而本家没有刷新接口（`supports_refresh = false`），
/// 因此那条路最终会落到一句「请重新登录」。两条都 401 才报错，一条 401
/// 一条成功时按成功处理（本家两段读数独立）。
pub async fn fetch_raw(credentials: &super::credentials::Credentials) -> Result<RawQuota, GatewayError> {
    let overview = fetch_one(credentials, endpoints::subscription_overview_url()).await;
    let config = fetch_one(credentials, endpoints::subscription_entry_config_url()).await;
    match (&overview, &config) {
        (Ok(overview), Ok(config)) => Ok(RawQuota {
            overview: overview.clone(),
            config: config.clone(),
        }),
        (Ok(overview), Err(_)) => Ok(RawQuota {
            overview: overview.clone(),
            config: Value::Null,
        }),
        (Err(_), Ok(config)) => Ok(RawQuota {
            overview: Value::Null,
            config: config.clone(),
        }),
        (Err(error), Err(_)) => {
            // 两条都失败：把第一条的错因透出（可读、且指向用户能做的事）
            let message = error.message.clone();
            let status = error.status_code;
            Err(GatewayError::with_status(status, message))
        }
    }
}

/// 打一条额度接口，返回响应体（`data` 层原样）。
async fn fetch_one(
    credentials: &super::credentials::Credentials,
    url: String,
) -> Result<Value, GatewayError> {
    let query = endpoints::encode_query(&endpoints::security_params(&credentials.fingerprint));
    let full = format!("{url}?{query}");
    let mut headers = endpoints::base_headers();
    headers.extend(endpoints::auth_headers(
        &credentials.cookie_header(),
        credentials.csrf_token().as_deref(),
    ));
    let response = send_raw("GET", &full, None, &headers, None, Some(QUOTA_TIMEOUT_MS))
        .await
        .map_err(|error| {
            if error.is_timeout() {
                GatewayError::with_status(504, "豆包工作额度接口超时")
            } else {
                GatewayError::with_status(502, format!("豆包工作额度接口请求失败：{error}"))
            }
        })?;
    match response.status {
        200..=299 => Ok(response.payload.unwrap_or(Value::Null)),
        401 | 403 => Err(GatewayError::with_status(
            401,
            format!(
                "豆包工作登录态已失效（HTTP {}）：请在账号页重新登录",
                response.status
            ),
        )),
        other => Err(GatewayError::with_status(
            502,
            format!(
                "豆包工作额度接口返回 HTTP {other}：{}",
                response
                    .payload
                    .as_ref()
                    .and_then(|payload| {
                        ["message", "msg", "error_msg"]
                            .iter()
                            .find_map(|key| payload.get(*key).and_then(Value::as_str))
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| "（无文案）".to_string())
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 正常形态：窗口进 wallets，主读数 = 第一窗口剩余
    #[test]
    fn shaped_usage_reads_windows() {
        let shaped = shape_usage(
            &json!({"data": {
                "membership_display_name": "豆包会员",
                "window_limit_groups": [
                    {"feature_group_name": "对话", "total_amount": 100, "used_amount": 30,
                     "resetsAt": "2026-11-01", "usage_exhausted": false}
                ]
            }}),
            &json!({}),
        );
        assert_eq!(shaped.get("unit").and_then(Value::as_str), Some("次"));
        assert_eq!(shaped.get("available").and_then(Value::as_f64), Some(70.0));
        let wallet = shaped
            .get("wallets")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(wallet.get("limit").and_then(Value::as_f64), Some(100.0));
        assert_eq!(wallet.get("used").and_then(Value::as_f64), Some(30.0));
        assert_eq!(wallet.get("resetAt").and_then(Value::as_str), Some("2026-11-01"));
        assert_eq!(
            shaped
                .get("subscription")
                .and_then(|value| value.get("planName"))
                .and_then(Value::as_str),
            Some("豆包会员")
        );
    }

    /// ⚠️ **契约**：读不到额度 → **不写 `available`**（写了会被当成余额 0 而跳过账号）
    #[test]
    fn available_is_omitted_when_nothing_was_parsed() {
        let shaped = shape_usage(&json!({}), &json!({}));
        assert!(
            shaped.get("available").is_none(),
            "读不到额度时写 available 会让账号被误判成余额不足"
        );
        assert_eq!(
            shaped.get("wallets").and_then(Value::as_array).map(Vec::len),
            Some(0)
        );
        assert_eq!(
            shaped
                .get("subscription")
                .and_then(|value| value.get("status"))
                .and_then(Value::as_str),
            Some("unknown")
        );
    }

    /// 不限量窗口：给 `unlimited` 标记而**不是**一个假数字
    #[test]
    fn unlimited_windows_are_marked_not_faked() {
        let shaped = shape_usage(
            &json!({"data": {"window_limit_groups": [
                {"feature_group_name": "不限量", "unlimited": true}
            ]}}),
            &json!({}),
        );
        let wallet = shaped
            .get("wallets")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(wallet.get("unlimited"), Some(&Value::Bool(true)));
        assert!(wallet.get("balance").is_none(), "不限量不该编一个剩余数");
        // 只有不限量窗口时也不该写 available
        assert!(shaped.get("available").is_none());
    }

    /// 两套接口的窗口都能读（`entry/config` 的 `window_limit_section` 一族）
    #[test]
    fn windows_come_from_both_endpoints() {
        let shaped = shape_usage(
            &json!({}),
            &json!({"data": {
                "window_limit_section": {
                    "windows": [
                        {"planName": "专业版", "window": {"limit": 200, "used": 50},
                         "resetsAt": "2026-12-01"}
                    ]
                },
                "hasActiveSubscription": true
            }}),
        );
        assert_eq!(shaped.get("available").and_then(Value::as_f64), Some(150.0));
        let wallet = shaped
            .get("wallets")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(wallet.get("displayName").and_then(Value::as_str), Some("专业版"));
        // 两段都没窗口才是不写 available；有一段有读数就要写
        assert_eq!(
            shaped
                .get("subscription")
                .and_then(|value| value.get("status"))
                .and_then(Value::as_str),
            Some("active")
        );
    }

    /// 奖励额度：单列一笔 wallet（与订阅窗口是两笔账）
    #[test]
    fn bonus_is_a_separate_wallet() {
        let shaped = shape_usage(
            &json!({"data": {
                "window_limit_groups": [{"total_amount": 100, "used_amount": 10}],
                "bonus": {"total_amount": 50, "used_amount": 20}
            }}),
            &json!({}),
        );
        let wallets = shaped
            .get("wallets")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(wallets.len(), 2);
        let bonus = wallets
            .iter()
            .find(|wallet| wallet.get("type").and_then(Value::as_str) == Some("bonus"))
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(bonus.get("balance").and_then(Value::as_f64), Some(30.0));
        // 主读数仍是订阅窗口（奖励不覆盖它）
        assert_eq!(shaped.get("available").and_then(Value::as_f64), Some(90.0));
    }

    /// 上游自己的展示串要带上（前端按形状探测，有就优先显示）
    #[test]
    fn available_view_is_carried_through() {
        let shaped = shape_usage(
            &json!({"data": {"window_limit_groups": [{"total_amount": 10, "used_amount": 1}],
                             "availableView": "剩 9 次（今日）"}}),
            &json!({}),
        );
        assert_eq!(
            shaped.get("availableView").and_then(Value::as_str),
            Some("剩 9 次（今日）")
        );
        // 只有 remainingPercent 时拼一句中文
        let percent = shape_usage(
            &json!({"data": {"window_limit_groups": [{"total_amount": 10, "used_amount": 1}],
                             "remainingPercent": 0.8}}),
            &json!({}),
        );
        assert_eq!(
            percent.get("availableView").and_then(Value::as_str),
            Some("剩余 80%")
        );
    }

    /// 剩余夹到 0（计量回滚会让服务端报超额）
    #[test]
    fn remaining_is_clamped_at_zero() {
        let shaped = shape_usage(
            &json!({"data": {"window_limit_groups": [
                {"total_amount": 10, "used_amount": 12, "usage_exhausted": true}
            ]}}),
            &json!({}),
        );
        assert_eq!(shaped.get("available").and_then(Value::as_f64), Some(0.0));
        let wallet = shaped
            .get("wallets")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(wallet.get("exhausted"), Some(&Value::Bool(true)));
    }

    /// 数字字符串也要认（上游有的接口把数量给成字符串）
    #[test]
    fn numeric_strings_are_accepted() {
        let shaped = shape_usage(
            &json!({"data": {"window_limit_groups": [
                {"total_amount": "100", "used_amount": "30"}
            ]}}),
            &json!({}),
        );
        assert_eq!(shaped.get("available").and_then(Value::as_f64), Some(70.0));
    }

    /// 去重：同一个窗口同时出现在两处只算一次
    #[test]
    fn duplicate_windows_are_deduplicated() {
        let windows = windows_of(&json!({"data": {
            "windows": [{"planName": "A", "total_amount": 10, "used_amount": 1}],
            "window_limit_section": {"windows": [{"planName": "A", "total_amount": 10, "used_amount": 1}]}
        }}));
        assert_eq!(windows.len(), 1);
    }
}
