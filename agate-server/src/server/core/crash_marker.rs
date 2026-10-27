//! 崩溃痕迹：把**上一次运行**的 panic 栈留到这一次的「应用日志」页。
//!
//! ```text
//! GET    /api/logs/crash   读（并清除）上一次运行的崩溃痕迹
//! DELETE /api/logs/crash   丢弃痕迹（用户点掉提示条）
//! ```
//!
//! ── 判据为什么不是「进程有没有干净退出」──────────────────────
//! 桌面服务进程（以及 Node 版 agent2api）常见做法是「启动时写一个 running
//! 标记，正常退出时删掉；下次启动看见标记还在就报崩溃」。**这个判据在 Android
//! 上会误报**：网关是 JNI 库，活在 App 进程里；App 退到后台后进程随时被系统
//! 回收，那是常态而不是故障。照搬会让用户每次从后台回来都看见「上次运行崩溃」。
//!
//! 所以判据反过来 —— **只认真发生了的 panic**：装一个 panic hook，panic 时把
//! 线程名 / 位置 / 消息（尽力带上 backtrace）写进 `kv` 表；下次启动时由日志页
//! 读出来，展示过后清掉。进程被系统杀掉不会留下任何痕迹，因为那条路径上
//! 压根没有 panic。
//!
//! ── 为什么要存进 kv 表而不是另开一个文件 ─────────────────────
//! 崩溃痕迹属于「没有固定结构、整份进出的零散状态」，正是 `kv` 表那一类
//! （见 `db::schema` 模块头）。另开文件要自己处理路径、权限、清理，而它只在
//! 崩溃时才写一次 —— 为一个罕用路径引入第二套持久化，不值。
//!
//! ── 为什么是「读即清」（take）而不是纯读 ─────────────────────
//! 痕迹的语义是「**上一次**运行崩了」。这次读出来展示完，它就不该再出现：
//! 否则用户清掉提示条之前，每次进日志页都会再弹一次，且「上次崩溃」这个说法
//! 会随时间越来越失真。读取侧顺手删除，是让「展示一次」这件事由存储保证，
//! 而不是靠前端记住「我弹过了」。

use std::panic;

use serde_json::{json, Value};

use crate::server::access;

/// `kv` 表里的固定键名（已登记进 `db::schema::RESERVED_KV_KEYS`，
/// 否则用户改一次配置就会把它静默删掉 —— 见那个清单的模块头）。
const KEY: &str = "agateCrash";

/// 单条崩溃痕迹的形状。
pub struct Crash {
    /// 崩溃发生的毫秒 Unix 时间戳（写入那一刻记的）。
    pub at: i64,
    /// panic 的 payload（`panic!("…")` 的文案，或 Box<dyn Any> 的 Debug）。
    pub message: String,
    /// 发生 panic 的线程名；取不到时为空串。
    pub thread: String,
    /// panic 位置（`src/foo.rs:12:5`）；取不到时为空串。
    pub location: String,
    /// 尽力采集的 backtrace 文本；未开启或不可用时为空串。
    pub backtrace: String,
}

impl Crash {
    /// 转成给前端的形状（键名与日志页读的那些对齐）。
    pub fn to_json(&self) -> Value {
        json!({
            "crashed": true,
            "at": self.at,
            "message": self.message,
            "thread": self.thread,
            "location": self.location,
            "backtrace": self.backtrace,
        })
    }
}

/// 装上 panic hook。
///
/// ── 为什么自己再包一层而不是直接 std 默认行为 ─────────────────
/// 默认 hook 只往 stderr 打一行，Android 上那行基本看不到（没有终端），
/// 用户也就无从判断「刚才那次是怎么没的」。这里在打印之后补一次落库，
/// 让下一次启动能读回来。
///
/// ── 为什么 panic 里不能再 panic ───────────────────────────────
/// hook 运行在 unwind 途中，里面任何 panic 会直接 abort。所以落库这一步
/// 全程 `catch_unwind` 兜住，且只用不会分配失败即崩的写法；失败就放弃写
/// 痕迹 —— 崩溃时少一条记录，好过把进程直接带崩（那样连已有的日志都保不住）。
///
/// ── 为什么落库走「抢不到锁就放弃」而不是普通写入 ────────────────
/// 这一句执行时，panic 可能恰恰发生在某个持着数据库连接锁的闭包里
/// （`Db` 是一把 `Mutex<Connection>`）。那时再调普通的 `kv_put` 会自己等自己：
/// std 的 `Mutex` **不可重入**，这不会「慢一下」，而是**永久挂死在崩溃处理里** ——
/// 进程既崩不掉也回不来，表现是 ANR，比丢一条崩溃记录糟得多。所以这里用
/// `access::kv_try_put`：抢到就写，抢不到（并发写或上面那个自持的情形）就
/// 放弃。崩溃痕迹是尽力而为的诊断信息，不是必须落地的业务数据。
///
/// 幂等：重复调用只装一次（OnceLock 语义）。
pub fn install_hook() {
    static INSTALLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if INSTALLED.set(true).is_err() {
        return;
    }

    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        // 先把默认行为（打印）走完 —— 即便落库失败，控制台记录仍在
        previous(info);

        // 落库这一步全程不许再 panic（见上面「为什么 panic 里不能再 panic」）
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let message = payload_text(info.payload());
            let location = info
                .location()
                .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
                .unwrap_or_default();
            let thread = std::thread::current()
                .name()
                .unwrap_or_default()
                .to_string();
            let backtrace = std::backtrace::Backtrace::force_capture().to_string();

            let value = json!({
                "at": crate::server::logging::now_ms(),
                "message": message,
                "thread": thread,
                "location": location,
                "backtrace": backtrace,
            });
            access::kv_try_put(KEY, &value.to_string());
        }));
    }));
}

/// 取出并清除上一次运行的崩溃痕迹；没有返回 None。
///
/// ── 为什么容忍坏值而不是报错 ─────────────────────────────────
/// 这行值可能来自更老的版本、也可能被写坏。它是**提示条**的数据源，不是
/// 业务判据：读不出来就当没崩过，界面显示正常 —— 好过为一条提示把整个
/// 日志页打成错误态。
pub fn take() -> Option<Crash> {
    let raw = access::kv_get(KEY)?;
    // 先删再解析：痕迹的语义是「**上一次**运行崩了」，读出来就算展示过。
    // 坏值同样要清 —— 留着它只会让下一次进来再走一遍"读不出来"的路径。
    access::kv_delete(KEY);
    parse(&raw)
}

/// 原始 JSON 文本 → [Crash]；任何不认识的形状都当「没崩过」。
///
/// 单独拆出来是为了**可测**：`take()` 走的是 `access` 的全局库句柄，而那是
/// 个 `OnceLock`（注入只生效一次、且早于本模块的测试可能已被别人注入），
/// 用例里没法可靠地指向自己的临时库。解析这一段才是真正需要覆盖的逻辑
/// （坏值 / 缺字段 / 类型不对），它也完全不依赖全局状态。
fn parse(raw: &str) -> Option<Crash> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let object = value.as_object()?;
    let at = object.get("at").and_then(Value::as_i64).unwrap_or(0);
    fn text(object: &serde_json::Map<String, Value>, key: &str) -> String {
        object
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
    Some(Crash {
        at,
        message: text(object, "message"),
        thread: text(object, "thread"),
        location: text(object, "location"),
        backtrace: text(object, "backtrace"),
    })
}

/// panic payload → 文本（`&str` / `String` 直接取，其余走 Debug）。
///
/// 与 `logging::humanize` 那类映射无关：这里不改写文案，只负责把任意 payload
/// 变成一个能落库的 &str —— 崩溃痕迹的价值在于**原样保留现场**。
fn payload_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ── 为什么这组用例只测 `parse` 而不测 `take` ──────────────────
    /// `take()` 的读写都走 `access` 的**全局**库句柄，而它由
    /// `access::attach_db` 用 `OnceLock::set` 注入 —— 只有第一次调用生效。
    /// 在 `cargo test` 的多线程环境里，另一个模块先注入自己的临时库是常态，
    /// 那样本模块注入会被静默丢弃，用例读写的其实是别人的库：结果要么假绿、
    /// 要么假红，而两种都会让这组测试变成负担。解析这一段才是本模块真正
    /// 需要覆盖的逻辑（坏值 / 缺字段 / 类型不对），且完全不依赖全局状态。
    ///
    /// 全局库那条路径由集成层覆盖：`api/logs_api.rs` 的 `take_crash` 在真实
    /// 启动流程里跑（那里 `attach_db` 已经生效）。

    #[test]
    fn parse_reads_all_fields() {
        let crash = parse(
            r#"{"at":1700000000000,"message":"boom","thread":"tokio-runtime-worker","location":"src/api/chat.rs:88:9","backtrace":"frame 0"}"#,
        )
        .expect("完整形状应当解析成功");
        assert_eq!(crash.at, 1_700_000_000_000);
        assert_eq!(crash.message, "boom");
        assert_eq!(crash.thread, "tokio-runtime-worker");
        assert_eq!(crash.location, "src/api/chat.rs:88:9");
        assert_eq!(crash.backtrace, "frame 0");
    }

    #[test]
    fn parse_tolerates_missing_fields() {
        // 旧版本可能只写了 at 与 message：其余字段读成空串，不该整条丢掉 ——
        // 提示条至少还能告诉用户"崩过、时间是某刻"。
        let crash = parse(r#"{"at":1,"message":"old"}"#).expect("缺字段也应当解析成功");
        assert_eq!(crash.at, 1);
        assert_eq!(crash.message, "old");
        assert_eq!(crash.thread, "");
        assert_eq!(crash.location, "");
        assert_eq!(crash.backtrace, "");
    }

    #[test]
    fn parse_treats_wrong_types_as_defaults() {
        // at 是字符串而不是数字：按 0 处理而不是整条判失败（展示层的容错取向）。
        let crash = parse(r#"{"at":"123","message":42}"#).expect("类型不对不应当让整条失败");
        assert_eq!(crash.at, 0);
        assert_eq!(crash.message, "");
    }

    #[test]
    fn parse_rejects_broken_shape() {
        assert!(parse("{not json").is_none(), "坏 JSON 应当被当成「没崩过」");
        assert!(parse("[1,2,3]").is_none(), "非对象应当被当成「没崩过」");
        assert!(parse("null").is_none(), "null 应当被当成「没崩过」");
    }

    #[test]
    fn payload_text_handles_str_and_string() {
        let owned = String::from("owned panic");
        assert_eq!(payload_text(&owned), "owned panic");
        assert_eq!(payload_text(&"static panic"), "static panic");
    }
}
