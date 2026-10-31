//! AtomCode 需要的一次性随机串：RFC 4122 v4 形态的 uuid。
//!
//! ── 谁在用 ──────────────────────────────────────────────────
//! 本家只有一个消费点：**登录任务表的本地关联串**（见 `core::login::atomcode`
//! 与 [`super::oauth::BrokerLogin`] 的说明）。上游的 `state` 由代理下发，不需要
//! 我们生成；这里要的是「本机任务表里唯一」的那个 key。
//!
//! 但仍然保留完整的 v4 形态（而不是拿时间戳凑一个）：登录任务表按它做取消 /
//! 查询的索引，两个并发登录拿到同一个串会让「取消其中一个」变成取消另一个。
//!
//! ── 为什么不引 uuid crate ────────────────────────────────────
//! 只为一处调用点引进一个依赖不划算 —— 本仓其它几家（lobsterai / chatglm /
//! trae / accio）都是自己用 `getrandom` 拼出同一个形态。放一处共用，避免第 N 次抄写。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

/// RFC 4122 v4 形态的随机 id（`xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx`）。
///
/// 随机源不可用时退回「时间去相关」的值 —— 本家用它做本机关联串，攻击面是
/// 本机进程（本机进程本来就能读账号库），所以不需要密码学强度；
/// 但**不能为空**（空串会让任务表的登记/取消失去意义）。
pub fn uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::getrandom(&mut bytes).is_err() {
        // 兜底：系统时间 + 进程内计数器，保证同一进程内不重复
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let millis = crate::server::logging::now_ms().max(0) as u128;
        let mixed = millis
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(counter as u128);
        bytes.copy_from_slice(&mixed.to_le_bytes());
    }
    // 版本位（第 7 字节高 4 位 = 0100）与变体位（第 9 字节高 2 位 = 10）
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let part = |start: usize, end: usize| hex.get(start..end).unwrap_or("").to_string();
    format!(
        "{}-{}-{}-{}-{}",
        part(0, 8),
        part(8, 12),
        part(12, 16),
        part(16, 20),
        part(20, 32)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v4 形态：36 字符、5 段、版本位是 4、变体位在 8/9/a/b 里
    #[test]
    fn uuid_shape_is_v4() {
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[2].chars().next(), Some('4'));
        assert!("89ab".contains(parts[3].chars().next().unwrap_or('z')));
    }

    /// 连续两次不重复（任务表的 key 要求唯一）
    #[test]
    fn uuid_is_unique() {
        assert_ne!(uuid_v4(), uuid_v4());
    }
}
