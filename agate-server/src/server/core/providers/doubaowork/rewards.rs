//! 签到 / 活动领取：`campaign/detail` 读签到态 → `reward/claim/<code>` 领奖励。
//!
//! ── 上游长什么样（[二进制] 字段名逐字）─────────────────────────
//! ```text
//!   GET  /alice/ug/campaign/v3/detail/m2          活动详情（签到态在这里）
//!        task_status  checked_in_today  daily_checkin
//!        ug_campaign_info  campaign_group_code  campaign_code
//!        pc_incentive_task_mvp_v3
//!        unclaimed  results
//!        /reward_summaries/0/reward_amount        ← 奖励金额的**路径**
//!   GET  /alice/ug/campaign/list/                活动列表
//!   POST /alice/ug/campaign/item/reward/claim/<campaign_item_code>   领取
//!        campaign_item_code
//! ```
//! 读法（本模块最重要的一段）：
//!   - `checked_in_today` / `daily_checkin` / `task_status` 是**签到态**：
//!     前两者说的是「今天签过没有」，`task_status` 是任务状态机（[二进制]
//!     里它与 `biz_error` 相邻 —— 出错的签到也是走这一路回话）；
//!   - `/reward_summaries/0/reward_amount` **是一个 JSON 路径**（[二进制] 里
//!     它以带斜杠的形态出现）：奖励金额在 `reward_summaries` 数组的第 0 条里，
//!     本实现按同一条路径读（见 [`reward_amount_of`]）；
//!   - `unclaimed` 是**可领取的条目**、`campaign_item_code` 是领取时要带的 id。
//!     本实现按「`unclaimed` 数组 → 逐条取 `campaign_item_code`」领。
//!
//! ── 签到与「领取」在本家是两件事，本模块**合成一次动作** ──────────
//! 上游没有「签到」这个动作 —— 签到表现为「活动里有一条 daily_checkin 任务，
//! 领它就是签到」。因此 [`claim_for_account`] 做的是：
//! ```text
//!   1. 读活动详情 → 今天签过没有？
//!        签过 → 回 success:false + alreadyCompleted:true + 中性文案（无事可做）
//!   2. 没签过 → 找出 unclaimed 里那条 daily_checkin / 签到类条目
//!        → POST reward/claim/<campaign_item_code>
//!        → 成功后再读一次活动详情核对「今天签过了」
//!   3. 没有可领的条目 → 如实回「本次活动期没有可领的奖励」
//! ```
//!
//! ⚠️ **`alreadyCompleted: true` 是本仓签到台账的完成判据**（见
//! `billing::checkin::checkin_completed_today`：它认 `success` /
//! `alreadyCompleted` / 文案里的「已签到 / 已领取」）。因此「今天已签」这一种
//! **无事可做**的结局必须显式带上它 —— 否则它既不算失败也不算完成，签到中心
//! 会一直显示「待签」并反复重打（与 AtomCode 那边同一教训）。
//! `success` 仍如实为 `false` —— 这一天确实没有发放任何东西（不谎报成功）。
//!
//! ── 诚实标注：接口与活动以官方实际返回为准 ──────────────────────
//! 本家**没有真实账号可核实**（插件文档原话「Not validated with a real
//! account; endpoints and campaigns depend on actual upstream responses」）。
//! 因此本模块的口径是**宽容解析 + 如实汇报**：
//!   - 认不出的字段不猜（读不到签到态就回「读不出签到态」而不是「没签到」）；
//!   - 领取失败把上游原文带回来（活动规则千变万化，编一个原因是误导）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::core::auth_http::send_raw;
use crate::server::errors::GatewayError;

use super::credentials::{self, Credentials};
use super::endpoints;

/// 签到 / 活动类请求的超时（毫秒）
const REWARD_TIMEOUT_MS: u64 = 20_000;

/// 签到类条目的识别关键词（`campaign_item_code` / `campaign_code` / 标题里出现即认）
///
/// ⚠️ 这里刻意**不做**「精确码表」：活动码是**运营配置的**（[二进制] 里
/// 有 `pc_incentive_task_mvp_v3` 这种看起来就是活动代号的名字），
/// 写死一份码表会在下一次运营换活动时静默失效。用关键词是更耐久的判据，
/// 而且「没有可识别的签到条目」这一种结局是**如实汇报**而不是失败
/// （见 [`daily_checkin_item`]）。
const CHECKIN_KEYWORDS: [&str; 8] = [
    "checkin",
    "check_in",
    "check-in",
    "daily",
    "signin",
    "sign_in",
    "签到",
    "daily_checkin",
];

/// 从一个条目对象里读一个字符串（多个候选键）
fn text_by_keys(value: &Value, keys: &[&str]) -> String {
    for key in keys {
        let text = value
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

/// 从一个条目对象里读一个布尔（数字 1/0 与字符串 "1"/"true" 都认）
fn bool_by_keys(value: &Value, keys: &[&str]) -> Option<bool> {
    for key in keys {
        if let Some(item) = value.get(*key) {
            match item {
                Value::Bool(flag) => return Some(*flag),
                Value::Number(number) => return Some(number.as_i64().unwrap_or(0) != 0),
                Value::String(text) => {
                    let lowered = text.trim().to_ascii_lowercase();
                    if matches!(lowered.as_str(), "1" | "true" | "yes" | "done" | "finished") {
                        return Some(true);
                    }
                    if matches!(lowered.as_str(), "0" | "false" | "no" | "pending" | "unclaimed") {
                        return Some(false);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// 活动详情响应的归一读数。
///
/// `None` 表示**读不出**（不是「没签到」）—— 见模块头的「认不出的字段不猜」。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckinState {
    /// 今天签到过没有（`None` = 上游没给这个信息）
    pub checked_in_today: Option<bool>,
    /// 可领取的条目（`(campaign_item_code, 展示名, 是否签到类)`）
    pub claimable: Vec<(String, String, bool)>,
    /// 上次签到距今连续天数（上游给了才带；本家不一定有这个字段）
    pub continuous_days: Option<i64>,
}

/// 读活动详情（`campaign/v3/detail/m2` 的响应 → [`CheckinState`]）
///
/// ── 签到态的取法（三处键，谁先有值用谁）─────────────────────────
/// [二进制] 给了三个键，它们的**优先级按「越具体越可信」排**：
/// ```text
///   checked_in_today   最直接（布尔语义明确）→ 首选
///   daily_checkin      次之（可能是对象或布尔）
///   task_status        最模糊（状态机字符串；`done` / `finished` 视为签过）
/// ```
/// 三处都没有 ⇒ `None`（读不出），**不猜 false**：猜 false 会让「上游接口变了」
/// 表现为「用户每天多点一次签到」，而那个假动作可能真的失败。
pub fn read_checkin_state(payload: &Value) -> CheckinState {
    let root = payload.get("data").unwrap_or(payload);
    let checked_in_today = bool_by_keys(root, &["checked_in_today", "checkedInToday"])
        .or_else(|| {
            root.get("daily_checkin")
                .and_then(|node| bool_by_keys(node, &["checked_in_today", "checked", "done", "status"]))
        })
        .or_else(|| bool_by_keys(root, &["task_status", "taskStatus"]));

    // ── 可领取条目：`unclaimed` 数组（也可能是 `results` 里带 unclaimed 标记的）──
    let mut claimable: Vec<(String, String, bool)> = Vec::new();
    let mut push_item = |item: &Value, forced_unclaimed: bool| {
        let code = text_by_keys(
            item,
            &["campaign_item_code", "campaignItemCode", "item_code", "itemCode", "code", "id"],
        );
        if code.is_empty() {
            return;
        }
        let name = text_by_keys(
            item,
            &["name", "title", "item_name", "itemName", "desc", "description"],
        );
        let unclaimed = bool_by_keys(item, &["unclaimed", "claimable", "can_claim"])
            .unwrap_or(forced_unclaimed);
        if !unclaimed {
            return;
        }
        let haystack = format!(
            "{} {} {}",
            code.to_ascii_lowercase(),
            name.to_ascii_lowercase(),
            text_by_keys(item, &["campaign_code", "campaignCode", "campaign_group_code", "type"])
                .to_ascii_lowercase()
        );
        let is_checkin = CHECKIN_KEYWORDS
            .iter()
            .any(|keyword| haystack.contains(keyword));
        if claimable.iter().any(|(existing, ..)| *existing == code) {
            return;
        }
        claimable.push((code, name, is_checkin));
    };

    if let Some(items) = root.get("unclaimed").and_then(Value::as_array) {
        for item in items {
            push_item(item, true);
        }
    }
    for pointer in ["/results", "/data/results", "/ug_campaign_info/items"] {
        if let Some(items) = payload.pointer(pointer).and_then(Value::as_array) {
            for item in items {
                push_item(item, false);
            }
        }
    }

    let continuous_days = root
        .get("continuous_days")
        .or_else(|| root.get("continuousDays"))
        .and_then(Value::as_i64)
        .filter(|value| *value >= 0);

    CheckinState {
        checked_in_today,
        claimable,
        continuous_days,
    }
}

/// 从活动详情里找出**签到类**的那一条可领条目。
///
/// 找不到返回 `None`，调用方据此回「本活动期没有可签到的任务」（如实汇报，
/// **不是**失败 —— 与「签到失败」是两种完全不同的用户可见结果）。
pub fn daily_checkin_item(state: &CheckinState) -> Option<(String, String)> {
    state
        .claimable
        .iter()
        .find(|(_, _, is_checkin)| *is_checkin)
        .map(|(code, name, _)| (code.clone(), name.clone()))
}

/// 奖励金额（`/reward_summaries/0/reward_amount` 这条路径，[二进制] 逐字）
pub fn reward_amount_of(payload: &Value) -> Option<f64> {
    payload
        .pointer("/reward_summaries/0/reward_amount")
        .and_then(|value| {
            value.as_f64().or_else(|| {
                value
                    .as_str()
                    .and_then(|text| text.trim().parse::<f64>().ok())
            })
        })
        .or_else(|| {
            // 领取响应的形态可能不同：把路径放宽到「第一处 reward_amount」
            for pointer in ["/data/reward_amount", "/reward_amount", "/data/rewardAmount"] {
                if let Some(value) = payload.pointer(pointer) {
                    if let Some(number) = value.as_f64() {
                        return Some(number);
                    }
                    if let Some(number) = value
                        .as_str()
                        .and_then(|text| text.trim().parse::<f64>().ok())
                    {
                        return Some(number);
                    }
                }
            }
            None
        })
}

/// 一次领取响应的判定结果
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimVerdict {
    /// 这次真的发放了（`success: true` 或业务码 0 + 无错误）
    Granted {
        /// 上游文案
        message: String,
        /// 奖励金额（读得出才带）
        amount: Option<String>,
    },
    /// 已经领过 / 今天已签（**成功语义**：停，不重复领）
    AlreadyHeld {
        /// 上游文案
        message: String,
    },
    /// 被拒（活动规则不允许、不在活动期、名额已满…）
    Refused {
        /// 上游文案
        message: String,
    },
}

/// 判定一次 `reward/claim` 的响应。
///
/// ── 判定顺序是契约（先看「已领过」，再看「成功」）─────────────────
/// 与 AtomCode 的 `classify_tier_response` 同一条教训：上游用
/// `already_claimed` / `claimed` / `duplicate` 一类标记表达「已持有」——
/// **那是成功语义**。把它当失败会让调用方继续重试（对一个幂等动作重试是
/// 无害的，但对「已领过」的那条路径重试会一直撞同一句话，用户看到一次
/// 失败文案而实际上什么都没坏）。
pub fn classify_claim(payload: &Value) -> ClaimVerdict {
    let message = {
        let text = text_by_keys(
            payload,
            &["message", "msg", "error_msg", "errorMsg", "desc", "description"],
        );
        if text.is_empty() {
            text_by_keys(payload.get("data").unwrap_or(payload), &["message", "msg", "desc"])
        } else {
            text
        }
    };
    let amount = reward_amount_of(payload).map(|number| {
        if number.fract() == 0.0 {
            format!("{}", number as i64)
        } else {
            format!("{number}")
        }
    });

    // ── 1. 已领过（成功语义，先判）──
    let already = bool_by_keys(
        payload,
        &["already_claimed", "alreadyClaimed", "claimed", "duplicate", "already_completed"],
    )
    .unwrap_or(false)
        || payload
            .get("data")
            .is_some_and(|data| {
                bool_by_keys(
                    data,
                    &["already_claimed", "alreadyClaimed", "claimed", "duplicate"],
                )
                .unwrap_or(false)
            });
    if already {
        return ClaimVerdict::AlreadyHeld { message };
    }

    // ── 2. 业务码：0 = 成功；非 0 = 被拒 ──
    let code = payload
        .get("code")
        .and_then(Value::as_i64)
        .or_else(|| payload.pointer("/data/code").and_then(Value::as_i64));
    let success = bool_by_keys(payload, &["success", "ok"]).unwrap_or(false)
        || payload
            .get("data")
            .is_some_and(|data| bool_by_keys(data, &["success", "granted"]).unwrap_or(false));
    match code {
        Some(0) => ClaimVerdict::Granted { message, amount },
        Some(_) => ClaimVerdict::Refused { message },
        None if success => ClaimVerdict::Granted { message, amount },
        None => {
            // 没有任何成功/失败标记：有文案按拒绝处理（上游常这么回规则说明），
            // 连文案都没有就当成**成功但没有回执**（HTTP 2xx + 空体是合法形态）
            if message.is_empty() {
                ClaimVerdict::Granted {
                    message: String::new(),
                    amount,
                }
            } else {
                ClaimVerdict::Refused { message }
            }
        }
    }
}

/// 读一次活动详情。
pub async fn fetch_checkin_state(
    credentials: &Credentials,
    timeout_ms: u64,
) -> Result<CheckinState, GatewayError> {
    let payload = fetch_json(credentials, endpoints::campaign_detail_url(), timeout_ms).await?;
    Ok(read_checkin_state(&payload))
}

/// 领取一个条目（`POST /alice/ug/campaign/item/reward/claim/<campaign_item_code>`）
pub async fn claim_item(
    credentials: &Credentials,
    campaign_item_code: &str,
    timeout_ms: u64,
) -> Result<Value, GatewayError> {
    if campaign_item_code.trim().is_empty() {
        return Err(GatewayError::with_status(400, "缺少 campaign_item_code，无法领取"));
    }
    let query = endpoints::encode_query(&endpoints::security_params(&credentials.fingerprint));
    let url = format!(
        "{}?{}",
        endpoints::reward_claim_url(campaign_item_code),
        query
    );
    let body = json!({ "campaign_item_code": campaign_item_code.trim() });
    let mut headers = endpoints::base_headers();
    headers.extend(endpoints::json_headers());
    headers.extend(endpoints::auth_headers(
        &credentials.cookie_header(),
        credentials.csrf_token().as_deref(),
    ));
    let response = send_raw(
        "POST",
        &url,
        Some(&body),
        &headers,
        None,
        Some(timeout_ms),
    )
    .await
    .map_err(|error| GatewayError::with_status(502, format!("豆包工作领取请求失败：{error}")))?;
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
                "豆包工作领取返回 HTTP {other}：{}",
                response
                    .payload
                    .as_ref()
                    .and_then(readable_error)
                    .unwrap_or_else(|| "（无文案）".to_string())
            ),
        )),
    }
}

/// 带鉴权取一个 JSON（活动详情与活动列表共用这一条）
async fn fetch_json(
    credentials: &Credentials,
    url: String,
    timeout_ms: u64,
) -> Result<Value, GatewayError> {
    let query = endpoints::encode_query(&endpoints::security_params(&credentials.fingerprint));
    let full = format!("{url}?{query}");
    let mut headers = endpoints::base_headers();
    headers.extend(endpoints::auth_headers(
        &credentials.cookie_header(),
        credentials.csrf_token().as_deref(),
    ));
    let response = send_raw("GET", &full, None, &headers, None, Some(timeout_ms))
        .await
        .map_err(|error| GatewayError::with_status(502, format!("豆包工作活动接口请求失败：{error}")))?;
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
                "豆包工作活动接口返回 HTTP {other}：{}",
                response
                    .payload
                    .as_ref()
                    .and_then(readable_error)
                    .unwrap_or_else(|| "（无文案）".to_string())
            ),
        )),
    }
}

/// 签到（= 领当天那条签到任务）。
///
/// 返回的形状与别家的签到实现一致（`{success, msg, alreadyCompleted?, …}`），
/// 交给 `billing::checkin::claim_result` 翻成签到结果行 —— 其中
/// `alreadyCompleted` 是内核台账「今天已完成」的键（见模块头）。
///
/// ── 三种结局对应用户看到的三种话 ─────────────────────────────
///   · **今天已签**：`success:false` + `alreadyCompleted:true` + 中性文案；
///   · **真领到**：`success:true` + 文案（带上奖励金额，读得到才带）；
///   · **本活动期没有可签到的任务**：`success:false` + `needsAction:false`
///     + 中性文案（**不谎报成功**，也不冒充失败）。
pub async fn claim_for_account(
    store: &AccountStore,
    account_id: &str,
) -> Result<Value, GatewayError> {
    let record = store.doubaowork_account_record(account_id);
    if !account_id.is_empty() && record.is_none() {
        return Err(GatewayError::with_status(
            401,
            format!("豆包工作账号 {account_id} 不存在（请重新添加）"),
        ));
    }
    let credentials = credentials::from_record(record.as_ref())?;

    // ── 第一步：读签到态 ──
    let state = fetch_checkin_state(&credentials, REWARD_TIMEOUT_MS).await?;
    if state.checked_in_today == Some(true) {
        let mut msg = String::from("今天已签到，无需重复领取");
        if let Some(days) = state.continuous_days {
            msg.push_str(&format!("（连续 {days} 天）"));
        }
        crate::server::logging::verbose(
            "[Checkin]",
            &format!("豆包工作 {account_id}: {msg}"),
        );
        return Ok(json!({
            "success": false,
            "alreadyCompleted": true,
            "msg": msg,
            "needsAction": false,
        }));
    }

    // ── 第二步：找签到条目 ──
    let Some((code, name)) = daily_checkin_item(&state) else {
        // 读不出签到态 + 没有可领条目 ⇒ 上游这条链路的口径变了或不在活动期。
        // 如实汇报（中性、needsAction:false），**不冒充成功也不冒充失败**。
        let msg = if state.checked_in_today.is_none() {
            "读不出签到状态（上游活动接口的响应里没有 checked_in_today / \
             daily_checkin / task_status，口径可能已变），本次活动期也无签到类奖励可领"
                .to_string()
        } else {
            "本次活动期没有签到类奖励可领".to_string()
        };
        crate::server::logging::verbose(
            "[Checkin]",
            &format!("豆包工作 {account_id}: {msg}"),
        );
        return Ok(json!({
            "success": false,
            "alreadyCompleted": true,
            "msg": msg,
            "needsAction": false,
        }));
    };

    // ── 第三步：领取 ──
    let payload = claim_item(&credentials, &code, REWARD_TIMEOUT_MS).await?;
    match classify_claim(&payload) {
        ClaimVerdict::AlreadyHeld { message } => {
            let msg = if message.is_empty() {
                "今天已签到（上游回执说已领取）".to_string()
            } else {
                message
            };
            crate::server::logging::log(
                "[Checkin]",
                &format!("豆包工作 {account_id}: {msg}"),
            );
            Ok(json!({
                "success": false,
                "alreadyCompleted": true,
                "msg": msg,
                "itemCode": code,
                "needsAction": false,
            }))
        }
        ClaimVerdict::Granted { message, amount } => {
            let mut msg = if message.is_empty() {
                if name.is_empty() {
                    "签到成功".to_string()
                } else {
                    format!("签到成功：{name}")
                }
            } else {
                message
            };
            if let Some(amount) = amount.as_deref() {
                msg.push_str(&format!("（+{amount}）"));
            }
            crate::server::logging::log(
                "[Checkin]",
                &format!("豆包工作 {account_id}: {msg}"),
            );
            Ok(json!({
                "success": true,
                "msg": msg,
                "itemCode": code,
                "amount": amount,
                "needsAction": true,
            }))
        }
        ClaimVerdict::Refused { message } => {
            let msg = if message.is_empty() {
                "签到被上游拒绝（未说明原因）".to_string()
            } else {
                message
            };
            crate::server::logging::log(
                "[Checkin]",
                &format!("豆包工作 {account_id}: 签到失败 —— {msg}"),
            );
            Ok(json!({
                "success": false,
                "msg": msg,
                "itemCode": code,
                "needsAction": true,
            }))
        }
    }
}

/// 领取全部可领奖励（**可选**：签到链只调 [`claim_for_account`]）。
///
/// 留在本模块是为了让「签到」与「领奖」共用一个解析器（[`read_checkin_state`]
/// 与 [`classify_claim`]）：活动规则会变，但「哪些条目可领」这件事只有一处
/// 判据。逐条领、逐条记结果 —— 一条失败不影响其余（每条独立）。
pub async fn claim_all_for_account(
    store: &AccountStore,
    account_id: &str,
) -> Result<Value, GatewayError> {
    let record = store.doubaowork_account_record(account_id);
    if !account_id.is_empty() && record.is_none() {
        return Err(GatewayError::with_status(
            401,
            format!("豆包工作账号 {account_id} 不存在（请重新添加）"),
        ));
    }
    let credentials = credentials::from_record(record.as_ref())?;
    let state = fetch_checkin_state(&credentials, REWARD_TIMEOUT_MS).await?;
    let mut results: Vec<Value> = Vec::new();
    let mut granted = 0usize;
    for (code, name, is_checkin) in &state.claimable {
        match claim_item(&credentials, code, REWARD_TIMEOUT_MS).await {
            Ok(payload) => match classify_claim(&payload) {
                ClaimVerdict::Granted { message, amount } => {
                    granted += 1;
                    results.push(json!({
                        "itemCode": code,
                        "name": name,
                        "isCheckin": is_checkin,
                        "outcome": "granted",
                        "message": message,
                        "amount": amount,
                    }));
                }
                ClaimVerdict::AlreadyHeld { message } => {
                    results.push(json!({
                        "itemCode": code,
                        "name": name,
                        "isCheckin": is_checkin,
                        "outcome": "already-held",
                        "message": message,
                    }));
                }
                ClaimVerdict::Refused { message } => {
                    results.push(json!({
                        "itemCode": code,
                        "name": name,
                        "isCheckin": is_checkin,
                        "outcome": "refused",
                        "message": message,
                    }));
                }
            },
            Err(error) => {
                results.push(json!({
                    "itemCode": code,
                    "name": name,
                    "isCheckin": is_checkin,
                    "outcome": "failed",
                    "message": error.message,
                }));
            }
        }
    }
    Ok(json!({
        "success": granted > 0,
        "alreadyCompleted": if state.checked_in_today == Some(true) { Value::Bool(true) } else { Value::Null },
        "grantedCount": granted,
        "results": results,
        "msg": if granted > 0 {
            format!("领取完成，共 {granted} 项")
        } else if state.claimable.is_empty() {
            "本次活动期没有可领的奖励".to_string()
        } else {
            "没有领取到新的奖励（可能已领过或不符合条件）".to_string()
        },
    }))
}

/// 从响应里读一条可读的错误文案
fn readable_error(payload: &Value) -> Option<String> {
    for key in ["message", "msg", "error_msg", "errorMsg", "desc", "description", "error"] {
        if let Some(text) = payload.get(key).and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 签到态：三处键的优先级（`checked_in_today` > `daily_checkin` > `task_status`）
    #[test]
    fn checkin_state_key_priority() {
        let direct = read_checkin_state(&json!({"checked_in_today": true}));
        assert_eq!(direct.checked_in_today, Some(true));
        let nested = read_checkin_state(&json!({"daily_checkin": {"checked_in_today": true}}));
        assert_eq!(nested.checked_in_today, Some(true));
        let status = read_checkin_state(&json!({"task_status": "done"}));
        assert_eq!(status.checked_in_today, Some(true));
        let pending = read_checkin_state(&json!({"task_status": "pending"}));
        assert_eq!(pending.checked_in_today, Some(false));
        // 数字 1/0 也要认
        assert_eq!(
            read_checkin_state(&json!({"checked_in_today": 1})).checked_in_today,
            Some(true)
        );
        assert_eq!(
            read_checkin_state(&json!({"checked_in_today": 0})).checked_in_today,
            Some(false)
        );
    }

    /// ⚠️ **读不出签到态时是 `None`，不是 `false`**（猜 false 会让用户每天多点一次）
    #[test]
    fn unreadable_checkin_state_is_none_not_false() {
        let state = read_checkin_state(&json!({"unexpected": "shape"}));
        assert_eq!(state.checked_in_today, None);
        assert!(state.claimable.is_empty());
        // data 包裹层也要能读到
        let wrapped = read_checkin_state(&json!({"data": {"checked_in_today": true}}));
        assert_eq!(wrapped.checked_in_today, Some(true));
    }

    /// 可领条目：`unclaimed` 数组；签到类按关键词识别（含中文「签到」）
    #[test]
    fn claimable_items_and_checkin_detection() {
        let state = read_checkin_state(&json!({
            "checked_in_today": false,
            "unclaimed": [
                {"campaign_item_code": "daily_checkin_v3", "title": "每日签到", "unclaimed": true},
                {"campaign_item_code": "watch_ad", "title": "看广告得奖励", "unclaimed": true}
            ]
        }));
        assert_eq!(state.claimable.len(), 2);
        let checkin = daily_checkin_item(&state);
        assert_eq!(
            checkin,
            Some(("daily_checkin_v3".to_string(), "每日签到".to_string()))
        );
        // 没有签到类条目 → None（调用方如实回「活动期没有可签到的」）
        let no_checkin = read_checkin_state(&json!({
            "unclaimed": [{"campaign_item_code": "watch_ad", "title": "看广告"}]
        }));
        assert_eq!(daily_checkin_item(&no_checkin), None);
    }

    /// `unclaimed` 为 false 的条目不算可领（`results` 里的要靠标记过滤）
    #[test]
    fn items_marked_unclaimed_false_are_skipped() {
        let state = read_checkin_state(&json!({
            "unclaimed": [
                {"campaign_item_code": "a", "unclaimed": false},
                {"campaign_item_code": "b", "unclaimed": true}
            ]
        }));
        assert_eq!(state.claimable.len(), 1);
        assert_eq!(state.claimable[0].0, "b");
    }

    /// 去重：同一个 `campaign_item_code` 只出一条
    #[test]
    fn duplicate_items_are_deduplicated() {
        let state = read_checkin_state(&json!({
            "unclaimed": [
                {"campaign_item_code": "x", "unclaimed": true},
                {"campaign_item_code": "x", "unclaimed": true, "title": "重复"}
            ]
        }));
        assert_eq!(state.claimable.len(), 1);
    }

    /// `results` 里的条目要靠 `unclaimed` / `claimable` 标记才会被收
    #[test]
    fn results_require_explicit_unclaimed_flag() {
        let state = read_checkin_state(&json!({
            "results": [
                {"campaign_item_code": "a", "claimable": true},
                {"campaign_item_code": "b"}
            ]
        }));
        assert_eq!(state.claimable.len(), 1);
        assert_eq!(state.claimable[0].0, "a");
    }

    /// ⚠️ 判定顺序是契约：**已领过优先于成功**（把已领过当失败会让界面反复报错）
    #[test]
    fn already_claimed_wins_over_success() {
        assert_eq!(
            classify_claim(&json!({
                "code": 0,
                "already_claimed": true,
                "message": "已领取"
            })),
            ClaimVerdict::AlreadyHeld {
                message: "已领取".to_string()
            }
        );
        assert_eq!(
            classify_claim(&json!({"success": true, "reward_amount": 10})),
            ClaimVerdict::Granted {
                message: String::new(),
                amount: Some("10".to_string())
            }
        );
        // 业务码非 0 → 被拒
        assert_eq!(
            classify_claim(&json!({"code": 40301, "message": "不在活动期"})),
            ClaimVerdict::Refused {
                message: "不在活动期".to_string()
            }
        );
        // 有文案没标记 → 被拒（上游常这么回规则说明）
        assert_eq!(
            classify_claim(&json!({"message": "名额已满"})),
            ClaimVerdict::Refused {
                message: "名额已满".to_string()
            }
        );
        // 空回执（HTTP 2xx + 空体）→ 成功但没回执
        assert_eq!(
            classify_claim(&json!({})),
            ClaimVerdict::Granted {
                message: String::new(),
                amount: None
            }
        );
    }

    /// 奖励金额：`/reward_summaries/0/reward_amount` 这条路径（[二进制] 逐字）
    #[test]
    fn reward_amount_follows_the_binary_path() {
        assert_eq!(
            reward_amount_of(&json!({"reward_summaries": [{"reward_amount": 15}]})),
            Some(15.0)
        );
        // 数字字符串也认
        assert_eq!(
            reward_amount_of(&json!({"reward_summaries": [{"reward_amount": "20.5"}]})),
            Some(20.5)
        );
        // 退路：data.reward_amount 一族
        assert_eq!(reward_amount_of(&json!({"data": {"reward_amount": 5}})), Some(5.0));
        assert_eq!(reward_amount_of(&json!({"unexpected": true})), None);
    }

    /// 登录态失效（401/403）在领取路径上要能识别成 401（走既有重试处置）
    #[test]
    fn claim_error_shapes_are_readable() {
        // 只测纯函数部分（网络路径由集成测试覆盖）
        let payload = json!({"message": "not logged in"});
        assert_eq!(readable_error(&payload).as_deref(), Some("not logged in"));
        assert_eq!(
            readable_error(&json!({"error_msg": "login required"})).as_deref(),
            Some("login required")
        );
        assert_eq!(readable_error(&json!({"code": 1})), None);
    }

    /// 连续天数：上游给了才带（不给就不编一个 0）
    #[test]
    fn continuous_days_is_optional() {
        let with = read_checkin_state(&json!({"continuousDays": 7}));
        assert_eq!(with.continuous_days, Some(7));
        let without = read_checkin_state(&json!({}));
        assert_eq!(without.continuous_days, None);
    }
}
