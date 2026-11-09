//! Agate JNI 控制面 —— Android 壳（dev.agate.android）与网关内核的唯一接口。
//!
//! 设计原则（docs/Agate移植方案-v3.0.md §3「双通信面」）：
//!   · 数据面：业务交互默认走 127.0.0.1 的 HTTP API（Compose UI 用 Retrofit
//!     消费）；Core 态（网关未运行）下管理面经 `apiCall` 在内核内存 Router
//!     上直调，不经端口 —— 两条路径共用同一套 Router/handler，JNI 只传
//!     JSON 文本，不解释任何业务数据结构；
//!   · 控制面：本文件提供 12 个 JNI 入口 —— 电源（start/stop/status/
//!     coreInit）、闹钟与签到（triggerTask/schedulerSet/checkinRunNow/
//!     checkinRunAndWait/requestCheckinStop）、日志回调（setLogCallback）、
//!     版本（version），以及管理面直调（apiCall）。
//!
//! 两级启动（B 包 · 方案 v2.0 三开关），桌面壳 `ensure_ready` 的序列拆两段：
//!   Core（随进程）：Android 显式数据目录覆盖 → `ServerState::bootstrap` →
//!     `access::attach_db`；后台循环任务（scheduled_tasks + auto_checkin +
//!     订阅内核 worker / zcode 配置同步 / 余额查询，见 bootstrap 末尾区块）
//!     随 Core 常驻，不监听任何端口，也不随网关启停；
//!   网关（手动）：重设 OAuth 回调端口 → `local_panel = true` →（bind_lan
//!     时面板闸门 + /v1 fail-closed）→ `server::start()` 拿 oneshot 停机
//!     通道 →（绑定成功后）`start_gateway_tasks` 提交 Sidecar autostart。
//! `start()` 是同步调用，内部经 `crate::spawn_task` 落到全局惰性 Tokio 运行时
//! （见 lib.rs），因此本桥不需要自建 Runtime —— 与 headless bin 的时序一致
//! （那边也是先 start、再建运行时等信号）。
//!
//! Android 数据目录经 `paths::set_config_dir_override` 注入；桌面/headless
//! 仍通过 `AGATE_PROXY_HOME` / `WORKBUDDY_PROXY_HOME` 环境变量配置。
//!
//! 与上游的全部差异：本文件 + Cargo.toml（lib 改名/cdylib/license/profile/
//! android deps）+ lib.rs 的模块挂载 + logging.rs 的 console_line 挂钩 +
//! bin 的 crate 名引用 —— 网关业务代码零改动。

use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jint, jlong, jstring};
use jni::{JNIEnv, JavaVM};
use serde_json::{json, Value};
use tokio::sync::oneshot;
use tower::ServiceExt;

use crate::server::{self, ServerState};

/// 内核版本（来自本 crate 的 Cargo.toml，与 APK 版本对齐溯源）
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// JNI 控制面契约版本：桥的返回结构/字段语义变更时 +1（R46/T2 首版 = 1）。
/// 随 `status_json()` 信封下发（`bridge` 字段）；Kotlin 读到 0 = 旧内核（未
/// 携带该字段的历史版本），按 R44 `kind` 的 null 兼容先例降级。字段变更必须
/// 同步 bump、改 Gateway.kt 两侧并更新 docs/JNI控制面契约.md。
pub const BRIDGE_CONTRACT_VERSION: u32 = 2;

/// logcat 优先级（android/log.h 的数值）
const LOG_INFO: i32 = 4;
const LOG_WARN: i32 = 5;
const LOG_ERROR: i32 = 6;
const LOGCAT_TAG: &str = "AgateCore";

extern "C" {
    fn __android_log_write(prio: i32, tag: *const u8, text: *const u8) -> i32;
}

struct Running {
    /// 只在 Running→Stopping 迁移时取走；None = 停机信号已发出。
    shutdown_tx: Option<oneshot::Sender<()>>,
    port: u16,
    bind_lan: bool,
    started_at: Instant,
    /// serve 任务的完成标记：true = listener 已释放。
    done: Arc<server::ServeDone>,
}

/// 停机已发出、但 listener 尚未确认退出的过渡态（R50/T1）。
///
/// 不能把这段窗口折算成「已停止」：旧实现的 `RUNNING.take()` 让
/// `status_json` 立刻返回 running=false，Kotlin 复核据此释放保活锁并退出
/// 前台，而端口可能仍被旧 serve 占着。过渡态要保留到 done 置位。
struct Stopping {
    port: u16,
    bind_lan: bool,
    started_at: Instant,
    done: Arc<server::ServeDone>,
}

/// 网关三态；Stopped 是唯一「没有 listener」的状态。
enum GatewayPhase {
    Stopped,
    Running(Running),
    Stopping(Stopping),
}

/// 网关相位；Stopped 无字段，可直接 const 构造。
static RUNNING: Mutex<GatewayPhase> = Mutex::new(GatewayPhase::Stopped);

/// 后台 serve 已退出时把残留相位归位 Stopped（异常退出 / 停止完成）。
/// 返回是否发生了归位。
fn reap_finished_phase(guard: &mut GatewayPhase) -> bool {
    let finished = match guard {
        GatewayPhase::Running(r) => r.done.is_done(),
        GatewayPhase::Stopping(s) => s.done.is_done(),
        GatewayPhase::Stopped => false,
    };
    if finished {
        *guard = GatewayPhase::Stopped;
        // serve 自行退出（异常或外部停机）也要归还 LAN Key 覆盖，避免下一次
        // Core 会话继承旧 Key；同时让 status/start 观察到真实停止（R50/T1）。
        crate::server::config::parse::clear_android_api_key_override();
        server::logging::log("[Server]", "监听已退出，网关状态归位为已停止");
    }
    finished
}
/// 串行化网关 start/stop：避免并发启动重复 bind，或重复 start 在
/// already_running 返回前改写当前会话的鉴权环境变量。
static GATEWAY_OP: Mutex<()> = Mutex::new(());

/// Kotlin 侧注册的日志回调（GlobalRef 保活）；`None` = 只写 logcat。
static LOG_CALLBACK: Mutex<Option<GlobalRef>> = Mutex::new(None);
/// 首次注册回调时记下的 JVM 句柄，供 Tokio 工作线程回调时附着用。
static JAVA_VM: OnceLock<JavaVM> = OnceLock::new();
thread_local! {
    /// 递归保护：回调里再打日志（例如 Kotlin 侧抛异常）时直接丢弃，防栈爆。
    static IN_FORWARD: AtomicBool = const { AtomicBool::new(false) };
}

// ── 日志转发（logging.rs::console_line 的 Android 挂钩入口）──────

fn logcat_write(prio: i32, tag: &str, text: &str) {
    use std::ffi::CString;
    // CString 构造失败（内嵌 NUL）时丢弃该行：日志不值得为它 panic（release 是 abort）
    if let (Ok(t), Ok(x)) = (CString::new(tag), CString::new(text)) {
        unsafe {
            __android_log_write(prio, t.as_ptr() as *const u8, x.as_ptr() as *const u8);
        }
    }
}

fn prio_of(tag: &str, text: &str) -> i32 {
    if text.contains("❌") || text.contains("失败") || text.contains("ERROR") {
        LOG_ERROR
    } else if text.contains("⚠️") || tag.contains("Security") {
        LOG_WARN
    } else {
        LOG_INFO
    }
}

fn prio_to_level(prio: i32) -> &'static str {
    match prio {
        LOG_ERROR => "error",
        LOG_WARN => "warn",
        _ => "info",
    }
}

/// console_line 的挂钩：logcat 全量输出；已注册回调时转发 Kotlin。
/// 线程安全：回调调用不持锁；先克隆 GlobalRef 保活到回调结束，因此 Kotlin
/// 侧即使并发撤销/替换监听，也不会释放本次正在使用的 JNI 全局引用。
pub fn forward_log(tag: &str, text: &str) {
    let prio = prio_of(tag, text);
    logcat_write(prio, tag, text);

    if IN_FORWARD.with(|f| f.load(Ordering::Relaxed)) {
        return;
    }
    let Some(vm) = JAVA_VM.get() else { return };
    // 克隆 GlobalRef 并把克隆持有到 Java 回调结束。若只取 raw jobject 后
    // 释放锁，另一个线程可同时 setLogCallback(null) 并销毁原 GlobalRef，
    // 造成回调线程使用已释放的 JNI 全局引用。GlobalRef 的克隆共享同一底层
    // 引用计数，能把引用生命周期覆盖到本次调用结束。
    let Some(callback) = LOG_CALLBACK
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().cloned())
    else {
        return;
    };
    let message = format!("{tag} {text}");
    let obj = callback.as_obj();
    IN_FORWARD.with(|f| f.store(true, Ordering::Relaxed));
    // 已附着（JNI 调用线程）→ 直接用；未附着（Tokio 工作线程）→ 附着并在
    // guard 离开作用域时自动脱离，不碰 Java 自己附着的线程
    if let Ok(mut env) = vm.get_env() {
        let _ = call_on_log(&mut env, &obj, prio_to_level(prio), tag, &message);
    } else if let Ok(mut guard) = vm.attach_current_thread() {
        let _ = call_on_log(&mut guard, &obj, prio_to_level(prio), tag, &message);
    }
    IN_FORWARD.with(|f| f.store(false, Ordering::Relaxed));
}

/// NewStringUTF 期待 modified UTF-8：把增补平面字符（4 字节序列）换成
/// U+FFFD，避免 JVM 按字节解释出乱码。BMP 内的中文/常用符号不受影响。
fn jni_safe(s: &str) -> String {
    s.chars()
        .map(|c| if (c as u32) > 0xFFFF { '\u{FFFD}' } else { c })
        .collect()
}

fn clear_pending_exception(env: &mut JNIEnv<'_>) {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
}

fn call_on_log(
    env: &mut JNIEnv<'_>,
    obj: &JObject<'_>,
    level: &str,
    tag: &str,
    message: &str,
) -> Result<(), jni::errors::Error> {
    let result = (|| {
        let l = env.new_string(jni_safe(level))?;
        let t = env.new_string(jni_safe(tag))?;
        let m = env.new_string(jni_safe(message))?;
        env.call_method(
            obj,
            "onLog",
            "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
            &[JValue::Object(&l), JValue::Object(&t), JValue::Object(&m)],
        )?;
        Ok(())
    })();
    // 日志回调是尽力而为的旁路能力，不能让 Kotlin listener 抛出的异常
    // 留在一个由 Java 管理的调用线程上，污染该线程后续的 JNI 操作。
    if result.is_err() {
        clear_pending_exception(env);
    }
    result
}

// ── 两级内核（B 包 · 方案 v2.0 三开关）──────────────────────────
// Core 态（随进程）：bootstrap 全部基础设施 + Core 级后台任务（凭证维护 /
// 模型目录刷新在 scheduled_tasks 里，auto_checkin 调度循环自带，订阅内核
// worker / zcode 配置同步 / 余额查询 sweeper 在 bootstrap 末尾区块），不监
// 听任何端口。网关态（手动）：bind 端口 + 网关启动动作（内核 autostart，
// 见 server::start_gateway_tasks）。调度器是 Core 里的独立开关，不随网关
// 启停 —— 网关停着，App 进程活着签到调度 / 余额查询照跑。

struct Core {
    state: ServerState,
    scheduler_on: bool,
    started_at: Instant,
}

/// Core 句柄；`None` = 尚未初始化（bootstrap 未跑）。
static CORE: Mutex<Option<Core>> = Mutex::new(None);
/// 引导锁：防两条线程同时 ensure_core 双跑 bootstrap。
static CORE_BOOT: Mutex<()> = Mutex::new(());

/// ensure_core 的失败。绝大多数是纯文案（`Plain`）；数据目录失配是可编程
/// 判别（`DataDirMismatch`）——core_init / start 兜底据此在错误信封里填机器
/// 可读的 `code`，Kotlin 侧 `CoreInitializationFailed` / `GatewayFailed` 按
/// 既有契约读取该字段；文案本身仍走 Rust 中文 reason（R47 T2.3）。
#[derive(Debug, PartialEq, Eq)]
enum EnsureError {
    Plain(String),
    DataDirMismatch(String),
}

/// Core 已在跑时，校验请求的数据目录与其绑定目录一致。
///
/// * 相同（规范化后）→ 幂等 `Ok(false)`，沿用「已在跑」语义；
/// * 不同 → `Err(DataDirMismatch)`，reason 同时带两个目录的绝对路径。
///
/// 规范化策略与 `data_dir_lock::canonical_dir` 同口径：canonicalize 失败
/// 退回原路径比对（跨进程正确性由 flock 保证，这里只影响失配判定）。
/// `existing` 来自 `DataDirLock::config_dir()`，已经是规范化路径。
///
/// 调用纪律：ensure_core 的两处「Core 已在跑」分支（首查 + CORE_BOOT
/// 双检）必须成对调用本函数——只改一处会给并发留窗口。两处都在
/// `paths::set_config_dir_override` 之前，失配请求不会污染进程全局
/// paths 状态。
fn check_data_dir_match(existing: &std::path::Path, requested: &str) -> Result<bool, EnsureError> {
    let requested = std::path::Path::new(requested);
    let requested = std::fs::canonicalize(requested).unwrap_or_else(|_| requested.to_path_buf());
    if requested.as_path() == existing {
        return Ok(false);
    }
    Err(EnsureError::DataDirMismatch(format!(
        "数据目录不一致：本进程 Core 已绑定（{}），本次请求（{}）。\
         请使用相同数据目录，或先停止当前 Core",
        existing.display(),
        requested.display(),
    )))
}

/// 初始化 Core（幂等）。Ok(true) = 本次新建，Ok(false) = 已在跑；请求的
/// 数据目录与已绑定目录不同时报 `DataDirMismatch`（错误信封带
/// `code: "DATA_DIR_MISMATCH"`，R47 T2.3）。
/// 端口只影响 OAuth 回调登记与 state.port 初值；网关启动时会按需重设。
fn ensure_core(data_dir: &str, port: u16) -> Result<bool, EnsureError> {
    {
        let Ok(guard) = CORE.lock() else {
            return Err(EnsureError::Plain("state poisoned".into()));
        };
        if let Some(core) = guard.as_ref() {
            return check_data_dir_match(core.state.data_dir(), data_dir);
        }
    }
    let Ok(_boot) = CORE_BOOT.lock() else {
        return Err(EnsureError::Plain("state poisoned".into()));
    };
    {
        let Ok(guard) = CORE.lock() else {
            return Err(EnsureError::Plain("state poisoned".into()));
        };
        if let Some(core) = guard.as_ref() {
            return check_data_dir_match(core.state.data_dir(), data_dir);
        }
    }
    // 数据目录注入：显式覆盖 paths::config_dir()，不修改进程全局环境变量。
    // Android 的数据目录属于宿主生命周期配置；桌面/headless 仍读取 env。
    crate::paths::set_config_dir_override(data_dir)
        .map_err(|reason| EnsureError::Plain(format!("配置目录初始化失败: {reason}")))?;
    let state = match ServerState::bootstrap(port, IpAddr::V4(Ipv4Addr::LOCALHOST)) {
        Ok(state) => state,
        Err(reason) => {
            logcat_write(LOG_ERROR, LOGCAT_TAG, &format!("bootstrap 失败: {reason}"));
            return Err(EnsureError::Plain(reason));
        }
    };
    // 面板访问控制的库句柄注入（Android 未注册管理员 → 该体系不启用）。
    // Core 态就装：access 状态不依赖端口，网关后启动也读得到同一份。
    server::access::attach_db(state.db().cloned());
    let Ok(mut guard) = CORE.lock() else {
        return Err(EnsureError::Plain("state poisoned".into()));
    };
    if let Some(core) = guard.as_ref() {
        // 引导锁窗口内被并发抢先：同目录 → 丢弃本次 bootstrap 产物（幂等）；
        // 异目录 → 经 check_data_dir_match 明确报 DATA_DIR_MISMATCH。
        //
        // 丢弃的 `ServerState` 里含 `Arc<DataDirLock>`。这里**不会**把数据目录锁
        // 弄丢：`data_dir_lock::acquire_shared` 的 Weak 登记表让同进程对同一目录
        // 的重复取锁返回**同一把** `Arc`（其登记表锁全程持有，并发首锁由
        // `concurrent_first_acquire_shared_same_dir_never_misreports` 钉住——
        // 本注释旧版声称的「并发重入验证」在该测试落地前并不存在），
        // 所以被抢先的那个 state 只是少了一份 strong 引用，锁仍由已装进
        // `CORE` 的那份持有。反过来，若这里是全进程最后一份引用（即 `CORE`
        // 之后又被清空），drop 也会正常释放 flock，下一次 `ensure_core`
        // 能重新取到 —— 两个方向都有测试钉住
        // （`same_process_reentry_returns_same_lock` 等）。
        return check_data_dir_match(core.state.data_dir(), data_dir);
    }
    *guard = Some(Core {
        state,
        // bootstrap 内部已按配置决定是否 start()；先记 true，core_init 随后
        // 用用户开关对齐（scheduler_set 幂等，重复 start 无害）。
        scheduler_on: true,
        started_at: Instant::now(),
    });
    server::logging::log(
        "[Server]",
        &format!("Agate Core 就绪（内核 v{VERSION}，后台调度随进程，无端口监听）"),
    );
    Ok(true)
}

/// ensure_core 的失败拆成错误信封字段：`DataDirMismatch` 额外带机器可读
/// `code`（Kotlin 既有契约读取），其余保持纯文案、code 缺省。
fn ensure_error_parts(error: EnsureError) -> (Option<&'static str>, String) {
    match error {
        EnsureError::Plain(reason) => (None, reason),
        EnsureError::DataDirMismatch(reason) => (Some("DATA_DIR_MISMATCH"), reason),
    }
}

/// Core 初始化 + 调度器对齐（Application 启动与 FGS 兜底都走这里）。
fn core_init(data_dir: &str, port: u16, scheduler_on: bool) -> Value {
    if let Err(error) = ensure_core(data_dir, port) {
        let (code, reason) = ensure_error_parts(error);
        return match code {
            Some(code) => {
                json!({ "ok": false, "stage": "bootstrap", "code": code, "error": reason })
            }
            None => json!({ "ok": false, "stage": "bootstrap", "error": reason }),
        };
    }
    scheduler_set(scheduler_on)
}

/// 调度器开关（独立于网关）。on = auto_checkin.start()，off = stop()。
fn scheduler_set(on: bool) -> Value {
    let Ok(mut guard) = CORE.lock() else {
        return json!({ "ok": false, "error": "state poisoned" });
    };
    let Some(core) = guard.as_mut() else {
        return json!({ "ok": false, "error": "core_not_ready" });
    };
    if on {
        core.state.auto_checkin().start();
    } else {
        core.state.auto_checkin().stop();
    }
    core.scheduler_on = on;
    server::logging::log(
        "[Checkin]",
        if on {
            "后台签到调度已开启"
        } else {
            "后台签到调度已关闭"
        },
    );
    json!({ "ok": true, "scheduler": on })
}

/// 同步触发一轮签到并等待完成（F3：闹钟 tick 直调内核，不再 HTTP 回环）。
/// 超时不算失败 —— fire 已在内核后台跑，只是调用方不再等它收尾。
fn checkin_run_now(timeout_ms: u64) -> Value {
    let checkin = {
        let Ok(guard) = CORE.lock() else {
            return json!({ "ok": false, "error": "state poisoned" });
        };
        let Some(core) = guard.as_ref() else {
            return json!({ "ok": false, "error": "core_not_ready" });
        };
        core.state.auto_checkin().clone()
    };
    let (tx, rx) = std::sync::mpsc::channel::<Option<Value>>();
    crate::spawn_task(async move {
        let _ = tx.send(checkin.fire("alarm").await);
    });
    match rx.recv_timeout(std::time::Duration::from_millis(timeout_ms)) {
        Ok(Some(result)) => json!({ "ok": true, "done": true, "result": result }),
        // None = 上一轮签到仍在执行（fire 的 CAS 抢锁失败），如实上报
        Ok(None) => json!({ "ok": false, "done": true, "error": "busy" }),
        Err(_) => json!({ "ok": true, "done": false, "error": "timeout" }),
    }
}

/// 执行一轮签到并等待 Rust 任务真正结束。
///
/// Android FGS 使用此入口持有 WakeLock 直到执行完成；不能用带超时的
/// `checkin_run_now`，因为超时只停止 JNI 调用方等待，不会取消 Tokio 任务。
/// `checkin_run_and_wait` 的等待上限。Kotlin FGS 的 WakeLock 用同一个
/// 15min 预算（另加 1min 余量）；超时只结束等待，不杀在途任务（R50/T5）。
const CHECKIN_WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// 等一轮签到的结果信封。单独抽出来便于 JVM 之外的单测锁超时语义。
fn wait_for_checkin_result(
    rx: std::sync::mpsc::Receiver<Option<Value>>,
    timeout: std::time::Duration,
    cancel_current: &dyn Fn(),
) -> Value {
    match rx.recv_timeout(timeout) {
        Ok(Some(result)) => json!({ "ok": true, "done": true, "result": result }),
        // None = 已有一轮在执行，本次没有启动新的签到。
        Ok(None) => json!({ "ok": false, "done": true, "error": "busy" }),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // 不杀任务：Provider 请求不能被硬中断；请求在当前账号边界停止，
            // 避免继续跑后续账号（返回 done=false，前端不得当成已完成）。
            cancel_current();
            json!({ "ok": false, "done": false, "error": "timeout" })
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            json!({ "ok": false, "done": true, "error": "checkin task ended without result" })
        }
    }
}

fn checkin_run_and_wait() -> Value {
    let checkin = {
        let Ok(guard) = CORE.lock() else {
            return json!({ "ok": false, "error": "state poisoned" });
        };
        let Some(core) = guard.as_ref() else {
            return json!({ "ok": false, "error": "core_not_ready" });
        };
        core.state.auto_checkin().clone()
    };
    let (tx, rx) = std::sync::mpsc::channel::<Option<Value>>();
    let checkin_task = checkin.clone();
    crate::spawn_task(async move {
        let result = checkin_task.fire("alarm").await;
        // None 也可能表示调度器/手动入口已经抢到 running 标记。
        // FGS 持锁期间必须等那一轮真实结束，不能把 busy 当作任务已结束。
        if result.is_none() {
            while checkin_task.is_running() {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }
        let _ = tx.send(result);
    });
    wait_for_checkin_result(rx, CHECKIN_WAIT_LIMIT, &|| {
        checkin.request_cancel_current();
    })
}

// =====================================================================
// 管理面内核直调（P2 通道层）：Android 管理操作不经端口/网络栈。
// 网关「运行/停止」只决定 8787 端口开不开（B2 语义），管理面在
// Core 态（进程存活、端口未监听）必须照常可用。
// =====================================================================

/// `/api/*` 管理请求的进程内直调路由缓存：键 = `(port, host, local_panel)`。
///
/// `ServerState` 的这三个字段是普通字段，`start_gateway` 改的是 CORE 里那
/// 一份；Router 持有的是首次 `apiCall` 时的 clone，不会跟着变。若缓存永不
/// 失效，改端口后 OAuth 回调地址与登录监听仍用旧端口（R50/T2）。Router
/// clone 是 Arc 级，键变化时重建即可。
type RouterCacheKey = (u16, IpAddr, bool);
type RouterCache = Option<(RouterCacheKey, axum::Router)>;

static API_ROUTER: Mutex<RouterCache> = Mutex::new(None);

fn api_call(method: &str, path: &str, body: Option<String>, api_key: &str) -> Value {
    // 参数显式校验：非法参数返回明确错误，不静默兜底。
    let Ok(method) = axum::http::Method::from_bytes(method.to_uppercase().as_bytes()) else {
        return json!({ "ok": false, "error": format!("invalid method: {method}") });
    };
    if !path.starts_with('/') {
        return json!({ "ok": false, "error": "path must start with /" });
    }
    let Ok(uri) = axum::http::Uri::try_from(path.to_owned()) else {
        return json!({ "ok": false, "error": format!("invalid path: {path}") });
    };
    let Some(state) = (|| -> Option<ServerState> {
        let guard = CORE.lock().ok()?;
        Some(guard.as_ref()?.state.clone())
    })() else {
        return json!({ "ok": false, "error": "core_not_ready" });
    };
    let key = (state.port, state.host, state.local_panel);
    let router = {
        let Ok(mut cached) = API_ROUTER.lock() else {
            return json!({ "ok": false, "error": "router cache poisoned" });
        };
        match cached.as_ref() {
            Some((cached_key, router)) if *cached_key == key => router.clone(),
            _ => {
                let router = server::http::router(state.clone());
                *cached = Some((key, router.clone()));
                router
            }
        }
    };
    let mut builder = axum::http::Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    if !api_key.is_empty() {
        builder = builder.header("authorization", format!("Bearer {api_key}"));
    }
    // 请求体用 axum::body::Body（上游 CORS 测试同款）——bytes::Bytes 直传会
    // 撞上 http-body 版本分裂的 trait impl 不匹配，Body 由 axum 自己的依赖树提供。
    let Ok(request) = builder.body(match &body {
        Some(b) => axum::body::Body::from(b.clone()),
        None => axum::body::Body::empty(),
    }) else {
        return json!({ "ok": false, "error": "build request failed" });
    };
    // oneshot 派到内核全局 runtime，mpsc 同步等结果（与 checkin_run_now 同款模式）。
    let (tx, rx) = std::sync::mpsc::channel::<Option<(u16, String)>>();
    crate::spawn_task(async move {
        let result = match router.clone().oneshot(request).await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                match axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024).await {
                    Ok(bytes) => Some((status, String::from_utf8_lossy(&bytes).into_owned())),
                    Err(_) => Some((
                        status,
                        "{\"success\":false,\"error\":\"body read failed\"}".to_owned(),
                    )),
                }
            }
            Err(_) => None,
        };
        let _ = tx.send(result);
    });
    match rx.recv_timeout(std::time::Duration::from_millis(120_000)) {
        Ok(Some((status, text))) => json!({ "ok": true, "status": status, "body": text }),
        Ok(None) => json!({ "ok": false, "error": "router dispatch failed" }),
        Err(_) => json!({ "ok": false, "error": "timeout after 120s" }),
    }
}

/// 管理面内核直调入口（P2 通道层）：method+path+body 走内存 `Router::oneshot`，
/// 完整复用 HTTP 路由与中间件栈（鉴权/CORS/错误信封一致），但不 bind 端口。
/// 返回 `{ok, status, body}`，body 为响应原文，调用方自行解析业务信封。
#[no_mangle]
pub extern "system" fn Java_dev_agate_core_Gateway_nativeApiCall<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    method: JString<'local>,
    path: JString<'local>,
    body: JString<'local>,
    api_key: JString<'local>,
) -> jstring {
    // JNI 参数读取失败不能折叠成空字符串：那会把参数错误伪装成路由/鉴权
    // 错误，而且 get_string 失败时可能已有 pending Java exception。
    let method = match read_jstring(&mut env, &method) {
        Ok(value) => value,
        Err(error) => return jni_argument_error(&mut env, "method", error),
    };
    let path = match read_jstring(&mut env, &path) {
        Ok(value) => value,
        Err(error) => return jni_argument_error(&mut env, "path", error),
    };
    let api_key = match read_jstring(&mut env, &api_key) {
        Ok(value) => value,
        Err(error) => return jni_argument_error(&mut env, "api_key", error),
    };
    // Kotlin 契约中 body 是 nullable：null 与空字符串都表示无请求体。
    let body = if body.is_null() {
        None
    } else {
        match read_jstring(&mut env, &body) {
            Ok(value) if !value.is_empty() => Some(value),
            Ok(_) => None,
            Err(error) => return jni_argument_error(&mut env, "body", error),
        }
    };
    json_reply(&mut env, api_call(&method, &path, body, &api_key))
}

fn start_gateway(data_dir: &str, port: u16, bind_lan: bool, api_key: Option<&str>) -> Value {
    let Ok(_op) = GATEWAY_OP.lock() else {
        return json!({ "ok": false, "error": "gateway operation lock poisoned" });
    };
    // 先判断运行态，再触碰进程级鉴权环境变量。否则「LAN 正在运行时，
    // 重复发起本机模式 start」会先 remove_var，再返回 already_running，
    // 导致正在运行的 LAN 网关瞬间失去 API Key。
    {
        let Ok(mut guard) = RUNNING.lock() else {
            return json!({ "ok": false, "error": "state poisoned" });
        };
        reap_finished_phase(&mut guard);
        match &*guard {
            GatewayPhase::Running(r) => {
                return json!({
                    "ok": false,
                    "error": "already_running",
                    "port": r.port,
                    "bind_lan": r.bind_lan,
                    "version": VERSION
                });
            }
            GatewayPhase::Stopping(s) => {
                // 上一个 stop 尚未收尾：此时 bind 必然撞旧 listener，明确拒绝。
                return json!({
                    "ok": false,
                    "error": "stopping",
                    "port": s.port,
                    "bind_lan": s.bind_lan,
                    "stopping": true,
                    "version": VERSION
                });
            }
            GatewayPhase::Stopped => {}
        }
    }
    // 登录期可能临时占着该端口（Core 态收回调用，见 core::login_listener）：
    // 必须先释放并等端口归还，否则下面的 bind 会撞自己。
    crate::server::core::login_listener::release();
    // Android host 的 Key 是显式运行时覆盖，不写进进程环境变量。
    // `Some(None)` 表示本机回环模式明确不注入 Key；LAN 模式必须有调用方传入的 Key。
    let runtime_key = api_key.filter(|key| bind_lan && !key.trim().is_empty());
    crate::server::config::parse::set_android_api_key_override(runtime_key);
    // Core 兜底：网关必先有 Core（通常 Application 已初始化，这里兜异常路径）
    if let Err(error) = ensure_core(data_dir, port) {
        crate::server::config::parse::clear_android_api_key_override();
        let (code, reason) = ensure_error_parts(error);
        return match code {
            Some(code) => {
                json!({ "ok": false, "stage": "bootstrap", "code": code, "error": reason })
            }
            None => json!({ "ok": false, "stage": "bootstrap", "error": reason }),
        };
    }
    let host: IpAddr = if bind_lan {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };
    let handles = {
        let Ok(mut core_guard) = CORE.lock() else {
            crate::server::config::parse::clear_android_api_key_override();
            return json!({ "ok": false, "error": "state poisoned" });
        };
        let Some(core) = core_guard.as_mut() else {
            crate::server::config::parse::clear_android_api_key_override();
            return json!({ "ok": false, "error": "core_not_ready" });
        };
        // 端口可能晚于 Core 就位（core_init 用默认口，这里才是用户配置口）
        // → 重设三家 OAuth 的回调登记 + 监听参数，与 bootstrap 首装同口径。
        crate::server::core::providers::accio::oauth::set_loopback_port(port);
        crate::server::core::providers::codearts::oauth::set_loopback_port(port);
        crate::server::core::providers::antigravity::oauth::set_loopback_port(port);
        core.state.port = port;
        core.state.host = host;
        // 管理界面（这里是 Compose UI）跑在本机：OAuth 回调可占本机登记端口
        core.state.local_panel = true;
        // 局域网闸门幂等设置：防上一次 LAN 会话的 panel_gate 残留到本次本机模式
        if bind_lan {
            server::access::set_panel_gate(true);
            if server::config::current().active_api_keys().is_empty() {
                server::access::set_v1_fail_closed(true);
                server::logging::log(
                    "[Security]",
                    "⚠️  局域网监听已开启但未配置 API Key：/v1/* 暂时拒绝服务，请在面板创建第一把 Key",
                );
            }
        } else {
            server::access::set_panel_gate(false);
        }
        // R49/T3：先绑定成功，再提交「随网关而生」的副作用。
        // start_gateway_tasks() 当前唯一内容是 Sidecar 自动拉起（三重守卫在
        // 函数内部）；旧顺序在 bind 失败时也会把它提交出去——用户看到
        // 「网关起不来」，后台却多了一个内核进程。
        match server::run_after_successful_bind(
            || server::start_tracked(&core.state),
            server::start_gateway_tasks,
        ) {
            Ok(handles) => handles,
            Err(conflict) => {
                crate::server::config::parse::clear_android_api_key_override();
                logcat_write(
                    LOG_ERROR,
                    LOGCAT_TAG,
                    &format!("网关启动失败: {}", conflict.message),
                );
                return json!({
                    "ok": false,
                    "stage": "bind",
                    "kind": conflict.kind.as_str(),
                    "error": conflict.message
                });
            }
        }
    };
    let Ok(mut guard) = RUNNING.lock() else {
        let _ = handles.shutdown.send(());
        crate::server::config::parse::clear_android_api_key_override();
        return json!({ "ok": false, "error": "state poisoned" });
    };
    server::logging::log(
        "[Server]",
        &format!(
            "Agate 网关运行中：{}:{port}（内核 v{VERSION}）",
            if bind_lan { "0.0.0.0" } else { "127.0.0.1" }
        ),
    );
    *guard = GatewayPhase::Running(Running {
        shutdown_tx: Some(handles.shutdown),
        port,
        bind_lan,
        started_at: Instant::now(),
        done: handles.done,
    });
    // SSE 总线（M3）：App 的 GatewayRepository 重连上即收到，UI 据此从
    // 「轮询降级」回到 SSE 通道（O9）。stop 不推 —— 那时 SSE 已断流，
    // 客户端靠 onFailure 感知，重连后的 hello 帧补位。
    crate::server::events::publish(
        "gateway_started",
        serde_json::json!({ "port": port, "bindLan": bind_lan, "version": VERSION }),
    );
    json!({ "ok": true, "port": port, "bind_lan": bind_lan, "version": VERSION })
}

fn stop_gateway() -> Value {
    let Ok(_op) = GATEWAY_OP.lock() else {
        return json!({ "ok": false, "error": "gateway operation lock poisoned" });
    };
    // Running → Stopping（取走停机信号）；已经在 Stopping 时保持原相位继续
    // 等待 —— 重复 STOP 不能回 not_running，否则「没停完」会被当成
    // 「本来就没跑」而幂等成功。serve 已自行退出的残留相位先在这里归位。
    let (port, bind_lan, done, was_running) = {
        let Ok(mut guard) = RUNNING.lock() else {
            crate::server::config::parse::clear_android_api_key_override();
            return json!({ "ok": false, "error": "state poisoned" });
        };
        reap_finished_phase(&mut guard);
        match std::mem::replace(&mut *guard, GatewayPhase::Stopped) {
            GatewayPhase::Stopped => {
                crate::server::config::parse::clear_android_api_key_override();
                return json!({ "ok": false, "error": "not_running" });
            }
            GatewayPhase::Running(mut r) => {
                if let Some(tx) = r.shutdown_tx.take() {
                    // 优雅停机：start() 内部的协调任务会把它转成 watch 广播，
                    // 在途请求有 graceful shutdown 收尾（见 server::start）。
                    let _ = tx.send(());
                }
                let done = r.done.clone();
                *guard = GatewayPhase::Stopping(Stopping {
                    port: r.port,
                    bind_lan: r.bind_lan,
                    started_at: r.started_at,
                    done: done.clone(),
                });
                (r.port, r.bind_lan, done, true)
            }
            GatewayPhase::Stopping(s) => {
                let done = s.done.clone();
                let (port, bind_lan) = (s.port, s.bind_lan);
                *guard = GatewayPhase::Stopping(s);
                (port, bind_lan, done, false)
            }
        }
    };

    // 等 serve 真正退出（listener drop）。5s 上限不变，判据从「端口探测」
    // 换成「任务完成标记」：探测成功只说明这一刻没人占，无法证明旧 listener
    // 已经交出端口；完成标记由 serve 任务的 drop guard 记账，是权威信号。
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while Instant::now() < deadline && !done.is_done() {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    if done.is_done() {
        if let Ok(mut guard) = RUNNING.lock() {
            if matches!(*guard, GatewayPhase::Stopping(_)) {
                *guard = GatewayPhase::Stopped;
            }
        }
        crate::server::config::parse::clear_android_api_key_override();
        if was_running {
            server::logging::log("[Server]", &format!("网关已停止（:{port}）"));
        }
        return json!({
            "ok": true,
            "port": port,
            "released": true,
            "stopping": false
        });
    }

    // 超时：保留 Stopping。下一次 status 仍返回 stopping=true，下一次 start
    // 被明确拒绝，直到 done 置位后自动归位 —— 不再把未确认伪装成已停止。
    crate::server::config::parse::clear_android_api_key_override();
    server::logging::log(
        "[Server]",
        &format!("网关停止超时（:{port}），listener 仍在收尾"),
    );
    json!({
        "ok": true,
        "port": port,
        "bind_lan": bind_lan,
        "released": false,
        "stopping": true
    })
}

fn status_json() -> Value {
    let core = {
        let Ok(guard) = CORE.lock() else {
            return json!({ "ok": false, "error": "state poisoned" });
        };
        guard
            .as_ref()
            .map(|c| (c.scheduler_on, c.started_at.elapsed().as_secs()))
    };
    let running = {
        let Ok(mut guard) = RUNNING.lock() else {
            return json!({ "ok": false, "error": "state poisoned" });
        };
        reap_finished_phase(&mut guard);
        match &*guard {
            GatewayPhase::Running(r) => {
                Some((r.port, r.bind_lan, r.started_at.elapsed().as_secs(), false))
            }
            GatewayPhase::Stopping(s) => {
                Some((s.port, s.bind_lan, s.started_at.elapsed().as_secs(), true))
            }
            GatewayPhase::Stopped => None,
        }
    };
    // running/port/bind_lan/uptime_sec 字段向后兼容（M3 的 UI 读这几个）；
    // core/scheduler 是 B 包新增的两级状态。
    let scheduler = core.as_ref().map(|c| c.0).unwrap_or(false);
    match running {
        Some((port, bind_lan, uptime, stopping)) => json!({
            "ok": true,
            "running": !stopping,
            "stopping": stopping,
            "core": true,
            "scheduler": scheduler,
            "bridge": BRIDGE_CONTRACT_VERSION,
            "port": port,
            "bind_lan": bind_lan,
            "uptime_sec": uptime,
            "version": VERSION,
        }),
        None => json!({
            "ok": true,
            "running": false,
            "stopping": false,
            "core": core.is_some(),
            "scheduler": scheduler,
            "bridge": BRIDGE_CONTRACT_VERSION,
            "core_uptime_sec": core.as_ref().map(|c| c.1).unwrap_or(0),
            "version": VERSION,
        }),
    }
}

/// 闹钟唤醒入口。M0 策略：转发到本机管理 API（与面板按钮同一入口），
/// 桥层不直接触达业务协程；路由映射跟随上游 http.rs。
fn trigger_task(task: &str) -> Value {
    let port = {
        let Ok(mut guard) = RUNNING.lock() else {
            return json!({"ok": false, "error": "state poisoned"});
        };
        reap_finished_phase(&mut guard);
        match &*guard {
            GatewayPhase::Running(r) => r.port,
            _ => return json!({"ok": false, "error": "not_running"}),
        }
    };
    let route = match task {
        "scheduled_tick" | "auto_checkin" | "auto-checkin" => "/api/auto-checkin/run",
        "claim_checkin" | "checkin" => "/api/checkin/claim-and-report",
        other => return json!({"ok": false, "error": format!("未知任务: {other}")}),
    };
    let url = format!("http://127.0.0.1:{port}{route}");
    crate::spawn_task(async move {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        // Android host 的 LAN Key 现在保存在显式运行时覆盖中，不再写入
        // AGATE_PROXY_API_KEY 环境变量。内部触发请求必须使用与鉴权中间件
        // 相同的取值入口，否则 LAN 模式会出现「外部请求能鉴权、闹钟内部
        // 请求 401」的不一致。桌面/headless 的环境变量兜底仍由该入口保留。
        let mut request = client.post(&url);
        if let Some(key) = crate::server::config::parse::env_api_key_value() {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
        match request.send().await {
            Ok(resp) => server::logging::log(
                "[Checkin]",
                &format!("闹钟触发 {url} → HTTP {}", resp.status().as_u16()),
            ),
            Err(e) => server::logging::log("[Checkin]", &format!("闹钟触发 {url} 失败: {e}")),
        }
    });
    json!({"ok": true, "dispatched": true, "route": route})
}

// ── JNI 导出（dev.agate.core.Gateway）────────────────────────────

fn read_jstring(env: &mut JNIEnv<'_>, value: &JString<'_>) -> Result<String, String> {
    match env.get_string(value) {
        Ok(value) => Ok(value.to_string_lossy().into_owned()),
        Err(error) => {
            // JNI 字符串读取失败可能伴随 pending Java exception。若不清理，
            // 后续 json_reply/new_string 也会失败，导致 Kotlin 收到 null，
            // 原始错误反而丢失。这里把异常转换成 Rust 错误契约后再清理。
            let message = error.to_string();
            clear_pending_exception(env);
            Err(message)
        }
    }
}

fn jni_argument_error(env: &mut JNIEnv<'_>, argument: &str, error: String) -> jstring {
    json_reply(
        env,
        json!({
            "ok": false,
            "stage": "jni",
            "argument": argument,
            "error": error,
        }),
    )
}

fn json_reply(env: &mut JNIEnv<'_>, value: Value) -> jstring {
    // JNI 规范（Exceptions and Return Values）：pending exception 未清理时再调用
    // 大多数 JNI 函数属未定义行为，且 native 方法带 pending 异常返回时 JVM 会在
    // 调用点抛出该异常 —— 两层兜底的「失败以结构化信封 / null 交付、异常绝不逃逸
    // 到 Kotlin」契约（GatewayEnvelopeTest 锚点）要求先清场再分配。兜底链自身
    // 的首次分配失败（OOM 级）正是会留下 pending 异常的场景（R47/T3.2）。
    clear_pending_exception(env);
    match env.new_string(value.to_string()) {
        Ok(s) => s.into_raw(),
        // OOM 级才走到：先试最小兜底信封（Kotlin 侧按结构化失败解析，不再让
        // 裸 JSONObject 解析点拿 null 抛 NPE）；连它都分配不出来才返回 null，
        // 由 Kotlin envelope() 收敛成 malformed native reply（两层兜底链）。
        Err(_) => {
            let fallback = env
                .new_string(r#"{"ok":false,"stage":"jni","error":"reply alloc failed"}"#)
                .map(|s| s.into_raw());
            if fallback.is_err() {
                // 第二次分配的 pending 异常同样必须清掉：否则 null 一到 Java
                // 侧就会被 JVM 换成抛异常，兜底契约失效（R47/T3.2）。
                clear_pending_exception(env);
            }
            fallback.unwrap_or(std::ptr::null_mut())
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_agate_core_Gateway_nativeStart<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    config: JString<'local>,
) -> jstring {
    let cfg_text = match read_jstring(&mut env, &config) {
        Ok(value) => value,
        Err(error) => {
            return jni_argument_error(&mut env, "config", error);
        }
    };
    let cfg: Value = match serde_json::from_str(&cfg_text) {
        Ok(v) => v,
        Err(e) => {
            return json_reply(
                &mut env,
                json!({"ok": false, "stage": "config", "error": e.to_string()}),
            )
        }
    };
    let Some(data_dir) = cfg
        .get("data_dir")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return json_reply(
            &mut env,
            json!({"ok": false, "stage": "config", "error": "data_dir 必填"}),
        );
    };
    let port = cfg
        .get("port")
        .and_then(Value::as_u64)
        .unwrap_or(8080)
        .clamp(1, 65535) as u16;
    // api_key（M3）：Android host 通过显式运行时覆盖注入，不修改进程环境变量，
    // config::active_keys_from 每次鉴权读取该覆盖，无需写 config.json；
    // 效果 = 上游「兼容模式」：局域网形态下壳的 /api 与 /v1 请求带这把 Key
    // 即过闸门，同时 /v1 不再 fail-closed（keys 非空），Key 可展示给局域网
    // 设备复用（M3 访问密钥页）。
    let api_key = cfg
        .get("api_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_owned);
    let bind_lan = cfg
        .get("bind_lan")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // admin_token：M0 预留 —— M1 局域网模式的 X-Internal-Admin-Token 免登守卫用它
    if let Some(token) = cfg.get("admin_token").and_then(Value::as_str) {
        server::logging::log(
            "[Security]",
            &format!(
                "admin_token 已登记（{} 字符），局域网守卫启用后生效",
                token.chars().count()
            ),
        );
    }
    json_reply(
        &mut env,
        start_gateway(&data_dir, port, bind_lan, api_key.as_deref()),
    )
}

#[no_mangle]
pub extern "system" fn Java_dev_agate_core_Gateway_nativeStop<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    json_reply(&mut env, stop_gateway())
}

/// Core 初始化 + 调度器对齐（Application 启动 / FGS 兜底）。幂等。
#[no_mangle]
pub extern "system" fn Java_dev_agate_core_Gateway_nativeCoreInit<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    data_dir: JString<'local>,
    port: jint,
    scheduler_on: jboolean,
) -> jstring {
    let dir = match read_jstring(&mut env, &data_dir) {
        Ok(value) => value,
        Err(error) => {
            return jni_argument_error(&mut env, "data_dir", error);
        }
    };
    let port = port.clamp(1, 65535) as u16;
    json_reply(&mut env, core_init(&dir, port, scheduler_on != 0))
}

/// 调度器独立开关（B 包：不随网关启停）。
#[no_mangle]
pub extern "system" fn Java_dev_agate_core_Gateway_nativeSchedulerSet<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    on: jboolean,
) -> jstring {
    json_reply(&mut env, scheduler_set(on != 0))
}

/// 同步跑一轮签到并等待（F3：闹钟 tick 直调）。timeout_ms ≤ 0 时默认 120s。
#[no_mangle]
pub extern "system" fn Java_dev_agate_core_Gateway_nativeCheckinRunNow<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    timeout_ms: jlong,
) -> jstring {
    let timeout = if timeout_ms > 0 {
        timeout_ms as u64
    } else {
        120_000
    };
    json_reply(&mut env, checkin_run_now(timeout))
}

/// 请求当前签到轮次在当前账号完成后停止；不取消在途 Provider 请求。
fn checkin_request_stop() -> Value {
    let checkin = {
        let Ok(guard) = CORE.lock() else {
            return json!({ "ok": false, "error": "state poisoned" });
        };
        let Some(core) = guard.as_ref() else {
            return json!({ "ok": false, "error": "core_not_ready" });
        };
        core.state.auto_checkin().clone()
    };
    let requested = checkin.request_cancel_current();
    json!({ "ok": true, "requested": requested, "running": checkin.is_running() })
}

/// FGS 专用：等待本轮签到任务真正结束后再返回，供 WakeLock 生命周期使用。
#[no_mangle]
pub extern "system" fn Java_dev_agate_core_Gateway_nativeRequestCheckinStop<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    json_reply(&mut env, checkin_request_stop())
}

/// FGS 专用：等待本轮签到任务真正结束后再返回，供 WakeLock 生命周期使用。
#[no_mangle]
pub extern "system" fn Java_dev_agate_core_Gateway_nativeCheckinRunAndWait<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    json_reply(&mut env, checkin_run_and_wait())
}

#[no_mangle]
pub extern "system" fn Java_dev_agate_core_Gateway_nativeStatus<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    json_reply(&mut env, status_json())
}

#[no_mangle]
pub extern "system" fn Java_dev_agate_core_Gateway_nativeTriggerTask<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    task: JString<'local>,
) -> jstring {
    let task = match read_jstring(&mut env, &task) {
        Ok(value) => value,
        Err(error) => {
            return jni_argument_error(&mut env, "task", error);
        }
    };
    json_reply(&mut env, trigger_task(&task))
}

#[no_mangle]
pub extern "system" fn Java_dev_agate_core_Gateway_nativeVersion<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    match env.new_string(VERSION) {
        Ok(value) => value.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// 回调对象契约：`void onLog(String level, String tag, String message)`。
/// 传 null 撤销。约定：网关运行期间不要换回调（见 forward_log 的线程安全说明）。
#[cfg(test)]
mod tests {
    use super::*;

    /// R46/T2：status 信封必须携带契约版本号（Kotlin 据此区分新旧内核）。
    /// 测试进程内 CORE/RUNNING 均为空 → 稳定走 not-running 分支。
    #[test]
    fn status_envelope_carries_bridge_contract_version() {
        let status = status_json();
        assert_eq!(status["ok"], json!(true));
        assert_eq!(status["running"], json!(false));
        assert_eq!(status["stopping"], json!(false));
        assert_eq!(status["bridge"], json!(BRIDGE_CONTRACT_VERSION));
    }

    /// R47/T2.3：目录一致（规范化后）→ 幂等 Ok(false)。
    /// R50/T5：等待必须有上限；超时要请求「账号边界停止」且 done=false。
    #[test]
    fn checkin_wait_times_out_and_requests_cancel() {
        let (_tx, rx) = std::sync::mpsc::channel::<Option<Value>>();
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let reply = wait_for_checkin_result(rx, std::time::Duration::from_millis(5), &|| {
            cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        assert_eq!(reply["error"], json!("timeout"));
        assert_eq!(reply["done"], json!(false));
        assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn check_data_dir_match_accepts_same_dir() {
        let dir = std::env::temp_dir().join(format!("agate-ddm-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        assert_eq!(
            check_data_dir_match(&canonical, &dir.to_string_lossy()),
            Ok(false)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R47/T2.3：目录失配 → DataDirMismatch，reason 同时带已绑定与请求目录。
    #[test]
    fn check_data_dir_match_rejects_different_dir() {
        let bound = std::path::Path::new("/data/data/dev.agate/files/agate");
        let requested = std::env::temp_dir();
        match check_data_dir_match(bound, &requested.to_string_lossy()) {
            Err(EnsureError::DataDirMismatch(reason)) => {
                assert!(reason.contains("数据目录不一致"));
                assert!(reason.contains(bound.to_string_lossy().as_ref()));
                assert!(reason.contains(requested.to_string_lossy().as_ref()));
            }
            other => panic!("失配必须报 DataDirMismatch，实际: {other:?}"),
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_agate_core_Gateway_nativeSetLogCallback<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    callback: JObject<'local>,
) -> jstring {
    let result = if callback.is_null() {
        match LOG_CALLBACK.lock() {
            Ok(mut guard) => {
                *guard = None;
                json!({"ok": true, "cleared": true})
            }
            Err(_) => json!({"ok": false, "error": "log_callback_lock_poisoned"}),
        }
    } else {
        match env.get_java_vm() {
            Ok(vm) => {
                let _ = JAVA_VM.set(vm);
                match env.new_global_ref(callback) {
                    Ok(global) => match LOG_CALLBACK.lock() {
                        Ok(mut guard) => {
                            *guard = Some(global);
                            json!({"ok": true})
                        }
                        Err(_) => json!({"ok": false, "error": "log_callback_lock_poisoned"}),
                    },
                    Err(e) => {
                        let message = e.to_string();
                        clear_pending_exception(&mut env);
                        json!({"ok": false, "error": message})
                    }
                }
            }
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        }
    };
    json_reply(&mut env, result)
}
