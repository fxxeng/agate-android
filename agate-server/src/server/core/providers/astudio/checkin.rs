//! AStudio 每日签到：上游**没有领取接口**，签到 = 当天会话首次活跃时的发放。
//!
//! ── 为什么只能靠「前后余额差」观测 ──────────────────────────
//! 桌面端的弹窗只做通知：服务端在账号会话当天首次活跃时直接加积分。没有可
//! 调用的领取接口，就只能：读余额 → 发几个与桌面端启动同类的只读请求
//! （触发活跃）→ 等 2.5 秒 → 再读余额。**涨了才算签到成功**。
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

/// 前后余额 → 结论（**纯函数**）。
///
/// 三种「没涨」要区分开：没有读数（格式变了 / 接口换了字段）、持平（今天已发过
/// 或发放挂在桌面端登录上）、倒退（积分被消费了）。它们的下一步动作不同，
/// 合并成一句「签到失败」等于把线索扔掉。
pub fn outcome(before: Option<f64>, after: Option<f64>) -> Value {
    let (Some(before), Some(after)) = (before, after) else {
        return result(false, "积分余额读数不完整（上游可能改了字段）".to_string());
    };
    let delta = after - before;
    if delta > 0.0 {
        let mut row = result(true, format!("每日登录积分 +{}", number_text(delta)));
        row["credit"] = json!(number_text(delta));
        return row;
    }
    if delta < 0.0 {
        return result(
            false,
            format!(
                "余额没有增加：期间减少了 {} 积分（今天可能已发过，或积分被消费）",
                number_text(-delta)
            ),
        );
    }
    result(
        false,
        "未观测到新发放：余额无变化（发放可能挂靠桌面端登录）".to_string(),
    )
}

/// 读一次通用积分总额
async fn total(
    credentials: &Credentials,
    proxy: Option<&ResolvedProxy>,
) -> Result<Option<f64>, StudioError> {
    let payload = client::studio_json(credentials, endpoints::POINTS_BALANCE, "积分余额", proxy).await?;
    Ok(payload
        .get("totalBalance")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite()))
}

/// 每日登录积分的领取（自动签到框架的 claim）。
///
/// 返回 `{success, msg, credit}` —— 与别家 claim 同一形状（`billing::checkin`
/// 的汇总只认 `success` / `msg`）。
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
    let before = match total(credentials, proxy).await {
        Ok(value) => value,
        Err(StudioError::Expired(message)) => {
            return Err(GatewayError::with_status(401, message))
        }
        Err(error) => return Ok(result(false, error.message().to_string())),
    };
    // ③ 触发活跃：与桌面端启动时同类的只读请求，失败不影响观测
    let _ = client::studio_json(credentials, endpoints::MEMBERSHIP_ME, "会员信息", proxy).await;
    let _ = client::studio_json(credentials, endpoints::POINTS_SUMMARY, "积分汇总", proxy).await;
    tokio::time::sleep(Duration::from_millis(TOUCH_WAIT_MS)).await;
    // ④ 签后读数
    let after = match total(credentials, proxy).await {
        Ok(value) => value,
        Err(StudioError::Expired(message)) => {
            return Err(GatewayError::with_status(401, message))
        }
        Err(error) => return Ok(result(false, error.message().to_string())),
    };
    Ok(outcome(before, after))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 涨了才算成功，且写清涨了多少
    #[test]
    fn a_rise_means_claimed() {
        let row = outcome(Some(10.0), Some(22.5));
        assert_eq!(row["success"], true);
        assert_eq!(row["credit"], "12.5");
        assert!(row["msg"].as_str().unwrap_or("").contains("+12.5"), "{}", row["msg"]);
    }

    /// 持平 = 未观测到发放（不能谎称已签）
    #[test]
    fn no_change_is_reported_as_unobserved() {
        let row = outcome(Some(10.0), Some(10.0));
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
        let row = outcome(Some(30.0), Some(18.0));
        assert_eq!(row["success"], false);
        assert!(row["msg"].as_str().unwrap_or("").contains("12"), "{}", row["msg"]);
    }

    /// 读数缺失 → 明确说读数不完整（不猜成 0，也不当成已签）
    #[test]
    fn missing_readings_are_not_zero() {
        let row = outcome(None, Some(10.0));
        assert_eq!(row["success"], false);
        assert!(row["msg"].as_str().unwrap_or("").contains("读数不完整"));
        // 两个都缺也一样
        assert_eq!(outcome(None, None)["success"], false);
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
