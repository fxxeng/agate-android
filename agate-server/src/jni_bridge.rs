//! Agate JNI 控制面 —— Android 壳（dev.agate.android）与网关内核的唯一接口。
//!
//! 设计原则（docs/Agate移植方案-v3.0.md §3「双通信面」）：
//!   · 数据面：全部业务交互走 127.0.0.1 的 HTTP API（Compose UI 用 Retrofit
//!     消费），JNI 不碰任何业务数据结构；
//!   · 控制面：本文件只提供 6 个「电源开关 + 闹钟 + 日志」函数。
//!
//! 两级启动（B 包 · 方案 v2.0 三开关），桌面壳 `ensure_ready` 的序列拆两段：
//!   Core（随进程）：AGATE_PROXY_HOME → `ServerState::bootstrap` →
//!     `access::attach_db`；后台循环任务（scheduled_tasks + auto_checkin +
//!     订阅内核 worker / zcode 配置同步 / 余额查询，见 bootstrap 末尾区块）
//!     随 Core 常驻，不监听任何端口，也不随网关启停；
//!   网关（手动）：重设 OAuth 回调端口 → `local_panel = true` →（bind_lan
//!     时面板闸门 + /v1 fail-closed）→ `start_gateway_tasks` →
//!     `server::start()` 拿 oneshot 停机通道。
//! `start()` 是同步调用，内部经 `crate::spawn_task` 落到全局惰性 Tokio 运行时
//! （见 lib.rs），因此本桥不需要自建 Runtime —— 与 headless bin 的时序一致
//! （那边也是先 start、再建运行时等信号）。
//!
//! 数据目录经 `AGATE_PROXY_HOME` 注入（与 Docker 部署同一机制），
//! `paths.rs` 零改动。
//!
//! 与上游的全部差异：本文件 + Cargo.toml（lib 改名/cdylib/license/profile/
//! android deps）+ lib.rs 的模块挂载 + logging.rs 的 console_line 挂钩 +
//! bin 的 crate 名引用 —— 网关业务代码零改动。

use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jint, jlong, jobject, jstring};
use jni::{JNIEnv, JavaVM};
use serde_json::{json, Value};
use tokio::sync::oneshot;
use tower::ServiceExt;

use crate::server::{self, ServerState};

/// 内核版本（来自本 crate 的 Cargo.toml，与 APK 版本对齐溯源）
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// logcat 优先级（android/log.h 的数值）
const LOG_INFO: i32 = 4;
const LOG_WARN: i32 = 5;
const LOG_ERROR: i32 = 6;
const LOGCAT_TAG: &str = "AgateCore";

extern "C" {
    fn __android_log_write(prio: i32, tag: *const u8, text: *const u8) -> i32;
}

struct Running {
    shutdown_tx: oneshot::Sender<()>,
    port: u16,
    bind_lan: bool,
    started_at: Instant,
}

/// 网关运行句柄；`None` = 已停止。Mutex 常量化初始化（Rust 1.63+）。
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

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
/// 线程安全：回调调用不持锁（只拷贝裸句柄）；Kotlin 侧约定仅在网关启停时
/// 调用 nativeSetLogCallback，运行期不会与这里竞争释放 GlobalRef。
pub fn forward_log(tag: &str, text: &str) {
    let prio = prio_of(tag, text);
    logcat_write(prio, tag, text);

    if IN_FORWARD.with(|f| f.load(Ordering::Relaxed)) {
        return;
    }
    let Some(vm) = JAVA_VM.get() else { return };
    let Some(raw) = LOG_CALLBACK
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|cb| cb.as_obj().as_raw()))
    else {
        return;
    };
    let message = format!("{tag} {text}");
    // 安全性：raw 来自仍存活的 GlobalRef（见上面的调用约定），此处只借用
    let obj = unsafe { JObject::from_raw(raw) };
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

fn call_on_log(
    env: &mut JNIEnv<'_>,
    obj: &JObject<'_>,
    level: &str,
    tag: &str,
    message: &str,
) -> Result<(), jni::errors::Error> {
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

/// 初始化 Core（幂等）。Ok(true) = 本次新建，Ok(false) = 已在跑。
/// 端口只影响 OAuth 回调登记与 state.port 初值；网关启动时会按需重设。
fn ensure_core(data_dir: &str, port: u16) -> Result<bool, String> {
    {
        let Ok(guard) = CORE.lock() else { return Err("state poisoned".into()); };
        if guard.is_some() {
            return Ok(false);
        }
    }
    let Ok(_boot) = CORE_BOOT.lock() else { return Err("state poisoned".into()); };
    {
        let Ok(guard) = CORE.lock() else { return Err("state poisoned".into()); };
        if guard.is_some() {
            return Ok(false);
        }
    }
    // 数据目录注入：paths::config_dir() 的唯一覆盖口（AGATE_PROXY_HOME）。
    // 进程内单 Core、首启先于任何内核线程，重启时重设同值无副作用。
    std::env::set_var("AGATE_PROXY_HOME", data_dir);
    let state = match ServerState::bootstrap(port, IpAddr::V4(Ipv4Addr::LOCALHOST)) {
        Ok(state) => state,
        Err(reason) => {
            logcat_write(LOG_ERROR, LOGCAT_TAG, &format!("bootstrap 失败: {reason}"));
            return Err(reason);
        }
    };
    // 面板访问控制的库句柄注入（Android 未注册管理员 → 该体系不启用）。
    // Core 态就装：access 状态不依赖端口，网关后启动也读得到同一份。
    server::access::attach_db(state.db().cloned());
    let Ok(mut guard) = CORE.lock() else { return Err("state poisoned".into()); };
    if guard.is_some() {
        // 引导锁窗口内被并发抢先：丢弃本次 bootstrap 产物
        return Ok(false);
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

/// Core 初始化 + 调度器对齐（Application 启动与 FGS 兜底都走这里）。
fn core_init(data_dir: &str, port: u16, scheduler_on: bool) -> Value {
    if let Err(reason) = ensure_core(data_dir, port) {
        return json!({ "ok": false, "stage": "bootstrap", "error": reason });
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
        if on { "后台签到调度已开启" } else { "后台签到调度已关闭" },
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

// =====================================================================
// 管理面内核直调（P2 通道层）：Android 管理操作不经端口/网络栈。
// 网关「运行/停止」只决定 8787 端口开不开（B2 语义），管理面在
// Core 态（进程存活、端口未监听）必须照常可用。
// =====================================================================

/// `/api/*` 管理请求的进程内直调路由缓存：进程内单 Core、`ensure_core`
/// 幂等永不重建 ServerState，Router 只组装一次。
static API_ROUTER: OnceLock<axum::Router> = OnceLock::new();

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
    let router = API_ROUTER.get_or_init(|| server::http::router(state));
    let mut builder = axum::http::Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    if !api_key.is_empty() {
        builder = builder.header("authorization", format!("Bearer {api_key}"));
    }
    let Ok(request) = builder.body(body.unwrap_or_default().into_bytes()) else {
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
                    Err(_) => Some((status, "{\"success\":false,\"error\":\"body read failed\"}".to_owned())),
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
    let mut get = |s: &JString<'local>| -> String {
        env.get_string(s).map(|v| v.to_string_lossy().into_owned()).unwrap_or_default()
    };
    let method = get(&method);
    let path = get(&path);
    let api_key = get(&api_key);
    let body = get(&body);
    let body = if body.is_empty() { None } else { Some(body) };
    json_reply(&mut env, api_call(&method, &path, body, &api_key))
}

fn start_gateway(data_dir: &str, port: u16, bind_lan: bool, api_key: Option<&str>) -> Value {
    // api_key → env（每次启动按本次参数注入；trigger_task 的内部请求也读它）
    if let Some(key) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
        std::env::set_var("AGATE_PROXY_API_KEY", key);
    }
    {
        let Ok(guard) = RUNNING.lock() else {
            return json!({ "ok": false, "error": "state poisoned" });
        };
        if let Some(r) = guard.as_ref() {
            return json!({ "ok": false, "error": "already_running", "port": r.port });
        }
    }
    // Core 兜底：网关必先有 Core（通常 Application 已初始化，这里兜异常路径）
    if let Err(reason) = ensure_core(data_dir, port) {
        return json!({ "ok": false, "stage": "bootstrap", "error": reason });
    }
    let host: IpAddr = if bind_lan {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };
    let shutdown_tx = {
        let Ok(mut core_guard) = CORE.lock() else {
            return json!({ "ok": false, "error": "state poisoned" });
        };
        let Some(core) = core_guard.as_mut() else {
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
        // 网关启动动作（当前只有内核 autostart；后台循环是 Core 级，已在
        // bootstrap 拉起，见 server::start_gateway_tasks 的文档）
        server::start_gateway_tasks();
        match server::start(&core.state) {
            Ok(tx) => tx,
            Err(conflict) => {
                logcat_write(LOG_ERROR, LOGCAT_TAG, &format!("网关启动失败: {}", conflict.message));
                return json!({ "ok": false, "stage": "bind", "error": conflict.message });
            }
        }
    };
    let Ok(mut guard) = RUNNING.lock() else {
        return json!({ "ok": false, "error": "state poisoned" });
    };
    server::logging::log(
        "[Server]",
        &format!(
            "Agate 网关运行中：{}:{port}（内核 v{VERSION}）",
            if bind_lan { "0.0.0.0" } else { "127.0.0.1" }
        ),
    );
    *guard = Some(Running {
        shutdown_tx,
        port,
        bind_lan,
        started_at: Instant::now(),
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
    let r = {
        let Ok(mut guard) = RUNNING.lock() else {
            return json!({ "ok": false, "error": "state poisoned" });
        };
        guard.take()
    };
    let Some(r) = r else {
        return json!({ "ok": false, "error": "not_running" });
    };
    // 优雅停机：start() 内部的协调任务会把它转成 watch 广播，
    // 在途请求有 graceful shutdown 收尾（见 server::start）。
    // 只停网关；Core 与签到调度器不受影响（B 包语义）。
    let _ = r.shutdown_tx.send(());
    server::logging::log("[Server]", &format!("网关已停止（:{}）", r.port));
    // F6：等端口真的释放（serve 退出 + listener drop）再返回 —— 否则
    // 「停止后立刻重启」会 bind 冲突。5s 上限、100ms 探测一次、连续 2 次
    // 成功才判释放（防单次 bind 成功的竞态误判）。
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    let mut hits = 0;
    while Instant::now() < deadline {
        if server::probe_port(r.port).is_ok() {
            hits += 1;
            if hits >= 2 {
                return json!({ "ok": true, "port": r.port, "released": true });
            }
        } else {
            hits = 0;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    json!({ "ok": true, "port": r.port, "released": false })
}

fn status_json() -> Value {
    let core = {
        let Ok(guard) = CORE.lock() else {
            return json!({ "ok": false, "error": "state poisoned" });
        };
        guard.as_ref().map(|c| (c.scheduler_on, c.started_at.elapsed().as_secs()))
    };
    let running = {
        let Ok(guard) = RUNNING.lock() else {
            return json!({ "ok": false, "error": "state poisoned" });
        };
        guard.as_ref().map(|r| (r.port, r.bind_lan, r.started_at.elapsed().as_secs()))
    };
    // running/port/bind_lan/uptime_sec 字段向后兼容（M3 的 UI 读这几个）；
    // core/scheduler 是 B 包新增的两级状态。
    let scheduler = core.as_ref().map(|c| c.0).unwrap_or(false);
    match running {
        Some((port, bind_lan, uptime)) => json!({
            "ok": true,
            "running": true,
            "core": true,
            "scheduler": scheduler,
            "port": port,
            "bind_lan": bind_lan,
            "uptime_sec": uptime,
            "version": VERSION,
        }),
        None => json!({
            "ok": true,
            "running": false,
            "core": core.is_some(),
            "scheduler": scheduler,
            "core_uptime_sec": core.as_ref().map(|c| c.1).unwrap_or(0),
            "version": VERSION,
        }),
    }
}

/// 闹钟唤醒入口。M0 策略：转发到本机管理 API（与面板按钮同一入口），
/// 桥层不直接触达业务协程；路由映射跟随上游 http.rs。
fn trigger_task(task: &str) -> Value {
    let Ok(guard) = RUNNING.lock() else {
        return json!({"ok": false, "error": "state poisoned"});
    };
    let Some(r) = guard.as_ref() else {
        return json!({"ok": false, "error": "not_running"});
    };
    let route = match task {
        "scheduled_tick" | "auto_checkin" | "auto-checkin" => "/api/auto-checkin/run",
        "claim_checkin" | "checkin" => "/api/checkin/claim-and-report",
        other => return json!({"ok": false, "error": format!("未知任务: {other}")}),
    };
    let url = format!("http://127.0.0.1:{}{route}", r.port);
    crate::spawn_task(async move {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        // M3-1 回归修复：env 注入 AGATE_PROXY_API_KEY 后闸门 keys 非空，
        // 内部触发请求也必须带这把 Key，否则闹钟签到 401。
        let mut request = client.post(&url);
        if let Ok(key) = std::env::var("AGATE_PROXY_API_KEY") {
            let key = key.trim().to_string();
            if !key.is_empty() {
                request = request.header("Authorization", format!("Bearer {key}"));
            }
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

fn json_reply(env: &mut JNIEnv<'_>, value: Value) -> jstring {
    match env.new_string(value.to_string()) {
        Ok(s) => s.into_raw(),
        // 基本不可能走到（OOM 级）；返回空串避免崩溃
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_agate_core_Gateway_nativeStart<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    config: JString<'local>,
) -> jstring {
    let cfg_text = match env.get_string(&config) {
        Ok(s) => s.to_string_lossy().into_owned(),
        Err(e) => {
            return json_reply(&mut env, json!({"ok": false, "stage": "config", "error": e.to_string()}))
        }
    };
    let cfg: Value = match serde_json::from_str(&cfg_text) {
        Ok(v) => v,
        Err(e) => {
            return json_reply(&mut env, json!({"ok": false, "stage": "config", "error": e.to_string()}))
        }
    };
    let Some(data_dir) = cfg.get("data_dir").and_then(Value::as_str).map(str::to_owned) else {
        return json_reply(&mut env, json!({"ok": false, "stage": "config", "error": "data_dir 必填"}));
    };
    let port = cfg.get("port").and_then(Value::as_u64).unwrap_or(8080).clamp(1, 65535) as u16;
    // api_key（M3）：壳层生成的本机 Key → env 注入。config::active_keys_from
    // 每次鉴权都实时读 env（parse::env_api_key_value），无需写 config.json；
    // 效果 = 上游「兼容模式」：局域网形态下壳的 /api 与 /v1 请求带这把 Key
    // 即过闸门，同时 /v1 不再 fail-closed（keys 非空），Key 可展示给局域网
    // 设备复用（M3 访问密钥页）。
    let api_key = cfg
        .get("api_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_owned);
    let bind_lan = cfg.get("bind_lan").and_then(Value::as_bool).unwrap_or(false);
    // admin_token：M0 预留 —— M1 局域网模式的 X-Internal-Admin-Token 免登守卫用它
    if let Some(token) = cfg.get("admin_token").and_then(Value::as_str) {
        server::logging::log(
            "[Security]",
            &format!("admin_token 已登记（{} 字符），局域网守卫启用后生效", token.chars().count()),
        );
    }
    json_reply(&mut env, start_gateway(&data_dir, port, bind_lan, api_key.as_deref()))
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
    let dir = match env.get_string(&data_dir) {
        Ok(s) => s.to_string_lossy().into_owned(),
        Err(e) => {
            return json_reply(&mut env, json!({ "ok": false, "error": e.to_string() }))
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
    let timeout = if timeout_ms > 0 { timeout_ms as u64 } else { 120_000 };
    json_reply(&mut env, checkin_run_now(timeout))
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
    let task = match env.get_string(&task) {
        Ok(s) => s.to_string_lossy().into_owned(),
        Err(e) => {
            return json_reply(&mut env, json!({"ok": false, "error": e.to_string()}))
        }
    };
    json_reply(&mut env, trigger_task(&task))
}

#[no_mangle]
pub extern "system" fn Java_dev_agate_core_Gateway_nativeVersion<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    json_reply(&mut env, json!(VERSION))
}

/// 回调对象契约：`void onLog(String level, String tag, String message)`。
/// 传 null 撤销。约定：网关运行期间不要换回调（见 forward_log 的线程安全说明）。
#[no_mangle]
pub extern "system" fn Java_dev_agate_core_Gateway_nativeSetLogCallback<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    callback: JObject<'local>,
) -> jstring {
    let result = if callback.is_null() {
        if let Ok(mut guard) = LOG_CALLBACK.lock() {
            *guard = None;
        }
        json!({"ok": true, "cleared": true})
    } else {
        match env.get_java_vm() {
            Ok(vm) => {
                let _ = JAVA_VM.set(vm);
                match env.new_global_ref(callback) {
                    Ok(global) => {
                        if let Ok(mut guard) = LOG_CALLBACK.lock() {
                            *guard = Some(global);
                        }
                        json!({"ok": true})
                    }
                    Err(e) => json!({"ok": false, "error": e.to_string()}),
                }
            }
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        }
    };
    json_reply(&mut env, result)
}
