//! 「账号 × 模型」的 session / run 缓存（**P0 唯一必须新写的核心零件**）。
//!
//! 上游按 **run** 计费：一个 run 内多次调用只扣一次智点。参考实现因此把
//! `(session_id, run_id)` 按「账号 × 模型」缓存 1 小时、不滑动续期、并发单飞，
//! 上游回 `SQLNoRows` /「会话不存在」时丢弃缓存**重建一次**。
//!
//! 本文件只实现可测的**缓存判据与状态机**：TTL、不滑动、失效重建一次。
//! 真并发单飞由接编排层时用 `tokio::sync::Mutex` 包住「取缓存 → 建会话」
//! 那一段（下一批接线做），这里不伪造一个假锁。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic；
//! 不持锁跨 await（本文件没有 async）。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use super::endpoints::SESSION_TTL_MS;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub session_id: String,
    pub run_id: String,
    pub created_ms: i64,
}

/// **不滑动续期**：只按创建时刻算，读一次不会把寿命往后推。
pub fn is_fresh(created_ms: i64, now_ms: i64) -> bool {
    now_ms.saturating_sub(created_ms) < SESSION_TTL_MS
}

/// 缓存键的唯一构造处（账号 id + 上游真名，全部小写）。
pub fn cache_key(account_id: &str, model: &str) -> String {
    format!("{}|{}", account_id.trim().to_lowercase(), model.trim().to_lowercase())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NextAction {
    UseCached(Session),
    /// 需要建新会话 / run。
    Build,
    /// 已经重建过一次仍失败：交 `OutboundFailure`，不再重试。
    GiveUp,
}

/// 缓存失效后的重建预算：只允许一次。
pub fn next_action(cached: Option<Session>, invalidated: bool, rebuilt_once: bool) -> NextAction {
    if invalidated {
        return if rebuilt_once { NextAction::GiveUp } else { NextAction::Build };
    }
    match cached {
        Some(session) => NextAction::UseCached(session),
        None => NextAction::Build,
    }
}

pub struct SessionCache {
    entries: Mutex<HashMap<String, Session>>,
}

impl SessionCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Session>> {
        self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 读缓存。**只读不续期**：过期条目顺手删掉，不留一条永远「差一点新鲜」的记录。
    pub fn lookup(&self, key: &str, now_ms: i64) -> Option<Session> {
        let mut entries = self.lock();
        // 先 clone 出来结束不可变借用，再决定要不要 remove（否则借用冲突）
        let fresh = entries
            .get(key)
            .filter(|session| is_fresh(session.created_ms, now_ms))
            .cloned();
        if fresh.is_none() {
            entries.remove(key);
        }
        fresh
    }

    pub fn store(&self, key: &str, session: Session) {
        self.lock().insert(key.to_string(), session);
    }

    pub fn invalidate(&self, key: &str) {
        self.lock().remove(key);
    }
}

impl Default for SessionCache {
    fn default() -> Self {
        Self::new()
    }
}

/// 进程内唯一的会话缓存实例（`prepare_body` 写、`build_chat_request` 读）。
/// 用 `OnceLock` 而不是各处 `SessionCache::new()`：否则每个调用点各有一份，
/// 「建会话 → 用会话」两步永远对不上。
pub fn global() -> &'static SessionCache {
    static CACHE: OnceLock<SessionCache> = OnceLock::new();
    CACHE.get_or_init(SessionCache::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session {
            session_id: "s1".to_string(),
            run_id: "r1".to_string(),
            created_ms: 1_000,
        }
    }

    #[test]
    fn ttl_is_fixed_and_never_slides() {
        assert!(is_fresh(1_000, 1_000 + SESSION_TTL_MS - 1));
        assert!(!is_fresh(1_000, 1_000 + SESSION_TTL_MS));
        // 反复 lookup 不该把过期时刻推后
        let cache = SessionCache::new();
        cache.store("a|m", session());
        assert!(cache.lookup("a|m", 1_000 + SESSION_TTL_MS - 1).is_some());
        assert!(cache.lookup("a|m", 1_000 + SESSION_TTL_MS).is_none());
    }

    #[test]
    fn cached_session_is_reused_within_the_hour() {
        let cache = SessionCache::new();
        let key = cache_key("ACC-1", "GLM-5.3");
        cache.store(&key, session());
        assert_eq!(cache.lookup(&key, 2_000), Some(session()));
        assert_eq!(key, "acc-1|glm-5.3");
    }

    #[test]
    fn invalidation_allows_exactly_one_rebuild() {
        assert_eq!(next_action(None, true, false), NextAction::Build);
        assert_eq!(next_action(None, true, true), NextAction::GiveUp);
    }

    #[test]
    fn expired_entry_is_removed_not_kept() {
        let cache = SessionCache::new();
        cache.store("a|m", session());
        assert!(cache.lookup("a|m", 1_000 + SESSION_TTL_MS + 1).is_none());
        cache.invalidate("a|m");
        assert!(cache.lookup("a|m", 1_001).is_none());
    }
}
