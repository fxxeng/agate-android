//! 营销活动**只读发现**与领取后的**确认回执**。
//!
//! ── 为什么要有这条链路（2026-11-03 起）───────────────────────
//! 上游把每日赠送套餐改由 `GET /api/v1/marketing/touch` 下发，
//! `billing/preview` 可能整段为空 —— 只查 billing 会漏掉**只在营销活动里
//! 出现**的套餐（周末活动 / 1024 Dev Day 这类）。因此它与 billing 探测
//! 并列为发现源（见父模块 `discover`）。
//!
//! ── 只读取元数据，不加载营销资源 ─────────────────────────────
//! `deliveries[]` 里带 banner / popup / 图片等富资源。本模块只做两件事：
//!   1. 递归扫出形如套餐的节点（有 `plan_id` 者）当元数据；
//!   2. 在几个固定按钮路径里找 `claim_zcode_plan` 动作，取它的 `plan_id`。
//! 不下载、不执行任何营销素材 —— 那既用不上，又扩大攻击面。
//!
//! ── 版本口径 ────────────────────────────────────────────────
//! 用父模块的 `activity_app_version()`（默认 3.18.0）：营销链路实测
//! 3.16.3 返回 0 条、3.17.1+ 才有交付。推理的 3.16.2 只服务 anthropic 端点。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::server::core::auth_http::send_raw;
use crate::server::core::proxies::ResolvedProxy;
use crate::server::errors::GatewayError;

// marketing 是 claim 的子模块：claim 自身的符号用 `super::`，而 region 是 claim
// 的父模块 zcode 下的兄弟模块，要走 `super::super::`（与 plan.rs 的测试模块同范式）。
use super::super::region::Region;
use super::{
    activity_app_version, error_message, parse_plan, platform, ClaimablePlan, REQUEST_TIMEOUT_MS,
};

const QUERY_PATH: &str = "/api/v1/marketing/touch";
const REPORT_PATH: &str = "/api/v1/marketing/touch/action";
/// 递归扫描元数据节点的深度 / 节点数上限（防御畸形或恶意的上游响应）。
const MAX_DEPTH: usize = 12;
const MAX_NODES: usize = 4096;
/// 单批活动数上限。
const MAX_DELIVERIES: usize = 32;

/// 营销接口共用的头（与上游 `headers` 逐条对齐）。
///
/// `X-Client-Language: zh-CN` 是营销接口要求的固定值；版本号用活动口径。
/// jwt 为空时发匿名请求（允许匿名发现活动），设备标识只在有值时带。
fn headers(jwt: &str, device_mid: Option<&str>) -> Vec<(String, String)> {
    let mut headers = vec![
        ("X-Client-Language".to_string(), "zh-CN".to_string()),
        ("X-ZCode-App-Version".to_string(), activity_app_version()),
        ("X-Platform".to_string(), platform().to_string()),
    ];
    if !jwt.trim().is_empty() {
        headers.push((
            "Authorization".to_string(),
            format!("Bearer {}", jwt.trim()),
        ));
    }
    if let Some(mid) = device_mid.map(str::trim).filter(|value| !value.is_empty()) {
        headers.push(("X-Device-Mid".to_string(), mid.to_string()));
    }
    headers
}

/// 从上游 JSON 里取一个受控的字符串标识（拒绝空串 / 控制字符 / 超长）。
fn identifier(value: Option<&Value>) -> Option<&str> {
    let text = value?.as_str()?.trim();
    (!text.is_empty() && text.len() <= 128 && !text.chars().any(char::is_control)).then_some(text)
}

/// 数字或数字字符串 → i64（上游时间字段两种形态都出现过）。
fn integer(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// 查询营销活动里可领取的套餐。
///
/// `404` = 这一区没部署营销接口，按「没有套餐」处理（与 billing 的
/// `NotDeployed` 同义，但营销这一路用空列表表达即可 —— 调用方只看合并结果）。
pub(super) async fn query(
    region: Region,
    jwt: &str,
    device_mid: Option<&str>,
    proxy: Option<&ResolvedProxy>,
) -> Result<Vec<ClaimablePlan>, GatewayError> {
    let url = format!("{}{QUERY_PATH}?seq=0", region.zcode_origin());
    let response = send_raw(
        "GET",
        &url,
        None,
        &headers(jwt, device_mid),
        proxy,
        Some(REQUEST_TIMEOUT_MS),
    )
    .await
    .map_err(|error| {
        if error.is_timeout() {
            GatewayError::with_status(504, "营销套餐查询超时")
        } else {
            GatewayError::with_status(502, format!("营销套餐查询失败：{error}"))
        }
    })?;
    if response.status == 404 {
        return Ok(Vec::new());
    }
    let payload = response
        .payload
        .ok_or_else(|| GatewayError::with_status(502, "营销套餐查询：上游响应不是 JSON"))?;
    let code = payload.get("code").and_then(integer);
    if !(200..300).contains(&response.status)
        || code != Some(0)
        || payload.get("success").and_then(Value::as_bool) == Some(false)
    {
        let status = if response.status == 401 || code == Some(401) {
            401
        } else {
            502
        };
        return Err(GatewayError::with_status(
            status,
            format!(
                "营销套餐查询失败：{}",
                error_message(&payload, response.status)
            ),
        )
        .with_optional_code(code));
    }
    let deliveries = payload
        .pointer("/data/deliveries")
        .and_then(Value::as_array)
        .ok_or_else(|| GatewayError::with_status(502, "营销套餐查询：上游未返回有效的活动列表"))?;
    if deliveries.len() > MAX_DELIVERIES {
        return Err(GatewayError::with_status(
            502,
            "营销套餐查询：活动列表超过上限",
        ));
    }

    Ok(parse_deliveries(deliveries))
}

/// 从 `data.deliveries[]` 解析出可领套餐（纯函数，便于用抓下来的响应钉判据）。
fn parse_deliveries(deliveries: &[Value]) -> Vec<ClaimablePlan> {
    let mut plans: Vec<ClaimablePlan> = Vec::new();
    for delivery in deliveries {
        let Some(campaign) = identifier(delivery.get("campaign_id")) else {
            continue;
        };
        // 先把这一条 delivery 里所有「像套餐」的节点扫成元数据表，后面按钮
        // 点名 plan_id 时用它补全名字 / 权益 / 起止时间。
        let mut metadata = BTreeMap::<String, ClaimablePlan>::new();
        let mut budget = MAX_NODES;
        collect_metadata(delivery, 0, &mut budget, &mut metadata);
        let priority = delivery.get("priority").and_then(integer).unwrap_or(0);
        let mut found = Vec::new();
        // 三个固定的按钮路径覆盖 banner / 成功弹窗 / 普通弹窗（上游同款）。
        for path in [
            "/banner/buttons",
            "/banner/success_popup/buttons",
            "/popup/buttons",
        ] {
            let Some(buttons) = delivery.pointer(path).and_then(Value::as_array) else {
                continue;
            };
            for button in buttons {
                let Some(action) = button.get("action") else {
                    continue;
                };
                if action.get("type").and_then(Value::as_str) != Some("claim_zcode_plan") {
                    continue;
                }
                let Some(plan_id) = identifier(action.pointer("/args/plan_id")) else {
                    continue;
                };
                let mut plan = metadata
                    .get(plan_id)
                    .cloned()
                    .unwrap_or_else(|| ClaimablePlan {
                        plan_id: plan_id.to_string(),
                        name: plan_id.to_string(),
                        description: String::new(),
                        priority,
                        starts_at: None,
                        ends_at: None,
                        entitlements: Vec::new(),
                        marketing_campaign_ids: Vec::new(),
                    });
                if plan.priority == 0 {
                    plan.priority = priority;
                }
                if !plan.marketing_campaign_ids.iter().any(|id| id == campaign) {
                    plan.marketing_campaign_ids.push(campaign.to_string());
                }
                found.push(plan);
            }
        }
        super::merge_plans(&mut plans, found);
    }
    plans
}

/// 递归扫描一个 delivery 节点，把「带 plan_id 的对象」解析成套餐元数据。
///
/// 深度 / 节点数双预算：营销资源是一棵可能很深的 JSON 树，无界递归在畸形响应
/// 下会拖垮进程（release 是 panic=abort，栈溢出不可恢复）。
fn collect_metadata(
    node: &Value,
    depth: usize,
    budget: &mut usize,
    metadata: &mut BTreeMap<String, ClaimablePlan>,
) {
    if depth > MAX_DEPTH || *budget == 0 {
        return;
    }
    *budget -= 1;
    match node {
        Value::Array(items) => {
            for item in items {
                collect_metadata(item, depth + 1, budget, metadata);
                if *budget == 0 {
                    break;
                }
            }
        }
        Value::Object(fields) => {
            if let Some(plan) = parse_plan(node) {
                match metadata.get_mut(&plan.plan_id) {
                    Some(existing) => super::merge_plan_fields(existing, plan),
                    None => {
                        metadata.insert(plan.plan_id.clone(), plan);
                    }
                }
            }
            for value in fields.values() {
                collect_metadata(value, depth + 1, budget, metadata);
                if *budget == 0 {
                    break;
                }
            }
        }
        _ => {}
    }
}

/// 领取 settle（成功 / 已领过）后，按 campaign id 逐个回执 `touch/action`。
///
/// ── 失败不阻断、也不重试 ─────────────────────────────────────
/// 回执是「告诉上游这个活动按钮已被消费」，套餐额度在 `claim` 那一步已经到账。
/// 回执失败只记日志、把原因带进响应的 `marketingError`，**不改领取结果**；
/// 也不自动重发、更不以 cancel 代替失败（上游同款口径 —— 重复回执可能重复计数）。
/// 因此返回 `Result<(), String>` 而不是 `GatewayError`：它是附加动作，不是主流程。
pub(super) async fn confirm(
    region: Region,
    jwt: &str,
    device_mid: Option<&str>,
    proxy: Option<&ResolvedProxy>,
    campaign_id: &str,
) -> Result<(), String> {
    let url = format!("{}{REPORT_PATH}", region.zcode_origin());
    let body = json!({ "campaign_id": campaign_id, "action_type": "confirm" });
    let response = send_raw(
        "POST",
        &url,
        Some(&body),
        &headers(jwt, device_mid),
        proxy,
        Some(REQUEST_TIMEOUT_MS),
    )
    .await
    .map_err(|error| format!("营销回执失败：{error}"))?;
    let payload = response
        .payload
        .ok_or_else(|| "营销回执响应不是 JSON".to_string())?;
    if !(200..300).contains(&response.status)
        || payload.get("code").and_then(integer) != Some(0)
        || payload.get("success").and_then(Value::as_bool) == Some(false)
    {
        return Err(format!(
            "营销回执失败：{}",
            error_message(&payload, response.status)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod vectors {
    //! 营销发现的解析判据：只认 `claim_zcode_plan` 按钮、campaign id 传递、
    //! 元数据递归补全、同一 plan_id 去重。喂的是按上游真实形状构造的响应。

    use serde_json::json;

    use super::parse_deliveries;

    #[test]
    fn a_claim_button_yields_a_plan_with_campaign_id_and_metadata() {
        let plans = parse_deliveries(&[json!({
            "campaign_id": "wknd-1108",
            "priority": 7,
            "banner": {
                "buttons": [
                    { "action": { "type": "open_link", "args": { "plan_id": "ignored" } } },
                    { "action": { "type": "claim_zcode_plan", "args": { "plan_id": "plan-a" } } }
                ]
            },
            // 元数据藏在深层节点里，靠递归扫出来补全名字 / 权益
            "payload": { "nested": { "plans": [{
                "plan_id": "plan-a", "name": "周末 1 亿",
                "entitlements": [{ "show_name": "体验额度", "unit_type": "tokens", "grant_units": 100000000 }]
            }] } }
        })]);
        assert_eq!(1, plans.len(), "open_link 不算 claim 按钮");
        let plan = &plans[0];
        assert_eq!("plan-a", plan.plan_id);
        assert_eq!("周末 1 亿", plan.name, "递归扫到的元数据补全了默认名");
        assert_eq!(7, plan.priority);
        assert_eq!(vec!["wknd-1108".to_string()], plan.marketing_campaign_ids);
        assert_eq!(1, plan.entitlements.len());
        assert_eq!(100000000, plan.entitlements[0].grant_units);
    }

    #[test]
    fn all_three_button_paths_are_recognized_and_deduplicated() {
        // 三条按钮路径分列三条 delivery（同一 campaign），逐一验证都被识别。
        let plans = parse_deliveries(&[
            json!({ "campaign_id": "camp", "priority": 3,
                "banner": { "buttons": [
                    { "action": { "type": "claim_zcode_plan", "args": { "plan_id": "p1" } } }
                ] } }),
            json!({ "campaign_id": "camp", "priority": 3,
                "banner": { "success_popup": { "buttons": [
                    { "action": { "type": "claim_zcode_plan", "args": { "plan_id": "p2" } } }
                ] } } }),
            json!({ "campaign_id": "camp", "priority": 3,
                "popup": { "buttons": [
                    { "action": { "type": "claim_zcode_plan", "args": { "plan_id": "p1" } } },
                    { "action": { "type": "claim_zcode_plan", "args": { "plan_id": "p3" } } }
                ] } }),
        ]);
        let ids: Vec<&str> = plans.iter().map(|p| p.plan_id.as_str()).collect();
        assert!(ids.contains(&"p1"), "banner 路径");
        assert!(ids.contains(&"p2"), "success_popup 路径");
        assert!(ids.contains(&"p3"), "popup 路径");
        assert_eq!(
            ids.iter().filter(|id| **id == "p1").count(),
            1,
            "跨 delivery 同 plan_id 去重"
        );
    }

    #[test]
    fn deliveries_without_campaign_or_claim_button_are_ignored() {
        let plans = parse_deliveries(&[
            json!({ "priority": 5, "popup": { "buttons": [
                { "action": { "type": "claim_zcode_plan", "args": { "plan_id": "p1" } } }
            ] } }), // 缺 campaign_id
            json!({ "campaign_id": "c2", "popup": { "buttons": [
                { "action": { "type": "other", "args": { "plan_id": "p2" } } }
            ] } }), // 无 claim 按钮
        ]);
        assert!(plans.is_empty());
    }
}
