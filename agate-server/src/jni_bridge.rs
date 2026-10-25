//! Agate JNI 控制面 —— Android 壳（dev.agate.android）与网关内核的唯一接口。
//!
//! 设计原则（docs/Agate移植方案-v3.0.md §3「双通信面」）：
//!   · 数据面：全部业务交互走 127.0.0.1 的 HTTP API（Compose UI 用 Retrofit
//!     消费），JNI 不碰任何业务数据结构；
//!   · 控制面：本文件只提供 6 个「电源开关 + 闹钟 + 日志」函数。
//!
//! 启动序列逐字对齐桌面壳 `backend.rs::ensure_ready`（去掉 Tauri 相关部分）：
//!   注入 AGATE_PROXY_HOME → `ServerState::bootstrap` → `access::attach_db`
//!   → `local_panel = true` →（bind_lan 时启用面板闸门 + /v1 fail-closed）
//!   → `server::start()` 拿 oneshot 停机通道。
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
use jni::sys::{jobject, jstring};
use jni::{JNIEnv, JavaVM};
use serde_json::{json, Value};
use tokio::sync::oneshot;

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

// ── 内核启停（对齐 backend.rs::ensure_ready）─────────────────────

fn start_gateway(data_dir: &str, port: u16, bind_lan: bool) -> Value {
    let Ok(mut guard) = RUNNING.lock() else {
        return json!({"ok": false, "error": "state poisoned"});
    };
    if let Some(r) = guard.as_ref() {
        return json!({"ok": false, "error": "already_running", "port": r.port});
    }
    // 数据目录注入：paths::config_dir() 的唯一覆盖口（AGATE_PROXY_HOME）。
    // 说明：std 在 2024 edition 才把 set_var 标 unsafe，本 crate 是 2021；
    // 进程内单网关、首启先于任何内核线程，重启时重设同值无副作用。
    std::env::set_var("AGATE_PROXY_HOME", data_dir);
    let host: IpAddr = if bind_lan {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };
    let mut state = match ServerState::bootstrap(port, host) {
        Ok(state) => state,
        Err(reason) => {
            logcat_write(LOG_ERROR, LOGCAT_TAG, &format!("bootstrap 失败: {reason}"));
            return json!({"ok": false, "stage": "bootstrap", "error": reason});
        }
    };
    // 桌面同款：面板访问控制的库句柄注入（Android 未注册管理员 → 该体系不启用）
    server::access::attach_db(state.db().cloned());
    // 管理界面（这里是 Compose UI）跑在本机：OAuth 回调可占本机登记端口
    state.local_panel = true;
    if bind_lan {
        // 局域网监听的安全闸门，语义与桌面 LAN 模式一致（见 backend.rs）
        server::access::set_panel_gate(true);
        if server::config::current().active_api_keys().is_empty() {
            server::access::set_v1_fail_closed(true);
            server::logging::log(
                "[Security]",
                "⚠️  局域网监听已开启但未配置 API Key：/v1/* 暂时拒绝服务，请在面板创建第一把 Key",
            );
        }
    }
    let shutdown_tx = match server::start(&state) {
        Ok(tx) => tx,
        Err(conflict) => {
            logcat_write(LOG_ERROR, LOGCAT_TAG, &format!("网关启动失败: {}", conflict.message));
            return json!({"ok": false, "stage": "bind", "error": conflict.message});
        }
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
    json!({"ok": true, "port": port, "bind_lan": bind_lan, "version": VERSION})
}

fn stop_gateway() -> Value {
    let Ok(mut guard) = RUNNING.lock() else {
        return json!({"ok": false, "error": "state poisoned"});
    };
    match guard.take() {
        Some(r) => {
            // 优雅停机：start() 内部的协调任务会把它转成 watch 广播，
            // 在途请求有 graceful shutdown 收尾（见 server::start）
            let _ = r.shutdown_tx.send(());
            server::logging::log("[Server]", &format!("网关已停止（:{}）", r.port));
            json!({"ok": true, "port": r.port})
        }
        None => json!({"ok": false, "error": "not_running"}),
    }
}

fn status_json() -> Value {
    let Ok(guard) = RUNNING.lock() else {
        return json!({"ok": false, "error": "state poisoned"});
    };
    match guard.as_ref() {
        Some(r) => json!({
            "ok": true,
            "running": true,
            "port": r.port,
            "bind_lan": r.bind_lan,
            "uptime_sec": r.started_at.elapsed().as_secs(),
            "version": VERSION,
        }),
        None => json!({"ok": true, "running": false, "version": VERSION}),
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
    if let Some(key) = cfg.get("api_key").and_then(Value::as_str) {
        let key = key.trim();
        if !key.is_empty() {
            std::env::set_var("AGATE_PROXY_API_KEY", key);
        }
    }
    let bind_lan = cfg.get("bind_lan").and_then(Value::as_bool).unwrap_or(false);
    // admin_token：M0 预留 —— M1 局域网模式的 X-Internal-Admin-Token 免登守卫用它
    if let Some(token) = cfg.get("admin_token").and_then(Value::as_str) {
        server::logging::log(
            "[Security]",
            &format!("admin_token 已登记（{} 字符），局域网守卫启用后生效", token.chars().count()),
        );
    }
    json_reply(&mut env, start_gateway(&data_dir, port, bind_lan))
}

#[no_mangle]
pub extern "system" fn Java_dev_agate_core_Gateway_nativeStop<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    json_reply(&mut env, stop_gateway())
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
