//! ModelScope 的**账号级失败判据**：两类不该被当成「速率窗口」的拒绝。
//!
//! ── 为什么单独一个文件 ────────────────────────────────────
//! 魔搭把「账号条件不满足」和「速率窗口撞上」都用 429 / 403 表达，**只能靠
//! 文案区分**，而两者的正确处置完全不同：前者在充值 / 完成验证之前一直不可用
//! （按自然日冷却），后者等一会儿就好（分钟级冷却）。
//!
//! ── 两类是什么（上游真实报文，来自上游插件实测）─────────────────
//! ```text
//!   ① 魔粒耗尽   429 {"error":{"message":"insufficient balance","request_id":"…"}}
//!   ② 未验证邮箱 403 正文含 "email verification" / "verified accounts" / "/my/settings/account"
//! ```
//!
//! ── 上游插件里还有一条，本实现**有意不照搬**（免得被当成漏掉）──────
//! 参考实现把 `503 + model_not_ready`（上游 vLLM 还在加载权重）当作「整家不可用」
//! 摘掉。本实现不做：那件事与账号无关（每把令牌都会被同样拒绝），罚账号是错的 ——
//! 默认走 `Fatal`，如实回给客户端由它重试。
//!
//! ⚠️ 判据一律**大小写不敏感的子串匹配**，且只在「这次请求已经失败」的分支上
//! 调用 —— 子串匹配本身不区分「上游报的错」与「模型自己生成的内容」。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

/// 一天（毫秒）
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// 北京时间的固定偏移（UTC+8）。
///
/// 用**固定偏移**而不是机器时区：网关可能跑在 UTC 容器里，而魔搭的一整套
/// 「当日」口径（签到、魔粒发放、日额度）都按北京时间算 ——「明天零点」必须是
/// 北京的那个。本仓 `core::beijing` 也钉在同一偏移上，两处不可分叉。
const DAY_ZONE_OFFSET_SECONDS: i32 = 8 * 3600;

/// 一次失败是否意味着**账号魔粒耗尽**（429 + `insufficient balance`）。
pub fn is_insufficient_balance(status: u16, message: &str) -> bool {
    status == 429 && contains_ci(message, "insufficient balance")
}

/// 一次失败是否意味着**账号未完成邮箱验证**（403 + 三选一的特征串）。
///
/// **必须同时看状态码与文案**：403 还有别的权限语义，只看状态码会把无关失败
/// 误判成「账号未验证」，让用户去查一个不存在的问题。
pub fn is_unverified_account(status: u16, message: &str) -> bool {
    if status != 403 {
        return false;
    }
    [
        "email verification",
        "verified accounts",
        "/my/settings/account",
    ]
    .iter()
    .any(|marker| contains_ci(message, marker))
}

/// 大小写不敏感的子串匹配（空 needle 一律 false）
fn contains_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    haystack.to_ascii_lowercase().contains(&needle.to_ascii_lowercase())
}

/// `now_ms` 之后最近的**北京时间零点**（毫秒时间戳，纯函数）。
///
/// 给「魔粒耗尽 / 账号未验证 ⇒ 冷却到明天」用：这两种状态在充值 / 完成验证
/// 之前一直不可用，而魔搭的日口径是北京零点 —— 跨过那个点再试一次，既不会
/// 在当天反复白撞，也不会把账号永久钉死。
///
/// 边界：恰好落在零点上时给**次日**零点。用 `div_euclid` 而不是 `/`：
/// 负数时刻（1970 前）在 Rust 里向零取整会算错整天边界。
pub fn next_day_boundary_ms(now_ms: i64) -> i64 {
    let offset = i64::from(DAY_ZONE_OFFSET_SECONDS) * 1000;
    let shifted = now_ms + offset;
    let start_of_today = shifted.div_euclid(DAY_MS) * DAY_MS;
    start_of_today + DAY_MS - offset
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两条判据必须同时看状态码 —— 只有文案不算
    #[test]
    fn account_level_verdicts_need_both_status_and_text() {
        assert!(is_insufficient_balance(429, "Insufficient Balance"));
        assert!(!is_insufficient_balance(400, "insufficient balance"));
        assert!(is_unverified_account(403, "please finish Email Verification"));
        assert!(!is_unverified_account(400, "email verification"));
    }

    /// 冷却点是「下一个北京零点」，且恰好落在零点时给次日
    #[test]
    fn cooldown_lands_on_next_beijing_midnight() {
        // 2026-10-31 00:04:40 +08:00 = 1789... 直接用边界值验证语义，不钉绝对秒数
        let midnight_utc = 0i64; // 1970-01-01T00:00Z = 北京 08:00
        let next = next_day_boundary_ms(midnight_utc);
        // 结果必须比输入晚、且不超过一天
        assert!(next > midnight_utc);
        assert!(next - midnight_utc <= DAY_MS);
        // 把结果再喂一次：恰好落在零点上时跳到次日
        assert_eq!(next_day_boundary_ms(next), next + DAY_MS);
    }
}
