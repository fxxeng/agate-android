//! Core 态（网关未运行）的登录回调监听器。
//!
//! ── 为什么需要它 ──────────────────────────────────────────
//! 四家的网页登录把回调地址拼成 `http://127.0.0.1:<网关端口>/...`
//! （accio / accio-cn / codearts / antigravity，端口来自进程级
//! `set_loopback_port`，bootstrap 写一次）。两级内核之后 Core 态**不监听端口**，
//! 于是授权页打得开、回调却打到一个没人听的端口上 —— 用户会一直等到
//! `LOGIN_TIMEOUT_MS`（5 分钟）才失败，且期间没有任何错误提示。
//!
//! 修法（与参考实现 `Android-AI-gateway` 的 `LoopbackCallbackServer` 同思路，
//! 但改动更小）：登录发起时**临时**把这台端口占起来，收完回调再释放。
//! - 不另开随机端口：四家的 redirect 都来自只读的 `OnceLock`，换端口要改 4 处
//!   拼装逻辑，收益不值；
//! - 复用 `server::start(state)`：它 bind 的就是 `state.port`、挂的是完整
//!   `router(state)`，鉴权/错误信封/所有回调路径与「网关运行中」逐字一致；
//! - 停止用它的 `shutdown_tx`（与 `stop_gateway` 同一手法），并等端口真的释放，
//!   避免「刚释放就启动网关」时自己撞自己。
//!
//! ── 什么时候释放 ──────────────────────────────────────────
//! 1. 网关启动前（`start_gateway` 第一行）——必须，否则 bind 会冲突；
//! 2. 进程退出（随进程回收）。
//! 登录成功后**不立即释放**：CodeArts 是两趟回调（第一趟 `secret`+`redirect`，
//! 第二趟才带 `code`，见 `core::login::codearts`），过早关端口会让第二趟打空
//! ——参考实现踩过这个坑（它的 `REDIRECT_SENTINEL`）。留到网关启动再关最安全。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use crate::server::ServerState;

/// 当前持有的临时监听：`(端口, 关停句柄)`。`None` = 没有在监听。
static HOLDER: Mutex<Option<(u16, oneshot::Sender<()>)>> = Mutex::new(None);

/// 是否已经由本模块占着端口（登录监听，不代表网关在跑）。
pub fn held() -> bool {
    HOLDER.lock().map(|guard| guard.is_some()).unwrap_or(false)
}

/// 确保 `127.0.0.1:<state.port>` 上有人收回调。
///
/// - 本模块已占着 → 直接返回（幂等）；
/// - 端口已在监听（多半是网关在跑）→ 直接返回，不去抢；
/// - 否则临时起一套服务，失败时把「端口被别的程序占用」说清楚。
pub fn ensure(state: &ServerState) -> Result<(), String> {
    if held() {
        return Ok(());
    }
    // 网关在跑：端口本来就有监听者，回调能正常落（探测失败才去 bind）
    if crate::server::probe_port(state.port).is_ok() {
        return Ok(());
    }
    let mut guard = HOLDER.lock().map_err(|_| "state poisoned".to_string())?;
    if guard.is_some() {
        return Ok(());
    }
    match crate::server::start(state) {
        Ok(shutdown_tx) => {
            *guard = Some((state.port, shutdown_tx));
            crate::server::logging::log(
                "[Server]",
                &format!(
                    "已为登录临时监听 127.0.0.1:{}（网关未运行；登录完成后随网关启动释放）",
                    state.port
                ),
            );
            Ok(())
        }
        Err(conflict) => Err(format!(
            "登录需要本机端口 {}，但{}",
            state.port, conflict.message
        )),
    }
}

/// 释放临时监听（供网关启动前调用）；无监听时是空操作。
pub fn release() {
    let taken = HOLDER.lock().ok().and_then(|mut guard| guard.take());
    let Some((port, shutdown_tx)) = taken else {
        return;
    };
    let _ = shutdown_tx.send(());
    wait_until_released(port);
    crate::server::logging::log(
        "[Server]",
        &format!("登录临时监听已释放（:{}）", port),
    );
}

/// 等端口真的归还（`stop_gateway` 同款：连续两次探测成功才算释放）。
fn wait_until_released(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut hits = 0;
    while Instant::now() < deadline {
        if crate::server::probe_port(port).is_ok() {
            hits += 1;
            if hits >= 2 {
                return;
            }
        } else {
            hits = 0;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}