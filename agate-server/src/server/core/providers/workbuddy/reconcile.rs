//! WorkBuddy 不确定签到的只读状态核验。
//!
//! 国内版读取 `BillingService::get_checkin_status()` 归一后的 `checkedIn`；
//! 国际版读取 `todayCheckedIn`，并要求 `statusAvailable=true`。
//! 两个状态都是“当前北京时间自然日”的业务状态，不是某次 HTTP 请求的归因证明。
//!
//! 因此只有执行开始日与查询观察日相同、且上游明确返回当日已签到时，才生成
//! `confirmed` 证据。跨日、字段缺失、查询失败或 false 都只能是 `unknown`。
//! 本模块只负责纯判定，不发请求、不修改台账。

use serde_json::{json, Value};

use super::region::Region;

/// 根据 WorkBuddy 当前签到状态构造对账证据。
///
/// `status` 是 `BillingService::get_checkin_status(Some(account_session))` 的结果。
pub fn classify_status(
    region: Region,
    run_started_at_ms: i64,
    observed_at_ms: i64,
    status: &Value,
) -> Value {
    let provider = region.provider_id();
    let unknown = |reason: &str| {
        json!({
            "verdict": "unknown",
            "provider": provider,
            "evidenceType": "checkin_status",
            "observedAt": observed_at_ms,
            "reason": reason,
        })
    };

    if run_started_at_ms <= 0 || observed_at_ms <= 0 {
        return unknown("签到运行时间或状态观察时间无效");
    }
    let Some(run_day) = crate::server::core::beijing::date_key_of_ms(run_started_at_ms) else {
        return unknown("无法解析签到运行日期");
    };
    let Some(observed_day) = crate::server::core::beijing::date_key_of_ms(observed_at_ms) else {
        return unknown("无法解析状态观察日期");
    };
    if run_day != observed_day {
        return unknown("签到运行日与当前状态观察日不同；当前状态不能证明历史日期的结果");
    }

    let checked_in = match region {
        Region::Cn => status.get("checkedIn").and_then(Value::as_bool),
        Region::Intl => {
            if status.get("statusAvailable").and_then(Value::as_bool) != Some(true) {
                return json!({
                    "verdict": "unknown",
                    "provider": provider,
                    "evidenceType": "checkin_status",
                    "observedAt": observed_at_ms,
                    "statusAvailable": false,
                    "reason": "国际版签到状态接口未明确报告可用",
                });
            }
            status.get("todayCheckedIn").and_then(Value::as_bool)
        }
    };
    match checked_in {
        Some(true) => json!({
            "verdict": "confirmed",
            "provider": provider,
            "evidenceType": "checkin_status",
            "observedAt": observed_at_ms,
            "businessDay": run_day,
            "statusAvailable": if region == Region::Intl { Value::Bool(true) } else { Value::Null },
            "upstreamCheckedIn": true,
            "upstreamStatus": "CHECKED_IN",
            "reason": "WorkBuddy 上游状态明确显示该北京时间自然日已签到；此证据确认业务结果，不归因到某次具体请求",
        }),
        Some(false) => json!({
            "verdict": "unknown",
            "provider": provider,
            "evidenceType": "checkin_status",
            "observedAt": observed_at_ms,
            "businessDay": run_day,
            "statusAvailable": if region == Region::Intl { Value::Bool(true) } else { Value::Null },
            "upstreamCheckedIn": false,
            "upstreamStatus": "NOT_CHECKED_IN",
            "reason": "WorkBuddy 上游当前状态未显示当日已签到；不能据此证明先前请求未执行",
        }),
        None => unknown("WorkBuddy 上游响应缺少明确的当日签到布尔字段"),
    }
}

#[cfg(test)]
mod tests {
    use super::{classify_status, Region};
    use serde_json::json;

    fn same_day_ms() -> (i64, i64) {
        let now = crate::server::logging::now_ms();
        (now, now)
    }

    #[test]
    fn cn_checked_in_today_is_positive_business_evidence() {
        let (started, observed) = same_day_ms();
        let evidence = classify_status(Region::Cn, started, observed, &json!({"checkedIn": true}));
        assert_eq!(evidence["verdict"], "confirmed");
        assert_eq!(evidence["businessDay"], crate::server::core::beijing::date_key_of_ms(started).unwrap_or_default());
        assert_eq!(evidence["upstreamCheckedIn"], true);
    }

    #[test]
    fn intl_requires_available_status_and_today_checked_in() {
        let (started, observed) = same_day_ms();
        let positive = classify_status(
            Region::Intl, started, observed,
            &json!({"statusAvailable": true, "todayCheckedIn": true}),
        );
        assert_eq!(positive["verdict"], "confirmed");
        let unavailable = classify_status(
            Region::Intl, started, observed,
            &json!({"statusAvailable": false, "todayCheckedIn": true}),
        );
        assert_eq!(unavailable["verdict"], "unknown");
    }

    #[test]
    fn false_or_cross_day_status_never_confirms() {
        let (started, observed) = same_day_ms();
        let negative = classify_status(Region::Cn, started, observed, &json!({"checkedIn": false}));
        assert_eq!(negative["verdict"], "unknown");
        let previous_day = started.saturating_sub(24 * 60 * 60 * 1000);
        let cross_day = classify_status(Region::Cn, previous_day, observed, &json!({"checkedIn": true}));
        assert_eq!(cross_day["verdict"], "unknown");
        assert!(cross_day["reason"].as_str().unwrap_or("").contains("不同"));
    }
}
