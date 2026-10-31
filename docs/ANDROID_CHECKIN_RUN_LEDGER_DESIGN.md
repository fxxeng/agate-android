# Check-in run ledger and crash-recovery design

Date: 2026-10-31  
Branch: `architecture/kotlin-rust-core`

## 1. Why the existing history table is not enough

`core::checkin_records` (schema v11) is a user-facing per-account result history. Its insert happens only after `checkin_for()` has returned. It cannot represent an account whose provider request was sent but whose process died before the response was durably stored.

The current regular check-in sequence is approximately:

1. `run_checkin_cancellable()` acquires the global non-queuing check-in gate.
2. `checkin_for()` performs one provider's remote workflow.
3. `AccountStore::mark_checkin()` best-effort updates `accounts.checkinAt` when `day_completed()` is true.
4. `checkin_records::record_row()` best-effort inserts the visible per-account history row.
5. After the loop, `checkin_history::record()` writes a batch summary.
6. Auto-check-in separately writes `autoCheckin.lastResult` and schedule result state.

These are independent writes. The global async mutex prevents overlapping in-process rounds; it does not make these writes crash-atomic and does not survive process death.

## 2. Required guarantee and explicit non-guarantee

The ledger should guarantee:

- a batch and each account attempt have durable identities;
- the intent to execute an account is committed before entering its provider workflow;
- once a provider result is returned, the complete normalized result JSON is committed before the existing best-effort projections are updated;
- startup recovery can distinguish never-started, result-known-but-not-projected, and outcome-unknown attempts;
- known results can be projected idempotently into `checkin_records` and `checkinAt` without calling the provider again;
- unknown outcomes are surfaced for reconciliation and are never automatically replayed.

It cannot guarantee exactly-once remote execution. SQLite and an upstream provider do not share a transaction. If the process dies after the upstream accepted a claim but before the response is committed locally, the local state is necessarily ambiguous unless that provider exposes a reliable idempotency key or a read-after-write reconciliation endpoint.

## 3. Implemented schema (v16 + v17)

Add two tables; keep `checkin_records` as the presentation/history table.

### `checkin_runs`

| Column | Meaning |
|---|---|
| `run_id TEXT PRIMARY KEY` | Stable unique ID for one invocation |
| `started_at INTEGER NOT NULL` | Unix milliseconds |
| `finished_at INTEGER` | Set after the account loop is closed |
| `source TEXT NOT NULL` | `manual`, `single`, `schedule`, `startup`, etc. |
| `reason TEXT NOT NULL` | Human-readable trigger reason snapshot |
| `status TEXT NOT NULL` | `running`, `completed`, `partial`, `cancelled`, `interrupted` |
| `target_count INTEGER NOT NULL DEFAULT 0` | Resolved target count |
| `result_count INTEGER NOT NULL DEFAULT 0` | Accounts with a durable returned result |
| `skipped_count INTEGER NOT NULL DEFAULT 0` | Existing skipped count |

### `checkin_run_accounts`

| Column | Meaning |
|---|---|
| `run_id TEXT NOT NULL` | Parent run |
| `account_id TEXT NOT NULL` | Account identity snapshot key |
| `account_name TEXT NOT NULL DEFAULT ''` | Display-name snapshot |
| `provider TEXT NOT NULL DEFAULT ''` | Provider snapshot |
| `ordinal INTEGER NOT NULL` | Position in the serial target list |
| `state TEXT NOT NULL` | `in_flight`, `result_saved`, `projected`, `uncertain` |
| `started_at INTEGER NOT NULL` | Before provider execution |
| `result_saved_at INTEGER` | When normalized result JSON was committed |
| `projected_at INTEGER` | When local history/checkinAt projections completed |
| `result_json TEXT` | Full normalized result row; nullable until known |
| `last_error TEXT` | Local persistence/reconciliation error, not a replacement for provider result |
| `PRIMARY KEY (run_id, account_id)` | Prevent duplicate account entries in one run |

Indexes: `(state, started_at)` for recovery scans and `(account_id, started_at)` for account diagnostics. Avoid foreign keys to `accounts`: accounts may be deleted while historical execution evidence must remain readable.

The run tables are implemented in `V16_SCHEMA`. Schema v17 adds nullable `checkin_records.run_id` and the partial unique index `idx_checkin_records_run_account` on `(run_id, account_id)` when `run_id IS NOT NULL`. The migration preserves all legacy rows and leaves non-run paths unchanged.

## 4. State machine

```text
                    begin account (commit first)
        ┌──────────────────────────────────────────┐
        ▼                                          │
    in_flight                                      │
        │                                          │
        │ provider returns a normalized Value      │
        │ persist complete result_json             │
        ▼                                          │
   result_saved                                    │
        │                                          │
        │ project checkinAt + checkin_records      │
        │ using the saved result, idempotently     │
        ▼                                          │
    projected                                      │
                                                   │
    process exits while in_flight ───────────► uncertain
```

Important ordering:

1. Insert the run row before starting the account loop.
2. Insert an `in_flight` account row and commit it before `checkin_for()`.
3. After `checkin_for()` returns, save the complete result JSON and transition to `result_saved` in one SQLite transaction.
4. Project the known result to existing tables using idempotent keys/UPSERTs.
5. Transition to `projected` only after the required projections succeed.
6. Close the run with `completed`, `partial`, or `cancelled` only after all targeted account states are known. If the process dies first, startup marks the run `interrupted` and its leftover `in_flight` rows `uncertain`.

A process crash after provider success but before step 3 leaves `in_flight`; startup cannot infer whether the remote claim happened, so it becomes `uncertain`. It must not be silently retried.

## 5. Idempotent projection requirements

The existing `checkin_records` table uses an auto-increment row ID. Schema v17 now adds nullable `run_id` and a partial unique index on `(run_id, account_id)`. Existing historical and independent activity rows remain null. `record_row_for_run()` can now write keyed projections; the regular check-in executor is not yet calling it.

`record_row()` should gain an internal variant accepting the execution key and use `INSERT ... ON CONFLICT DO NOTHING` (or an equivalent guarded insert). This avoids duplicate visible rows when recovery repeats a projection.

`checkinAt` is naturally idempotent for the same saved timestamp. Recovery must use the timestamp derived/stored with the durable result, not `now_ms()` on each replay, otherwise each recovery changes the account's apparent completion time.

The batch `checkin_history` and auto-check-in summaries should eventually be derived from the closed `checkin_runs` row and its account rows. Until that migration is complete, keep the existing writes as compatibility projections, but make them repeatable and keyed by `run_id`; do not treat their independent success as the authoritative run state.

## 6. Startup recovery policy

Recovery runs after `Db` and the account store are installed, before auto-check-in is allowed to fire.

- `result_saved`: replay local projections only; never call a provider.
- `projected`: no action.
- `in_flight`: mark `uncertain`; retain account/provider/run snapshots and timestamps.
- old `running` runs with no remaining active account: close as `interrupted`.
- `uncertain`: expose in diagnostics/history. No automatic retry. A future provider-specific reconciler may query upstream state if that provider has a reliable read endpoint.

Recovery must be bounded and must not hold the SQLite connection lock while making network requests. Any provider-specific reconciliation is a separate async phase outside `Db::with*`.

## 7. Integration points

Regular check-in:

- `billing::checkin::run_checkin_cancellable()` owns run creation, account intent, result persistence, projection, and finalization. This is the shared manual/scheduler boundary already protected by the global gate.
- `checkin_for()` remains provider orchestration and should not know about SQLite run bookkeeping.
- Keep `checkin_records::record_activity()` outside this first slice. `api::billing::run_activity` is a separate operation with different modes and should receive its own run type only after the regular check-in state machine is proven.

Startup:

- install the run-ledger DB handle alongside `checkin_records`;
- run recovery before starting the auto-check-in scheduler;
- if the DB is unavailable, preserve current degraded-start behavior and do not pretend a durable ledger exists.

## 8. Implementation sequence / gates

1. **Schema + pure ledger store — implemented:** v16 run tables, typed state constants, transactional methods, and state-transition tests (Rust tests are present but not run in this environment).
2. **Idempotent display-history projection primitive — implemented:** v17 `run_id` column, partial unique index, `record_row_for_run()`, and SQLite migration/UPSERT smoke test. The execution path does not call it yet.
3. **Regular check-in write-ahead integration — next:** create run, begin account, save result JSON, then project. `AccountStore::mark_checkin(id, at)` already accepts an explicit timestamp; use the ledger's persisted `result_saved_at` during initial projection and recovery so repeated writes retain the same `checkinAt`. The method returns a bool and does not change `updatedAt`.
4. **Startup recovery:** replay `result_saved` using the same timestamp; quarantine `in_flight` as `uncertain`; ensure recovery completes before scheduler startup.
5. **Batch/auto summary convergence:** derive history and scheduler summaries from the durable run ledger.
6. **Provider-specific reconciliation:** only for providers with a documented read-after-write or idempotency contract. Never add a generic retry for `uncertain`.

Each step needs schema tests and state-transition tests. The final validation still requires Cargo/Rust tests, Android build, and device crash-injection tests at each boundary; static scripts alone are not enough.

## 9. Risks and open questions before code landing

- Confirm the exact initialization point where `AccountStore`, `Db`, and `AutoCheckin` are all available; recovery must precede scheduler activation.
- `AccountStore::mark_checkin(id, at)` already accepts a caller-supplied timestamp and is idempotent for the same timestamp, but its account lock / read-modify-write is not part of the `checkin_records` transaction. Recovery must sequence the two projections and only mark `projected` after both return success; do not claim cross-store atomicity.
- Decide retention for `checkin_runs` and `checkin_run_accounts`. Unlike user-facing history, execution evidence should be retained long enough to diagnose interrupted claims; do not prune active/uncertain runs.
- Decide UI/API visibility for `uncertain` and interrupted runs. Initially an internal diagnostic API/log may be safer than changing the current user-facing result contract.
- Activity (`run_activity`) is not covered by the first implementation slice.
- The current environment has no Cargo/Rustc/Rustfmt/Gradle/Android SDK, so implementation can be statically reviewed here but cannot be declared compiled or device-verified.

## 10. Decision

Proceed with schema and store primitives first, then integrate the regular shared check-in path. Do not add automatic retries for interrupted/in-flight provider operations. Do not advertise exactly-once remote claims. The durable ledger is an execution journal plus local reconciliation mechanism, not a distributed transaction coordinator.

## 11. 第一阶段代码落地（2026-10-31）

已在 `architecture/kotlin-rust-core` 实现 schema v16/v17 与独立存储模块
`core::checkin_run_ledger`：

- `checkin_runs`：批次身份、来源、状态与计数；
- `checkin_run_accounts`：账号快照、顺序、执行状态与完整结果 JSON；
- `create_run` / `begin_account` / `save_result` / `mark_projected` /
  `mark_uncertain` / `finish_run`：以 SQLite 事务或条件更新约束状态转换；
- 相同结果重复保存、重复投影是幂等操作；不同结果不能覆盖已保存证据；
- 批次正常关闭时拒绝存在 `in_flight` / `result_saved` 的账号；
- 单元测试覆盖幂等、结果不可覆盖、未决账号阻止关闭和 uncertain 隔离。

本节记录的是 v16 存储原语最初落地时的状态。后续第 13 节已将其接入
`run_checkin_cancellable()` 并加入启动恢复；第 14 节继续补上批次汇总和时间线恢复。

验证限制：当前工作环境仍缺少 Cargo / Rust 工具链，新增 Rust 单元测试尚未
实际编译或运行。静态脚本只能验证仓库结构，不能替代 `cargo test`。

## 12. 第二阶段：展示历史幂等投影键（schema v17）

已完成：

- `checkin_records.run_id` 可空列；历史数据保持 `NULL`，不回填虚构批次身份。
- 部分唯一索引 `(run_id, account_id) WHERE run_id IS NOT NULL`。
- `record_row_for_run(..., run_id)` 新入口；旧 `record_row()` 与 `record_activity()` 仍不带批次键。
- SQL 使用带部分索引谓词的 `ON CONFLICT ... DO NOTHING`，同一执行批次重复投影不会插入第二条记录。
- SQLite 冒烟验证覆盖旧行保留、同 run/account 幂等和 NULL 旧路径可重复写入。

尚未完成：执行链尚未传入 run_id；该 API 当前只是可用的持久化投影原语。`AccountStore::mark_checkin(id, at)` 已接受显式毫秒时间戳，可在保存结果时将 `result_saved_at` 作为稳定时间并在恢复时重用；但账号表与展示历史是两个顺序投影，只有两者均成功后才能标记 `projected`。


## 13. 第三阶段：常规执行链接入与启动恢复（2026-10-31）

本阶段已把 CheckinRun 接入真实的 `run_checkin_cancellable()`，并在
`ServerState::bootstrap()` 中、自动签到调度器初始化前执行恢复。

执行链现在严格按以下顺序：

1. 取得全局签到门，解析目标账号；
2. 对历史遗留批次先执行本地恢复，并确认没有未关闭的 running 批次；
3. 创建 `run_id`；
4. 每个账号调用 Provider 前提交 `in_flight`；
5. Provider 返回后先保存完整结果 JSON 与稳定 `result_saved_at`；
6. 用该时间戳更新账号 `checkinAt`，再用 `(run_id, account_id)` 幂等写入展示历史；
7. 两项本地投影都成功后才标记 `projected`；
8. 全部目标完成或合作式停止后关闭批次。

关键失败策略：

- 创建批次或写入 `in_flight` 失败：Provider 尚未调用，直接停止。
- Provider 返回但 `result_saved` 未能提交：保留 `in_flight`；恢复时转为 `uncertain`，不重放。
- `result_saved` 已提交但账号时间 / 历史写入失败：保留 `result_saved`；下一次启动或下一轮签到前只重做本地投影。
- 历史已写入但 `projected` 未提交：唯一索引使重放历史写入幂等。
- 账号在签到过程中被删除：保留签到历史，不再尝试更新不存在的账号行。
- `uncertain` 账号在同一北京时间自然日内从批量目标中跳过；单账号调用返回 409。次日允许再次执行，因为上游日额度已跨日重置。
- 恢复仍有错误时，遗留 `running` 批次会使新签到入口返回 503，避免在恢复未完成时继续调用 Provider。

恢复也会在每次新签到进入时做一次轻量预检，因此同一进程内遇到可恢复的本地落账故障，
后续签到请求也能先补账，不必强制重启。该恢复过程不发起任何网络请求。

**仍未完成的可靠性工作：**

- `checkin_history` 批次时间线与自动签到 `lastResult` 尚未改为从 run ledger 派生；
  进程若在关闭 run 后、写这些兼容摘要前退出，摘要仍可能缺失，但账号结果与明细历史
  已由执行台账保护。
- `run_activity` 国际版活动路径尚未接入独立执行台账。
- `uncertain` 目前只按当日隔离，没有界面上的人工核对 / 解除入口；诊断和人工处置仍需后续补齐。
- 没有 provider-specific read-after-write reconciliation；不承诺 exactly-once。
- 新增 Rust 测试尚未由 Cargo 编译运行；Android / 真机故障注入未执行。

本阶段完成的是**常规签到路径的写前日志、结果先存、本地幂等补账和保守隔离**，
不是整个签到系统的全部可靠性收尾。


## 2026-10-31：独立活动入口接入

`POST /api/checkin-activity` 已纳入全局签到门与 CheckinRun。其活动结果快照包含
`mode`，恢复时按 `source=activity` 分派到 `record_activity_for_run_at()`，保留
`keepalive` 与 `full/claim` 的不同 outcome 语义。Provider 调用前写入 `in_flight`，
返回后先保存快照，再投影 `checkinAt` 和明细历史，最后标记 `projected`。

当前 CheckinRun 仍未解决批次摘要的持久化派生、uncertain 人工处置和 Provider 专属
read-after-write 核实。Rust 编译/测试与设备崩溃注入仍待执行。

## 14. 第五阶段：批次汇总快照与签到时间线恢复（schema v18）

已完成第一部分：解决批次关闭与 `checkinHistory` 时间线写入之间的崩溃窗口。

- schema v18 为 `checkin_runs` 增加 `summary_json` 和 `history_projected`。
- 创建批次时先保存目标数和跳过数，保证进程中断后仍可重建统计口径。
- 批量签到在关闭批次的同一个 SQLite 事务中保存精简汇总快照（计数、失败摘要、日期、原因）；不重复保存每个账号的完整结果 JSON，账号结果仍以 `checkin_run_accounts.result_json` 为唯一明细证据。
- 若进程在最后一个账号投影完成后、批次关闭前退出，恢复会根据已保存的账号结果快照重建一个 `interrupted` 批次摘要；`uncertain` 账号以明确文案列入失败摘要，不会被重放。
- `checkin_history::record_for_run()` 将 `run_id` 写入兼容时间线，并以 run_id 去重替换；同一批次恢复多次不会重复生成时间线行。
- 时间线写入成功后才将 `history_projected` 置为 1。若进程在批次关闭后、时间线写入前退出，或写入后标记前退出，下一次 `recover_startup()` 会从 `summary_json` 幂等补投影。
- 时间线配置写入失败不会把已成功的远端签到伪装成失败；汇总仍留在 SQLite，投影标记保持 pending，等待下一次恢复。
- 单账号与独立活动入口不生成批量时间线汇总，继续使用原有明细记录。

本切片只收敛了 `checkinHistory`。AutoCheckin 顶层及 schedule 条目的 `lastResult` 仍由 `AutoCheckin::fire_for()` 在执行返回后写入，尚未与 run ledger 形成同一持久化投影协议；进程若在批次关闭后、`lastResult` 写入前退出，该兼容摘要仍可能缺失。下一阶段需要把 schedule 身份和 AutoCheckin 摘要一并纳入批次终态快照，再做幂等恢复。

验证限制：已增加 schema v18 迁移和汇总快照状态单测，但当前环境无 Cargo/Rustc/Rustfmt，Rust 编译和单测未运行。SQLite DDL / 状态 SQL 将另行执行独立冒烟验证；Android 构建与真机崩溃注入仍未执行。

## 15. 第六阶段：AutoCheckin 摘要恢复投影（schema v19）

本阶段将自动签到兼容摘要也纳入执行台账的恢复协议，覆盖此前批次关闭后、
`AutoCheckin::record_success()` 写入顶层或 schedule `lastResult` 之前进程退出的窗口。

- schema v19 为 `checkin_runs` 增加 `schedule_id`、`auto_checkin`、
  `auto_result_projected`。普通手动 API 批次不参与 AutoCheckin 投影；
  `AutoCheckin::fire_for()` 明确传入 schedule 身份和来源标记。
- `project_pending_auto_results()` 从已关闭批次的 `summary_json` 重放本地兼容视图：
  先写顶层 `autoCheckin.lastResult`，再写对应 schedule 的 `lastResult`，两者成功后
  才提交 `auto_result_projected = 1`。
- schedule 已被用户删除时，不重新创建条目；顶层摘要仍会恢复，条目投影视为无目标。
- `write_state_checked()` 与 `mark_result_checked()` 将配置持久化失败显式返回，
  保留 pending 标记供下次启动/签到前恢复。
- 正常 AutoCheckin 成功路径在现有兼容写入后立即调用待投影器，完成批次标记；
  若中途进程退出，`recover_startup()` 会执行相同的幂等本地投影。

验证：schema v16→v19 DDL 与批次元数据 SQL 已通过独立 SQLite 冒烟；新增 Rust
迁移/台账单测尚未因当前环境缺少 Cargo/Rustc 而运行。尚未进行真实 config 写盘故障
注入、Rust 编译和真机恢复测试。


## 16. 第七阶段：uncertain 只读诊断入口

本轮新增 `GET /api/checkin-runs/uncertain?limit=&offset=`，挂在 protected 管理路由下，
用于查看所有仍处于 `uncertain` 的账号执行记录。返回批次 ID、账号 ID/展示名、Provider、
来源、原因、批次时间、账号请求时间和隔离原因；不返回凭证、请求头、请求体或完整结果 JSON。

该接口刻意保持只读：没有“强制成功”“强制失败”“清除 uncertain”或“重放 Provider”
动作。人工核对必须先有 Provider 专属的可靠查询证据，再设计独立的审计状态转换；
仅凭用户点击不能证明上游未执行。分页 limit 在 API 和存储层双重限制为 1..500。

同时修正 `finish_run_with_summary()` 状态白名单中的重复 `RUN_PARTIAL` 条件。

验证：`git diff --check`、`tools/rust_balance.py`、`tools/check_eol.py` 通过；新增
`query_uncertain` 的 Rust 单测已写入，但 Cargo/Rustc/Rustfmt 不可用，尚未编译或执行。
Android 构建和真机验证仍未执行。

## 17. Provider 专属对账能力审计（2026-10-31）

已按当前源码审查 WorkBuddy 国内/国际版、Qoder 两地区、Trae、AutoClaw 两地区、Raccoon、KukuAI、OfficeAce、Loomy、CodeArts 的签到写链路与只读回读能力。完整矩阵、字段语义、限制及实施门槛见 [`ANDROID_CHECKIN_PROVIDER_RECONCILIATION_MATRIX.md`](ANDROID_CHECKIN_PROVIDER_RECONCILIATION_MATRIX.md)。

当前首选候选为 Qoder：活动列表有 `actionType=CLAIM_BENEFIT`、`claimStatus=CLAIMED` 以及 `startAt/endAt` 活动窗口；国际版查询必须复用 uid 与设备风控头重拉。只有唯一匹配当前 run 时间窗口的每日权益活动明确为 `CLAIMED` 才能作为正向证据。`CLAIMABLE` 不足以证明之前的超时写请求未生效，因此首版不据此自动重试。

WorkBuddy 国内/国际版状态接口可作为第二批正向证据候选；Trae 的设备维度 `did_checked_in` 不能在未持久化本轮随机 device id 时用于恢复判定；Raccoon 的当日正积分、OfficeAce 的余额增量可能混入其他奖励；CodeArts 的 `CONFIRMED` 不带确认日期；其余 Provider 的只读字段尚不足以构成严格对账证据。

本阶段仅完成源码审计与矩阵文档，没有改动 Provider 请求行为，没有增加自动重试、强制清除或自动解决 uncertain 的动作。

## 17. Qoder 首版只读对账探针

最初新增受保护只读探针 `GET /api/checkin-runs/reconcile?runId=&accountId=`（后在第 18 节改为带证据持久化副作用的 POST）。它只接受仍处于 `uncertain` 的记录，并针对 Qoder 中国版 / 国际版读取活动列表；其它 Provider 返回 `unknown`。匹配使用账号级执行开始时间（毫秒）与 campaign `startAt/endAt`（Unix 秒）窗口，要求唯一 `CLAIM_BENEFIT` 活动且状态为 `CLAIMED` 才给出正向确认。

`CLAIMABLE` 不被解释为“请求未执行”，因为上游列表可能有最终一致性延迟。国际版复用 uid → UMID 风控头 → 重拉流程。探针不调用 claim，不修改台账，不自动解除隔离；凭证过期时仅允许现有 token refresh 认证维护。

当前响应是临时诊断结果，未持久化证据，尚未具备从 uncertain 到 projected 的状态迁移能力。下一步必须新增 append-only 对账证据记录与事务化迁移，再考虑任何自动投影。源码测试已添加，待 Rust 工具链可用后执行。

## 18. Qoder 对账证据持久化与正向解决（schema v20）

本轮把 Qoder 临时 `confirmed/unknown` 结果纳入可审计持久化链路：

- schema v20 新增 `checkin_reconciliation_evidence`，每次对账独立追加一条证据；证据只保存 Provider、verdict、证据类型、观察时间，以及严格 allowlist 的 campaign ID、业务窗口、上游状态和原因，不保存原始上游响应、凭证、请求头或请求体。
- 证据表由 SQLite trigger 禁止常规 UPDATE/DELETE。它提供数据库层只追加约束，不是抵御数据库文件所有者主动删除触发器的密码学防篡改机制。每次查询都会留下一条 `unknown` 或 `confirmed` 记录；查询失败也以 `unknown` 留痕。
- `checkin_run_accounts` 新增 `reconciled_at` 与 `reconciliation_evidence_id`。原始 `state='uncertain'` 不被覆盖，解决状态由独立标记表达；只有 Qoder/Qoder Intl、`campaign_claim_status`、非空 campaign ID、有效窗口、有效观察时间且 `upstreamStatus=CLAIMED` 的完整证据才有资格解决。
- 插入证据与设置解决标记处于同一 SQLite 事务。持久化层再次校验 campaign 窗口必须覆盖台账中原始账号执行开始时间，不能只信任上层探针传入的 verdict。并发对账时，只有第一个仍未解决的 confirmed 证据能设置标记；后续证据仍追加，但 `resolved=false`，不会覆盖首条解决证据。
- `uncertain_accounts_today()` 与待核对列表现在只包含 `reconciled_at IS NULL` 的记录；历史执行行仍保留 `state='uncertain'`，并可通过证据查询接口审计。
- `POST /api/checkin-runs/reconcile?runId=&accountId=` 执行上游只读查询并持久化证据（由原 GET 改为 POST，明确其本地写入副作用）；`GET /api/checkin-runs/evidence?runId=&accountId=&limit=&offset=` 分页读取不可变证据列表。
- 不存在 claim 重放、强制成功/失败或直接清除 uncertain 的接口。`CLAIMABLE`、查询失败和未知状态仍不能解除隔离。

测试已覆盖：unknown 证据保持隔离、完整 Qoder CLAIMED 证据事务解决、重复证据审计、证据表 UPDATE 被触发器拒绝，以及缺字段 confirmed 被拒绝。Rust 测试尚待工具链执行。

验证与边界：schema v20 DDL 的独立 SQLite 冒烟覆盖 unknown 留痕、confirmed 原子解决、证据 UPDATE/DELETE 触发器拒绝及后续证据不能替换首条解决证据；`git diff --check`、Rust 结构平衡、深度和行尾检查通过。由于环境缺少 Cargo/Rustc/Rustfmt，新增 Rust 单测与编译未运行；Gradle/Android 构建和真机并发/崩溃测试也未运行。SQLite trigger 是应用层只追加保护，不是对数据库文件所有者的密码学防篡改保证。


## 19. WorkBuddy 国内/国际版正向对账接入（2026-10-31）

用户提供的 Agent2Providers 源码确认：WorkBuddy 国内版签到状态通过 `POST /v2/billing/meter/checkin-activity-status` 读取，状态字段归一为 `checkedIn`；国际版使用不带 `/v2` 的 `/billing/meter/checkin-activity-status`，字段为 `todayCheckedIn`，并保留 `statusAvailable` 以区分接口不可用。Agate 当前 `BillingService::get_checkin_status(Some(account_session))` 已实现这两条路径，因此对账器直接复用，不重复实现网络层。

新增 `providers/workbuddy/reconcile.rs`：

- 国内版只接受显式 `checkedIn=true`；国际版必须同时 `statusAvailable=true` 与 `todayCheckedIn=true`。
- 运行开始日与状态观察日必须是同一北京时间自然日。跨日后当前状态不能证明历史签到结果。
- `false`、缺字段、`null`、状态接口错误均为 `unknown`，不推导为未执行。
- API 通过 `AccountStore::get_session_by_id()` 取得目标账号会话，调用现有 BillingService 状态查询；不调用签到 claim。
- 证据写入仍复用 schema v20 append-only 表；存储层在事务内验证 provider、执行日期、观察日期、`upstreamCheckedIn=true` 与观察时间顺序。
- v20 阶段的解决标记只释放当日隔离，不改变原始 `state='uncertain'`；v21 的独立历史投影见第 19 节。

业务语义限制：`checkedIn/todayCheckedIn` 只能证明该账号当日业务结果已完成，不能归因到当前 CheckinRun 的特定 HTTP 请求；这足以防止当日重复签到，但不能作为请求级 exactly-once 证明。

测试已添加：CN true、Intl true + statusAvailable、Intl unavailable、false、跨日，以及 CN/Intl 证据通过存储层事务解决。由于当前环境没有 Cargo/Rustc/Rustfmt，测试未编译/执行。


## 19. 对账结果的签到历史投影（schema v21）

Qoder / WorkBuddy 的 `confirmed` 证据不再停留在诊断接口响应。事务提交后，
系统会将已解决证据幂等投影到账号 `checkinAt` 与 `checkin_records`：

- `checkin_run_accounts.reconciliation_projected_at` 记录投影完成时间；
- `checkin_records.reconciliation_evidence_id` 关联唯一的正向证据；
- 历史 `source=reconciliation`，UI 显示为“恢复核实”；
- 历史 `outcome=already` 表示当日上游状态已完成，不伪装为实时 `success`；
- message 明确说明该行是恢复核实，不代表本次请求响应。

历史行时间与账号 `checkinAt` 使用原始账号执行开始时间（而不是对账观察时间），
避免跨午夜核实时把前一业务日错误显示成今天。账号 `checkinAt` 只接受比当前值
更新的时间，延迟恢复旧批次不能覆盖后来成功签到。对账观察时间仍保留在证据表与
台账 `reconciled_at`。

投影顺序为账号 `checkinAt` → 历史行 → 台账投影标记。三步不跨数据库事务，
但每一步可幂等重放：账号标记可重复写，历史行由 `(run_id, account_id)`
唯一索引保护，最后才置 `reconciliation_projected_at`。因此进程在任意一步退出，
下次启动会重试；已解决证据与原始 `state=uncertain` 不被覆盖。

对账 API 在成功保存证据后也会立即尝试投影；若投影失败，证据和解决标记仍然
保留，响应包含 `historyProjected=false` / `projectionError`，启动恢复继续补齐。

## 20. 自用范围与来源许可

当前 Agate 主要为个人本地使用，不计划分发。Agent2Providers 的 AGPL-3.0 来源
源码本轮仅用于协议行为交叉验证，没有复制其实现文件或 SDK。个人私有使用、
未向他人提供副本或服务，与公开分发是不同情形；若未来分发修改版本或向其他
用户提供网络服务，应重新核对 AGPL-3.0 的对应义务。

## 19. Android 管理端：签到异常核实页（2026-10-31）

新增 Android Compose 页面 `CheckinRecoveryPage`，由签到中心的「异常核实」快捷入口进入。

- `CheckinRecovery` 数据层封装 `GET /api/checkin-runs/uncertain`、
  `POST /api/checkin-runs/reconcile` 和 `GET /api/checkin-runs/evidence`；
  runId/accountId 使用 URL 编码，分页参数有界。
- 页面展示账号、Provider、原始执行时间、批次、触发来源、失败原因和 UNCERTAIN 状态。
- 用户只能主动触发 Provider 专属只读核实，查看每次追加证据；没有「重试签到」「强制成功」「强制清除」入口。
- `confirmed + resolved + historyProjected` 才展示完整恢复成功；已解决但投影失败会明确显示待恢复，不将其伪装为完整成功。
- 当前页在成功后保留该条目直到手动刷新，便于用户立即查看刚写入的证据；下一次刷新由后端待核实列表移除已解决项。签到记录仍可看到“恢复核实”来源和 evidence ID。

本页是本地管理 UI，不新增远程服务或第三方依赖。Android Compose/Gradle 编译与设备交互尚未验证（Gradle Wrapper 需要下载 8.11.1，但当前环境 DNS 不可用）。


## 21. CheckinRun 故障窗口状态机回归测试（2026-10-31）

新增两组 Rust 单测，按持久化边界模拟进程退出，而不是 mock Provider 网络：

1. `crash_after_provider_response_keeps_saved_result_recoverable`：模拟 Provider 已返回且 `result_saved` 已提交，但本地投影尚未执行。验证 running 批次继续关闭新上游请求、不能以 `interrupted` 掩盖未投影结果、重复保存同一响应不改写首次 `result_saved_at`，随后本地投影完成后才能关闭批次并释放恢复门。
2. `uncertain_account_cannot_be_reopened_or_filled_with_late_result`：模拟 Provider 请求结果丢失后已隔离为 `uncertain`。验证迟到结果不能把它改写为 `result_saved`、不能直接投影、重复隔离不产生状态变化；关闭批次后仍保留 uncertain 证据。

这两项是 SQLite 持久化状态机测试，不是真实杀进程/重启测试，也没有实际触发 Provider 请求。既有 `checkin_records` 幂等唯一键测试继续覆盖“历史行已写入、投影标记未写入”的重放边界。

验证：`git diff --check`、`rust_balance.py`、`rust_depth.py`、`check_eol.py` 通过。当前环境没有 Cargo/Rustc/Rustfmt，因此新增 Rust 测试尚未编译或执行；不得将静态检查描述为测试通过。下一步必须在有 Rust 工具链的环境运行 `cargo test`，再做可控进程终止与 SQLite 重启恢复测试。
