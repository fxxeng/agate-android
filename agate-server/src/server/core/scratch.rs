//! 可写的临时目录（**唯一入口**）。
//!
//! ── 为什么不能直接用 `std::env::temp_dir()` ─────────────────────
//! Android 上它给的是 `/data/local/tmp`（属于 shell，app 进程只有读权限之外的
//! 空权限）—— 往那里写就是 `Permission denied (os error 13)`。真机反馈「导入
//! QuWork 登录库 → 写入临时文件失败 /data/local/tmp/…」就是这条：代码在桌面上
//! 跑得好好的，搬到手机上第一件事就是建不了文件。
//!
//! 内核跑在 app 里时，JNI 会把 `filesDir` 传进来并落成环境变量
//! `AGATE_PROXY_HOME`（见 `jni_bridge::ensure_core`）；这里优先在那下面建
//! `scratch/`。命令行模式（CI / 桌面调试）没有那个变量，回落到系统临时目录。
//!
//! ── 使用纪律 ────────────────────────────────────────────────
//! 只给「本次请求内读一下就删」的文件用（凭证库 / Cookie 库 / 一次性导出）。
//! **不要**放长期数据：这里可能在系统临时目录下，随时会被清掉。清理由调用方
//! 负责 —— 本模块不记账、不扫目录。

use std::path::PathBuf;

/// 可写的临时目录（必要时创建 `scratch/` 子目录）。
///
/// 不保证返回的目录一定存在：建不出来时原样返回父目录，让调用方的写失败文案
/// 带上「实际想写的路径」—— 那比在这里抛一个没有上下文的错误有用。
pub fn dir() -> PathBuf {
    let base = match std::env::var_os("AGATE_PROXY_HOME").map(PathBuf::from) {
        // 目录一般已由 bootstrap 建好；没建好时这里补一次（首次启动的时序不保证）
        Some(path) if path.is_dir() || std::fs::create_dir_all(&path).is_ok() => path,
        _ => std::env::temp_dir(),
    };
    let scratch = base.join("scratch");
    if scratch.is_dir() || std::fs::create_dir_all(&scratch).is_ok() {
        scratch
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_is_usable() {
        // 目录要么已存在，要么它的父目录可写 —— 两者都没有的话，本次请求的
        // 导入功能就是坏的，值得在这里炸出来
        let dir = dir();
        assert!(
            dir.is_dir() || dir.parent().is_some_and(|parent| parent.is_dir()),
            "临时目录不可用：{}",
            dir.display()
        );
    }
}
