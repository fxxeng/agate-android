# R43 · T3 Implementation Report — Rust 数据目录跨进程 Owner Lock

**Date**: 2026-11-07
**Baseline**: R42 `ad1bc9cef3a257d9776e1bcf89fac3da5528f649` (CI Run `42235540958` green)
**Branch**: `verify/arch-8faa5f5`
**Tests**: `774 passed; 0 failed; 11 ignored` (full `agate-server` lib suite)

---

## What was added

A cross-process Owner Lock for the Agate data directory. One directory, one process, enforced at startup.

| File | Change |
|---|---|
| `agate-server/src/data_dir_lock.rs` | **new**, 571 lines (module + 13 tests) |
| `agate-server/src/lib.rs` | +1 line: `pub mod data_dir_lock;` |
| `agate-server/Cargo.toml` | +`libc = "0.2"` with rationale comment |
| `agate-server/Cargo.lock` | +`"libc"` in `agate-server` deps (no new packages; libc 0.2.191 already in tree) |
| `agate-server/src/server/mod.rs` | +54: `ServerState.data_dir_lock` field + lock acquisition in `bootstrap()` |
| `agate-server/src/jni_bridge.rs` | +10: lock-lifecycle note on the discarded-`ServerState` branch |

---

## Why it matters

Two Agate processes on the same `{config_dir}` corrupt each other: SQLite `-wal` checkpoints interleave, config snapshots and `kernel-state.json` cross-overwrite. Real reachable combinations: desktop double-launch, Android FGS + debug process, container `restart` racing an old process.

---

## Design decisions

### flock + libc, not `File::try_lock()`
`try_lock()` stabilized in 1.89. This crate declares MSRV 1.88 (from rusqlite 0.40 → libsqlite3-sys 0.38). Using it would silently raise MSRV. `libc` is already in `Cargo.lock` at 0.2.191 via 8 transitive paths — promoting it to a direct dependency adds **zero** packages.

### Never unlink the lock file
`Drop` only closes the fd. Unlinking a locked file lets a new process create a **new inode** at the same path and lock it successfully — two processes, different inodes, both think they own the directory. Test `drop_keeps_lock_file_on_disk` pins this.

### FD_CLOEXEC with double coverage
`open(2)` with `libc::O_CLOEXEC`, then `F_GETFD`/`F_SETFD` re-check. Sidecars (Qoder `runtime-info`, mihomo) exec via `tokio::process` and would inherit an unmarked fd — the host exits, the child lives, the lock never releases, and the next launch is blocked by its own leftover child.

### Same-process re-entry is idempotent (Android requirement)
`flock` counts per **open file description**, not per process — two fds in one process still conflict. Android's `core_init` runs from both Application startup and the FGS fallback. `acquire_shared` uses a `Weak` registry: same process + same dir returns the **same `Arc`**, entries self-clean when `strong_count` hits 0.

### Failures never degrade to "run unlocked"
`ENOSYS` (shared storage without flock support) becomes `Io`, not a silent pass. `LockError` deliberately does **not** implement `From<io::Error>` — that would let call sites `?`-swallow an `Occupied` as a generic IO error.

---

## Startup order (order is correctness)

```
bootstrap():
  1. config_migration::pending_reason()   // pure read-only; returns Err on hit
  2. create_dir_all(config_dir)
  3. data_dir_lock::acquire_shared(...)   // ← LOCK
  4. Db::open(...)
  5. config::init(...)
  6. logging::init_store(...)
  7. account_bootstrap::run(...)
  8. spawn_worker() and other background tasks
```

`pending_reason()` is the first line of `bootstrap` and is read-only, so the "must not create an empty new dir before migration" constraint holds: on a pending migration we return at step 1 and step 2 never runs. That is also why the lock file can live at `{config_dir}/.agate.lock` without a parent-directory workaround.

Lock-success diagnostics use `logging::console_line`, not `logging::log`: `init_store` hasn't run yet, so `log()`'s DB channel silently drops it (`store()` returns `None`). `console_line` goes to stderr + Android logcat, the only visible channel at that point.

---

## Entry-point audit — no bypass

| Location | Kind | Bypasses lock? |
|---|---|---|
| `bin/agate-server.rs:81` | production (headless) | no — uses `bootstrap` |
| `jni_bridge.rs:221` | production (Android) | no — uses `bootstrap` |
| `server/mod.rs:383/416/516/646/853` | inside `bootstrap` | n/a — already under lock |
| `api/upgrade_api.rs:154` | runtime re-run of `account_bootstrap` | no — `ServerState` already alive, lock held |
| `db/mod.rs:326` | `#[cfg(test)] mod test_temp` | n/a — test-only |
| all other `Db::open` / `TempDb::open` | inside `#[cfg(test)]` | n/a |

**No production initialization path bypasses the unified `bootstrap`.** The bin never opens the DB itself (uses `state.db()`); JNI's `ensure_core` is an idempotent reuse.

---

## Tests — 13, all passing

| # | Case |
|---|---|
| 1 | `same_data_dir_second_lock_is_rejected_without_side_effects` |
| 2 | `lock_released_after_drop_allows_reacquire` |
| 3 | `different_data_dirs_can_lock_concurrently` |
| 4 | `symlink_or_alias_to_same_dir_is_rejected` |
| 5 | `bootstrap_conflict_does_not_open_db_or_write_config` |
| 6 | `drop_keeps_lock_file_on_disk` |
| 7 | `lock_file_descriptor_is_close_on_exec` |
| 8 | `cross_process_second_process_is_rejected_then_acquires_after_release` (child gets `Occupied`) |
| 9 | same test (child gets `ACQUIRED` after release) |
| 10 | `same_process_reentry_returns_same_lock` |
| 11 | `lock_error_carries_path_and_kind` |
| 12 | `lock_file_name_is_stable` |
| 13 | `zz_child_probe` / `zz_child_hold_lock` (cross-process fixtures) |

Cross-process cases spawn real children via `std::process::Command` — not same-process double-open. No new dev-dependency; std-only temp dirs.

### A real parallel-test pitfall (fixed)

First parallel run: 1 flaky failure; single-threaded: 13/13 green. Root cause was **not** a library bug:

> `Command::spawn()` doesn't return until the child has `exec`'d, but **between fork and exec** the child copies **all** parent fds — including lock fds held by unrelated tests. If a test happens to `drop` its own lock and immediately re-acquire inside that window, the lock is still held by the child's copy → spurious `Occupied`.

Fix: a test-internal `FORK_GUARD` mutex serializing tests that spawn children against tests that drop-and-reacquire. Result: **8 consecutive parallel runs, 8/8 green.**

(My initial hypothesis — "flock survives fork across exec" — was **disproven** by an isolated probe: `exec` closes fds without CLOEXEC. Only the fork→exec window remains.)

---

## Android filesystem — measured, not assumed

Measured `flock(LOCK_EX|LOCK_NB)` on a real Android 14 device:

| Path | FS | Result |
|---|---|---|
| `/data/data/<pkg>/files/agate` (what `AgateApp.kt:22` / `AgateForegroundService.kt:276` pass) | ext4/f2fs private | **LOCK_OK** |
| another path on same partition | same | **LOCK_OK** |
| `/storage/emulated/0/...`, `/sdcard/...` | FUSE / sdcardfs | **ENOSYS(38)** |

Mainline Android data dir supports the lock. Pointing `AGATE_PROXY_HOME` at shared storage yields `Io` and refuses to start — correct, since SQLite WAL on shared storage is unreliable anyway.

**Measured on a real device** (Android 14 / aarch64) via a standalone executor that compiles the **production source** verbatim (`#[path]`-included, zero `crate::` deps):

| # | Requirement | Result |
|---|---|---|
| 1 | two processes on the real `config_dir` | ✅ second gets `OCCUPIED` |
| 2 | second gets a clear conflict | ✅ message carries the lock path |
| 3 | first exits → second starts cleanly | ✅ `ACQUIRED` after release |
| + | SIGKILL → kernel releases lock | ✅ re-acquires immediately |
| + | `/sdcard` negative case (`ENOSYS`) | ✅ classified as `Io`, refuses to start |

**Still out of scope (deferred to T4):** two real *application processes* calling `coreInit` inside the APK. The executor validates the lock module and Android filesystem semantics; it does **not** cover the `ServerState`/bootstrap integration path — that is covered by the 774 host lib tests plus the ordering audit in §3.

---

## Verification evidence

```
$ cargo test --lib --bins --locked --offline
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 784 filtered out
test result: ok. 774 passed; 0 failed; 11 ignored; 0 measured; 0 filtered out
EXIT=0

$ cargo test --lib --locked --offline data_dir_lock
test result: ok. 13 passed; 0 failed

$ cargo check --lib --bins --locked --offline
EXIT=0    # 7 warnings, all pre-existing (Windows-only dead code + 1 test-only unused import)

$ git apply <patch>            # 在工作区之外的干净基线上
APPLY OK                       # 逐文件 md5 与应用后工作区完全一致
$ cargo check --lib --bins     # 该干净树独立编译
EXIT=0                         # 证明 patch 不依赖任何未纳入的改动

$ cargo fetch --locked         # OK — hand-updated Cargo.lock consistent
```

### CI compile scope widened to `--bins`

`.github/workflows/android-build.yml` 的两条命令从 `--lib` 改为
`--lib --bins`。原因：`src/bin/agate-server.rs` 是 headless 生产入口，
而本批的锁正是在它的启动路径上生效 —— 只跑 `--lib` 覆盖不到。

这个改动当场兑现了价值：`--bins` 第一次跑就暴露出
`warning: field data_dir_lock is never read`（`--lib` 看不到）。
处理方式不是压制，而是按本仓既有惯例（`jni_bridge.rs` 的 `_class: JClass`）
改名为 `_data_dir_lock`，让「刻意不读」这件事在类型签名上就可见。

---

## Three-state

- **Implemented**: full module, wiring, tutorial-level comments explaining every constraint.
- **Verified**: full `--lib --bins` suite 774/0/11 + bin target, lock module 13/13, 8× parallel stability, zero new warnings (7 pre-existing), `--locked` consistency, Android private-dir flock measured OK (4/4 semantics), patch applies cleanly to a clean baseline and that tree compiles on its own.
- **Pending**: Android on-device two-process conflict / release / SIGKILL recovery (needs two live processes); CI `--locked --bins` actual run on GitHub.