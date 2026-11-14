//! 导入**用户自己拷来的** QuWork 桌面客户端登录库（`qurwork-desktop.sqlite`）。
//!
//! ── 为什么不按本机固定路径读 ──────────────────────────────────
//! 上游 `desktop.rs` 在桌面程序里直接拼 `%APPDATA%\qurwork-desktop\qurwork-desktop.sqlite`
//! 去读登录态。本仓跑在 Android 上，那条路径**不存在**，给入口只会稳定失败。
//! 但用户可以在电脑上把那个文件拷到手机，通过「导入凭证文件」选进来 —— 于是这里
//! 只处理**这一次请求带来的字节**，一个字节都不从本机固定路径读。
//!
//! ── 文件长什么样（实测本机 1.6.x 客户端）────────────────────────
//! ```text
//!   qurwork-desktop.sqlite
//!     auth_session(id, schema_version, session_json, updated_at)   ← 只有一行
//!     session_json = {
//!       "accessToken": "<JWT>",   // 账号面：续期 / 积分 / 签到 / 兑换
//!       "llmToken":    "<opaque>",// 模型面：对话
//!       "expiresAt":   1794629131563,
//!       "user": { "id": 5217, "phone": "…", … }
//!     }
//! ```
//! 明文（不是 DPAPI 密文）—— 这一点很关键：用户「拷文件过来」这条路可行，
//! 不需要连桌面端的 `account.key` 一起带。
//!
//! ── 为什么先落临时文件再读 ────────────────────────────────────
//! rusqlite 按**路径**打开。上游与本仓都只有「打开一个 SQLite 文件」这条读链，
//! 不另写「从内存字节解析 SQLite」（那等于让两边对「库有效」的判定分叉）。
//! 临时文件的生命周期只覆盖本次读取，**无论成败立即删除**（与 KukuAI 的
//! Cookies 导入同一处置，见 `api::accounts::kuku_credentials_from_b64_file`）。
//!
//! ⚠️ 目录必须取 `core::scratch::dir()`：Android 上 `std::env::temp_dir()` 是
//! `/data/local/tmp`（shell 所有），app 写进去就是 `Permission denied (os error 13)`
//! —— 真机反馈的第一张图就是这个。`scratch::dir()` 优先用 JNI 传进来的
//! `filesDir`（环境变量 `AGATE_PROXY_HOME`）。
//!
//! ── WAL：拷主库可能读到旧会话 ────────────────────────────────
//! 客户端在运行时最近的写入可能还在 `-wal` 里。这里读不到会话时，错误文案直接
//! 告诉用户「先完全退出客户端，再连同 -wal/-shm 一起拷」—— 这是唯一能给出
//! 确定结论的做法（猜「可能没登录」会把人带偏）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use rusqlite::OptionalExtension;
use serde_json::Value;

use super::credentials;

/// SQLite 文件头（`SQLite format 3\0`，共 16 字节）
const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";
/// 桌面客户端登录库里的会话表
const SESSION_TABLE: &str = "auth_session";
/// 单个文件上限（客户端库实测几百 KB；超过就明显不是这个文件）
pub const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;

/// 这段字节是不是 SQLite 文件（看文件头，不看扩展名）。
pub fn looks_like_sqlite(bytes: &[u8]) -> bool {
    bytes.len() >= SQLITE_MAGIC.len() && &bytes[..SQLITE_MAGIC.len()] == SQLITE_MAGIC
}

/// 临时文件路径（`core::scratch` 下的随机名，手法同 `api::accounts` 里 KukuAI 那条）
///
/// ⚠️ 目录取 `core::scratch::dir()` 而不是 `std::env::temp_dir()`：Android 上后者
/// 是 `/data/local/tmp`，app 无权写 ⇒ 真机报 `Permission denied (os error 13)`。
fn temp_path() -> std::path::PathBuf {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(crate::server::logging::now_ms() as u64);
    hasher.write_u64(std::process::id() as u64);
    let suffix = (hasher.finish() & 0xffff_ffff) as u32;
    crate::server::core::scratch::dir().join(format!(
        "quwork-session-{}-{}-{suffix:08x}.sqlite",
        std::process::id(),
        crate::server::logging::now_ms()
    ))
}

/// 打开临时库，取出 `auth_session.session_json` 的**最新一行**。
///
/// 用默认的读写方式打开（不是 `SQLITE_OPEN_READ_ONLY`）：客户端库是 WAL 模式，
/// 只读连接在 `-wal` / `-shm` 缺失时可能连打开都失败，而读写连接能就地完成
/// WAL 恢复。改的是**我们自己刚写出的临时副本**，与用户原文件无关，读完即删。
fn read_session_json(path: &std::path::Path) -> Result<String, String> {
    let connection = rusqlite::Connection::open(path)
        .map_err(|error| format!("无法作为 SQLite 数据库打开（{error}）"))?;
    // 表不存在时给的是「没找到这张表」而不是通用的 SQL 错误：用户拿错文件
    // （比如拷了别家的库）时，这句话要能直接指出问题。
    let exists: i64 = connection
        .query_row(
            "select count(*) from sqlite_master where type = 'table' and name = ?1",
            rusqlite::params![SESSION_TABLE],
            |row| row.get(0),
        )
        .map_err(|error| format!("读取数据库结构失败（{error}）"))?;
    if exists == 0 {
        return Err(format!(
            "这不是 QuWork 桌面客户端的登录库：里面没有 {SESSION_TABLE} 表。\
             请确认选的是 qurwork-desktop.sqlite"
        ));
    }
    let read = |sql: &str| -> Result<Option<String>, rusqlite::Error> {
        connection
            .query_row(sql, rusqlite::params![], |row| row.get::<_, String>(0))
            .optional()
    };
    let session = match read(&format!(
        "select session_json from {SESSION_TABLE} order by updated_at desc limit 1"
    )) {
        Ok(session) => session,
        // 老版本 schema 没有 updated_at（客户端自己带 schema_version，说明表会演进）：
        // 退回「随便取一行」—— 这张表实测只有一行，退化的只是排序而不是数据
        Err(_) => read(&format!("select session_json from {SESSION_TABLE} limit 1"))
            .map_err(|error| format!("读取登录会话失败（{error}）"))?,
    };
    session.filter(|text| !text.trim().is_empty()).ok_or_else(|| {
        format!(
            "数据库里没有登录会话（{SESSION_TABLE} 为空或 session_json 为空）。\
             最新一次登录很可能还在 -wal 文件里没合并：请先完全退出 QuWork 客户端\
             （退出时会把 -wal 合并回主库），再把 qurwork-desktop.sqlite 拷一次重试"
        )
    })
}

/// `session_json` → 本仓凭证对象（`accessToken` / `llmToken` / `user` / `expiresAt`）。
///
/// 走 `credentials::parse` 同一套校验：`accessToken` 会被改名成 `loginToken`
/// （本仓转发层占用了 `accessToken` 这个名字），`deviceIdentifier` 之类附加字段
/// 若在 session 里也一并保留。
pub fn secret_from_session_json(session_json: &str) -> Result<Value, String> {
    let parsed: Value = serde_json::from_str(session_json)
        .map_err(|error| format!("登录会话不是有效 JSON（{error}）"))?;
    if !parsed.is_object() {
        return Err("登录会话格式异常（不是 JSON 对象）".to_string());
    }
    credentials::parse(&parsed).map_err(|error| error.message)
}

/// 从用户拷来的客户端数据库字节解析出凭证（**唯一入口**）。
pub fn credentials_from_sqlite_bytes(bytes: &[u8]) -> Result<Value, String> {
    if bytes.is_empty() {
        return Err("文件是空的".to_string());
    }
    if bytes.len() > MAX_FILE_BYTES {
        return Err(format!(
            "文件过大（{} MB，上限 {} MB），这不像是 QuWork 的登录库",
            bytes.len() / (1024 * 1024),
            MAX_FILE_BYTES / (1024 * 1024)
        ));
    }
    if !looks_like_sqlite(bytes) {
        return Err(
            "这不是 SQLite 数据库文件。QuWork 的登录库是 qurwork-desktop.sqlite；\
             如果你选的是 .credentials.yaml，请改用文本粘贴（里面有 QURIVS_LLM_API_KEY 行）"
                .to_string(),
        );
    }
    let path = temp_path();
    if let Err(error) = std::fs::write(&path, bytes) {
        let _ = std::fs::remove_file(&path);
        return Err(format!(
            "写入临时目录失败（{}）：{error}；这是 App 私有目录不可写（不是文件的问题），\
             请确认存储空间充足后重启 App 再试",
            path.display()
        ));
    }
    let session = read_session_json(&path);
    // 读后即删：成功时凭证已在内存，失败时内容也不该留在盘上
    let _ = std::fs::remove_file(&path);
    let session = session?;
    secret_from_session_json(&session)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_check_only_trusts_the_header() {
        assert!(looks_like_sqlite(b"SQLite format 3\0rest"));
        assert!(!looks_like_sqlite(b"{\"llmToken\":\"x\"}"));
        assert!(!looks_like_sqlite(b"SQLite"));
    }

    #[test]
    fn session_json_maps_access_token_to_login_token() {
        // 实测形态：accessToken 是 JWT，llmToken 是不透明串
        let session = r#"{
            "accessToken": "a.b.c",
            "llmToken": "llm-raw",
            "expiresAt": 1800000000000,
            "user": { "id": 5217, "phone": "13900000000" }
        }"#;
        let secret = secret_from_session_json(session).unwrap_or_default();
        assert_eq!(secret.get("loginToken").and_then(Value::as_str), Some("a.b.c"));
        assert_eq!(secret.get("llmToken").and_then(Value::as_str), Some("llm-raw"));
        assert_eq!(
            secret.pointer("/user/uid").and_then(Value::as_str),
            Some("5217")
        );
        assert!(secret.get("accessToken").is_none(), "转发占用的键不能被写进去");
    }

    #[test]
    fn a_session_without_llm_token_is_refused() {
        let session = r#"{ "accessToken": "a.b.c" }"#;
        let error = secret_from_session_json(session).err().unwrap_or_default();
        assert!(error.contains("llmToken"), "实际文案：{error}");
    }

    #[test]
    fn a_non_json_file_says_what_it_looked_like() {
        let error = credentials_from_sqlite_bytes(b"not a database")
            .err()
            .unwrap_or_default();
        assert!(error.contains("SQLite"), "实际文案：{error}");
    }

    #[test]
    fn reads_a_real_database_from_bytes() {
        // 现场造一个与桌面客户端同构的库，走完整链路：字节 → 临时文件（scratch 目录）
        // → 读 auth_session → 解析成凭证。钉住「有没有可写目录」这一步 —— 真机上
        // 写 /data/local/tmp 就是在这里失败的。
        let path = crate::server::core::scratch::dir().join(format!(
            "quwork-test-{}-{}.sqlite",
            std::process::id(),
            crate::server::logging::now_ms()
        ));
        let built = (|| -> Result<(), String> {
            let connection = rusqlite::Connection::open(&path).map_err(|e| e.to_string())?;
            connection
                .execute_batch(
                    "create table auth_session(\
                       id text, schema_version integer, session_json text, updated_at integer);",
                )
                .map_err(|e| e.to_string())?;
            connection
                .execute(
                    "insert into auth_session values ('s1', 1, ?1, 1794640000000)",
                    rusqlite::params![
                        r#"{"accessToken":"a.b.c","llmToken":"llm-raw","user":{"id":"5217"}}"#
                    ],
                )
                .map_err(|e| e.to_string())?;
            Ok(())
        })();
        assert!(built.is_ok(), "建测试库失败：{built:?}");
        let bytes = std::fs::read(&path).unwrap_or_default();
        let _ = std::fs::remove_file(&path);
        let secret = credentials_from_sqlite_bytes(&bytes).unwrap_or_default();
        assert_eq!(
            secret.get("llmToken").and_then(Value::as_str),
            Some("llm-raw")
        );
        assert_eq!(
            secret.get("loginToken").and_then(Value::as_str),
            Some("a.b.c")
        );
        assert_eq!(
            secret.pointer("/user/uid").and_then(Value::as_str),
            Some("5217")
        );
    }
}
