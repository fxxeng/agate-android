# R49 — 运行时正确性收口（STOP 生命周期 / 补签取消 / 启动顺序 / 端口）

- 基线：`verify/arch-8faa5f5` @ `90418ac`
- 范围：R49 任务书 §2–§5（T1–T4 代码 + T5 文档与门禁）
- 本地验证：`cargo fmt --all -- --check` 干净；`cargo test --locked --lib` **805 passed / 0 failed / 11 ignored**（R48 基线 796，新增 9 条 Rust 用例；宿主 CI 为 802——3 条 `jni_bridge` 用例是 `cfg(target_os = "android")`）
- CI 验证：Run `42597312744` 双 job 全绿（JVM 136/136、Instrumentation 2/2）
- 真机状态见文末「验证状态」：未执行的项明确标注，不折算为通过。

## 0. 四个缺陷，一句话各一条

| # | 用户可见现象 | 根因 |
|---|---|---|
| T1 | 点「停止网关」失败后，前台服务退出、保活锁释放，网关其实还在监听 | `stopWith()` 返回 `Boolean`，`true` 只表示「没被新 START 抑制」，Native `ok=false` / JNI 异常 / `released=false` 都走同一条「成功」分支 |
| T2 | 关掉自动签到后，App 启动时排好的补签计划继续一条条跑完 | `stop()` 在 `inner.task.is_none()` 时提前 return；补签任务没有代次、名字固定，谁也取消不了它 |
| T3 | 端口被占用、网关没起来，后台却多了 Sidecar 内核进程 | `start_gateway_tasks()` 在 `server::start()`（bind）之前提交 |
| T4 | 改了网关端口后，「网关控制」页的探测仍打 8787；健康卡文案写死 8787 | `GatewayControlPage` 里 `val port = 8787` 是常量，另有独立默认值副本 |

## 1. T1 — STOP 生命周期

### 1.1 契约

`StopExecutionResult`（`core-gateway/.../StopExecutionResult.kt`）四态：

| 结果 | 触发条件 | Service 动作 |
|---|---|---|
| `SkippedByNewerIntent` | 守卫读到更新的 START | 不动（交给新 START） |
| `Stopped` | Native `ok=true`（含 `not_running` 幂等） | 释放 WakeLock/WifiLock + `stopSelfResult(startId)` |
| `StopTimedOut` | `ok=true` 且 `released=false` | 保留 FGS 与保活锁 + 通知 + 有界复核 |
| `Failed(code,message,cause)` | `ok=false` 或 JNI 抛异常 | 同上（通知带失败原因） |

- `not_running` 是唯一被接受的幂等成功；**任何其它错误都不解释为「已经停了」**。
- JNI 异常不再上抛：`Failed.cause` 保留原始 `Throwable`，信息不丢失；Service 记录 `code/message/cause` 后走与 `ok=false` 相同的路径。
- `StopLifecyclePolicy`（纯 Kotlin）把结果翻译成动作，Service 内只剩接线；判定逻辑可 JVM 单测。

### 1.2 复核（bounded，不无限重试）

- 节奏 `10s × 3`（`STOP_CONFIRM_RETRY_MS / STOP_CONFIRM_MAX_ATTEMPTS`）。
- 每轮先查 `gatewayIntent.mayStopNow()`：期间到达的新 START 会让旧 STOP 的复核直接放弃。
- 探测改走 `RuntimeController.refreshStatus()`（生命周期串行边界上的权威观测），失败视为「未确认」，不视为「已停」。
- **次数用尽不再退出前台**：保留 FGS + 保活锁，通知 `STOP_FAILED_TEXT_FINAL`，等待用户重试或下一次 START/STOP 重新校准。R48 的「超限后退出前台」正是把停止失败伪装成成功的最后一环。

### 1.3 测试

- `RuntimeControllerTest`：5 条旧用例适配四态（含异常用例改为「不上抛 + `Failed.cause` 同一实例」），新增 2 条（失败不伪装 Stopped、超时结果与快照一致）。
- `StopLifecyclePolicyTest`：8 条（四态映射 + 复核 `Exit/Retry/KeepService` 矩阵 + `Failed` 字段保真）。
- Service 本身无法在 JVM 实例化（模块只有 junit + org.json，无 Robolectric），接线由 CI 编译 + 真机验收覆盖；判定已全部下沉到纯逻辑，Service 只保留 `when` 分支。

### 1.4 Android Foreground Service 停止契约

本节是 FGS 停止行为的权威说明（其它文档引用这里，不再各写一份）：

- **Service 只做执行器**：STOP 分支必须先经 `StopLifecyclePolicy.actionFor(result)`，不得在 Service 内重新解释 `ok/released`。
- **释放顺序**：`Stopped` → `releaseKeepAwakeLocks()` → `stopIfCurrentStart(startId)`（`stopSelfResult(startId)` + `stopForeground(REMOVE)`）。放锁一定在确认停止之后。
- **startId 边界**：STOP/TICK 不做跨 startId 合并；`stopSelfResult(startId)` 只能结束实际执行的那次 start，旧 STOP 不得结束新 START。
- **保留态**：`StopTimedOut` / `Failed` → 保留 FGS 与 WakeLock/WifiLock、更新通知、`10s × 3` 复核；每轮先查 `gatewayIntent.mayStopNow()`，探测走 `RuntimeController.refreshStatus()`。
- **复核用尽**：保留 FGS 与保活锁，通知 `STOP_FAILED_TEXT_FINAL`（可诊断），**不退出前台**。
- **onDestroy**：仍走「尽力 stop + 本地锁兜底释放」，失败只记日志；服务正在销毁，不做复核也不保留资源。
- **禁止**：把 FGS STOP / Gateway STOP 映射成 `shutdown_core()`；把 `released=false` 或 `ok=false` 当停止成功。

## 2. T2 — 自动签到补签的取消

### 2.1 根因链

1. `stop()`：`if inner.task.is_none() { return; }` —— 轮询任务没起来时，`stop()` 什么都不做；
2. 启动补签是**独立注册**的 Core 任务（`checkin-backfill`），不归 `inner.task` 管；
3. 任务名固定：supervisor 同名去重会吃掉「关→开」后新一代的补签；
4. `configure(enabled=false)`（UI 关闭签到的主路径）只写配置，不停调度。

### 2.2 实现

- `Inner.generation: u64`；`stop()` **无条件**推进代次后 abort 轮询任务；`start()` 每次开启新代次。
- 补签任务捕获启动代次，`backfill_is_current(gen)` = 总开关仍开 **且** 代次未变。
- 任务名 `checkin-backfill:{generation}`：旧代次收尾时新一代仍能注册自己的补签。
- `configure(enabled=false)` 直接 `stop()`；`tick()` 的总开关早退保留，作为停机窗口/直接改 `config.json` 的兜底。
- 取消语义抽成 `run_backfill(pending, is_current, fire)`：每条计划**启动前**与**收尾后**各判一次；已启动的那一条允许自然收尾（不中断在途 Provider 请求），只阻止后续计划。

### 2.3 测试（11 条，全确定性）

| 用例 | 覆盖的竞态 |
|---|---|
| `backfill_stops_after_in_flight_plan_when_scheduler_closes` | stop 落在首条执行中 → 首条收尾、第二条不启动（`Notify` 闸门，不 sleep 猜时序） |
| `backfill_starts_no_plan_when_stopped_before_task_runs` | stop 在任务真正跑起来之前 → 一条都不执行 |
| `backfill_keeps_order_and_never_overlaps_plans` | 未取消时按 pending 顺序串行、并发度恒为 1 |
| `stale_generation_cannot_run_new_generation_plans` | 旧代次失效后不执行；新代次正常执行 |
| `stop_advances_generation_even_without_dispatch_loop` | 无轮询任务时 `stop()` 仍推进代次（旧 early-return 缺陷回归） |
| `configure_disable_invalidates_backfill_generation` | `configure(false)` 让旧代次失效且总开关关闭（并断言轮询任务已停） |
| `start_without_pending_plans_registers_no_backfill` | §3.5 基本 #1：无待补签 → 不注册补签任务 |
| `start_dispatches_all_pending_plans` | §3.5 基本 #2：`start()` 把全部待补签交给补签任务 |
| `stop_then_immediate_start_runs_only_the_new_generation` | 竞态 #4：stop 后立即 start，旧代次一条都不跑、新代次照常 |
| `repeated_stop_is_idempotent` | 竞态 #5：连续 stop 幂等、无残留轮询任务、随后 start 正常 |
| `repeated_start_leaves_no_duplicate_backfill` | 竞态 #6：连续 start 只保留有效代次，任务全部收尾不留孤儿 |

后 5 条用真实 `start()`/`stop()` 驱动（空账号库 ⇒ 零 Provider 网络），观察点是条目记账
（`fire_for` 入口即写 `lastFiredDate`）与 supervisor 的任务名（`has_live_task`，R49 新增只读诊断），
不靠固定 `sleep()` 猜时序。

未单列的 §3.5 项：**计划失败隔离**由 `fire` 的签名（`FnMut(Schedule) -> Future<Output=()>`，
失败在 `fire_and_settle` 内部落账）结构性保证，外层没有共享失败状态；**Core shutdown** 归
`shutdown_core_tasks` 的既有测试（`shutdown_cancels_and_joins_only_requested_scope` 等）覆盖，
本批不重复造轮子。

## 3. T3 — 先 bind，再提交随网关副作用

- 新增 `server::run_after_successful_bind(bind, start_gateway_tasks)`：`bind()?` 成功后才执行后续动作，`E` 原样返回。
- `jni_bridge::start_gateway` 与 headless `bin/agate-server.rs` 都改用它；`start_gateway_tasks()` 文档改为「在 `server::start` 成功之后调用」。
- 单测 3 条：bind 失败不提交、成功恰好一次、错误原样透传（失败分支里放 `panic!` 证明不会被执行）。
- 守卫判定抽成 `autostart_allowed(should_autostart, manually_halted, running)`（闭包入参保持原短路，
  前置条件不满足时不做端口探活），补 5 条：未安装/无订阅、手动关闭内核、内核已运行、三重守卫全通过、
  前置守卫短路。覆盖任务书 §4.4 表的「手动关闭 / 未安装 / 无订阅 / 已运行不重启」四行；
  `available()`/`has_enabled()` 的真实取值仍属集成路径，由真机 S3/S4 验证。
- 范围说明：Sidecar 自动拉起内部的三重守卫未改；本次只修正「提交时机」。

## 4. T4 — 端口只有一个来源

- 新增 `GatewayPortChoice.probePort(configured, running)`：运行中的实际监听端口优先，否则配置端口；`DEFAULT_PORT = 8787` 只在此处作为契约常量存在。
- `GatewayControlPage`：`configuredPort`（`GatewayPrefs.port`）+ `runningPort`（`GatewayRuntimeState.Running.port`）双来源，5s 轮询每轮刷新；页面重新进入重新读取，不缓存旧端口。
- `ConsolePage` 健康卡文案改用 `GatewayPrefs.port(ctx)`，不再写死 8787。
- 失败卡片区分 `stage=="stop"`：徽标「上次停止未完成」+ 提示「网关可能仍在运行，可再次停止」，不再说成「上次启动失败」。
- 单测 5 条：默认/自定义/运行态优先/停止后回落/默认值契约。

## 5. 验证状态

| 项 | 状态 | 证据 |
|---|---|---|
| Rust 格式门 | PASS（本地） | `cargo fmt --all -- --check` 无输出 |
| Rust library 测试 | PASS（本地） | `cargo test --locked --lib`：815 passed / 0 failed / 11 ignored |
| Kotlin 纯逻辑（`StopExecutionResult`/`StopLifecyclePolicy`） | PASS（本地） | `kotlinc` 编译通过 |
| Android Kotlin 编译 / JVM 单测 / Instrumentation | PASS（CI） | Run `42605799073`：JVM **136/136**、Instrumentation **2/2**、双 APK 校验通过 |
| Clippy 六 deny（宿主 + Android target） | PASS（CI） | 同一 Run：`Panic discipline gate (clippy)` 与 `Android target clippy` 均 success |
| Rust 测试（CI 宿主口径） | PASS（CI） | Run `42605799073`：`812 passed / 0 failed / 11 ignored`（+10） |
| 真机验收（FGS 停止失败保留、端口切换、关闭签到取消补签） | 未执行 | 见 §6，需真实设备 |

本次没有执行任何真机验收，因此**不得**把上表「未执行」当作通过；CI 证据只覆盖构建、单测与模拟器 Instrumentation。

> 文档收口提交标注 `[skip ci]`：R49 的**代码基线**是 `9fe250e`（Run `42605799073`），后续文档提交不改变代码。

## 6. 真机验收清单（未执行）

完整 24 项（Gateway 9 / AutoCheckin 6 / Sidecar 4 / 端口 5）见
`docs/PHASE2_REAL_DEVICE_ACCEPTANCE_CHECKLIST.md` §11；下面只列最关键的 6 条。


1. **停止失败保留前台**：运行网关 → 用外部手段让 `Gateway.stop()` 返回 `ok=false`（例如临时改 Rust 返回值，仅在测试构建）→ 点「停止网关」→ 期望：通知显示「停止失败…」、前台服务仍在、WakeLock 仍持有、控制页失败卡片显示「上次停止未完成」。
2. **超时复核归位**：构造 `released=false` → 期望 10s×3 内若 status 显示未运行则退出前台并放锁；若 status 一直显示运行则保留服务并显示最终文案。
3. **竞态：STOP 后马上 START**：点停止后 1s 内点启动 → 期望旧 STOP 不杀新启动，快照最终为 Running。
4. **关闭自动签到取消补签**：留两条今天未执行的计划 → 冷启动 App 触发补签 → 立即关闭自动签到 → 期望日志「调度已关闭或已换代，停止启动后续补签计划」，第二条不执行。
5. **端口切换**：设置页改端口但不重启网关 → 期望控制页探测仍打实际监听端口；重启后探测打到新端口。
6. **Sidecar 顺序**：占用目标端口后启动网关 → 期望失败卡片提示端口被占用，且没有新增 Sidecar 进程（`ps` 对照）。

## 7. 不在本次范围

- 未改 `BRIDGE_CONTRACT_VERSION`：wire 字段（`ok/error/code/released`）零变更，只改 Kotlin 侧消费语义。
- 未改 Sidecar 自动拉起的守卫逻辑、未改签到 Provider 执行路径、未改手动签到接口。
- 未对 `onDestroy` 的兜底 stop 增加复核：进程/服务正在销毁时保留资源没有意义，这里仍以「尽力停止 + 释放本地锁」收尾（失败只记录日志）。

## 8. 交付物与报告归档

| 交付物 | 位置 |
|---|---|
| 基线确认 | `_temp/agate-verify/R49/R49_BASELINE_CONFIRMATION.md` |
| 本文件（运行时正确性记录） | `docs/R49_RUNTIME_CORRECTNESS.md` |
| 可复现提交 | `90418ac..9fe250e`：T1 `dc36c9c` / T2 `d0036db` / T3 `cb3414a` / T4 `40fc80c` / T5 `c65ccfc`+`3e47d97` / 文档 `05a1f96` / 测试 `9fe250e` |
| Rust / JVM / Instrumentation / clippy / JNI / APK 报告 | `_temp/agate-verify/R49/reports/`（`INDEX.md` + 原始 XML 与 CI 日志） |
| CI Run | `42605799073`（代码基线 `9fe250e`，双 job 全绿） |
| 未完成真机清单 | `docs/PHASE2_REAL_DEVICE_ACCEPTANCE_CHECKLIST.md` §11（24 项，全部「未执行」） |
| APK 产物 | 留在 CI artifacts（`agate-android-apks`），按约定由用户手动下载安装 |
