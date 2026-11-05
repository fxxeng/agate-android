//! 账号列表快照缓存 —— 「一批账号记录」的内存副本 + 失效判据。
//!
//! ── 为什么需要它（一句话：读路径在每个请求上跑一遍全量读）──────
//! `AccountStore::load` 做的是「读出全部账号行 → 逐行解密 → 逐行建 `StoredAccount`」，
//! 而它的读消费方全在热路径上：
//!   - `list_accounts()` —— 转发选路每次尝试都要（`rotate::accounts_in_providers`），
//!     一次转发里 429 降级几次就调几次；
//!   - 账号页、`/api/session`、模型目录的 `provider_available` 也会各调一次。
//! 账号记录的量级不大（几十条），但每条都要 `AES-256-GCM` 解一次、再 `serde_json`
//! 解析一遍，几十条就是几百微秒 —— 而它的内容在「用户没有改账号」的时候**一个
//! 字节都不会变**。把这批数据留在内存里、只在真正变过时重建，是这条链路上最直接
//! 的收益。
//!
//! ── 失效判据：库里的版本号，不是内存里的计数器 ────────────────
//! 判据是 `kv['accountsRevision']`，由 `accounts` 表的三个写事件触发器维护
//! （`db/schema.rs` 的 `V16_SCHEMA`）。选它而不是「进程内 `AtomicU64`，各写路径
//! 自己加一」的理由写在 `SCHEMA_VERSION` 的 v16 段：本项目有 30 多个
//! `*_accounts.rs` 各自写库，「新增写路径要记得把计数器加一」是一条必然失守的
//! 约定，而失守的表现（界面改了名、转发仍按旧记录选路）极难定位。触发器挂在
//! 表上，连用户用 `sqlite3` 手工改库都绕不过去。
//!
//! ── 不做「最多容忍 N 秒陈旧」 ────────────────────────────────
//! 上游那份实现在缓存命中路径上带一个 1 秒的 `checked_at` 窗口（连版本号都不读）。
//! 这里不跟：本项目的缓存**每次命中都要读一次版本号**，代价是 `kv` 上的一次主键
//! 查找（同一把连接锁、一条 `SELECT`，微秒级），换来的保证是「刚改完账号，下一次
//! 读就是新的」。带时间窗口的话，改完备注名再刷新会话页可能看到旧值 —— 而
//! 「界面写的东西必须是我们真算过的」是本项目的一条硬口径，不值得为省一条
//! 主键查找去模糊它。
//!
//! ── 并发：不用额外的构建锁 ──────────────────────────────────
//! `load` 的签名要求调用方持有 `AccountStore::guard`（那把粗粒度账号锁），而
//! **所有**账号写入都要先拿同一把锁 —— 于是「读版本号 → 判定失效 → 重建」
//! 这一段天然串行，不存在两个线程同时重建、也不用 double-check 之后再校验一遍。
//! 缓存本身只需要 `RwLock`（读多写极少）。

use std::sync::{Arc, RwLock};

use super::state::AccountState;

/// 一份缓存：版本号 + 那一版对应的账号状态。
struct Cached {
    revision: u64,
    state: Arc<AccountState>,
}

/// 账号快照缓存本体（挂在 `AccountStore` 上，随句柄共享）。
#[derive(Default)]
pub(crate) struct SnapshotCache {
    slot: RwLock<Option<Cached>>,
}

impl SnapshotCache {
    /// 命中则给出那一版（`Arc` 共享，不复制记录）；版本不同或还没建过则 None。
    ///
    /// 读锁中毒（某次重建 panic）按「没有缓存」处理：重建一次就能继续用，
    /// 比让所有账号读路径永久失败好得多。
    pub(crate) fn get(&self, revision: u64) -> Option<Arc<AccountState>> {
        let slot = self.slot.read().ok()?;
        let cached = slot.as_ref()?;
        (cached.revision == revision).then(|| cached.state.clone())
    }

    /// 存下刚建好的那一版。旧的一版在这里被替换掉（`Arc` 的最后一个引用若
    /// 恰好在此刻归零，释放的是那块内存，不影响任何在飞的读者 —— 它们手上
    /// 各自持有自己的 `Arc`）。
    pub(crate) fn put(&self, revision: u64, state: Arc<AccountState>) {
        if let Ok(mut slot) = self.slot.write() {
            *slot = Some(Cached { revision, state });
        }
    }

    /// 当前缓存的版本号（`None` = 空缓存）—— 只有测试与排障用。
    #[cfg(test)]
    pub(crate) fn revision(&self) -> Option<u64> {
        self.slot.read().ok().and_then(|slot| slot.as_ref().map(|cached| cached.revision))
    }
}

#[cfg(test)]
mod tests {
    //! 缓存的判据只有两条：**同版本返回同一份**（真的没复制）、**版本变了就不
    //! 命中**（真的会重建）。这两条错了的后果分别是「缓存没用」与「改了账号看不到
    //! 变化」，都不会报错、只会让人以为功能坏了，所以用测试钉住。
    //!
    //! 触发器的行为（写入 → 版本号递增）在 `account_store::sql::tests` 里测，
    //! 那一侧用的是真 SQLite + 真 schema。

    use super::*;
    use crate::server::core::account_store::state::AccountState;

    fn state_with(id: &str) -> Arc<AccountState> {
        let mut state = AccountState::default();
        state.accounts = vec![crate::server::core::account_store::state::StoredAccount::from_value(
            serde_json::json!({ "id": id, "provider": "workbuddy" }),
        )
        .expect("记录")];
        Arc::new(state)
    }

    #[test]
    fn an_empty_cache_never_hits() {
        let cache = SnapshotCache::default();
        assert!(cache.get(0).is_none());
        assert!(cache.revision().is_none());
    }

    #[test]
    fn the_same_revision_hands_out_the_same_copy() {
        let cache = SnapshotCache::default();
        let state = state_with("a");
        cache.put(7, state.clone());
        let hit = cache.get(7).expect("命中");
        // 必须是**同一块**数据（Arc 指针相同）：说明命中路径没有重新构造
        assert!(Arc::ptr_eq(&hit, &state));
    }

    #[test]
    fn a_new_revision_does_not_hit_the_old_copy() {
        let cache = SnapshotCache::default();
        cache.put(7, state_with("a"));
        assert!(cache.get(8).is_none(), "版本变了必须重建");
        // 缓存里留着的仍是旧版本（重建失败时不该把缓存清空、让下一次连旧值都没有）
        assert_eq!(cache.revision(), Some(7));
    }

    #[test]
    fn putting_again_replaces_the_slot() {
        let cache = SnapshotCache::default();
        cache.put(7, state_with("a"));
        cache.put(9, state_with("b"));
        assert_eq!(cache.revision(), Some(9));
        assert!(cache.get(7).is_none());
        assert!(cache.get(9).is_some());
    }
}
