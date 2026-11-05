//! AStudio 余额：网页会话读 `points/balance` 与 `membership/me`。
//!
//! ── 两种账号形态 ────────────────────────────────────────────
//! ```text
//!   有网页会话   →  通用积分 / 星火积分 / 会员档 / 到期时间（真实读数）
//!   只有模型凭据 →  用目录接口探活，如实说「无网页会话，读不到积分」
//! ```
//! 所有读取都不计费；只有模型调用才消耗积分。第二种形态**不是故障**（见
//! `credentials` 的模块头），所以它给的是中性结果 + `noSession` 标记，
//! 不是一个红色错误 —— 用户看到「账号坏了」会去删账号，而其实只是没粘会话。
//!
//! ── 单项失败不塌方，但会话失效必须透传 ──────────────────────
//! 余额与会员是两条接口：一条挂了另一条还能显示。但如果其中任何一条说
//! **会话失效**，整体就必须报 401 —— 否则用户看到的是「账号信息读取失败」，
//! 而不是「请重新登录」，排查方向完全错。
//!
//! ── `available` 为什么只取通用积分 ──────────────────────────
//! 上游把「通用积分」与「星火积分」分成两个钱包，说明它们不是同一口径
//! （星火积分很可能只能在星火模型上用）。把两者相加当成「可用」会夸大额度，
//! 让用户以为能在任意模型上花；所以 `available` 只取主钱包，`availableView`
//! 把两者都写出来 —— 与 kiro 那条「Bonus 不参与可用性判断」同一条纪律。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::proxies::ResolvedProxy;
use crate::server::errors::GatewayError;

use super::client::{self, StudioError};
use super::credentials::Credentials;
use super::{endpoints, ID};

/// 展示单位
const UNIT: &str = "积分";

fn number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        })
        .filter(|value| value.is_finite())
}

/// 数字的短写法（12 / 12.5 / 0.25，不要 `12.50` 那种假精度）
///
/// `pub(super)`：签到那边报「这次领到多少积分」用的是同一个格式化器 ——
/// 两处各写一份会让「+12.5」与「+12.50」在不同界面出现。
pub(super) fn number_text(value: f64) -> String {
    if value.fract().abs() < 0.005 {
        return format!("{}", value.round() as i64);
    }
    format!("{value:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

/// epoch → 毫秒。
///
/// 上游的时间戳单位不统一（有的给秒、有的给毫秒、有的给字符串），这里只做
/// **一个**判定：小于 `1e11` 的当秒（那是 1973 年，毫秒时间戳不会这么小），
/// 其余当毫秒。判不出来（非数字）返回 `None` —— 宁可少一个字段，也不把一个
/// 单位不明的数字塞进毫秒字段让前端渲染成 1970 年。
fn to_ms(value: Option<&Value>) -> Option<i64> {
    let raw = number(value)?;
    if raw <= 0.0 {
        return None;
    }
    let millis = if raw < 1e11 { raw * 1000.0 } else { raw };
    Some(millis as i64)
}

/// 一条窗口读数（界面按 `windows[]` 一格画一条）
fn window(name: &str, value: f64) -> Value {
    json!({
        "name": name,
        "label": name,
        "balance": value,
        "remaining": value,
        "display": number_text(value),
        "unit": UNIT,
        "aside": false,
    })
}

/// 把两条接口的返回折成网关的用量报告（**纯函数**）。
///
/// `failures` 是「不致命但值得说」的失败文案（网络 / 5xx）。会话失效不进这里
/// —— 那条路在 [`query`] 里已经变成 401 了。
pub fn report(balance: Option<&Value>, membership: Option<&Value>, failures: &[String]) -> Value {
    let mut windows: Vec<Value> = Vec::new();
    let mut wallets: Vec<Value> = Vec::new();
    let general = balance.and_then(|value| number(value.get("totalBalance")));
    let spark = balance.and_then(|value| number(value.get("sparkTotalBalance")));
    if let Some(general) = general {
        let mut row = window("通用积分", general);
        // 活动积分有到期时刻时标出来：它不改变余额数字，但决定了「这些积分
        // 什么时候会消失」。数字与字符串两种形态都原样带上（见 `to_ms`）
        if let Some(activity) = balance.and_then(|value| number(value.get("activityBalance"))) {
            if activity > 0.0 {
                row["runsOut"] = json!(activity);
                if let Some(at) = balance
                    .and_then(|value| value.get("activityNextExpireTime"))
                    .filter(|value| !value.is_null())
                {
                    match to_ms(Some(at)) {
                        Some(millis) => row["runsOutAt"] = json!(millis),
                        // 不是数字（可能是 ISO 串）：原样放在另一个键里，
                        // 不污染 `runsOutAt` 的毫秒语义
                        None => {
                            if let Some(text) = at.as_str().filter(|text| !text.trim().is_empty()) {
                                row["runsOutText"] = json!(text.trim());
                            }
                        }
                    }
                }
            }
        }
        windows.push(row);
        wallets.push(json!({ "type": "general", "displayName": "通用积分", "balance": general }));
    }
    if let Some(spark) = spark {
        windows.push(window("星火积分", spark));
        wallets.push(json!({ "type": "spark", "displayName": "星火积分", "balance": spark }));
    }

    let mut out = json!({
        "provider": ID,
        "unit": UNIT,
        "windows": windows,
        "wallets": wallets,
    });
    // 主钱包当「可用」：见模块头（星火积分口径不同，不许相加）
    if let Some(general) = general {
        out["available"] = json!(general);
    } else if let Some(spark) = spark {
        out["available"] = json!(spark);
    }
    if let Some(text) = available_view(general, spark) {
        out["availableView"] = json!(text);
    }

    let mut plan = String::new();
    if let Some(membership) = membership {
        if let Some(name) = membership
            .get("planName")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            plan = name.to_string();
        }
        if let Some(uid) = membership.get("uid").filter(|value| !value.is_null()) {
            out["user"] = json!(uid.to_string());
        }
        let mut subscription = json!({ "planName": plan });
        if let Some(expire) = to_ms(membership.get("endTime")) {
            subscription["expireAt"] = json!(expire);
        }
        if plan.is_empty() {
            subscription["planName"] = json!("AStudio");
        }
        out["subscription"] = subscription;
    }
    // 没有会员信息时给一个中性档名（界面那一格不该是空白）
    out["plan"] = json!(if plan.is_empty() {
        "AStudio".to_string()
    } else {
        plan.clone()
    });

    // 余额接口通了但一个可识别的数字都没有：这不是「0 积分」，是「读法不对了」。
    // 如实标注，别让界面显示成 0（用户会以为积分没了）
    if windows.is_empty() && failures.is_empty() {
        if balance.is_some() {
            out["error"] = json!("余额接口没有返回可识别的积分字段（上游格式可能变了）");
        } else if membership.is_none() {
            out["error"] = json!("AStudio 没有返回可用的账号信息");
        }
    } else if !failures.is_empty() {
        out["error"] = json!(failures.join("；"));
    }
    out
}

/// 「可用」的紧凑展示串（两个钱包都在时都写出来）
fn available_view(general: Option<f64>, spark: Option<f64>) -> Option<String> {
    match (general, spark) {
        (Some(general), Some(spark)) => Some(format!(
            "通用 {} · 星火 {} {UNIT}",
            number_text(general),
            number_text(spark)
        )),
        (Some(general), None) => Some(format!("剩 {} {UNIT}", number_text(general))),
        (None, Some(spark)) => Some(format!("剩 {} {UNIT}", number_text(spark))),
        (None, None) => None,
    }
}

/// 查一次余额。
pub async fn query(
    credentials: &Credentials,
    proxy: Option<&ResolvedProxy>,
) -> Result<Value, GatewayError> {
    if !credentials.has_session() {
        return probe(credentials, proxy).await;
    }
    let (balance, membership) = tokio::join!(
        client::studio_json(
            credentials,
            endpoints::POINTS_BALANCE,
            "积分余额",
            proxy
        ),
        client::studio_json(credentials, endpoints::MEMBERSHIP_ME, "会员信息", proxy),
    );
    // 会话失效优先：任一接口说会话没了，整体就是「请重新登录」
    for outcome in [&balance, &membership] {
        if let Err(StudioError::Expired(message)) = outcome {
            return Err(GatewayError::with_status(401, message.clone()));
        }
    }
    let mut failures: Vec<String> = Vec::new();
    let balance = match balance {
        Ok(value) => Some(value),
        Err(error) => {
            failures.push(error.message().to_string());
            None
        }
    };
    let membership = match membership {
        Ok(value) => Some(value),
        Err(error) => {
            failures.push(error.message().to_string());
            None
        }
    };
    Ok(report(balance.as_ref(), membership.as_ref(), &failures))
}

/// 只有模型凭据的账号：用目录接口探活（读取免费）。
///
/// 这条路的产出**故意**不是错误：账号能对话，只是没有可读的积分。给一句
/// 中性说明 + `noSession`，前端把它画成「未配置」而不是「失败」。
async fn probe(
    credentials: &Credentials,
    proxy: Option<&ResolvedProxy>,
) -> Result<Value, GatewayError> {
    let mut out = json!({
        "provider": ID,
        "plan": "AStudio",
        "unit": UNIT,
        "windows": [],
        "wallets": [],
        "noSession": true,
        "availableView": "无网页会话",
    });
    match client::catalog(&credentials.key, proxy).await {
        Ok(_) => {
            out["note"] = json!("该账号只有模型凭据（无网页会话），读不到积分；对话不受影响");
        }
        Err(StudioError::Expired(message)) => {
            // 凭据被拒是真问题：模型调用也会失败，必须让用户重贴
            return Err(GatewayError::with_status(401, message));
        }
        Err(error) => {
            out["error"] = json!(format!("AStudio 不可达：{}", error.message()));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn balance(total: f64, spark: Option<f64>) -> Value {
        let mut payload = json!({ "totalBalance": total });
        if let Some(spark) = spark {
            payload["sparkTotalBalance"] = json!(spark);
        }
        payload
    }

    /// 两个钱包各自成行；`available` 只取通用积分（不相加，见模块头）
    #[test]
    fn the_two_wallets_stay_separate_and_only_one_is_available() {
        let membership = json!({ "planName": "星火会员", "uid": 10086 });
        let out = report(
            Some(&balance(12.5, Some(30.0))),
            Some(&membership),
            &[],
        );
        assert_eq!(out["provider"], ID);
        assert_eq!(out["unit"], "积分");
        let windows = out["windows"].as_array().expect("有窗口");
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0]["name"], "通用积分");
        assert_eq!(windows[0]["display"], "12.5");
        assert_eq!(windows[1]["name"], "星火积分");
        // 可用 = 主钱包，不是 42.5
        assert_eq!(out["available"], 12.5);
        assert_eq!(out["availableView"], "通用 12.5 · 星火 30 积分");
        assert_eq!(out["plan"], "星火会员");
        assert_eq!(out["user"], "10086");
        assert_eq!(out["subscription"]["planName"], "星火会员");
    }

    /// 只有主钱包时展示串不说「星火」
    #[test]
    fn a_single_wallet_is_reported_alone() {
        let out = report(Some(&balance(0.0, None)), None, &[]);
        assert_eq!(out["windows"][0]["display"], "0");
        assert_eq!(out["available"], 0.0);
        assert_eq!(out["availableView"], "剩 0 积分");
        assert_eq!(out["plan"], "AStudio", "没有会员信息时的兜底档名");
        assert!(out.get("subscription").is_none());
    }

    /// 活动积分的到期时刻：数字按单位归一，字符串原样放另一个键
    #[test]
    fn activity_expiry_is_normalised_or_kept_as_text() {
        let seconds = json!({
            "totalBalance": 10,
            "activityBalance": 5,
            "activityNextExpireTime": 1_800_000_000,
        });
        let out = report(Some(&seconds), None, &[]);
        assert_eq!(out["windows"][0]["runsOut"].as_f64(), Some(5.0));
        assert_eq!(
            out["windows"][0]["runsOutAt"],
            1_800_000_000_000i64,
            "秒要换成毫秒"
        );

        let text = json!({
            "totalBalance": 10,
            "activityBalance": 5,
            "activityNextExpireTime": "2026-12-01 00:00:00",
        });
        let out = report(Some(&text), None, &[]);
        assert!(out["windows"][0].get("runsOutAt").is_none(), "不把字符串塞进毫秒字段");
        assert_eq!(out["windows"][0]["runsOutText"], "2026-12-01 00:00:00");
    }

    /// 活动积分为 0 时不标到期（否则界面会出现「0 积分即将过期」）
    #[test]
    fn zero_activity_is_not_an_expiry() {
        let payload = json!({
            "totalBalance": 10,
            "activityBalance": 0,
            "activityNextExpireTime": 1_800_000_000,
        });
        let out = report(Some(&payload), None, &[]);
        assert!(out["windows"][0].get("runsOutAt").is_none());
        assert!(out["windows"][0].get("runsOut").is_none());
    }

    /// 一条接口挂了另一条照常显示，但要把失败说出来
    #[test]
    fn one_broken_endpoint_does_not_break_the_other() {
        let out = report(
            Some(&balance(8.0, None)),
            None,
            &["会员信息网络失败：timed out".to_string()],
        );
        assert_eq!(out["windows"][0]["balance"], 8.0);
        assert_eq!(out["error"], "会员信息网络失败：timed out");
        // 两条都挂了 → 空报告 + 失败文案
        let out = report(None, None, &["a".to_string(), "b".to_string()]);
        assert_eq!(out["windows"].as_array().map(Vec::len), Some(0));
        assert_eq!(out["error"], "a；b");
    }

    /// 余额通了但没有可识别字段：如实标「格式不对」，不能显示成 0 积分
    #[test]
    fn an_unreadable_balance_is_called_out() {
        let out = report(Some(&json!({ "data": {} })), None, &[]);
        assert!(out.get("available").is_none(), "读不到就不给 available");
        assert!(
            out["error"].as_str().unwrap_or("").contains("可识别"),
            "{}",
            out["error"]
        );
    }

    /// 会员到期时间也按同一条单位判定
    #[test]
    fn membership_expiry_is_converted() {
        let membership = json!({ "planName": "Pro", "endTime": 1_800_000_000_000i64 });
        let out = report(Some(&balance(1.0, None)), Some(&membership), &[]);
        assert_eq!(out["subscription"]["expireAt"], 1_800_000_000_000i64);
        let seconds = json!({ "planName": "Pro", "endTime": 1_800_000_000 });
        let out = report(Some(&balance(1.0, None)), Some(&seconds), &[]);
        assert_eq!(out["subscription"]["expireAt"], 1_800_000_000_000i64);
    }

    /// 数字短写法（不要 12.50 / 12.00 这类假精度）
    #[test]
    fn numbers_are_shortened() {
        assert_eq!(number_text(12.0), "12");
        assert_eq!(number_text(12.5), "12.5");
        assert_eq!(number_text(12.345), "12.35");
        assert_eq!(number_text(0.0), "0");
    }
}
