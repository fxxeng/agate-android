# R44 — Android Runtime 运行时稳定性 · 审计与收口（2026-11-07）

基线 `5d6201c662`（R43）。本轮主题：把台账里挂了多轮的「静态风险」逐条核验，真实缺口修掉，设计有意的写成裁定。

## 1. 台账四条「静态风险」核验结论

| # | 台账记录 | 核验 | 结论 |
|---|---|---|---|
| 1 | `stop_kernel()` 的 `child.wait()` 无超时 | `runtime.rs:832` 已有 `timeout(3s, wait)` + `kill_on_drop` 兜底 + 注释 | **台账过时，已修，风险关闭** |
| 2 | 注释漂移指向 `backend::shutdown` | `auto_checkin.rs` 停机入口注释确实还写着 `backend::shutdown(&AppState)` 与「Tauri 的应用状态」 | **属实，本轮修正** |
| 3 | `auto_checkin` 任务不 join | ~~设计有意~~ **注释不可靠**：`stop()` 有运行时调用方（JNI `schedulerSet(off)`），「进程退出时用」不成立；循环任务与 `fire_for` 同体，abort 把进行中的签到掐在 HTTP await 点（lastFiredDate 未写）——与「不打断」注释相反 | **R44b 真修复**（见 §6） |
| 4 | `GLOBAL_RUNTIME` 跨 runtime arm | ~~设计有意~~ **注释不可靠**：`try_current()` 分支让任务落点取决于调用线程（桌面 tauri 遗留假兼容）；bin 文档头声称 `#[tokio::main]`（实为手动 builder）。核验后 Android/headless 事实上单落 GLOBAL_RUNTIME，但这是巧合不是设计 | **R44b 真修复**（见 §6） |

## 2. 新发现并收口：`port_conflict.rs` 桌面版残留

Android-only 转向后，该模块残留整段桌面语境：

- 模块头：默认端口 3065、`WSAEADDRINUSE/10048` Windows 错误码、netsh 保留段、
  `backend.rs` Win32 查进程/杀软启发式 —— 全部失真。已重写为 Android 现状
  （Linux `EADDRINUSE`/`EACCES`、判据仍是错误码、occupant 查询已随桌面版移除）。
- `describe()` 的 `Reserved` 文案还在讲 Hyper-V/WSL2/winnat —— Android 上的真实场景是
  特权端口（<1024）或 SELinux 策略。已改为 Android 语义。
- `Occupied` 且无 occupant 时的文案「查不到是哪个进程（可能是权限不足）」—— Android
  壳侧从不填 occupant，用户必然命中这句误导文案。已改为「可结束占用该端口的程序，
  或改用其它端口（设置页）」。
- `Occupant`/`with_occupant`/`can_end_occupant`/`StartupFailure` 是 pub API 但壳侧零调用，
  保留结构不删（R45 代码边界清理再议）。

## 3. 真实缺口修复：端口冲突用户闭环（T1）

**修复前**：Rust 侧 `PortConflict` 已按错误码分类（`ConflictKind::Occupied/Reserved/Other`）
且 `message` 含处置建议，但 JNI 回传只给 `{"ok":false,"stage":"bind","error":message}` ——
**kind 丢失**；Kotlin `RuntimeController.startWith` 有 `GatewayFailed(stage, code, message)`
事件、reducer 折成 `GatewayRuntimeState.Failed`，但**没有任何 UI 消费点**；
`AgateForegroundService.runStart` 通知文案是裸拼 `"stage: error"`。

**修复后**（文案单一出口原则不变 —— 界面不拼说明文案，只透传内核生成的 message）：

1. Rust `jni_bridge.rs`：bind 失败 JSON 增加 `"kind": conflict.kind.as_str()`；
   `ConflictKind::as_str()`（occupied/reserved/other，与 serde kebab-case 一致）。
2. Kotlin `Gateway.StartResult` 解析 `kind`。
3. `GatewayStartFailure`（core-gateway，纯函数）：Kotlin 侧失败文案统一出口 ——
   error 非空原样透传，空时按 stage 兜底。`runStart()` 失败分支改走它（去掉裸 stage 前缀）。
4. `RuntimeStateEvent.GatewayFailed` / `GatewayRuntimeState.Failed` 增加 `kind`
   （默认 null，旧事件兼容），reducer 透传 —— 结构化锚点从此流到 UI。
5. `GatewayControlPage` 新增启动失败卡片（5s 轮询快照，读内存 reducer 状态、不涉 JNI）：
   Failed 时按 kind 显示徽标（端口被占用 / 端口被系统拒绝 / 上次启动失败）+ 内核指引文案。

**测试**（详见 §5）：Rust `port_conflict` 3 条（as_str 契约 + 两类文案的 Android 关键词锚定，
Hyper-V/winnat/WSL2 出现即失败）；Kotlin JVM `GatewayStartFailureTest` 4 条（透传、
兜底分派、kind 常量与 Rust 契约、reserved 文案关键词镜像）。

## 4. 顺手账面（T3）

- **R43 D1 遗留**：`docs/R43_T2_RUST_COMPILE_SCOPE_AUDIT.md` 两处过时表述
  （「决定 A 不改 workflow」「本机无 Rust 工具链」）加【R44 补记】，原文保留存档。
- **M1 遗留**：`workbuddy_import.rs` 的 `CodeBuddyExtension/Data/Public/auth` 固定段
  抽纯函数 `auth_relative_segments()`，Windows/macOS 两个 cfg 分支复用；
  段序 + fold/join 等价性测试全平台可跑（R39 坑：cfg 分支在 Linux CI 编不到，
  共享常量 + 全平台测试是唯一能锁住它的手段）。Linux 非 test 编译下加
  `#[cfg_attr(not(test), allow(dead_code))]`（R40 `resolve_absolute_path` 同款）。

## 5. 本轮验证

- Host（Termux，Rust 1.88）：`cargo check --lib --bins` 通过，lib 警告 **7 条与 R43 基线逐条一致（新增 0）**
  （实施中途 `auth_relative_segments` 曾报第 8 条 never used，加 `#[cfg_attr(not(test), allow(dead_code))]` 后消除）；
  `cargo test --lib`：**779 passed + 11 ignored，0 failed**（R43 基线 774 → +5：port_conflict 3、workbuddy 2）。
- Kotlin JVM（真 junit 4.13.2 + hamcrest 1.3，Termux kotlinc）：`GatewayStartFailureTest` 4/4 OK。
- CI：推送后 Run 结果见台账 §10 回填。
- 设备侧（C 类，并入待验清单）：R43 锁真机语义 4 项 + headless 信号退出 +
  mihomo 子进程回收 + 端口冲突卡片/通知的真机观感。

## 6. R44b — 注释不可靠，按代码事实重构（同日追加）

> R44 首版曾把表 3/4 以「注释背书的设计有意」裁定关闭。复核结论：**注释不是证据**。
> 两处的注释均与代码行为不符（或被后续改动漂移），本轮按代码事实真修复。

### 6.1 `auto_checkin` stop 语义（真缺陷）

核验事实：
- `stop()` 两个调用方：`shutdown_core()`（进程停机）与 `jni_bridge::schedulerSet(off)`
  （**运行时开关**，用户在设置页关调度器）——「进程退出时用」不成立。
- 调度循环任务体内 `tick().await` → `fire_for().await`：**fire 与循环同体**。
  `stop()` abort 循环任务 = 把进行中的签到掐在 HTTP await 点，lastFiredDate 未写，
  与注释「不打断正在跑的那一次签到」**相反**。
- `start()` 的启动补签是裸 `spawn_task`：stop() 管不到、停机无人收尾，还漏了
  tick 路径的条目记账（qoderIntlPending 重试落账）。

修复（复用 R35 的 CoreTaskSupervisor 范式，不发明新机制）：
1. `tick()` 只扫描到点条目，fire 移入 supervisor（`TaskScope::Core` +
   `"checkin-fire"` 固定名，同名活任务忽略 = 到点窗口重入天然防重）；
   循环任务体内从此只剩 sleep/扫描，`stop()` 的 abort 立即生效且无副作用。
2. `stop()` 语义自此与实现一致：abort 循环、fire 跑完自灭（关调度器不掐签到）；
   进程停机的有界 abort+join 由 `shutdown_core_tasks` 承担（顺序：stop_global
   先 abort 循环 → shutdown_core_tasks 收 fire/scheduled_tasks，见 lib.rs:46-48）。
3. `fire_and_settle()`（fire_for + 记账同体）抽出，补签路径统一走它（补齐记账），
   注册为 `"checkin-backfill"`。
4. `stop()` 注释重写为真实语义（含旧实现的缺陷记录）。

测试边界（诚实记录）：`AutoCheckin::new` 依赖 AccountStore/BillingService 真实例
（AuthService 构造链深），本轮不造假实例测试；行为等价性由结构保证 ——
循环体内不再存在 fire await（编译期可见），fire 的防重与收尾由 supervisor
既有测试（duplicate_live_registration_is_ignored / shutdown_cancels_and_joins…）背书。
另加 `lib.rs::runtime_tests::spawn_task_works_from_plain_thread` 回归锚点。

### 6.2 runtime 收敛：单一权威运行时

核验事实：Android（JNI 链路无 Tokio 上下文 + serve 本身由 spawn_task 拉起）与
headless（bootstrap/start 全在 main 同步段）的**业务任务事实上只有
GLOBAL_RUNTIME 一个落点**；`try_current()` 的 Ok 分支命中的 Handle 就是它自己。
但这是巧合而非设计 —— 落点随调用线程变化的不确定性真实存在（bin 文档头还
声称 `#[tokio::main]`，实为手动 builder，注释漂移又一例）。

修复（`lib.rs::spawn_task`）：删除 `try_current()` 分支，恒走 `global_runtime()`。
行为等价性：两条宿主链路的旧落点本来就是 GLOBAL_RUNTIME（见上），删除分支
只消灭不确定性，零行为变化；`bin` 自建 runtime 保留（专职信号监听与 shutdown
block_on 驱动，跨 runtime await/abort 合法），文档头注释重写为核实过的拓扑。
