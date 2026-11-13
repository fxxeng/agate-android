//! 灵犀通道选择：`sse`（默认）还是 `ws`（P3）。
//!
//! ── 为什么要一个开关，而不是默认走 WS ────────────────────────
//! 两条通道拿到的东西不一样，**没有一条是全能的**：
//!   · `sse`（默认）：多轮 messages 原样透出、有 token 用量，但拿不到工具
//!     执行（模型只能在正文里说要调什么）；
//!   · `ws`：上游**真执行**工具与沙箱代码（`c_execute_code` 的 output 是真实
//!     stdout），但该协议只收**一段** `agent_input.text`（多轮要压平）、
//!     `run.end` 只给智点消耗不给 token 用量，账号级代理也只作用于建会话。
//! 因此默认保持 `sse`，由用户按需要切 —— 切换后**下一条请求**即生效
//! （`is_stateful()` 每请求现读，没有重启门槛）。
//!
//! ── 存储形态 ────────────────────────────────────────────────
//! 两项标量不到、永远整份读写 ⇒ 落 `kv` 的固定键（与 `proxy_settings` 同类），
//! 键名登记在 `db::schema::RESERVED_KV_KEYS`（否则 `config::save_raw` 写一次
//! 配置就会把它删掉）。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

use serde_json::{json, Value};

use crate::server::db::Db;

/// `kv` 表里的键名（保留键，见 `db::schema::RESERVED_KV_KEYS`）。
pub const KV_KEY: &str = "lingxiChannel";

/// 通道形态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// 私有 SSE（默认，多轮 + usage 完整）
    Sse,
    /// cowork WebSocket（工具执行 + 沙箱，单段文本输入）
    Ws,
}

impl Channel {
    pub fn id(self) -> &'static str {
        match self {
            Channel::Sse => "sse",
            Channel::Ws => "ws",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "sse" | "" => Some(Channel::Sse),
            "ws" | "websocket" | "cowork" => Some(Channel::Ws),
            _ => None,
        }
    }
}

/// 进程级库句柄（照 `core::proxy_settings` 的形态：bootstrap 时 install 一次）。
static DB: OnceLock<Option<Db>> = OnceLock::new();

/// 缓存：`0` = 未初始化，`1` = sse，`2` = ws。
///
/// 为什么不每次读库：`is_stateful()` 在**每条请求**上被问一次（编排层分流），
/// 一次多余的 sqlite 往返不值得；而且设置只有本模块写，缓存不会与库分叉
/// （`set` 同时更新两者）。
static CACHE: AtomicU8 = AtomicU8::new(0);

/// 接线（`ServerState::bootstrap` 调用）。重复调用无副作用。
pub fn install(db: Option<Db>) {
    let _ = DB.set(db);
}

fn read_raw() -> Option<String> {
    let db = DB.get().and_then(Option::as_ref)?;
    db.with(|conn| {
        conn.query_row("SELECT value FROM kv WHERE key = ?1", [KV_KEY], |row| {
            row.get::<_, String>(0)
        })
        .ok()
    })
    .flatten()
}

fn write_raw(channel: Channel) -> Result<(), String> {
    let Some(db) = DB.get().and_then(Option::as_ref) else {
        return Err("设置数据库不可用".to_string());
    };
    let text = json!({ "channel": channel.id() }).to_string();
    let saved = db.with(|conn| {
        conn.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![KV_KEY, text],
        )
        .map(|_| ())
    });
    match saved {
        Some(Ok(())) => Ok(()),
        Some(Err(error)) => Err(format!("保存灵犀通道设置失败: {error}")),
        None => Err("设置数据库不可用".to_string()),
    }
}

fn stored() -> Channel {
    let raw = read_raw()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or_else(|| json!({}));
    raw.get("channel")
        .and_then(Value::as_str)
        .and_then(Channel::parse)
        .unwrap_or(Channel::Sse)
}

fn cache_value(channel: Channel) -> u8 {
    match channel {
        Channel::Sse => 1,
        Channel::Ws => 2,
    }
}

/// 当前通道（**每请求**现读，带进程内缓存）。
pub fn current() -> Channel {
    match CACHE.load(Ordering::Relaxed) {
        1 => Channel::Sse,
        2 => Channel::Ws,
        _ => {
            let channel = stored();
            CACHE.store(cache_value(channel), Ordering::Relaxed);
            channel
        }
    }
}

/// 保存通道。返回保存后的值（与 `current()` 一致）。
pub fn set(channel: Channel) -> Result<Channel, String> {
    write_raw(channel)?;
    CACHE.store(cache_value(channel), Ordering::Relaxed);
    Ok(channel)
}

/// 公开形态 `{channel: "sse"|"ws"}`（API 的读契约）。
pub fn view(channel: Channel) -> Value {
    json!({ "channel": channel.id() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_documented_values_parse() {
        assert_eq!(Channel::parse("sse"), Some(Channel::Sse));
        assert_eq!(Channel::parse("WS"), Some(Channel::Ws));
        assert_eq!(Channel::parse("websocket"), Some(Channel::Ws));
        assert_eq!(Channel::parse(""), Some(Channel::Sse));
        assert_eq!(Channel::parse("grpc"), None);
    }

    #[test]
    fn default_is_sse_when_nothing_is_stored() {
        // 没 install 过库 ⇒ 读不到 ⇒ 必须回落 sse（默认通道不能因为读库失败而变化）
        assert_eq!(stored(), Channel::Sse);
    }

    #[test]
    fn ids_round_trip() {
        assert_eq!(Channel::parse(Channel::Sse.id()), Some(Channel::Sse));
        assert_eq!(Channel::parse(Channel::Ws.id()), Some(Channel::Ws));
        assert_eq!(view(Channel::Ws)["channel"], json!("ws"));
    }
}
