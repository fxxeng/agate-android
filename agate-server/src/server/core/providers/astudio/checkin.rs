//! AStudio 每日签到：`POST tenant-app/v2/init-app` 触发发放，前后余额差核对。
//!
//! ── 发奖动作（上游 3.4.2 实测修正）──────────────────────────
//! 旧实现靠「只读请求触发活跃」+ 前后余额差，实测**只读查询不发奖**，于是
//! 恒报「未观测到新发放」。真正动作是桌面端启动时那个无 body、带 Cookie 的
//! `POST tenant-app/v2/init-app`（返回 `banned`）。链路因此改成：读两个余额池
//! → init-app → 等 2.5 秒 → 再读两个余额池 → 通用 / 星火**独立核对**。
//! `init-app` 失败必须透出（会话失效 / 封禁 / 业务拒绝），不能折叠成「余额没涨」。
//!
//! ── 绝不谎称已签 ────────────────────────────────────────────
//! 余额没涨时如实报「未观测到新发放」。把「可能发了也可能没发」写成
//! `success: true` 会让签到台账变成一份没发生过的断言 —— 用户按台账找上游
//! 理论（「你明明记着我签了」）时，我们手里没有任何证据。反过来，报
//! `success: false` 的代价只是台账那天显示未领取，而明天照常会再试。
//!
//! ── 只有模型凭据的账号不参与 ────────────────────────────────
//! 没有网页会话就读不到余额，也就观测不了发放。这类账号**一次网络都不发**
//! 直接返回中性结果（`ineligible`）：硬发两个请求只会得到两条 401，
//! 而定时任务每天都会把它们重打一遍。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::time::Duration;

use serde_json::{json, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::core::proxies::ResolvedProxy;
use crate::server::errors::GatewayError;

use super::client::{self, StudioError};
use super::credentials::Credentials;
use super::usage::number_text;
use super::{endpoints, ID};

/// 触发活跃后等多久再读余额（上游参考实现同一取值）
const TOUCH_WAIT_MS: u64 = 2_500;

/// 结果行的固定字段（`billing::checkin` 的汇总只认 `success` / `msg`）
fn result(success: bool, message: String) -> Value {
    json!({
        "provider": ID,
        "success": success,
        "msg": message,
        "credit": Value::Null,
    })
}

/// 活动是否明确关闭（`dailyLoginEnabled == false`）
pub fn activity_off(config: &Value) -> bool {
    config.get("dailyLoginEnabled").and_then(Value::as_bool) == Some(false)
}

/// 两个余额池（通用 / 星火）。缺读数保留 `None`，不能当 0 —— 上游只发其中
/// 一个池时，另一个池的 0 会把它误判成「减少」。
#[derive(Clone, Copy)]
struct Balance {
    points: Option<f64>,
    spark: Option<f64>,
}

fn balance_number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|text| text.trim().parse::<f64>().ok()))
        })
        .filter(|number| number.is_finite() && *number >= 0.0)
}

fn balance_delta(before: Option<f64>, after: Option<f64>) -> Option<f64> {
    let delta = after? - before?;
    delta.is_finite().then_some(delta)
}

/// 前后余额 → 结论（**纯函数**，两个池独立核对）。
///
/// 三种「没涨」要区分开：没有读数（格式变了 / 接口换了字段）、持平（今天已发过
/// 或暂未到账）、倒退（积分被消费了）。它们的下一步动作不同，合并成一句
/// 「签到失败」等于把线索扔掉。只到账星火积分也算成功。
pub fn outcome(before: &Balance, after: &Balance) -> Value {
    let points = balance_delta(before.points, after.points);
    let spark = balance_delta(before.spark, after.spark);
    if points.is_none() && spark.is_none() {
        return result(false, "积分余额读数不完整（上游可能改了字段）".to_string());
    }
    let rewards: Vec<String> = [("通用积分", points), ("星火积分", spark)]
        .iter()
        .filter_map(|(label, delta)| {
            (*delta)
                .filter(|value| *value > 0.0)
                .map(|value| format!("{label} +{}", number_text(value)))
        })
        .collect();
    if !rewards.is_empty() {
        let credit = rewards.join("，");
        let mut row = result(true, format!("每日登录奖励已到账：{credit}"));
        row["credit"] = json!(credit);
        return row;
    }
    let drops: f64 = [points, spark]
        .iter()
        .filter_map(|delta| (*delta).filter(|value| *value < 0.0))
        .map(|delta| -delta)
        .sum();
    if drops > 0.0 {
        return result(
            false,
            format!(
                "余额没有增加：期间减少了 {} 积分（今天可能已发过，或积分被消费）",
                number_text(drops)
            ),
        );
    }
    result(
        false,
        "未观测到新发放：每日登录已执行，但积分无变化（可能今日已领取或暂未到账）"
            .to_string(),
    )
}

/// 读一次两个余额池；两个都缺才算读数不完整。
async fn read_balance(
    credentials: &Credentials,
    proxy: Option<&ResolvedProxy>,
) -> Result<Balance, StudioError> {
    let payload = client::studio_json(credentials, endpoints::POINTS_BALANCE, "积分余额", proxy).await?;
    Ok(Balance {
        points: balance_number(payload.get("totalBalance")),
        spark: balance_number(payload.get("sparkTotalBalance")),
    })
}

/// 每日登录积分的领取（自动签到框架的 claim）。
///
/// 返回 `{success, msg, credit}` —— 与别家 claim 同一形状（`billing::checkin`
/// 的汇总只认 `success` / `msg`）。
pub async fn claim_for_account(
    store: &AccountStore,
    account_id: &str,
) -> Result<Value, GatewayError> {
    let (_, credentials, proxy, _) = super::adapter::prepare(store, account_id).await?;
    claim_daily(&credentials, proxy.as_ref()).await
}

/// 同一个 claim 的核心（凭证已就绪）：见模块头。
pub async fn claim_daily(
    credentials: &Credentials,
    proxy: Option<&ResolvedProxy>,
) -> Result<Value, GatewayError> {
    if !credentials.has_session() {
        let mut row = result(
            false,
            "该账号只有模型凭据（无网页会话），无法执行每日登录积分".to_string(),
        );
        // `ineligible` 给台账看：这不是「今天没领到」，是「这个账号根本没这条链」
        row["ineligible"] = json!(true);
        return Ok(row);
    }
    // ① 先问活动还在不在：关掉之后每天发一串请求只会白跑（且可能被风控）
    let config = match client::studio_json(credentials, endpoints::PLAN_CONFIG, "签到配置", proxy).await
    {
        Ok(config) => config,
        Err(StudioError::Expired(message)) => {
            return Err(GatewayError::with_status(401, message))
        }
        Err(error) => {
            // 读不到配置 → 这次不签（不猜活动有没有），如实报原因
            return Ok(result(false, error.message().to_string()));
        }
    };
    if activity_off(&config) {
        let mut row = result(false, "当前没有每日登录积分活动".to_string());
        row["inactive"] = json!(true);
        return Ok(row);
    }
    // ② 签前读数
    let before = match read_balance(credentials, proxy).await {
        Ok(value) => value,
        Err(StudioError::Expired(message)) => {
            return Err(GatewayError::with_status(401, message))
        }
        Err(error) => return Ok(result(false, error.message().to_string())),
    };
    // ③ 发奖动作：POST init-app（只读请求实测不发奖）。失败必须透出 ——
    //    会话失效要用户重新登录，业务拒绝 / 封禁不能折叠成「余额没涨」。
    if let Err(error) = client::init_app(credentials, proxy).await {
        return match error {
            StudioError::Expired(message) => Err(GatewayError::with_status(401, message)),
            StudioError::Failed(message) => Ok(result(false, message)),
        };
    }
    tokio::time::sleep(Duration::from_millis(TOUCH_WAIT_MS)).await;
    // ④ 签后读数
    let after = match read_balance(credentials, proxy).await {
        Ok(value) => value,
        Err(StudioError::Expired(message)) => {
            return Err(GatewayError::with_status(401, message))
        }
        Err(error) => return Ok(result(false, error.message().to_string())),
    };
    Ok(outcome(&before, &after))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn balance(points: Option<f64>, spark: Option<f64>) -> Balance {
        Balance { points, spark }
    }

    /// 涨了才算成功，且写清涨了多少
    #[test]
    fn a_rise_means_claimed() {
        let row = outcome(&balance(Some(10.0), None), &balance(Some(22.5), None));
        assert_eq!(row["success"], true);
        assert_eq!(row["credit"], "通用积分 +12.5");
        assert!(row["msg"].as_str().unwrap_or("").contains("+12.5"), "{}", row["msg"]);
    }

    /// 两个余额池独立核对：只到账星火积分也算成功
    #[test]
    fn a_spark_only_rise_also_counts() {
        let row = outcome(
            &balance(Some(10.0), Some(3.0)),
            &balance(Some(10.0), Some(8.0)),
        );
        assert_eq!(row["success"], true);
        assert_eq!(row["credit"], "星火积分 +5");
    }

    /// 持平 = 未观测到发放（不能谎称已签）
    #[test]
    fn no_change_is_reported_as_unobserved() {
        let row = outcome(&balance(Some(10.0), None), &balance(Some(10.0), None));
        assert_eq!(row["success"], false);
        assert_eq!(row["credit"], Value::Null);
        assert!(
            row["msg"].as_str().unwrap_or("").contains("未观测到"),
            "{}",
            row["msg"]
        );
    }

    /// 倒退要说清是「减少了」而不是「没变化」
    #[test]
    fn a_drop_is_explained() {
        let row = outcome(&balance(Some(30.0), None), &balance(Some(18.0), None));
        assert_eq!(row["success"], false);
        assert!(row["msg"].as_str().unwrap_or("").contains("12"), "{}", row["msg"]);
    }

    /// 读数缺失 → 明确说读数不完整（不猜成 0，也不当成已签）
    #[test]
    fn missing_readings_are_not_zero() {
        let row = outcome(&balance(None, None), &balance(Some(10.0), None));
        assert_eq!(row["success"], false);
        assert!(row["msg"].as_str().unwrap_or("").contains("读数不完整"));
        assert_eq!(outcome(&balance(None, None), &balance(None, None))["success"], false);
    }

    /// 余额字段是字符串数字也要认（上游两种形态都出现过）
    #[test]
    fn balance_numbers_accept_string_values() {
        assert_eq!(balance_number(Some(&json!("12.5"))), Some(12.5));
        assert_eq!(balance_number(Some(&json!(-1))), None);
        assert_eq!(balance_number(None), None);
    }

    /// 活动开关：只有明确的 `false` 才算关（缺字段/其它值都当还开着）
    #[test]
    fn only_an_explicit_false_turns_the_activity_off() {
        assert!(activity_off(&json!({ "dailyLoginEnabled": false })));
        assert!(!activity_off(&json!({ "dailyLoginEnabled": true })));
        assert!(!activity_off(&json!({})));
        assert!(!activity_off(&json!({ "dailyLoginEnabled": "false" })));
    }
}
