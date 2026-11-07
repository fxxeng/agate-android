//! Agate 网关本体（独立 crate）。
//!
//! ── 本 crate 服务两种形态 ────────────────────────────────────
//!   · Tauri 桌面端：壳进程把本 crate 当库链接，网关作为进程内 HTTP
//!     服务器跑在 tauri 的 Tokio 运行时上（`server::start`）；
//!   · headless 服务器 / Docker：`agate-server` 二进制（`src/bin/`）
//!     自建 Tokio 运行时、自己托管管理界面（`server::static_files`），
//!     不链接任何 GUI 依赖。
//!
//! 模块结构是**刻意保留的一层壳**：`server/`、`port_conflict.rs` 的目录
//! 与文件名与拆分前（桌面 crate 的 `src/server`）完全一致，内部所有
//! `crate::server::…` / `crate::port_conflict::…` 路径零改动 —— 桌面侧
//! 用 `use agate_server as server` 重导出后，两边的既有路径都继续成立。
//!
//! ── 为什么 spawn 必须经由 [`spawn_task`] ─────────────────────
//! 拆分前模块里用的是 `tauri::async_runtime::spawn`（壳提供全局运行时，
//! 任意线程可调）。Android-only 后只剩两种宿主，且**业务任务只有唯一落点**：
//!   · Android：JNI 从 Kotlin 线程同步进入（无 Tokio 上下文），网关 serve
//!     任务本身也由 [`spawn_task`] 从这条链路拉起 —— 全部落在
//!     [`GLOBAL_RUNTIME`]；axum handler 的"当前 runtime"命中时拿到的
//!     Handle 也是它自己（serve 就跑在上面）。
//!   · headless：bootstrap/start 全在 main 的同步段（block_on 之前）执行，
//!     同样落 [`GLOBAL_RUNTIME`]；bin 自建的 runtime 专职信号监听与
//!     shutdown 的 block_on 驱动（跨 runtime await/abort 合法），零业务任务。
//!
//! 旧的 `Handle::try_current()` 分支是桌面 tauri 宿主的遗留假兼容：它让
//! 「任务落点」取决于调用线程是否恰好处于某个 Tokio 上下文，停机编排
//! （`shutdown_core_tasks`）因此没有统一视野。现在入口内**只走
//! [`global_runtime`]**，落点由代码路径唯一确定；release 档 panic=abort，
//! 这里保证任何线程调用都不会 panic（与拆分前统一走
//! `tauri::async_runtime::spawn` 的动机一致）。

pub mod data_dir_lock;
pub mod paths;
pub mod port_conflict;
pub mod server;
pub mod web_shim;
pub mod task_supervisor;

/// Result of a host-requested graceful Core shutdown.
#[derive(Clone, Debug)]
pub struct CoreShutdownReport {
    pub sidecar: task_supervisor::ShutdownReport,
    pub core: task_supervisor::ShutdownReport,
}

/// Stop process-owned Core work for a host that is actually exiting.
///
/// This must not be called for Gateway stop, Android Activity destruction, or
/// Android FGS stop: those events do not mean the Rust Core process is exiting.
pub async fn shutdown_core(timeout: std::time::Duration) -> CoreShutdownReport {
    server::core::auto_checkin::stop_global();
    let sidecar = server::core::sub_kernel::shutdown(timeout).await;
    let core = task_supervisor::shutdown_core_tasks(timeout).await;
    CoreShutdownReport { sidecar, core }
}

/// Agate Android：JNI 控制面（src/jni_bridge.rs，仅 Android 目标编译）。
#[cfg(target_os = "android")]
pub mod jni_bridge;

use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::Runtime;

/// 惰性全局运行时：只承接「在非 Tokio 上下文里发起的后台任务」，
/// 正常路径（handler / 既有任务体内）不会碰到它。
static GLOBAL_RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// 项目统一的 spawn 入口：Android JNI 与 headless 共用（原
/// `tauri::async_runtime::spawn` 的替身，落点论证见 crate 文档头）。
pub fn spawn_task<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    global_runtime().spawn(future)
}

fn global_runtime() -> &'static Runtime {
    GLOBAL_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("创建后台 Tokio 运行时失败")
    })
}

#[cfg(test)]
mod runtime_tests {
    /// R44b 收敛单一权威 runtime 后的回归锚点：spawn_task 必须仍可从**任意**
    /// 线程（无 Tokio 上下文）调用且任务能跑完 —— 旧的 try_current 分支删除
    /// 后唯一路径是 global_runtime，这条测试守住「任何线程调用不 panic」的
    /// 既有保证（release panic=abort，误用即进程崩溃，见 crate 文档头）。
    /// 跨 runtime await JoinHandle 合法（JoinHandle 不绑定创建它的 runtime）。
    #[test]
    fn spawn_task_works_from_plain_thread() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let out = rt.block_on(async {
            let handle = std::thread::spawn(|| crate::spawn_task(async { 41u32 + 1 }))
                .join()
                .expect("spawning thread");
            handle.await.expect("join handle")
        });
        assert_eq!(out, 42);
    }
}
