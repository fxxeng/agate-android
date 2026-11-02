# Batch 4B / 4C / 4D 源码审计记录（基于 53758d9）

> 日期：2026-11-01  
> 基线：`verify/arch-8faa5f5`，GitHub 验证 HEAD `53758d9`（R14 / Run `41305086716`）  
> 本文是源码审计与设计决策记录，不代表本批已经修改 RuntimeController/JNI/Rust 业务代码。源码归档不含 `.git`，因此不能从归档本身验证 Git commit 身份；身份来自用户提供的 R14 CI 报告。

## 1. Batch 4A 关闭记录

R14 报告确认 Run `41305086716` / commit `53758d9` 的 `rust-tests`、`android-build` 双 Job 成功，Android Instrumentation 2/2 通过。诊断日志清理仅修改 `AgateForegroundService.kt` 与交接文档；`mayStopNow()`、`GatewayIntentTracker`、单测与 workflow 未变。

结论：Batch 4A 关闭。R13 的执行时刻守卫作为当前实现定案。R14 无诊断日志绿跑补充证据，但竞态窗口未必实际触发；不得将单次 instrumentation 绿跑描述为已压力证明竞态修复。

## 2. Batch 4B — RuntimeController 源码审计与状态机决策

### 2.1 当前实现事实

`RuntimeController.kt` 当前只有一个单线程 Executor、worker 线程重入识别、同步 `runSerialized()`、异步 `submitSerialized()`：

- `runSerialized()` 将调用提交到单线程线程池；同一 worker 线程内直接执行，避免嵌套等待死锁。
- 等待期间收到 `InterruptedException` 时不取消已提交任务，而是继续等待任务真实结果，最后恢复调用线程 interrupt 标记。
- `submitSerialized()` 在控制线程执行操作，并在同一控制线程执行 `onComplete`；回调异常被 `runCatching` 隔离。
- R17 后已有纯 Kotlin `RuntimeSnapshot` / Reducer / `NativeStatusObserved` 模型；Batch 4B-2B-1 工作快照进一步让 RuntimeController 持有 `AtomicReference<RuntimeSnapshot>`，提供只读 `snapshot()` 与 worker-confined `recordStateEvent()`（待 CI）。尚无状态发布订阅，也尚未接入 Gateway 生命周期 JNI 调用。
- 当前 Executor 使用 `newSingleThreadExecutor`，底层队列无界；没有关闭 API、队列长度/拒绝策略/等待时长指标。

### 2.2 当前调用面

| 调用点 | 当前行为 | 收口判断 |
|---|---|---|
| `AgateApp.onCreate()` | 后台线程调用 `RuntimeController.runSerialized { Gateway.coreInit(...) }` | 已串行；仍由 Application 自己创建线程 |
| `AgateForegroundService.runStart()` | `runSerialized { Gateway.start(...) }` | 已串行 |
| Service STOP 命令体 | 在 `runSerialized` 内执行 `mayStopNow()` + `Gateway.stop()` | 必须保留此原子顺序 |
| Service TICK | `runSerialized { Gateway.coreInit(...) }`，之后在队列外等待签到 | Core 初始化已串行；长签到不应占生命周期队列 |
| Service `onDestroy()` | 命令线程 + `runSerialized` 执行守卫与 stop | 已串行，属于 R13 竞态修复关键点 |
| 设置页 scheduler toggle | `submitSerialized { Gateway.schedulerSet(on) }` | 已入同一队列，但偏好值先落盘，结果失败时 UI 显示未成功 |
| `GatewayControlPage` 调试入口 | 直接 `Gateway.status()` | 只读探测，但不经过统一观察层 |
| `ConsolePage` / `AssistantPage` / `GatewayRepository` / `KeepalivePrefs` / Tile | 多处直接 `Gateway.status()` | 观察入口分散；不是 start/stop 竞争，但会形成多个状态读取与错误解释口径 |

### 2.3 状态模型建议（先决策，后分批接入）

不要把 Core 与 Gateway 合并成一个 `Running/Stopped` 枚举。现有 Rust 语义明确是两级内核：Core 初始化后可持续运行，Gateway listener 可独立启停；scheduler 又是 Core 内独立开关。

建议 RuntimeController 发布一个不可变快照：

```kotlin
data class RuntimeSnapshot(
    val core: CoreRuntimeState,
    val gateway: GatewayRuntimeState,
    val schedulerEnabled: Boolean?,
    val revision: Long,
    val updatedAtMillis: Long,
)

sealed interface CoreRuntimeState {
    data object NotInitialized : CoreRuntimeState
    data object Initializing : CoreRuntimeState
    data object Ready : CoreRuntimeState
    data class Failed(val stage: String?, val code: String?, val message: String) : CoreRuntimeState
}

sealed interface GatewayRuntimeState {
    data object Stopped : GatewayRuntimeState
    data object Starting : GatewayRuntimeState
    data class Running(val port: Int, val bindLan: Boolean, val version: String) : GatewayRuntimeState
    data object Stopping : GatewayRuntimeState
    data class Failed(val stage: String?, val code: String?, val message: String) : GatewayRuntimeState
}
```

注意：`schedulerEnabled` 允许 `null` 表示尚未从 Native 获得可信状态；不要把 SharedPreferences 中的期望值冒充 Native 实际状态。

推荐规则：

1. 所有会改变 Core/Gateway/scheduler 生命周期的命令都由 RuntimeController 提供具名入口并在同一控制队列执行；业务请求 `apiCall`、长耗时签到等待、普通数据请求不进入该队列。
2. 每个命令在执行前发布 transitional state（Starting/Stopping/Initializing），成功后用 Native 返回值或一次 `status()` reconcile；失败后发布带 stage/code/message 的 Failed，不吞掉异常。
3. `Gateway.status()` 变为 RuntimeController 的受控观察入口。为避免状态查询与变更交错，reconcile 查询应排在控制队列内；高频 UI 观察由单一 Repository 轮询/订阅，不让多个页面各自 JNI 轮询。
4. RuntimeController 是 Kotlin 状态快照的唯一发布者；Native `status_json()` 仍是运行事实来源。二者不是互相替代：Kotlin transitional state 表达命令进行中，Native status 用于完成后校准与进程恢复后的重建。
5. Service 仍拥有 Android FGS、通知、WakeLock；RuntimeController 不持有 Context/Service，不直接控制通知或 Android 生命周期。
6. `GatewayIntentTracker` 继续只负责“最新用户意图”防止过期 STOP；不要把它与状态快照合并，也不能用状态 revision 替代 `mayStopNow()` 的执行时刻读取。
7. 首批实现不要直接给 `core-gateway` 引入 StateFlow：该模块当前 `build.gradle.kts` 只有 JUnit 测试依赖，没有显式 kotlinx-coroutines 运行时依赖。可先用纯 Kotlin listener/snapshot 接口，或明确评估新增依赖后再决定；App 层已有协程使用，但不等于 core-gateway 可依赖其传递实现。

### 2.4 当前测试缺口

现有 `RuntimeControllerTest` 覆盖：多线程串行、嵌套重入、中断等待后返回已提交结果并恢复 interrupt、异常原样透传。缺少：

- Core 与 Gateway 两套状态的独立迁移；
- Starting/Stopping 中重复命令的幂等策略；
- Core 初始化失败后重试；Gateway start 失败后 stop/restart；
- 状态查询与 lifecycle 命令的顺序一致性；
- 旧命令结果不得覆盖更新 revision 的状态；
- Application 初始化与 FGS tick 并发触发 coreInit 的行为；
- Service 销毁/重建期间状态重建；
- scheduler 偏好值与 Native 实际状态不一致时的恢复策略。

### 2.5 Batch 4B 结论

本轮不直接把整个 Service/UI/Repository 改成新状态机。下一次代码切片应先新增纯 Kotlin `RuntimeSnapshot` / 状态转移器及 JVM 单测，不改现有 JNI 调用路径；通过 CI 后再把 `coreInit/start/stop/schedulerSet/status` 逐个迁入具名 RuntimeController API。这样可以先证明状态模型，不把 Service 时序修复和 UI 状态改造绑成一个高风险提交。

## 3. Batch 4C — JNI 入口契约盘点

### 3.1 数量核对

本快照 `Gateway.kt` 有 **12** 个 `private external fun`；`jni_bridge.rs` 有 12 个对应 `Java_dev_agate_core_Gateway_*` 导出。`scripts/verify-jni-exports.py` 会从 Kotlin 自动提取声明，并与 `.so` 的 `nm -D --defined-only` 符号精确比较。故旧计划中的“11 个 JNI 入口”是过时计数，应统一更正为 12。

| Kotlin / JNI 方法 | 参数与返回 | 当前 Rust 边界行为 | 主要契约风险/后续测试 |
|---|---|---|---|
| `nativeStart` | config JSON → `jstring` JSON | 读取字符串失败返回 `stage=jni`；JSON/字段错误返回结构化错误；调用 `start_gateway` | config null/空/非法 JSON、port clamp、api_key/bind_lan 组合、start 失败回滚；Rust panic 边界 |
| `nativeStop` | 无 → JSON | 调 `stop_gateway()` | 未初始化 stop、重复 stop、graceful timeout/Key 清理、并发 start/stop |
| `nativeStatus` | 无 → JSON | `status_json()` | Core 未初始化、Core ready/Gateway stopped、Gateway running、锁 poisoned |
| `nativeTriggerTask` | task String → JSON | 字符串读取失败结构化错误，调用 `trigger_task` | 空/未知 task、Core 未就绪、内部 HTTP 鉴权、异步 dispatch 与真实完成语义 |
| `nativeCoreInit` | dataDir String + port + boolean → JSON | 字符串错误结构化；port clamp；调用 `core_init` | 空/相对/无权限目录、并发首次 bootstrap、重复 init 参数变更语义 |
| `nativeSchedulerSet` | boolean → JSON | 调 `scheduler_set` | Core 未就绪、重复开关、与 coreInit/start/stop 并发 |
| `nativeCheckinRunNow` | timeoutMs Long → JSON | <=0 使用 120s 默认值，调用 `checkin_run_now` | timeout 是等待超时还是取消；busy/后台继续；不要误用于 WakeLock 完成等待 |
| `nativeCheckinRunAndWait` | 无 → JSON | 调 `checkin_run_and_wait()`，同步等待真实任务完成 | 必须后台线程；无界等待/取消/STOP 合作式停止；WakeLock 必须覆盖真实任务 |
| `nativeRequestCheckinStop` | 无 → JSON | 调 `checkin_request_stop()` | Core 未就绪、requested=false 重试语义、与新 TICK 快照隔离 |
| `nativeVersion` | 无 → 普通字符串 | `VERSION` 直接 new Java String | 返回形态不是 JSON；分配失败返回 null；Kotlin 非空类型边界 |
| `nativeSetLogCallback` | nullable callback → JSON | null 清 GlobalRef；非 null 取 JavaVM、创建 GlobalRef、更新回调 | Kotlin 当前忽略返回 JSON；注册失败可能静默；回调替换/撤销与 GlobalRef 生命周期；JNI 异常清理 |
| `nativeApiCall` | method/path/body?/apiKey → JSON | method/path/apiKey 必须读取；body null/空均折叠为 None；调用内存 Router | HTTP method/path/body 限制、空值语义、鉴权错误、Core 未就绪、响应体大小与异常传播 |

### 3.2 横切问题

- **panic containment：** 当前 JNI 导出函数没有统一 `catch_unwind` 边界；`agate-server/Cargo.toml` 的 release profile 明确 `panic = "abort"`。这意味着 Release panic 不可能被 Kotlin 当作普通错误接住，而是进程级终止。当前不能简单声称“JNI 已统一捕获 panic”。需要审计每个 JNI 调用路径的 panic 触发条件，并决定 debug unwind 与 release abort 的真实契约；不能在 abort 构建里承诺 catch_unwind 能恢复。
- **返回 null：** `json_reply()` 和 `nativeVersion()` 在 `NewString` 失败时返回 null。正常路径返回 JSON/字符串，但 OOM 边界与 Kotlin 平台类型的处理尚未形成统一契约。
- **日志回调返回值：** `Gateway.setLogListener()` 调用 `nativeSetLogCallback()` 后没有解析其 JSON 返回值；注册失败不会通过当前 Kotlin API 明确反馈。建议后续将它变成返回 `Result` 的可观测操作，至少记录结构化失败。
- **错误格式不统一：** 大多数入口返回 JSON，但 `nativeVersion` 是普通字符串；部分错误包含 `stage`，部分只有 `error`；还没有统一 `NativeResult` / `code` / `apiVersion` 契约。
- **线程契约不一致：** `nativeCheckinRunAndWait` 明确是长等待；`apiCall` 是数据面请求；`status` 是只读快照；生命周期控制方法需由 RuntimeController 串行化。JNI 函数自身没有 Kotlin 调用线程检查。
- **字符串边界：** `read_jstring` 会将读取失败转为结构化 JNI 参数错误并清 pending exception；但每个入口的 null/空白/长度/字符集策略并未完全统一。

### 3.3 建议顺序

先不改 JNI ABI。第一步增加 Kotlin 侧 `NativeResult` 解析/测试与日志回调结果检查；第二步逐入口补 Rust 单元/集成测试；第三步再讨论 API version 与统一错误码。panic=abort 只能通过降低 panic 可能性、边界输入校验和隔离策略改善，不能伪装成可恢复异常。

## 4. Batch 4D — Rust 平台依赖审计（本轮扩展发现）

### 4.1 本次检索范围

对 `agate-server/src` 检索环境变量、HOME/USERPROFILE、Windows 路径、进程启动/杀进程、监听器、系统时间、线程与 Tokio task、signal 等关键词，得到 296 条匹配行（包含测试、注释和文档性代码，不能把它当作 296 个生产依赖）。以下按实际语义分类，而非按关键词计数。

| 类别 | 源码位置/例子 | 平台语义 | Android 处理判断 |
|---|---|---|---|
| 宿主启动参数 | `src/bin/agate-server.rs`：`AGATE_HOST`、`AGATE_PANEL_PORT`、`AGATE_UI_DIR`、ctrl-c/signal | CLI/headless 入口 | JNI Android 不走 bin main；保留桌面/headless 启动路径，不迁入 Android JNI |
| 主数据目录 | `paths.rs`、`config_migration.rs` | HOME/env 与目录迁移 | Android override 已有；继续确保 override 不污染桌面 env 语义 |
| Provider 配置/凭证 | `server/config/parse.rs`、各 provider 的 token/cookie/endpoint/env 开关 | 业务配置兼容 | 保留 env override；不要一刀切迁 Kotlin |
| 桌面用户目录/旧客户端导入 | `import_ccswitch.rs`、`clash.rs`、各 credentials/import 模块 | 扫描桌面应用数据 | Android 必须明确“不支持/用户显式导入”，不能回退 `.`；既有启动期门禁不等于所有手动导入路径已安全 |
| Windows 专用系统命令 | `zcode/activation.rs` 的 `tzutil`/`reg`、`sub_kernel/download.rs` 的 `powershell` | Windows 系统能力 | 已由 `cfg(windows)` 分支保护的路径保留；不应在 Android 暴露为可用能力 |
| 子内核进程管理 | `sub_kernel/runtime.rs` 的 `Command::spawn`、`taskkill`/`kill`、PID 与端口探测 | 下载并运行外部代理内核 | 高优先级 Android 能力审计：Android 是否打包对应 ABI 二进制、执行权限、SELinux、进程生命周期、存储目录和 Play/系统限制；不能因 Rust 可编译就判定可用 |
| TCP/HTTP 监听 | `server/mod.rs` 主 Gateway bind、登录回调 listener、Trae callback server | 本地服务和 OAuth 回调 | Android 允许的 loopback/LAN socket 需真机/系统限制验证；监听地址由 Kotlin 产品配置控制；不要误将 `TcpListener` 全部视为桌面专属 |
| Tokio 后台任务/定时器 | `server/mod.rs` scheduled tasks、`auto_checkin`、provider loops、Trae/CodeArts session | Core 业务后台任务 | 这是 Rust Core 业务层，应保留平台无关；必须审计启动/取消/shutdown 是否随 Core 生命周期收敛，不能由 Activity 生命周期驱动 |
| 标准线程 | `request_stats/maintenance.rs` `std::thread::spawn` 执行 VACUUM | 阻塞数据库维护 | 属于 Core 任务实现；Android 需评估线程退出、进程终止和数据库锁，不因使用 std::thread 就强制搬 Kotlin |
| 系统时间/临时目录/PID | `SystemTime`、`temp_dir`、`process::id` | 跨平台基础能力 | 通常可保留；临时目录权限/清理与 Android app-private 语义需设备验证 |

### 4.2 高优先级问题

1. **子内核运行能力必须独立验收。** `sub_kernel/runtime.rs` 不仅探测端口，还实际 `Command::new(&binary).spawn()` 并按平台调用 `taskkill`/`kill`。Android 侧需要确认产品是否支持运行该子内核；若支持，必须定义 ABI/下载源/执行目录/权限/进程监督/停止语义；若不支持，API 与 UI 应明确报告平台不支持，而不是等到 spawn 失败才显示通用错误。
2. **桌面导入 API 的能力声明。** 启动期自动导入门禁已保护 Raccoon/CatPaw/AutoClaw，但 `api/accounts.rs` 等手动导入仍需逐接口检查 `importDesktop` 在 Android 的行为，确保返回明确 unsupported，不扫描 app-private 当前目录或 `.`。
3. **Core 后台任务关闭契约。** Rust Core 包含定时器、订阅 worker、provider session、自动签到等任务；目前 Android 的 Core 生命周期是进程级常驻语义。后续应建立任务清单：启动者、持有句柄、取消信号、shutdown 等待方式、是否允许任务跨 Gateway stop 继续运行。Gateway stop 不应误停 Core 任务。
4. **JNI 与 CLI 启动层隔离。** `bin/agate-server.rs` 的信号监听和 CLI env 参数属于 headless 启动器，不应要求 Android JNI 模拟 signal；但共享 server 模块必须避免隐式依赖 bin 初始化。

### 4.3 本轮没有实施的改动

本轮没有修改 Rust 平台调用、Provider 导入、子内核进程管理或启动器；没有运行 Cargo/Gradle/Android 真机验证。当前结论是风险分类和下一步优先级，不是 Android 平台兼容性通过证明。

## 5. 本批验证边界

- 源码审计基于用户提供的 `53758d9` tar.gz；归档没有 `.git`。
- Kotlin 声明与 Rust 导出源码静态计数均为 12，名称一一匹配；未实际运行 `verify-jni-exports.py`，因为需要构建产物 `.so` 与 `nm`。
- 未运行 Cargo、Gradle、Instrumentation 或真机测试。
- 本轮不修改 Kotlin/Rust 运行逻辑；只更新交接与审计文档。

## 6. Batch 4B-1 实施记录（R15 基线 9182025）

本批在审计建议基础上新增纯 Kotlin `RuntimeSnapshot`、Core/Gateway 独立状态 sealed interface、`RuntimeStateEvent` 与纯函数 `RuntimeSnapshotReducer`，并新增 8 项 JVM 单元测试。只新增两个 Kotlin 文件；没有修改 RuntimeController、Gateway、Service、JNI、Rust、Gradle 依赖或 CI workflow。

Reducer 对无效/乱序完成事件与重复事件返回原快照；只有状态变化才递增 revision 并更新调用方提供的时间戳。`schedulerEnabled` nullable 表示尚未观测 Native 实际值。Core/Gateway 不互相隐式改变状态。

**边界：** 当前只是可测试状态模型，不是已接入的 RuntimeController 状态机；尚无唯一状态发布、Native reconcile、观察者订阅、进程重建恢复、旧异步回调 token 防护。`revision` 不替代 `GatewayIntentTracker.mayStopNow()`。

**验证：** `RuntimeSnapshot.kt` 使用 kotlinc 1.9.0 独立编译通过。Gradle 单测命令因 Wrapper 下载 Gradle 8.11.1 时 DNS `UnknownHostException` 未能启动；不能记为 Gradle/JUnit 通过。GitHub CI、Instrumentation、真机均待验证。


## 7. Batch 4B-2A：Native Status 对账事件（R16 基线 73f781a）

新增 `RuntimeStateEvent.NativeStatusObserved`，将 Native status 的 `core/running/port/bindLan/version/scheduler` 事实表达为一个原子观测事件。Reducer 以该事件对账 Core、Gateway、Scheduler 三个维度；Gateway 停止不隐式重置 Core，scheduler 独立更新。重复相同观测不增加 revision。

新增 4 项 JVM 测试，覆盖三维状态对账、Gateway stopped 保留 Core Ready、Core/Gateway 未就绪状态、相同 Native 观测幂等。原 8 项测试保留，目标总数 12。

**关键约束：** `NativeStatusObserved` 是权威观测/恢复事件，不是命令。只有在 RuntimeController 串行控制边界内、没有未完成生命周期命令时才能应用；不能让任意周期轮询结果覆盖 Starting/Stopping 状态。该约束必须在 4B-2B 接线时落实。

本批没有接入 RuntimeController/Service/UI/Repository，没有改 Gateway JNI、Rust、Gradle 或 workflow。当前环境无 `kotlinc` / Gradle 命令，所有新增测试与构建待 GitHub CI 验证；未执行 Cargo、APK、Instrumentation、真机测试。


## 8. R17 Batch 4B-2A 验证收官与 Batch 4B-2B-1 实施记录

### 8.1 R17 Batch 4B-2A 关闭

- R17 HEAD `414dd67`，完整提交 `414dd6706d2f889f913068b907b3dc26aca651ab`；GitHub Actions Run `41360505254` 一次过双绿。
- `:core-gateway:compileDebugUnitTestKotlin`、`:core-gateway:testDebugUnitTest`、`:app:testDebugUnitTest` 成功；RuntimeSnapshot 相关 `@Test` 共 12 项。
- Instrumentation XML 为 2 tests / 0 failures / 0 errors / 0 skipped；双 APK 与 `.so` 校验产物正常。
- R17 对账基线 `73f781a` 共 756 文件；新增/删除 0，修改 4 个文件（RuntimeSnapshot、RuntimeSnapshotReducerTest、HANDOFF、审计文档），与声明一致。
- NativeStatusObserved 映射已由 CI 编译/测试，但尚未由生产调用方触发；不能把它描述为运行时状态恢复已完成。

### 8.2 Batch 4B-2B-1 工作快照

基线 R17 `414dd67`。修改：
- `RuntimeController.kt`：新增 `AtomicReference<RuntimeSnapshot>` 作为唯一快照存储；`snapshot()` 提供线程安全只读访问；`recordStateEvent()` 限制只能由生命周期 worker 调用，再交给纯 Reducer 更新。
- `RuntimeControllerTest.kt`：新增 3 项测试，覆盖快照只读读取、worker 外写入拒绝、串行 worker 内事件更新与 revision/timestamp 发布。
- 本批没有增加 listener/Flow/协程依赖，没有新增具名 JNI 生命周期入口，没有迁移 App/FGS/Repository/UI，也没有改 `GatewayIntentTracker` 或 Service STOP 逻辑。

**并发边界：**
- 快照读可在任意线程执行；写只能由 `runSerialized` 的 worker 执行。
- `recordStateEvent()` 的线程检查只保证单写者，不自动证明 `NativeStatusObserved` 时机正确。调用方仍必须确保没有 START/STOP 命令在途；任意轮询结果不得直接写入。
- `mayStopNow()` 与 `Gateway.stop()` 的既有同一串行区间不变，后续具名 stop API 必须保留该原子顺序。

**验证状态：** 本地无 Kotlin/Gradle 工具，本批未执行编译/JUnit；GitHub CI、Rust tests、APK、Instrumentation 与真机均待验证。工作归档不含 `.git`，没有 Git commit SHA。


## 9. R18 Batch 4B-2B-1 验证收官与 Batch 4B-2B-2 源码审计

### 9.1 R18 关闭证据

- R18 HEAD `238d9f3`，完整 SHA `238d9f39eeea1ea475db7a8516ccef10b3a51c94`；Run `41366079448` 一次过双绿。
- Rust tests / Android build 均成功；`RuntimeControllerTest` 7 项、`:app:testDebugUnitTest` 成功；Instrumentation 2/2，双 APK 正常。
- 相对 R17 `414dd67`：756 文件，ADD/DEL 0，UPD 4；文档、快照 diff 和 patch 逐字节对账一致。
- Batch 4B-2B-1 关闭，但没有生产状态写入调用方；不代表状态链路已接通。

### 9.2 当前 RuntimeController 具名 API 增量

本工作快照在 R18 `238d9f3` 上新增：
- `coreInit(dataDir, port, schedulerOn): JSONObject`
- `refreshStatus(): Gateway.Status`
- internal `coreInitWith()` 与 `refreshStatusWith()` 测试缝，以及 `Gateway.Status`→`NativeStatusObserved` 的单一映射。

`coreInitWith` 在一个 `runSerialized` 任务内：
1. 记录 CoreInitializationStarted；
2. 执行 Native Core 初始化；
3. Native `ok=false` 时仍读取 status 并应用 NativeStatusObserved；若 Native Core 仍未就绪，再记录结构化 CoreInitializationFailed；若 Core 已就绪（例如 scheduler 对齐失败），快照保留 Native 实际状态；
4. 初始化调用抛异常时记录 CoreInitializationFailed 并原样抛出；
5. Native `ok=true` 时记录 CoreInitializationSucceeded，随后读取 Gateway.status 并应用 NativeStatusObserved；
6. 返回 Native 初始化 JSONObject。

成功和 Native `ok=false` 两条结果路径的 status reconcile 都在同一个串行任务内，不能被其他生命周期命令插入。若失败路径 status 读取自身抛异常，先记录 Native 初始化失败，再传播 status 异常。

`refreshStatusWith` 把 status 读取和快照写入包在 `runSerialized` 内，排在此前已提交的控制命令之后。API 不暴露通用 `recordStateEvent` 给生产调用方，后者仍为 internal 且 worker-confined。

### 9.3 Gateway Native 契约核实

源码 `agate-server/src/jni_bridge.rs`：
- `core_init(data_dir, port, scheduler_on)` 先 `ensure_core()`；失败返回 `{ok:false, stage:"bootstrap", error:...}`；成功后返回 `scheduler_set(scheduler_on)`。
- `scheduler_set()` 在 Core 未就绪时返回 `{ok:false,error:"core_not_ready"}`；成功返回 `{ok:true,scheduler:on}`。
- `status_json()` 在 Core 存在时返回 `core:true` 和 scheduler 实际值；Gateway running 独立返回 `running/port/bind_lan/uptime_sec`；Gateway stopped 时仍可 `core:true`，并提供 `version`。
- 因此 `coreInit` 成功后读取 `Gateway.status()` 是获取 Core/Gateway/Scheduler 真实联合状态的合适路径；不能仅根据 `coreInit` 的 `scheduler` 字段构造全部状态。
- Native `status_json()` 锁错误会返回 `{ok:false,error:"state poisoned"}`；本批同步修正 Kotlin `Gateway.Status.parse()`：必须 `ok=true`，否则抛出带 Native error 的异常。这样 refreshStatus 不会把锁错误伪装为可信 `core=false`。更完整的 NativeResult/error-code 契约仍留在 Phase 1 后续批次。

### 9.4 调用方迁移边界

- `AgateApp.onCreate()` 当前在自建后台线程中调用 `RuntimeController.runSerialized { Gateway.coreInit(...) }`。
- `AgateForegroundService.runTick()` 当前也以 `runSerialized { Gateway.coreInit(...) }` 做兜底。
- 本批只新增 RuntimeController API，不迁移上述调用方；因此快照的首次生产写入仍未发生。
- 下一批建议只迁移 `AgateApp.onCreate()`，并保留 FGS tick 旧调用，随后独立迁移 FGS。这样可以单独验证 RuntimeController API 的第一条真实生产写入路径。
- `Gateway.status()` 仍有多个页面/Repository 直接读取；本批 `refreshStatus()` 不代表所有只读调用已收口。

### 9.5 本批新增测试与未验证项

新增 3 项 RuntimeController 测试：成功初始化后同队列 status reconcile；失败 JSON 仍做 status reconcile 并保留 bootstrap 失败；refreshStatus 排在已入队生命周期工作之后。RuntimeController 测试目标计数 10（原 7 + 新 3）。另新增 1 项 RuntimeSnapshotReducer 测试，覆盖 Native NotInitialized 对账后保留本次 Core 初始化失败原因。

当前容器无 `kotlinc` / Gradle，新增测试没有本地执行。Rust/Android/Instrumentation/真机均未执行，等待 GitHub CI。工作快照无 `.git`，不声明 commit SHA。


## 10. R19 Batch 4B-2B-2 收官与 Batch 4B-2B-3 实施审计（2026-11-02）

### 10.1 R19 验证收官

- R19 HEAD `40801fcaf9c8e35cb61d6ce57618c6083bdd65b3`，父基线 R18 `238d9f3`；GitHub Actions Run `41387453485` 双绿。
- `RuntimeControllerTest` 10 项、`RuntimeSnapshotReducerTest` 13 项；Instrumentation 2/2，无失败、错误、跳过；双 APK 与 instrumentation report artifact 正常。
- 首次推送因新增测试漏 `assertFalse` import 编译失败；验证方只补入 `import org.junit.Assert.assertFalse` 后通过。后续快照需保留该 import。
- Batch 4B-2B-2 关闭：RuntimeController 现有 `coreInit()` 与 `refreshStatus()` 具名 API 经 CI 验证；但 R19 收官时仍无生产调用方，不能将其称为生产链路已接通。

### 10.2 HANDOFF 状态基准纠正

R19 审查发现 HANDOFF 正文早期章节包含原方案分支 `architecture/kotlin-rust-core` 的历史接线描述及提交 SHA；这些提交不属于验证分支 `verify/arch-8faa5f5` 的历史，不能作为当前验证分支的代码事实。本轮在 HANDOFF 标题后新增醒目声明：早期正文仅作方案分支历史参考；验证分支真实状态以文档尾部 R* 记录和本审计记录为准。后续维护不得再将方案分支的 `AgateApp`/FGS/schedulerSet 已接入描述套用到验证分支。

### 10.3 Batch 4B-2B-3：AgateApp 单一生产调用方迁移（当前工作快照，待 CI）

基线：R19 HEAD `40801fc`。仅迁移 `app/src/main/java/dev/agate/android/AgateApp.kt`：
- 原代码由 Application 创建后台 Thread，再调用 `RuntimeController.runSerialized { Gateway.coreInit(...) }`。
- 新代码保留原后台 Thread，改为调用 `RuntimeController.coreInit(dataDir, port, schedulerOn)`。
- `filesDir/agate`、`GatewayPrefs.port(this)`、`GatewayPrefs.schedulerOn(this)` 三个参数来源不变。
- 原有 catch(Throwable) 与 Log.e 错误记录保留；初始化仍不在主线程执行。
- 移除 AgateApp 对 `Gateway` 的直接 import/依赖。Native Core 初始化、Started/Success/Failed 事件及 Native status reconcile 由 RuntimeController 的单一串行任务处理。
- `AgateForegroundService.runTick()` 仍保留旧 `runSerialized { Gateway.coreInit(...) }` 路径；这是下一批独立迁移对象。Instrumentation 测试直接调用 Gateway.coreInit 属于测试代码，不应误判为生产调用方漏迁移。

### 10.4 变更范围与风险边界

- 预期修改 3 个文件：AgateApp.kt、HANDOFF、Source Audit；不新增/删除文件。
- 未改变 JNI ABI、Rust、Gradle、CI workflow、FGS、GatewayIntentTracker、start/stop/schedulerSet。
- 未引入 StateFlow/协程/订阅；未改变 Gateway 手动启动语义。
- 保留 `RuntimeControllerTest.kt` 中 R19 修复的 `import org.junit.Assert.assertFalse`；本批没有触碰测试文件。
- 静态检索确认生产 Kotlin 源码中 `Gateway.coreInit` 仍存在于 FGS `runTick()`；RuntimeController 内部生产门面实现与 Instrumentation 测试调用也存在，分别属于封装内部和测试夹具。后续 4B-2B-4 应迁移 FGS 唯一剩余的生产直接调用。

### 10.5 验证状态

本轮仅完成源码修改、静态调用点核对、文档一致性修正与归档检查。未执行 Kotlin/Gradle 编译、JUnit、Rust tests、APK、Instrumentation 或真机验证；均待以 R19 `40801fc` 为父提交的 GitHub CI/设备验证。当前源码快照不含 `.git`，不宣称为 Git commit。


## 11. R20 Batch 4B-2B-3 验证收官与 Batch 4B-2B-4 实施审计（2026-11-02）

### 11.1 R20 验证收官

- 验证分支：`verify/arch-8faa5f5`；R20 HEAD `44b8be8af228...`，父基线 R19 `40801fc`；GitHub Actions Run `41406380716`，Rust tests / Android build 一次过双绿。
- R20 快照对账：756 blobs，ADD 0 / DEL 0 / UPD 3；仅 AgateApp.kt 与两份文档变更，patch 和关键文件 blob 匹配。
- `AgateApp.onCreate()` 的 Core 初始化生产调用已经迁移到 `RuntimeController.coreInit()`；后台 Thread、初始化参数、异常处理和“Core 初始化不自动启动 Gateway”的行为边界保留。
- Batch 4B-2B-3 正式关闭。
- 计数纠正：R19 的 `RuntimeControllerTest` 为 10 项，`RuntimeSnapshotReducerTest` 实测 13 项；HANDOFF 与本审计的 R19 记录均已更正。R20 未触碰测试文件，且确认 `import org.junit.Assert.assertFalse` 保留。

### 11.2 Batch 4B-2B-4：FGS runTick() Core 初始化调用迁移（当前工作快照，待 CI）

基线：R20 HEAD `44b8be8`。本批只修改 `core-gateway/src/main/java/dev/agate/core/AgateForegroundService.kt` 的 `runTick()` 调用点，以及 HANDOFF / Source Audit 两份文档。

- 原调用：`RuntimeController.runSerialized { Gateway.coreInit(filesDir/agate, GatewayPrefs.port, GatewayPrefs.schedulerOn) }`。
- 新调用：`RuntimeController.coreInit(filesDir/agate, GatewayPrefs.port, GatewayPrefs.schedulerOn)`。
- 参数来源与调用线程不变；`RuntimeController.coreInit()` 自身负责将 Native 初始化、状态事件和 status reconcile 放入同一生命周期队列任务。
- FGS 仍通过原有 try/catch 处理初始化异常，并在 Native `ok=false` 时走原有失败通知分支。
- 该迁移使 Application 与 FGS 两条生产 Core 初始化路径均使用 RuntimeController 具名 API；不代表 start/stop/schedulerSet 已迁移。

### 11.3 FGS 不变量与风险边界

静态对比本批前后 `runTick()`，以下结构与顺序保持：
1. 获取 PARTIAL_WAKE_LOCK 的位置不变；仍由 `finally` 释放。
2. Core 初始化结束后才调用 `Gateway.checkinRunAndWait()`。
3. `checkinRunAndWait()` 仍在 RuntimeController 生命周期队列之外，长签到等待不会占用生命周期 worker。
4. 签到结果日志和 `notifyCheckinResult(reply)` 保持。
5. `finally` 中依次释放 WakeLock、`pendingTicks.complete(startId)`、读取 Gateway running 状态，并在 Gateway 未运行时调用 `stopIfCurrentStart(startId)`。
6. 未修改 `GatewayIntentTracker.mayStopNow()`、STOP 与 `Gateway.stop()` 的原子区间、`onDestroy()` 清理。
7. 未增加 Gateway 自动启动行为；本批不改变 Tick 的“只确保 Core 可用并执行签到，不拉起 Gateway”语义。

### 11.4 静态调用点审计

- `AgateApp.onCreate()`：使用 `RuntimeController.coreInit()`。
- `AgateForegroundService.runTick()`：本批改为 `RuntimeController.coreInit()`。
- `RuntimeController.kt` 内部仍调用 `Gateway.coreInit()`，这是封装实现，属于预期。
- Instrumentation 测试中直接调用 `Gateway.coreInit()` 属测试夹具，不属于生产调用方遗漏。
- 后续需继续审计 start/stop/schedulerSet 与 `Gateway.status()` 读取入口，不能将 Core 初始化接线等同于整个 Runtime 生命周期已收口。

### 11.5 变更范围与验证状态

- 预期修改 3 个文件：`AgateForegroundService.kt`、HANDOFF、Source Audit；0 新增、0 删除。
- 不修改测试文件，因此保留 R19 的 `assertFalse` import；不修改 JNI ABI、Rust、Gradle、CI workflow、Manifest、权限或调度策略。
- 本批已进行静态代码对照、调用点检索、归档及逐文件校验；Kotlin/Gradle 编译、JUnit、Rust tests、APK、Instrumentation 和真机测试仍待 GitHub CI / 设备验证，不得提前标绿。
- 工作快照不含 `.git`，不声明 Git commit。下一轮基线必须为 R20 `44b8be8`，目标分支 `verify/arch-8faa5f5`；不动 `main`、不合并。

### 11.6 下一批建议

- Batch 4B-2B-5：单独迁移 `Gateway.start()` 到 RuntimeController，定义 Starting/Running/Failed 状态事件，保留现有 FGS 命令队列、GatewayIntentTracker 与通知/锁行为。
- Batch 4B-2B-6：单独迁移 `Gateway.stop()`；`mayStopNow()` 检查与 `Gateway.stop()` 必须在同一个 `runSerialized` 区间内执行，不能把守卫和 stop 拆成两个 API 调用。
- Batch 4B-2B-7：单独迁移 `schedulerSet()`，审计偏好值、Native 实际值和 RuntimeSnapshot 的失败恢复语义。
- 之后进行 Phase 2 生产入口总审计；本批只完成 Core 初始化两条生产调用路径的接入，不代表 Phase 2 已完成。


## 12. R21 Batch 4B-2B-4 收官与 Batch 4B-2B-5 实施审计（2026-11-02）

### 12.1 R21 CI / 源码对账收官

- R21 最终 HEAD `e539580b21bacf85a8fbc68529c97acbebc7808c`，父基线 R20 `44b8be8`，GitHub Actions Run `41422134668` attempt 2 双绿。
- attempt 1 `android-build` 在 Instrumentation 启动前被 CPU PSI settle 门禁拦截，零 `INSTRUMENTATION_STATUS`；attempt 2 rerun failed job 后 settle 成功，`:app:connectedDebugAndroidTest` BUILD SUCCESSFUL，Instrumentation 2/2，双 APK artifacts 正常。
- R21 验证报告记录远端 tree 与 stage8 756 文件字节级一致；本轮 CI 结果不代表真机长期稳定性验证。
- Batch 4B-2B-4 正式关闭；Application `onCreate()` 与 FGS `runTick()` 两条生产 Core 初始化调用均使用 `RuntimeController.coreInit()`。

### 12.2 Batch 4B-2B-5：Gateway.start() 接入（当前工作快照，待 CI）

基线 R21 `e539580b`。本批修改范围限定为：
- `RuntimeController.kt`：新增 `startWith()` 内部测试缝与 `start(dataDir, port, bindLan, apiKey)` 具名 API。
- `AgateForegroundService.kt`：`runStart()` 改为调用 `RuntimeController.start()`。
- `RuntimeControllerTest.kt`：覆盖成功、`already_running`、Native 失败结果、旧 Running 快照下失败恢复和异常传播；`RuntimeSnapshot.kt` 调整 GatewayStartStarted 的可接受前态；`RuntimeSnapshotReducerTest.kt` 新增旧 Running → Starting → Running 转换测试。
- HANDOFF 与本 Source Audit：追加 R21 收官和本批实施记录。

### 12.3 状态语义与关键兼容点

- Rust `start_gateway()` 在网关已运行时返回 `ok=false`、`error="already_running"`，并提供当前 `port`、`bind_lan`、`version`。这不是启动失败；RuntimeController 将 `ok || error == "already_running"` 视为可发布 Running 状态的结果。
- `already_running` 特殊分支仍留在 `AgateForegroundService.runStart()`，继续按现有结果对齐 KeepAwake 锁和通知；本批没有合并/重写 Service 分支。
- 其它失败结果记录 `GatewayFailed(stage, null, error)` 并原样返回；抛出的异常记录 `GatewayFailed("start", null, message)` 后原样抛出。
- 成功/已运行结果的 `StartResult` 已包含快照所需字段，因此 `RuntimeController.start()` 不额外执行 `Gateway.status()`。避免额外 JNI 失败将已成功启动误报为失败，也避免第二次状态读取覆盖刚记录的结果。
- `GatewayStartStarted` / `GatewayStartSucceeded` / `GatewayFailed` 事件被复用；`GatewayStartStarted` 现在也允许从 Running 进入 Starting，以覆盖 STOP 尚未迁移期间可能残留的旧 Running 快照；未增加新的状态枚举，也未改 NativeStatusObserved 的 reconcile 规则。

### 12.4 不变量与禁止扩展

- `runStart()` 仍在 Service 原有单线程命令队列执行，RuntimeController 仍是内层生命周期串行控制面；未改变命令 coalescer。
- 启动参数来源保持：`filesDir/agate`、`GatewayPrefs.port(this)`、`GatewayPrefs.bindLan(this)`、`GatewayPrefs.apiKey(this)`。
- `runStart()` 原有异常捕获、失败通知、alert 通知、成功通知、KeepAwake 锁获取/释放及 already_running 分支均保留。
- STOP 仍由原来的 `runSerialized` 区间执行，`mayStopNow()` 与 `Gateway.stop()` 连续处于同一闭包；本批未改该代码。
- 不改 `runTick()`、`onDestroy()`、schedulerSet、JNI ABI、Rust、Gradle、CI workflow；不引入 StateFlow/协程/新线程池。

### 12.5 测试与验证状态

- `RuntimeControllerTest` 从 R21 基线 10 项增加 5 项，当前工作快照静态计数为 15 项；`RuntimeSnapshotReducerTest` 从 13 项增加 1 项，当前为 14 项。
- 保留 `import org.junit.Assert.assertFalse`。
- 当前容器未执行 Gradle/Kotlin 编译、JUnit、Rust tests、APK、Instrumentation 或真机测试；所有执行性验证待 CI/设备。不得将静态审阅写成测试通过。
- 当前快照不含 `.git`，不宣称 Git commit。上传父提交必须为 `e539580b`，目标分支 `verify/arch-8faa5f5`；不动 `main`，不合并。

### 12.6 下一批

- Batch 4B-2B-6：只迁移 `Gateway.stop()`。`mayStopNow()` 判断与 `Gateway.stop()` 必须在同一 `RuntimeController.runSerialized` 闭包中连续执行，保留 R13 START/STOP 意图竞争修复。
- Batch 4B-2B-7：只迁移 `Gateway.schedulerSet()`，先审计偏好值写入时机与 Native 返回失败语义。
- 两批完成后再进行生产调用点总审计；不得把当前进度描述为 Phase 2 已完成。


## 13. R22 Batch 4B-2B-5 验证收官与 Batch 4B-2B-6 实施审计（2026-11-03）

### 13.1 R22 CI 证据

- `7cd6366d` 首次提交因 `RuntimeCheckResult` 错误取 `.key` 编译失败；`77c289c8` 修复为 `gateKey(...)` 后 Gradle tests、APK + ELF/JNI 核对通过。
- Run `41445770982` 首跑在 Instrumentation 启动前因 CPU PSI settle 超时退出；rerun 后 `connectedDebugAndroidTest` 2/2、0 skipped、0 failed。
- R22 报告明确要求安装 APK 做 Gateway START 真机冒烟；该项仍是待用户设备执行，不因 CI 通过而视为完成。

### 13.2 R22 源码调用点审计

对 `77c289c8` 归档全量 Kotlin 生产源码检索：
- `Gateway.start()`：生产实现仅在 `RuntimeController.kt` 内，FGS `runStart()` 已迁移。
- `Gateway.stop()`：FGS 普通 STOP 与 `onDestroy()` 两处仍是直接生产调用。
- `Gateway.schedulerSet()`：`GatewayControlPage.kt` 仍有一处直接生产调用。
- Instrumentation 中的 Gateway 直接调用属于测试夹具，不作为生产遗漏。

因此不能把 R22 报告中的“Batch 4B-2B 全部 CI 就绪”解读为 Gateway 生命周期 API 已全部收口。按原方案继续完成 STOP 与 Scheduler 切片；R22 提出的真机 START 冒烟作为并行但不可省略的设备门槛。

### 13.3 Batch 4B-2B-6 实施：Gateway STOP 迁移（当前工作快照，待 CI）

基线 R22 `77c289c8`。本批改动：
- `RuntimeController.kt` 新增 `stopWith()` 测试缝和 `stopIfAllowed()` 生产 API。
- `AgateForegroundService.kt` 普通 STOP 与 `onDestroy()` 清理两处改用 `RuntimeController.stopIfAllowed { gatewayIntent.mayStopNow() }`。
- `RuntimeControllerTest.kt` 增加 5 项测试。
- HANDOFF / 本审计文档追加记录。

**原子性不变量：** `stopWith()` 的 `mayStopNow()` 与 `stop()` 在同一个 `runSerialized` 闭包中执行；guard 由生命周期 worker 在执行时刻读取。guard=false 时不调用 Native、不写状态。不得把 guard 和 Native stop 拆成两个独立具名 API。

**结果语义：**
- Native `ok=true` → GatewayStopSucceeded。
- Native `ok=false,error=not_running` → 幂等 GatewayStopSucceeded。
- 其它 Native 错误 → GatewayFailed(stage=stop)，但 `stopIfAllowed` 的 Boolean 仍表达 guard 是否允许执行 STOP，保持 Service 原 `skipStop` / 锁清理决策。
- 调用异常 → GatewayFailed 后原样抛出，由 FGS 既有 catch 处理。
- 不执行额外 `Gateway.status()`，不隐式修改 Core/Scheduler。

**边界：** 未修改 GatewayIntentTracker、Service 命令队列/coalescer、startId、WakeLock、通知、START/TICK 业务流程、JNI/Rust/Gradle/CI。普通 STOP 和 onDestroy 两条生产路径均通过同一个具名 API；guard 与 JNI 的同任务原子性由 `stopWith` 结构保证，尚待 CI 测试和真机竞争场景验证。

**测试：** RuntimeControllerTest 静态计数 20（R22 15 + 本批 5）；覆盖 worker 原子区间、guard 拒绝、not_running、Native 失败、异常传播。保留 `assertFalse` import。当前未执行 Gradle/JUnit/Rust/APK/Instrumentation/真机测试，不能记为通过。

### 13.4 变更范围核对与下一步

预期 5 个文件修改、0 新增、0 删除：RuntimeController.kt、AgateForegroundService.kt、RuntimeControllerTest.kt、HANDOFF、Source Audit。下一批仅迁移 `GatewayControlPage` 的 `schedulerSet()`；必须先核对 UI 当前是否先写 Preference、Native 失败如何呈现、快照如何对齐，不能盲目把单行调用替换成具名 API。
