//! 宿主名（`COMPUTERNAME` / `HOSTNAME`）。
//!
//! ── 为什么单独一个模块 ──────────────────────────────────────
//! 三家 provider 都要用它，而且用的地方都是「要让上游以为这是一台真机器」的
//! 字段：Kiro 的镜像 UA（`KiroIDE-<版本>-<hash>`，hash 由宿主名派生）、
//! Tabbit 的设备名与 `device_id_hash`。以前这些地方各写一份，取不到时的兜底
//! 还不一样 —— 一个给 `unknown`、一个给空串，而空串拼进这类字符串就是
//! `KiroIDE-1.1.70-<空>` 这种一眼假的形态。
//!
//! ── 与 Qoder 那份的关系 ────────────────────────────────────
//! `providers::qoder::risk` 里还有一个同名私有函数，它**故意**保留独立实现：
//! 它的返回值会被拼进 `Cosy-*` 机器指纹，空串与 `unknown` 派生出的是两个不同
//! 的指纹（改了等于让上游认成一台新机器）。那是「指纹口径」，不是「宿主名」，
//! 两者的契约不同，不该合。
//!
//! 硬约束：release 是 `panic=abort`，本模块零 unwrap/expect/panic。

/// 宿主名；取不到（或取到空串）时给 `unknown`。
///
/// Windows 上是 `COMPUTERNAME`，类 Unix 上是 `HOSTNAME`。两处都读不到时给
/// `unknown` 而不是空串：调用方都是把它拼进一个要**像机器**的字符串。
pub fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .map(|value| value.trim().to_string())
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hostname_is_never_empty() {
        // 取不到也只可能是 `unknown`：空串拼进 UA 会得到一个一眼假的形态
        let host = hostname();
        assert!(!host.is_empty());
        assert_eq!(host.trim(), host, "不该带首尾空白");
    }
}
