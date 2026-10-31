# Android Check-in Request Timeout Audit

**Audit date:** 2026-10-31  
**Scope:** Agate Android Kotlin + Rust migration branch; Rust provider check-in chains invoked by `billing::checkin::run_checkin_cancellable`.  
**Purpose:** distinguish per-request transport bounds from whole-account / whole-batch bounds before changing STOP, WakeLock, or provider cancellation semantics.

## Executive result

The provider check-in paths inspected here generally set explicit per-request timeouts. That is important because `egress` keeps a long default read timeout for streaming workloads. However, a per-request timeout is **not** a whole-account timeout: a provider can perform several sequential requests, optional refreshes, retries, or post-claim verification. Since accounts run serially, total batch duration is the sum of those chains.

Do not wrap an entire provider future in `tokio::time::timeout` as a blanket fix. In particular, CodeArts and Trae have multi-step write/confirm flows. Dropping such a future after a remote write may leave the server having accepted the write while local confirmation / ledger work never runs.

## Request chain inventory

| Provider | Check-in chain | Per-request bound found | Important aggregate behavior |
|---|---|---:|---|
| WorkBuddy CN | billing claim; optional gift + compensation | billing request policy is in `billing::request`; auxiliary calls are sequential | up to three sequential business actions; enterprise accounts skip the two auxiliary reward calls |
| WorkBuddy Intl | activity probe/report/claim + optional model keepalive/web conversation | activity request and body-read stages explicitly use 20s guards | number of configured keepalive models changes total time; not one request |
| Raccoon | daily grant claim + billing-ledger reconciliation | 20s request; grant reconciliation 25s | claim and ledger check are distinct; don't interrupt between them without checking idempotency semantics |
| AutoClaw | daily task-complete; promotion ledger read; optional user-info trigger; 1.5s settle; promotion reread; failure-only task-list explanation | shared userapi GET/POST helper uses 20s | optional path has multiple sequential requests; a failed claim can add a read-only task-list request |
| Qoder CN | campaign list; optional token refresh/re-list; campaign claim | `auth::request` uses 15s | refresh and claim add sequential requests |
| Qoder Intl | campaign list → risk identity augmentation → campaign list → optional claim; optional token refresh path | HTTP requests 15s; UMID subprocess has its own 25s guard | two list calls are normal; risk generation is separate from HTTP |
| Trae | status → claim → same-device status verification; optional forced renewal and replay | each check-in HTTP request 20s | up to three sequential calls per attempt; verification is required before reporting success |
| Loomy | first-login points refresh | shared client uses 20s | single write request |
| KukuAI | business STOKEN preparation; panel read; LOGIN task write; CHAT task write | task HTTP calls explicitly use 30s; shared helper default 30s | up to three sequential HTTP calls after session preparation; task writes are intentionally sequential and idempotent |
| OfficeAce | subscription snapshot → bonus claim | each request 20s | two sequential requests; pre-claim snapshot is best-effort |
| CodeArts | delivery → claim → confirm → delivery reread | each request 20s | four-stage write/verification chain; local idempotency key is persisted before the write |

The table describes configured request-level guards, not a hard promise for an entire account. Conditional refresh, optional work, multiple campaigns, and provider-specific helper behavior can extend the account chain.

## Defect fixed in this audit

Two non-streaming reward clients swallowed response-body read errors with `response.text().await.unwrap_or_default()`:

- `providers/officeace/subscription.rs`
- `providers/codearts/welfare.rs`

`reqwest::RequestBuilder::timeout` applies through response-body consumption. A timeout or mid-body disconnect could therefore be converted into an empty string and later misreported as malformed JSON / an unconfirmed claim, hiding the transport failure.

Both paths now:

1. propagate response-body read errors instead of replacing them with an empty body;
2. return HTTP-style `504` for timeout and `502` for other transport read failures;
3. preserve the underlying `egress::describe_error_detail` chain for diagnosis;
4. also distinguish request-send timeout (`504`) from other send failures (`502`).

A follow-up inspection found the same failure mode in the shared Trae HTTP helper used by the check-in chain. `Response::bytes()` was read with errors discarded, so a timeout or mid-body disconnect during the post-claim same-device verification became an empty body. This was especially misleading because Trae only reports success after that verification. The helper now:

1. propagates send and body-read errors (504 for timeout, 502 for other transport failures);
2. reads response chunks incrementally and stops at the 1 MiB cap, rather than buffering an arbitrarily large body and truncating afterward;
3. keeps non-2xx HTTP responses as normal `Reply` values, preserving the existing candidate/status decision logic.

This does **not** change successful response parsing, upstream business-code handling, the CodeArts idempotency ledger, or either provider's claim/confirm sequence.

## STOP and WakeLock implications

- `run_checkin_cancellable` observes STOP only at account boundaries. The current provider is allowed to finish and persist its outcome.
- Trae's post-claim verification now surfaces response-body transport failures instead of treating them as an empty/invalid response; the caller can distinguish a timeout from an upstream business rejection.
- This is deliberate: cancellation in the middle of CodeArts claim/confirm or Trae claim/verification can create remote/local ambiguity.
- A single account can still occupy the foreground service and partial WakeLock for the duration of its sequential chain. The request timeouts make that finite for the inspected HTTP stages, but do not establish a strict account-level SLA.
- The Android executor remains FIFO: STOP can signal cooperatively while a TICK is running, but the service lifecycle stop itself waits behind that TICK.
- Do not release the WakeLock merely because a UI-side wait timed out. Release only when the underlying JNI/Rust operation has actually returned.

## Next implementation gate

Before adding an account-level deadline, split each provider chain into explicit phases and classify each phase as:

- **read-only / safely droppable**;
- **idempotent write / safe to retry but not necessarily safe to abandon**;
- **non-idempotent or multi-step write / must complete its local reconciliation path**.

Then add cancellation/deadline checkpoints only between safe phase boundaries. Prioritize CodeArts, Trae, AutoClaw, and Qoder Intl because their chains have the most stateful or conditional phases. A global future timeout or thread interruption is not an acceptable substitute.

## Validation boundary

Static source review and `git diff --check` can verify this patch's shape. Full Rust compilation, provider mock tests, Gradle/Android build, and device validation require the project toolchains and must be run in CI or a configured development environment; they are not claimed by this audit.

## Follow-up: Qoder post-refresh claim result classification

A further review of `providers/qoder/checkin.rs` found an asymmetric result path: the initial campaign claim recognized the upstream `409` / `ALREADY_CLAIMED` response as an idempotent "already completed" outcome, but the one permitted claim retry after a `401` refresh went directly to `auth::payload()`. If that retried claim returned the same already-claimed result (for example, a concurrent client completed the campaign between the first request and retry), it was surfaced as an HTTP failure instead of the normal completed-today result.

The response classification is now shared by both the first claim and the post-refresh retry. It recognizes HTTP `409` and the `ALREADY_CLAIMED` business marker, and maps either to `{success:false, alreadyCompleted:true}`. The retry count and request sequence are unchanged; transport errors are still propagated and are not retried. Three focused unit tests cover HTTP 409, the business marker on HTTP 200, and an ordinary upstream failure.

## Follow-up: Trae malformed check-in response must not become code 0

A subsequent review found a second, distinct response-handling defect in `providers/trae/checkin.rs`: `round_trip()` converted JSON parse failures into `{}`, while `status_of()` defaulted a missing `code` to `0`. An empty body, HTML error page, or structurally invalid JSON could therefore be treated as a successful business response. This is especially risky for the claim stage: the mandatory status reread could observe a concurrent completion and make the current operation look like a confirmed claim even though its own claim response was unusable.

The check-in path now validates every status/claim response before business-code dispatch:

- body must parse as JSON;
- top-level value must be an object;
- `code` must be an integer;
- invalid JSON or missing/invalid code returns `502` and cannot enter the success/verification path.

This does not add a retry, change the JWT/Bearer fallback sequence, or alter the same-device status verification rule. Three focused tests cover a valid response, empty/non-JSON bodies, and missing/string/non-object business-code shapes.


## Follow-up: AutoClaw user-info must validate business success

A further pass found that the optional activity-reward trigger in `providers/autoclaw/checkin.rs::trigger_login_active()` treated every HTTP-200 `user-info` response as a successful login trigger (`Ok(_)`). The shared HTTP helper returns `Value::Null` when a 2xx body cannot be parsed, and the upstream may also report application-level failure inside a 200 JSON response. The old branch could therefore wait 1.5 seconds and perform a ledger reread even though the login-active action was not confirmed.

The trigger now requires a JSON object with an integer `code == 0`. A non-zero code logs the upstream `msg` (or the code); a missing, malformed, or non-object response is logged as unconfirmed. These remain best-effort auxiliary failures: they do not change the primary daily-signin result, and no automatic retry was added. The settlement delay and promotion-ledger reread only run after explicit business success. Three focused unit tests cover success, non-zero business code, and missing/malformed response shapes.

## Validation boundary for the AutoClaw follow-up

`git diff --check`, `tools/rust_balance.py`, and `tools/check_eol.py` pass for the AutoClaw change. The three new Rust unit tests have not been run because Cargo/Rustc/Rustfmt are unavailable in this environment. No Gradle/Android build or device test is claimed.

## Cross-entrypoint serialization audit (2026-10-31)

A follow-up review found that the existing serial loop only serialized accounts **within one invocation**. `AutoCheckin::running` protected scheduler invocations from one another, but the manual `POST /api/accounts/checkin` route called `billing::checkin::run_checkin()` directly and did not share that flag. A manual batch and a scheduled batch (or two manual requests) could therefore execute provider claims concurrently for the same accounts, contradicting the documented anti-risk serialization invariant.

The shared execution boundary now owns a process-wide `tokio::sync::Mutex` gate. `run_checkin_cancellable()` uses `try_lock()` and returns HTTP 409 semantics (`签到正在执行中，请稍候`) rather than queueing a second round. This avoids silently starting a delayed duplicate batch after the first one completes.

The scheduler handles this specific 409 as contention, not a failed check-in: it clears the schedule's prewritten `lastFiredDate`, leaves `lastResult` untouched, and lets a later tick retry. Manual API callers receive 409. The gate is held for the entire batch, including per-account persistence, and is released on normal return or future drop.

Static validation passed (`git diff --check`, `tools/rust_balance.py`, `tools/check_eol.py`). Rust compilation and async concurrency tests remain unverified because Cargo/Rustc are unavailable in this environment. A device/runtime test should overlap a manual batch with a scheduled tick and confirm one provider round runs at a time, the manual caller receives 409 when it loses the race, and the scheduled entry retries on a later tick.

Implementation commit: `475bacb fix(checkin): serialize all execution entrypoints`.

## Final branch snapshot (2026-10-31)

- HEAD: `cbde30d docs(android): record global checkin serialization`.
- Functional change: `475bacb fix(checkin): serialize all execution entrypoints`.
- Branch `architecture/kotlin-rust-core`, 35 commits ahead of the local ZIP-import baseline ref; worktree clean; no GitHub push.
- Static checks and archive integrity passed. Rust compilation/tests and Android/Gradle/device validation were not run because the toolchains are unavailable.

## 2026-10-31 follow-up: durable run-ledger design gate

The existing `checkin_records` table is a user-facing per-account result history, not a write-ahead execution journal: its row is inserted only after `checkin_for()` returns. It therefore cannot distinguish a provider request that never started from one that may have succeeded remotely before the process died.

The source audit confirms the regular manual and scheduled paths converge at `billing::checkin::run_checkin_cancellable()`, already protected by the global async gate. The `AccountStore` is constructed with a clone of the same `Db` handle used by `checkin_records`, but `AccountStore::mark_checkin()` also uses the account-domain guard and its own read/modify/write flow. A single atomic projection across account JSON encryption, visible history, and a run row should not be assumed without a dedicated transaction-level API.

A concrete schema/state-machine proposal is now documented in `docs/ANDROID_CHECKIN_RUN_LEDGER_DESIGN.md`. It introduces `checkin_runs` and `checkin_run_accounts`, writes an account's `in_flight` intent before provider execution, saves the normalized provider result before local projections, and replays only known saved results. A process exit while `in_flight` becomes `uncertain`; it is never automatically retried. The proposal explicitly rejects an exactly-once claim because SQLite cannot transact with remote providers.

Startup ordering was inspected: `AccountStore` is constructed during bootstrap, and the auto-check-in scheduler is started near the end of bootstrap. This gives a viable recovery window before scheduler activation. `run_activity` remains a separate API path and is intentionally outside the first regular-check-in slice.

The design-gate commit itself changed no schema or runtime behavior. Subsequent commits implemented schema v16 and the isolated ledger-store methods. Full Rust tests and crash-injection/device validation remain mandatory; current environment lacks the toolchains.

## 2026-10-31 CheckinRun ledger foundation

Schema v16 now contains a durable run table and per-account execution table, with an isolated Rust store module. The store commits `in_flight` intent, saves normalized result JSON before projection, supports idempotent repeated result/projection transitions, and quarantines uncertain attempts without reopening them. A batch cannot close while an account remains `in_flight` or `result_saved`.

This is foundation-only: `run_checkin_cancellable()` is not wired to the new ledger yet, and startup recovery is not implemented. Schema v17 and `record_row_for_run()` now provide the execution-key-based idempotent display-history projection primitive. `AccountStore::mark_checkin(id, at)` already accepts an explicit timestamp, so integration can persist `result_saved_at` and reuse it during recovery. No provider retry is permitted for `uncertain` outcomes.

The v16 DDL and guarded SQL transitions passed a Python SQLite smoke test. New Rust unit tests are present but unrun because Cargo/Rustc/Rustfmt are unavailable; no Rust compile, Android build, or device test is claimed.


## 2026-10-31 CheckinRun projection-key groundwork (schema v17)

Schema v17 adds nullable `checkin_records.run_id` and a partial unique index on `(run_id, account_id)` for non-null run IDs. Legacy history remains null; activity and older non-batch paths retain repeated-row behavior. `checkin_records::record_row_for_run()` accepts an optional run ID, and the insert uses the matching partial-index UPSERT target so a repeated projection for the same batch/account is a no-op.

This is deliberately only the idempotent display-history projection primitive. `run_checkin_cancellable()` does not yet create a run ID or call the new API. `AccountStore::mark_checkin(id, at)` accepts an explicit timestamp and is repeatable for the same value; integration should use the ledger's persisted `result_saved_at`. Account timestamp and history remain sequential projections, and the run account must not become `projected` unless both return success.

Validation in this environment: `git diff --check`, `rust_balance.py`, `check_eol.py`, and a Python SQLite migration/UPSERT smoke test passed. Cargo/Rustc/Rustfmt, Gradle/Android SDK and device testing remain unavailable; Rust compilation and unit tests were not run.


## 2026-10-31 CheckinRun 执行链接入与启动恢复

常规签到入口 `run_checkin_cancellable()` 已接入执行台账：批次创建、账号调用前
`in_flight`、Provider 返回后 `result_saved`、账号时间与签到历史本地投影、最终
`projected` 和批次关闭。签到历史严格写入失败时不会推进状态或继续下一个账号；
`record_row_for_run_at()` 使用 `result_saved_at` 作为历史时间，恢复重复投影不会改变时间。

`ServerState::bootstrap()` 在自动签到调度器初始化前调用 `recover_startup()`：
`result_saved` 只重放本地投影，`in_flight` 转为 `uncertain`，旧批次关闭为
`interrupted`。每轮签到开始前也会执行恢复预检；遗留 `running` 批次未能恢复时返回
503。当前北京时间当天存在 `uncertain` 的账号会从批量目标中排除，单账号调用返回
409；次日才重新允许执行。

严格边界：远端请求与本地 SQLite 不构成分布式事务；若 Provider 已成功但结果快照未能
提交，只能保守标为 uncertain。没有通用自动重试，也没有 exactly-once 承诺。

尚未覆盖：批次 `checkin_history` / 自动签到 `lastResult` 的派生投影、独立
`run_activity` 路径、uncertain 人工核对 UI、provider-specific reconciliation。
Cargo/Rust 编译和单测、Gradle/Android 构建、设备崩溃注入仍未执行。


## 2026-10-31 补充：独立活动入口与常规签到串行化

`POST /api/checkin-activity` 原先不经过 `run_checkin_cancellable()`，因此会绕开
常规签到的全局互斥门。现已复用 `try_acquire_checkin_gate()`，并将活动 Provider
调用纳入 CheckinRun 写前日志、结果快照和本地幂等投影。该入口与常规批量/定时签到
不会并发执行；并发请求返回 409。该改动尚未完成 Rust 编译与真机验证。

## 2026-10-31：批次汇总与签到时间线崩溃恢复（schema v18）

批次创建时即持久化目标数和跳过数；若进程在批次关闭前退出，恢复从账号结果快照
重建 `interrupted` 摘要（不确定账号明确列出但不重试）。正常批量签到将精简
`summary_json` 与批次终态放在同一 SQLite 事务提交，并通过 `history_projected`
标记 `checkinHistory` 兼容投影。时间线使用 `runId` 去重；
恢复可补写已关闭但尚未投影的批次，也可安全处理“时间线已写、投影标记未写”的
崩溃窗口。汇总只保存计数与前五条失败摘要，不重复复制每账号完整结果 JSON。

该改造目前只覆盖签到中心 `checkinHistory`。AutoCheckin 的全局 / schedule
`lastResult` 尚未纳入批次终态快照；仍需下一步把 schedule 身份与该摘要持久化，
然后在恢复时幂等更新兼容配置。Rust 编译与测试仍待具备工具链的环境执行。

## 2026-10-31：AutoCheckin lastResult 纳入批次恢复协议（schema v19）

`checkin_runs` 新增 `schedule_id`、`auto_checkin` 与 `auto_result_projected`。批次创建
时记录 schedule 归属；普通手动 API 与 AutoCheckin 触发批次明确区分。恢复器对已关闭
且仍待投影的 AutoCheckin 批次，从 `summary_json` 重写顶层 `autoCheckin.lastResult`
及对应 schedule 的 `lastResult`，两处写入成功后才清除 pending 状态。schedule 已删除
时不复活该条目，只恢复顶层兼容摘要。配置写入失败会保留待恢复标记。

正常 `fire_for()` 成功路径在现有兼容写入后调用同一投影器；启动恢复也调用该投影器。
这覆盖批次关闭后、顶层摘要写入前，以及顶层写入后、schedule 写入前的崩溃窗口。

本轮独立 SQLite v16→v19 迁移与元数据写入冒烟通过；`git diff --check`、Rust 结构
平衡、Rust 深度扫描和行尾检查通过。由于环境无 Cargo/Rustc/Rustfmt，Rust 编译和
单元测试未运行；config 写盘故障注入、Android 构建与真机测试也未运行。
