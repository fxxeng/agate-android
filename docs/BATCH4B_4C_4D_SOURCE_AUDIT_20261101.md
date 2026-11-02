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
