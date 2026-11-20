//! 在 async 上下文里跑**纯 CPU 活**的两个小工具。
//!
//! ── 它解决什么 ──────────────────────────────────────────────
//! 转发链上有些活是同步的、纯 CPU 的、而且**可能不短**：会话图片压缩（一张几 MB
//! 的图解码 + Lanczos 缩放 + JPEG 编码，几百毫秒）、请求体脱敏、日志库裁剪。
//! 直接在一个 async 函数里同步跑完，就把那一个 tokio worker 线程摁住了 ——
//! 同一 worker 上排队的其它请求（包括别的会话的 SSE 分片）全部延后。多线程运行
//! 时 worker 数等于核数，几张图同时到就能让整个网关「卡一下」。
//!
//! ── 为什么不是 `spawn_blocking` ────────────────────────────
//! `spawn_blocking` 要求闭包 `Send + 'static`，而转发链上的活几乎都借用着栈上的
//! 上下文（`&mut Value`、`&ProviderContext`）—— 用它就得先把数据搬进所有权、
//! 再搬回来，多两次克隆，而这份数据往往正是请求体（最大的一块）。
//! `block_in_place` 换个做法：告诉运行时「这个 worker 我要占一会儿，你先补一个
//! 线程顶上」，闭包仍可借用栈上数据。**这才是本模块存在的理由**。
//!
//! ── 为什么必须判运行时形态（JNI 这个特殊场景）─────────────────
//! `block_in_place` 在 **current-thread** 运行时上会 panic（运行时就一个线程，
//! 没有「别的线程来顶」这回事）。本项目有两种运行形态：
//! - 线上：`lib.rs`（JNI）与 `bin/agate-server.rs` 都用 `new_multi_thread` → 可以 `block_in_place`；
//! - 测试：`adapter.rs` 里的用例自建 `new_current_thread` 运行时 → 必须就地跑。
//! 所以先问一句运行时形态，多线程才让出，其余一律就地执行（就地跑只是慢，
//! 不会 panic —— 对一个「加速」工具来说，这就是正确的降级方向）。

/// 在**可能**让出当前 worker 的前提下同步执行 `action`。
///
/// 多线程运行时 → `block_in_place`（运行时会补一个线程接管本 worker 的队列）；
/// current-thread 运行时或**不在任何运行时里**（同步测试、启动期）→ 就地执行。
///
/// 返回 `action` 的返回值。闭包不需要 `Send`：这正是相对 `spawn_blocking`
/// 的优势（见模块头）。
pub(crate) fn in_place<T>(action: impl FnOnce() -> T) -> T {
    let multithread = tokio::runtime::Handle::try_current()
        .is_ok_and(|handle| handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread);
    if multithread {
        tokio::task::block_in_place(action)
    } else {
        action()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_place_runs_outside_any_runtime() {
        // 没有运行时（同步测试）：必须就地跑通，绝不能 panic
        assert_eq!(in_place(|| 7 * 6), 42);
    }

    #[test]
    fn in_place_degrades_on_current_thread_runtime() {
        // current-thread 运行时：`block_in_place` 会 panic，本函数必须走就地分支
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let value = runtime.block_on(async { in_place(|| "就地") });
        assert_eq!(value, "就地");
    }

    #[test]
    fn in_place_borrows_across_the_call() {
        // 核心卖点：闭包能借用栈上数据（`spawn_blocking` 做不到）
        let mut buffer = String::from("abc");
        in_place(|| buffer.push_str("def"));
        assert_eq!(buffer, "abcdef");
    }
}
