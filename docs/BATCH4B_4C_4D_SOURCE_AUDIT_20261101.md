# Batch 4B / 4C / 4D 源码审计记录（基于 53758d9）

> 日期：2026-11-01  
> 基线：`verify/arch-8faa5f5`，GitHub 验证 HEAD `53758d9`（R14 / Run `41305086716`）  
> 本文是源码审计与设计决策记录，不代表本批已经修改 RuntimeController/JNI/Rust 业务代码。源码归档不含 `.git`，因此不能从归档本身验证 Git commit 身份；身份来自用户提供的 R14 CI 报告。

> **当前验证分支状态（R33 / 2026-11-04）**：验证分支 `verify/arch-8faa5f5` 的最新已验证提交为 `82e27c41e8`，CI Run `41830352114` 双绿。R33 源码归档不含 `.git`；commit 身份以 CI 报告为准。当前 Batch 4D-1A 候选基于 R33，尚未 CI 验证。

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


## 14. R23 STOP 收官与 Batch 4B-2B-7 Scheduler 迁移审计（2026-11-03）

### 14.1 R23 验证证据与版本边界

- Batch 4B-2B-6：提交 `c31f3a88`，Run `41485850670` success；Rust 与 Android 双绿。
- CI-Fix9：验证方提交 `c8049b4e`，CI 双绿；当前 verify HEAD 为 `c8049b4e`。优化涉及多 ABI Rust 构建、Gradle setup/cache + tests/assemble 合并、PSI 采样 5s→10s（总等待仍 480s，阈值和三连静默判定不变）。CI 耗时对比及归因见 R23 报告；不可把无 KVM 模拟器自然波动全部算作优化收益。
- R23 对账确认 STOP 变更与 main 侧 113 文件改动交集为 0。verify/main 已显著分叉，未来合并必须单独进行双向对账。
- 本轮收到归档为 c31f3a88，非 c8049b4e 最终 HEAD；本批补丁不触及 CI 文件，可在 c8049b4e 上应用，但尚未取得 c8049b4e 全量归档，故不声称全树匹配。

### 14.2 Scheduler 生产路径现状

R23 源码 `GatewayControlPage` 当前逻辑顺序：
1. `schedulerOn = on`：Compose Toggle 立即反映用户操作；
2. `GatewayPrefs.setSchedulerOn(ctx, on)`：立即持久化用户期望值；
3. `RuntimeController.submitSerialized { Gateway.schedulerSet(on) }`：异步排入生命周期串行队列；
4. 完成回调切回主线程追加结果文本。

Rust `scheduler_set(on)`：
- CORE 锁 poisoned → `{ok:false,error:"state poisoned"}`；
- Core 不存在 → `{ok:false,error:"core_not_ready"}`；
- 成功时启动/停止 `auto_checkin`，更新 `core.scheduler_on`，返回 `{ok:true,scheduler:on}`。
`core_init(..., scheduler_on)` 会在 `ensure_core()` 成功后调用 `scheduler_set(scheduler_on)`，所以 UI 在 Core 未就绪时保存的期望值可在后续 Core 初始化时应用。

### 14.3 设计结论：Preference 是 desired，RuntimeSnapshot 是 effective

- `GatewayPrefs.schedulerOn` 是跨进程/重启保存的用户期望配置；`RuntimeSnapshot.schedulerEnabled` 是 Native 最近一次确认的有效状态。二者不能混为一谈。
- 因此 Native `core_not_ready` 不应导致 Preference 回滚；否则用户刚保存的设置会丢失，后续 Core 初始化也无法按用户期望对齐。
- Native 成功返回时必须以回复中的 Boolean `scheduler` 作为快照值；不能只根据输入参数 `on` 推断成功。
- Native 返回失败或调用异常时不发布 `SchedulerObserved`，保留 last-known effective state；UI 明确报告“期望值已保存但当前未应用”，并说明下次 Core 初始化会按 Preference 对齐。
- 成功回复缺少 Boolean `scheduler` 属协议异常，不能使用 `optBoolean("scheduler", on)` 默认值伪造有效状态。

### 14.4 本批实施边界与验证

- `RuntimeController` 新增 `schedulerSetWith()` 与公开 `schedulerSet(on)`；Native 调用与快照更新处于同一个 `runSerialized` 任务。
- `GatewayControlPage` 只把 Native 调用改为具名 API；保留 Toggle 乐观更新、Preference 先写入、submitSerialized 异步回调。
- 新增 4 项 `RuntimeControllerTest`：成功状态从 Native 回复发布、失败保留快照、异常传播、缺失实际值拒绝伪成功。
- 当前测试计数：RuntimeControllerTest 24；RuntimeSnapshotReducerTest 14。静态计数不代表已执行。
- 未改 GatewayPrefs、Rust/JNI、AgateApp、FGS、START/STOP、CI workflow；不引入 StateFlow/协程/新线程池。
- 本轮未执行 Gradle/JUnit/Rust/APK/Instrumentation/真机测试；待 CI/设备验证。

### 14.5 下一步审计清单

1. 在真实 c8049b4e 仓库应用补丁，确认 CI-Fix9 的 `.github` 与 scripts 文件不变。
2. CI 通过后，关闭 Batch 4B-2B-7。
3. 初步静态检索显示 `Gateway.coreInit/start/stop/schedulerSet` 直接生产调用已收敛在 RuntimeController；`Gateway.status()` 仍分散在 KeepalivePrefs、GatewayRepository、AgateNavHost、AssistantPage、ConsolePage、GatewayControlPage、AgateTileService、FGS runTick finally 等只读路径。下一轮逐点审计线程上下文、频率和是否需要 `refreshStatus()`。
4. 审计 `submitSerialized`/`runSerialized` 的线程调用契约，确认没有 UI 主线程同步 JNI。
5. 完成 Gateway START 真机冒烟；增加 STOP/START 意图竞争和 Scheduler 开关 / Core 未就绪恢复验证。
6. 不因这批 CI 绿灯宣称 Phase 2 完成，也不提前进入 Phase 4C。


## 15. R25 Batch 4B-2B-7 收官与 Batch 4B-2B-8 只读状态边界审计（2026-11-03）

### 15.1 R25 验证证据

- R25 报告：Batch 4B-2B-7 修复后 Run `41550532331` 双绿；Android 单测 48/48，Rust tests 55/55。
- R24 attempt 1 的 org.json stub 假阳性通过 `testImplementation("org.json:json:20240303")` 修复；剩余时间戳断言失败来自 JUnit4 方法顺序不确定与 RuntimeSnapshot 单例状态残留，测试通过显式 seed 修复。
- CI-Fix10 报告 Artifact 上传常驻；下一次单测红跑应读取 XML 的 assertion message 与 stack trace。
- 归档 `a82339a6` SHA256 `eda68fec3b36a6d37aa68725449801914b9995b6eec005e13e65a15948659615`。归档无 `.git`；提交 hash 来自 R25 CI 报告。

### 15.2 R25 归档中的状态读取调用点

对 R25 源码 Kotlin 文件进行全量检索，生产代码的实际 `Gateway.status()` 调用分布为：
- `RuntimeController.coreInit()` 的成功/失败后对账；
- `RuntimeController.refreshStatus()` 的显式串行 reconcile；
- `AgateForegroundService.runTick()` finally；
- `AgateTileService` 后台磁贴状态读取；
- `GatewayRepository.syncGatewayRunning()` 高频探活；
- `KeepalivePrefs.serviceRunning()` 设置页读数；
- `GatewayControlPage` 运维诊断按钮；
- `ConsolePage` 5 秒级状态展示；
- `AssistantPage` 初始化状态读取。
`AgateNavHost` 只有注释；`NativeRuntimeInstrumentedTest` 的直接调用属于测试夹具。

### 15.3 设计判断

Rust `status_json()` 分别获取 `CORE` 和 `RUNNING` 锁内快照，并返回 JSON；它不做网络 I/O。两个锁读取不构成一个跨生命周期操作的原子快照。故：
- 高频探活/展示不应写 RuntimeSnapshot，也不应等待生命周期队列；
- 仅在明确需要权威对账时调用 `refreshStatus()`；
- Core init 的对账继续留在 `coreInitWith()` 同一生命周期任务内；
- 生产只读读取通过 `RuntimeController.readStatus()` 命名收口，但该函数保持调用线程直读，不隐式排队、不更新快照；
- 所有生产调用方原本都在后台执行：Repository IO scope、Console/Assistant Dispatchers.IO、Keepalive 设置页 Dispatchers.IO、GatewayControlPage IO scope、Tile 独立后台线程、FGS command executor。

### 15.4 Batch 4B-2B-8 改动范围

- 新增 `RuntimeController.readStatus()` 与 internal `readStatusWith()`，明确只读、不串行、不写快照的契约。
- 将 7 个生产调用方迁移到具名读取入口：`GatewayRepository`、`KeepalivePrefs`、`AssistantPage`、`ConsolePage`、`GatewayControlPage`、`AgateForegroundService`、`AgateTileService`。
- 新增 1 项 RuntimeController 单测，验证生命周期队列阻塞时 readStatus 仍在调用线程执行，且 RuntimeSnapshot 对象不变。
- 预计 9 个 Kotlin 文件 + 2 个文档文件，共 11 个文件修改；0 新增、0 删除。测试数量 `RuntimeControllerTest` 24→25。
- 不改 JNI/Rust/Gradle/CI、Native 状态 schema、轮询间隔、Service 队列或生命周期操作语义。
- 当前仅完成静态审阅与变更准备，未运行 Gradle/JUnit/Rust/APK/Instrumentation/真机测试。必须以 R25 `a82339a6` 为基线提交 CI 验证。

### 15.5 下一步

- 在真实验证仓库应用补丁，确保 R25 `org.json` 测试依赖与 CI-Fix10 未被覆盖。
- CI 双绿后关闭 Batch 4B-2B-8；再继续状态消费者审计与设备回归，不以 CI 代替真机。
- 不将只读 status 轮询迁入生命周期队列；不提前宣布 Phase 2 完成或启动 Phase 4C。

---

## 2026-11-03 R27 收官与 Batch 4B-2B-10 审计补记

### R27 验证基线

- R27 代码 `cff1b42f` + CI-Fix11 `1d96372605`；最终 GitHub Actions Run `41636386777` success。
- JVM 80/80、Instrumentation 2/2、Rust tests 与 Android build 均通过。CI-Fix11 将模拟器 PSI settle 预算 480s 调整为 900s，阈值/连续采样/fail-closed 规则不变。
- 最新源码基线为 `1d96372605`。所有后续 `.github/` 改动必须先对账远端 CI-Fix11，不得从旧归档覆盖。

### 新发现：Native status 查询异常被误判为 Gateway stopped

R27 `GatewayRepository.syncGatewayRunning()` 使用 `runCatching(...).getOrNull()`，随后无条件执行 `onGatewayRunning(status?.running == true)`。`Gateway.status()` 的 `Status.parse()` 对 `{ok:false,error:...}` 明确抛异常；异常被折叠为 null 后会变成 `running=false`，进而停止 SSE。这混淆了两个不同事实：

- `Available(Status(running=false))`：Native 明确报告 Gateway 已停止，可以执行停止态同步。
- `Unavailable(Throwable)`：status 当前无法读取，不足以推断 Gateway 已停止；应保留 last-known 状态并重试。

Batch 4B-2B-10 已在当前工作快照中新增 typed `GatewayStatusObservation`，并让 `GatewayRepository` 只在 Available 时更新状态/调用 `onGatewayRunning`；Unavailable 只记录错误并保留现有 SSE/started 与最后有效状态。新增 2 个 JVM 测试覆盖有效 stopped 与查询异常两种结果。

### 审计边界

- 本修正只处理 status 读取失败的错误分类，不证明跨 Native lifecycle 操作的读取具备原子快照语义。
- `readStatus()` 仍是调用线程只读探测，不进入 RuntimeController lifecycle queue；不要将高频轮询改为 `refreshStatus()`。
- Batch 4B-2B-10 尚未 CI 验证；没有执行本地 Gradle/JUnit/Rust/APK/Instrumentation/真机测试。


---

## R28 收官与 Batch 4B-2B-11 源码审计（2026-11-04）

### R28 验证结论

- Batch 4B-2B-10 基线 `1d96372605`，最终 commit `97c44551d3e0a181fbf66ffdad7413a36ac5a571`，Run `41658936617` success。
- JVM 82/82、Instrumentation 2/2、Rust tests 与 Android build 均通过；源码对账 758 blobs，未声明改动 0。
- Native status 三态修复正式关闭：合法 `running=false` 是 Available；读取异常是 Unavailable，不得推断 Gateway stopped 或断开 SSE。

### Batch 4B-2B-11：异步提交回调与生命周期队列隔离

**基线：** `97c44551d3`。

**发现：** `RuntimeController.submitSerialized()` 原来把 `operation` 与 `onComplete` 放在同一个 lifecycle executor Runnable 中。回调虽然由 `runCatching` 防止异常逃逸，但若回调阻塞，仍会占用 `AgateNativeLifecycle` 单线程队列，延迟后续生命周期命令。

**改动：**
- 新增单线程 daemon `AgateNativeCallbacks` executor；`submitSerialized` 的 operation 保持在 `AgateNativeLifecycle`，callback 改由独立 dispatcher 串行执行。
- callback 不是 Main；UI 调用方必须自行切回 Main。`GatewayControlPage` 当前已用 Handler post Main，无需改调用方。
- operation 异常仍作为 `Result.failure` 传给 callback；callback 异常隔离，不影响后续 callback/lifecycle operation。
- 新增 `submitSerializedWith` internal seam 测试 enqueue rejection：入队拒绝同步抛出，operation/callback 均不运行。
- KDoc 明确无界 FIFO、无取消/backpressure；不允许把长业务工作塞入此 API。

**测试：**
- 新增 3 个 RuntimeControllerTest：callback 阻塞不阻塞 lifecycle worker；operation failure 交付且 callback failure 隔离；enqueue rejection 同步传播。
- RuntimeControllerTest 静态 `@Test` 数量从 25 增至 28；JVM 模块总数预计 82→85。实际执行数量以 R29 CI 为准。
- 本地未执行 Gradle/JUnit/Rust/APK/Instrumentation/真机测试；当前仅静态审查与补丁应用校验，不得标记通过。

**文件范围：** 2 Kotlin + 2 文档，0 新增/删除。未改 GatewayControlPage、Service 命令队列、SSE、STOP guard 原子区间、WakeLock、JNI、Rust、Gradle、CI workflow。

**风险与边界：**
- callback dispatcher 是进程期 daemon 单线程队列，保持回调顺序，但 callback 自身仍须短小、非阻塞；慢回调会延迟后续 callback，不会阻塞 lifecycle worker。
- lifecycle executor 当前无界 FIFO、无 shutdown API。拒绝语义是同步抛出边界，不是可依赖的正常流控。
- 不保证 callback 在 UI/Main 线程；不得删除调用方显式线程切换。
- 不要把长时间 `apiCall`、`checkinRunAndWait` 或 SSE 业务放入 lifecycle queue。

### 下一步

1. 以 R28 `97c44551d3` 为唯一基线应用 Batch 4B-2B-11 补丁，CI 验证后关闭。
2. Batch 4B-2B-12 合并完成 RuntimeController、GatewayRepository、Service 生产入口与并发边界总审计，尽量完成 Phase 2 代码收尾。
3. 真机 START/STOP、Scheduler/Core 恢复、Service destroy/进程回收仍为 Phase 2 必须通过的验收项；CI instrumentation 不替代设备验证。


---

## 2026-11-04 R29 收官与 Batch 4B-2B-12 生命周期总审计

### R29 证据

- Batch 4B-2B-11 最终 commit `3ad7aa5f09aa1429ce0d12d16a4c62b7acc380cd`，Run `41674284609` success。
- Rust tests / Android build 双绿；JVM 85/85；Instrumentation 2/2。
- 新基线 `3ad7aa5f09`；源码归档（`.tar.gz`）SHA-256 `995d9cbb3397f78f189ce91d26747556bdcaf7f13901ab62d25b86157656419c`，同轮 `.zip` SHA-256 `13ba866b31ec1d394fcb152b6541ecc39d1290f4479d067aaeb9761e4908e477`。

### 生产入口与线程审计

- `Gateway.coreInit/start/stop/schedulerSet` 直接生产调用仅位于 `RuntimeController.kt`；生产入口来自 `AgateApp`、FGS、`GatewayControlPage`。Instrumentation 中的直接调用属于测试夹具。
- 生产 status 读取通过 `RuntimeController.readStatus()`；所有调用点位于 IO、后台线程或 FGS command executor。`refreshStatus()` 无生产调用方，继续保留为显式串行 reconcile，不用于高频轮询。
- `Gateway.requestCheckinStop()` / `checkinRunAndWait()` 是长业务控制接口，留在生命周期队列之外；STOP 信号有独立 executor。
- `RuntimeSnapshot` 暂无生产消费者；当前不是 UI StateFlow。`GatewayRepository.gatewayStatus` 是单独的状态流。
- callback FIFO 顺序依赖 lifecycle worker 是 callback dispatcher 唯一生产者；已补 KDoc 前提。

### 本批发现并修复：FGS tick status failure semantics

原 `runTick()` finally 将 status 查询异常折叠成 `running=false`，可能在 Gateway 仍运行时停止 FGS。修复后以 nullable observation 表示三态：true=保留，false=允许停止，null=查询失败、保留 FGS并记录 warning。新增 `shouldStopServiceAfterTick(Boolean?)` 与 1 项单测覆盖三种状态。此为 fail-closed 选择，未知时可能暂时留下 FGS，但避免 Gateway 失去前台服务承载。

### 后续风险：GatewayRepository SSE lifecycle

`started` 的 volatile 可见性不等于 `start/stop/onGatewayRunning` 与 `eventSource/retryJob/pollJob` 复合更新的原子性。自动探活（IO）与手动控制页入口可能并发；SSE listener 也没有校验迟到回调是否来自当前 EventSource。下一批 4B-2B-13 应集中做连接生命周期串行化和 EventSource identity/generation guard，并补确定性测试。当前手动断开只临时生效，NavHost 仍会每 5 秒按 Gateway 状态自动重连。

### 本批验证边界

- 预期改动：新增 2 Kotlin 文件、修改 2 Kotlin 文件、更新 2 文档；0 删除。
- R29 JVM 85 项，新增 1 项后预计 86；以 R30 CI XML 为准。
- 本地仅做静态源码审计/修改/差异检查；没有运行 Gradle、JUnit、Rust、APK、Instrumentation 或真机测试。


---

## R30 收官与 Batch 4B-2B-13 源码审计（2026-11-04）

### R30 验证结论

- Batch 4B-2B-12 最终 commit `70e34054fa42d4a79cb1f79168fa005f737805aa`，Run `41738565657` success（32m49s）。
- Rust tests / Android build 双绿；JVM 86/86；Instrumentation 2/2。
- R30 源码归档 `.zip` SHA-256 `bcd15a3936ba529b1ae8c2f50834694a96edea53dee8c50a5f5a93417ace28c5`；`.tar.gz` SHA-256 `3f029fc179617df0604e47acfe527b6a4e0772a9b9a79eb20111ed66cd294a5f`。
- 基线 zip + 独立 patch + 成品 zip 三重校验：patch dry-run/应用/diff-check 通过，patched tree 与交付 zip 760/760 文件逐字节一致。Batch 4B-2B-12 正式关闭；新唯一基线 `70e34054fa`。
- R29 归档 hash 格式歧义已更正：`.tar.gz` 为 `995d9cbb…`，`.zip` 为 `13ba866b…`；Batch 4B-2B-12 文件计数措辞也已明确区分生产策略文件、测试文件与注释文件。

### Batch 4B-2B-13 实施审计

- 新增 `SseLifecycleGuard`，以一个 monitor 管理 started/generation/current EventSource identity；所有状态访问通过 `withLock`，`*Locked` 方法要求调用者持锁。
- Repository 的 `start/stop/onGatewayRunning`、连接安装、断流失效、retry/poll 代次验证均使用该 guard。stop 与 Native stopped observation 的复合状态变更不再分成“先检查、后 stop”两个竞争窗口。
- `onOpen/onEvent/onClosed/onFailure` 都验证 generation + 对象身份。第一个有效 failure/closed 回调立即 invalidates 当前连接；重复或旧连接回调不再更新 link/failures。
- retry/poll coroutine 绑定 generation；停止或新连接后，旧任务即使完成网络等待，也必须再次验证 token 才能继续或重连。
- EventSource/Job 的 cancel 在锁外执行，避免取消动作导致回调重入时持锁。
- `GatewayControlPage` 文案改为「临时断开事件流」，明确说明 Gateway 仍运行时 5 秒探活会自动重连；没有偷偷引入永久禁用开关。
- 新增 4 项纯 JVM guard 测试。它们覆盖 generation/identity 状态机，不等价于 OkHttp EventSource 全链路并发集成测试；listener 接线仍需审阅与 CI/设备验证。

### 本批变更边界与验证状态

- 预计 4 个 Kotlin 文件（修改 `GatewayRepository.kt`、`GatewayControlPage.kt`；新增 `SseLifecycleGuard.kt`、`SseLifecycleGuardTest.kt`）+ 2 份文档；0 删除。
- R30 JVM 86 项，新增 4 项后预计 90；最终以 R31 `TEST-*.xml` 为准。
- 未修改 Native Service 命令队列、STOP 原子区间、WakeLock、RuntimeController、JNI、Rust、Gradle 或 `.github/`。
- 当前只完成源码静态审查、补丁应用/归档对账；未执行 Gradle/Kotlin/JUnit/Rust/APK/Instrumentation/真机测试。Batch 4B-2B-13 等待 R31 CI。
- 唯一补丁基线为 `70e34054fa`；只推 `verify/arch-8faa5f5`，不动 `main`，不合并。

### 下一步

1. 在真实 `70e34054fa` 仓库应用 patch，核对变更文件 hash，再推送验证分支。
2. R31 CI 通过后关闭 Batch 4B-2B-13；若失败，优先读 JUnit XML 与 Android build 日志，不扩大到 Service/Rust。
3. 随后转入 Phase 2 真机验收，不继续堆叠微型 CI 批次：START/STOP 意图竞争、Scheduler/Core 恢复、Service destroy、进程回收/重建。
4. 设备验收通过后再集中推进 Phase 3；CI 双绿不能代替真机长稳。


## R31 收官与 Phase 2 真机验收准备（2026-11-04）

### Batch 4B-2B-13 最终验证

- 基线 `70e34054fa`；首次提交 `49072638ff` 编译失败，原因是 `stopInternal()` 中 `detached` 被显式声明为 nullable Triple，但 Elvis `?: return false` 已排除外层 null。
- 修复 commit `28c007268d63e209ea0b62c92dea9fcddb1dd608` 只移除该 Triple 类型上的 `?`，不改变三个成员各自 nullable 的语义；1 行修改。
- CI Run `41765943802` success：Rust / Android build / JNI / APK / Instrumentation 均通过；JVM 90/90，Instrumentation 2/2。
- R31 `.tar.gz` SHA-256 `b6cc6bfa5fe2fd48feeab8ac63449cae6f90e69cbe48d1f39987ed902ac3a074`；762 regular files + 122 directories；归档不含 `.git`。
- Batch 4B-2B-13 关闭。不能把首次失败提交 `49072638ff` 当作后续基线。

### 测试缺口与补强

R31 mutation M1 删除 `currentConnection === connection` 后原 4 项测试仍通过，说明 identity 分支未被单独隔离。新增 `sameGenerationRejectsDifferentEventSourceIdentity`：
- 相同 generation、不同 EventSource 对象必须被 `isCurrentLocked()` 拒绝；
- 对该对象的 invalidate 必须返回 null；
- 当前真实连接身份必须保持不变。
此测试应能杀死 M1 变异体；JVM 总数预期 91。该改动只增强测试，不调整生产状态机。

### 注释清理

`GatewayRepository.kt` 三处注释统一改为「SSE 的 started 状态」，避免读者误以为 Repository 仍有独立 `started` 字段。R30 反馈中指出的 R29 归档 hash 格式歧义已在后续文档中按 `.zip` / `.tar.gz` 分别标明；本轮保持该修正。

### Phase 2 真机验收（当前主线）

不再安排额外生产逻辑微批次。下一步应执行并归档真实 Android 设备验收：
1. START/STOP 意图竞争及 STOP→START 交错；
2. Scheduler 与 Core 未就绪/恢复；
3. Service destroy 与 WakeLock/pending tick 清理；
4. 进程被系统回收后的重建与 Boot/Alarm 恢复。
每项记录设备/系统版本、操作时间线、logcat、Gateway 状态和重复次数。CI instrumentation 2/2 不能代替设备验收，Phase 2 在设备验收通过前保持未关闭。


---

## R32 收官与 Phase 2 设备验收转段（2026-11-04）

### Batch 4B-2B-14 验证结论

- 基线 `28c007268d63e209ea0b62c92dea9fcddb1dd608`；最终提交 `91dd6c26e54080e266dafa0e3f3911e9a93c0754`；CI Run `41774982028` success。
- Rust tests / Android build 双绿；JVM 91/91；Instrumentation 2/2。`SseLifecycleGuardTest` 5/5。
- 新增 `sameGenerationRejectsDifferentEventSourceIdentity` 在相同 generation 下区分当前对象与不同对象；M1 删除 identity 比较后，真实实现 5/5、变异体 4/5，只有新用例失败。测试有效性由实测支持。
- 本批没有生产逻辑变化；三处 Repository 注释修正、一个测试新增、两份文档更新。Batch 4B-2B-14 关闭。

### 源码与交付审计

- R32 归档 762 个普通文件、122 个目录，无 `.git`；CI 报告确认 4 个变更文件，无新增/删除，且与 patch 声明一致。
- 文档来源轮次已更正：R29 归档 hash 容器格式歧义由 R30 反馈提出，不是 R31。
- R32 报告指出前轮 SHA256SUMS 列项与实际交付不一致。后续包的 manifest 必须与实际文件集合一一对应；打包后逐项校验存在性和 SHA256。

### Phase 2 当前状态与执行要求

- 不再安排无明确收益的微型生产逻辑批次；转入真机验收。
- `docs/PHASE2_REAL_DEVICE_ACCEPTANCE_CHECKLIST.md` 为本轮新增执行单，覆盖 START/STOP 竞争、Scheduler/Core 恢复、Service destroy/Tick、进程回收/Boot/Alarm、SSE 自动重连，并提供 ADB 日志采集和结果模板。
- 当前没有用户设备实测证据；A–E 场景全部保持 NOT RUN / 待执行，Phase 2 未关闭。CI Instrumentation 2/2 不可替代真机证据。
- 只有主场景均有真实设备记录，失败/阻塞均有处置，才可关闭 Phase 2；然后推进 Phase 3。Phase 4 Rust 平台解耦继续暂缓。

### 后续唯一基线

`91dd6c26e54080e266dafa0e3f3911e9a93c0754`（R32）。归档不含 `.git`，提交身份来自 CI 报告。保持 `verify/arch-8faa5f5`，不触碰 `main`、不合并。


## R32 后续：Batch 4C-1 Native 日志回调契约补强（基于 R32，待 CI）

### 用户推进策略调整

R32 后曾建议暂停代码批次、先完成 Phase 2 真机验收。用户明确选择继续按批次推进，并在 APP 日常使用过程中逐步发现/记录真机问题。因此后续采用并行策略：
- 有明确收益、边界独立的架构/代码批次继续推进；不为了批次数量制造无价值的 1–2 行微改。
- Phase 2 A–E 设备场景改为滚动验收，日常使用中收集真实时间线、logcat、dumpsys 和复现次数。
- 真机证据不足时保留 NOT RUN / BLOCKED；Phase 2 最终关闭仍需证据，但不再作为 Native Bridge / Rust 边界批次的前置阻塞条件。
- CI、静态审计、真机使用证据分开记录，不互相替代。

### R32 源码发现

`Gateway.setLogListener()` 原来在调用 `nativeSetLogCallback()` 后忽略 JSON 返回值，并在 Native 调用前先更新/清空 Kotlin `sink`。这会造成两个问题：
1. Native 注册/撤销返回 `{ok:false}` 时，调用方无法得知失败；
2. Native 更新失败时，Kotlin 强引用状态仍被提前改写，可能与 Rust `LOG_CALLBACK` 中实际保留的 GlobalRef 不一致。

此外，`nativeSetLogCallback()` 的注册与清除分支使用 `if let Ok(LOG_CALLBACK.lock())`，锁 poisoned 时会跳过实际更新，却仍返回 `{ok:true}`。这是明确的结果语义错误：Native 未完成操作却报告成功。

### Batch 4C-1 本批实施

- 新增 `LogCallbackUpdateResult`：对外返回 `ok / cleared / error`，不改 JNI ABI 与 JSON 字段。
- 新增纯 Kotlin `LogListenerRegistry`：先执行 Native 更新，只有 Native 返回 `ok=true` 才提交 Kotlin 当前强引用；Native 返回失败或调用抛异常时保留上一次成功状态。
- 新增 `parseLogCallbackUpdateResponse()`：要求响应必须含 `ok`，解析 `cleared/error`；畸形响应抛出，不伪装成功。
- `Gateway.setLogListener()` 返回结构化结果；注册和清除均解析 Native 响应。
- `nativeSetLogCallback()` 的 `LOG_CALLBACK` mutex poisoned 分支改为 `{ok:false,error:"log_callback_lock_poisoned"}`，不再静默报告成功。新 GlobalRef 在锁失败时自然释放。
- Android Instrumentation 现对真实 JNI 注册与撤销结果作断言；新增 JVM 测试覆盖成功注册、注册失败保留旧 listener、清除失败保留旧 listener、成功清除、Native 异常保留状态及 JSON 响应解析。

### 批次范围与验证预期

- 预计修改：`Gateway.kt`、`NativeRuntimeInstrumentedTest.kt`、`jni_bridge.rs`；新增 `LogListenerRegistry.kt`、`LogListenerRegistryTest.kt`；同步两份文档与 Phase 2 checklist。
- JVM 测试：R32 91 项 + 新增 8 项 = 预期 99 项；Instrumentation 仍为 2 项，但其中 Native JNI 测试新增注册/撤销返回值断言。最终以 R33 CI XML 与 instrumentation 报告为准。
- Rust host `rust-tests` 不会编译 `#[cfg(target_os = "android")] jni_bridge`；因此必须依赖 Android ABI library build 检查 JNI 修改能否编译。当前没有声称已经执行 Cargo/Gradle/Kotlin/Instrumentation/真机测试。
- 不改 JNI 方法签名、导出数量、GlobalRef 的跨线程持有模型、回调线程、日志内容格式、Gateway/Service 生命周期队列、WakeLock 或 RuntimeController。
- 当前基线唯一为 R32 `91dd6c26e54080e266dafa0e3f3911e9a93c0754`；只推 `verify/arch-8faa5f5`，不动 `main`、不合并。

### 下一步

1. 对纯 Kotlin `LogListenerRegistry` 做本地可运行的最小验证；对 Kotlin/Android 与 Rust JNI 改动做静态审查和 diff 检查。
2. 将本批推送验证分支，R33 CI 必须检查 Android Rust ABI 编译、JNI export、Gradle/JVM 99 项、APK 与 Instrumentation 2/2。
3. R33 通过后关闭 Batch 4C-1；下一批按 JNI 契约风险优先级继续补入口级测试，不立即改统一错误 ABI，也不提前做 UniFFI 迁移。


### R32 归档计数差异（审计备注）

R32 反馈报告称 762 个普通文件；本轮对实际上传的 R32 `.tar.gz` 直接列项并解包统计为 763 个普通文件、122 个目录，归档 SHA-256 与报告所指交付物一致。当前 Batch 4C-1 源码树新增 2 个文件，实际为 765 个普通文件、122 个目录，无 `.git`。该项作为计数口径差异记录；交付验收以文件清单、逐文件 SHA-256 和 patch 应用结果为准。


## 2026-11-04 推进策略修订：集中开发/集中验证与 Phase 4 主线

### 结论

此前按 1–2 个文件切片分别触发 GitHub CI，验证往返耗时偏高。后续采用主题级批量开发和集中验证：关联改动连续完成，先做本地静态/单测/差异检查，再按验证边界合并触发 CI；CI 反馈按同轮聚类修复。不得为了减少 CI 而合并无关风险，也不得省略模块级验收。

### 主线判断

Batch 4C-1 是 Native Bridge 的局部契约修复，不等于 Phase 4 Rust Core 平台解耦。项目主线应转向 Rust Core 平台依赖边界：

1. **4D-1 `sub_kernel` 能力审计**：子进程启动/分发、ABI、执行权限、路径、进程终止、超时、管道、Android SELinux/存储限制，以及 Desktop 现有行为。输出支持/需抽象/不支持矩阵。
2. **4D-2 Core Worker 生命周期审计**：后台 worker/task 创建、运行、取消、超时、join、shutdown、重启与退出语义；区分业务任务和宿主任务，不把长任务放入 Kotlin RuntimeController 生命周期队列。
3. **4D-3 最小平台适配实现**：在前两项决策后抽取 Rust 平台能力接口，分别实现 Android/Desktop adapter，并保留 desktop/headless 启动、环境变量和配置语义。相关改动连续完成后集中验证 Android ABI、Rust tests、Desktop/headless 构建/行为。
4. **4E 双实例互斥**：独立处理进程与数据目录资源所有权，不与 4D 平台抽象混批。
5. **Phase 5/6**：JNI vs UniFFI 仅做证据驱动的 spike/ADR；观测、压力测试、崩溃恢复和发布门禁后续合并规划。

### 4C-1 与 CI 排期

R32 `91dd6c26e54080e266dafa0e3f3911e9a93c0754` 仍是唯一已验证基线。当前 Batch 4C-1 源码候选尚无 R33 CI 证据。建议先完成该候选的一轮 R33 验证；4D-1 源码审计可并行进行，但不把未完成设计的 4D 生产代码混入 R33。4D-2/4D-3 的关联实现可在本地连续推进，再形成一个完整验证候选，集中触发 CI。

### 真机证据

Phase 2 A–E 改为滚动验收：APP 日常使用中记录设备信息、操作时间线、logcat、必要的 dumpsys 和复现次数。未实测保持 NOT RUN，受限保持 BLOCKED。设备验证不阻塞独立架构批次，但也不能被 CI 结果替代。


## R33 收官与 Batch 4D-1 / 4D-2 审计补记（2026-11-04）

### R33 验证结果

- HEAD `82e27c41e8`，GitHub Actions Run `41830352114` 双绿，33.5 min。
- Rust 747/747；Gradle JVM 99/99；Instrumentation 2/2；JNI export 12/12；APK/ELF 检查通过。
- Batch 4C-1 Native log callback contract 正式关闭。模拟器实际 JNI 注册/撤销断言通过；真实手机日志回调仍待滚动观察。
- 源码归档 SHA256 `7b2ee86df483788b07e608edb8529c09f539354f9d14222530695ac8e4c95137`，765 files；无 `.git`。

### Batch 4D-1A：平台能力显式化

本轮实现不是 Android 子内核支持，而是将已有“不存在 Android 官方资产”的隐含行为提升为明确的平台契约。

- 资产解析变成纯函数 `asset_for_platform(os, arch)`，保留 Windows/macOS/Linux x86_64 与 aarch64 的 v1.19.32 文件名和 SHA256 pin。
- Android 不被映射到 Linux ARM64；`platform_info()` 暴露 `{os, arch, supported, asset, reason}`，`status_json()` 增加 `available` 与 `platform` 字段。
- `installed()` 仍表示磁盘上有文件；新增 `available()` 表示平台受支持且文件已安装，避免残留文件触发自动启动或 listener reconcile。
- 下载、重启、刷新、节点测试、自动启动均加入平台能力快速失败/门禁；删除仍允许按物理文件状态清理残留。
- `accounts.rs` 在 Android 拒绝隐式 `importDesktop:true` 固定路径读取；显式文件导入（`authFileContent` / `authFileB64`）优先级保留，手动粘贴不受影响。
- 新增 6 项纯函数 Rust tests。R33 的 747 是已验证结果，本批 753 只是预期计数，等待 CI。

### Batch 4D-2：Core Worker 生命周期审计

R33 源码表明任务所有权至少分为 Core process scope、Gateway scope、Request scope、Sidecar scope。当前 `sub_kernel` worker 使用 OnceLock + unbounded mpsc，JoinHandle 丢弃，sender 没有显式关闭；`sub_kernel::shutdown()` 在当前归档中没有生产调用点，且只 kill 当前 child，不停止队列 worker。`GLOBAL_RUNTIME` 为进程级 OnceLock Runtime；scheduled_tasks / usage_query / zcode config_sync 等长期循环没有显式取消句柄。与此同时，Gateway HTTP serve 已使用 oneshot→watch graceful shutdown，AutoCheckin / RunGuard 具备局部句柄管理，callback server 使用 watch 信号。后续应统一 owner/scope 契约，而非统一 abort。

完整方案见 `docs/CORE_WORKER_LIFECYCLE_AUDIT_20261104.md`。本轮不修改 worker 生命周期生产逻辑，避免在没有定义 Core shutdown 与 Gateway stop 区别前引入竞态。下一批先设计最小 `CoreTaskSupervisor` / `TaskScope`，再选一个循环任务做可验证 spike。

### 当前状态

| 项目 | 状态 |
|---|---|
| R33 / Batch 4C-1 | 已验证关闭 |
| Batch 4D-1A 平台能力门禁 | 已实施，待集中 CI |
| Batch 4D-2 Worker 生命周期审计 | 审计完成，实施未开始 |
| Batch 4D-3 平台进程 adapter | 等 4D-1/4D-2 决策 |
| Phase 2 真机验收 | 滚动进行，不阻塞开发 |
| 当前已验证 HEAD | `82e27c41e8` |

### 下一次集中验证建议

先完成 4D-1A 的 Rust/Android ABI/Gradle/JNI/APK/Instrumentation 集中 CI（预期 Rust tests 753）；在该候选验证期间继续准备 CoreTaskSupervisor 设计，但不把尚未实施的 Worker 生产改动混入本候选。4D-2 的代码 spike 与后续平台 adapter 连续开发后，再合并成下一轮集中验证。
