# Agate Android：Kotlin + Rust 双层架构改造交接文档


> **状态基准声明（重要）**：本文正文早期章节包含原方案分支（如 `architecture/kotlin-rust-core`）的历史实施记录与提交台账（包括 `cc7c868`、`d6004e3`、`90d419a` 等）。这些内容**不代表**验证分支 `verify/arch-8faa5f5` 的当前代码状态。验证分支的真实状态，以本文后部按轮次追加的 R* 验证/实施记录及 `docs/BATCH4B_4C_4D_SOURCE_AUDIT_20261101.md` 中对应批次记录为准；不得将方案分支的历史接线描述当作验证分支已完成的生产迁移。

> 文档用途：供后续开发者接手、评审和持续推进 Android 架构改造。
>
> 当前工作分支：`architecture/kotlin-rust-core`（仅在本地工作副本建立）  
> 当前基线：`e5fb279`（从用户提供的源码 ZIP 导入）  
> 最近代码改造：`3406bb1`（加固 Android Service 销毁清理与签到通知契约）；此前平台路径修复：`e852b95`；平台依赖审计：`aeca960`；交接文档随当前工作副本持续更新
> 本文档对应工作目录：仓库根目录

## 1. 项目目标与边界

将 Agate Android 调整为 **Android 原生 Kotlin 主控 + 平台无关 Rust Core**，不是把 Rust 全面重写成 Kotlin，也不是让 Android 继续由 Rust/JNI 主导生命周期。

目标：

- Android 平台职责由 Kotlin 明确拥有：UI、Android Service、前台服务、通知、权限、生命周期、Boot/Alarm、WakeLock、调度触发、设备能力与系统集成。
- Rust 保留跨平台业务核心：Gateway、Provider、Routing、Protocol、Account、Storage、Authentication、上游请求、SSE 等。
- JNI/Native Bridge 只承担稳定、精简、可版本化的跨语言契约，不暴露 Rust 内部实现细节。
- 桌面/headless 运行路径和现有配置语义不得因 Android 改造而回归。
- 分阶段迁移，每阶段独立 Git commit；每阶段必须有构建、自动化测试和必要的真机验证。没有实际执行的验证不得标记为通过。

**明确不做：**

- 不全面重写 Rust 业务逻辑为 Kotlin。
- 不为了引入 UniFFI 而强行改写所有现有 JNI/API。
- 不把 Android Context、Service、Lifecycle、通知或权限对象传入 Rust Core。
- 不在一个大提交中同时重构 UI、服务、Rust 核心和协议层。

## 2. 目标架构

```text
┌─────────────────────────────────────────────────────────────┐
│                    Android App (Kotlin)                      │
│                                                             │
│ Compose UI / ViewModel / Repository                          │
│            │                                                │
│            ▼                                                │
│ RuntimeController  ───── Settings / State / Diagnostics      │
│            │                                                │
│            ├── ForegroundService / Notification              │
│            ├── BootReceiver / AlarmReceiver / Scheduler      │
│            ├── Permission / WakeLock / Device capabilities   │
│            │                                                │
│            ▼                                                │
│             Native Bridge (JNI first; UniFFI evaluated)      │
└────────────────────────────┬────────────────────────────────┘
                             │ narrow, versioned contract
┌────────────────────────────▼────────────────────────────────┐
│                  Rust Core (platform-neutral)                │
│                                                             │
│ Gateway / HTTP / SSE                                         │
│ Provider / Routing / Protocol / Upstream                     │
│ Account / Storage / Authentication / Config                 │
│ Core runtime state / Background business tasks               │
└─────────────────────────────────────────────────────────────┘
```

### 2.1 Kotlin 层职责

- 唯一管理 Android Service 的启动、停止、重启和销毁。
- 负责前台服务通知、前台服务启动限制、WakeLock 获取与释放。
- 负责 Boot、Alarm、权限与系统回调。
- 管理 Android 侧配置、用户操作和可观察运行状态。
- 将 Native 调用串行化或明确区分可并发调用，避免多个线程竞争同一 Core 生命周期。
- 对 JNI 返回值进行契约解析，向 UI 暴露稳定状态和错误。

### 2.2 Rust 层职责

- 负责与 Android 无关的网关、协议、Provider、账号、路由、存储和鉴权。
- 不直接持有 Android 生命周期对象。
- 不自行决定 Android 前台服务是否存活，也不直接操作 Android 通知/权限。
- 允许桌面/headless 入口继续使用环境变量、原有启动方式和平台适配层。
- Android 专属能力通过窄接口注入，不应污染通用业务模块。

### 2.3 Native Bridge 原则

- 输入输出优先采用简单、稳定的数据类型（字符串/JSON 或明确 DTO）。
- 统一错误结构：错误码、阶段、可读信息、是否可重试；不能把异常统一吞成 `null`。
- 定义 API/协议版本和能力探测入口。
- 长任务、SSE 和事件流需要明确线程、取消、释放与回调生命周期。
- 不允许 Rust panic 穿越 JNI 边界。
- UniFFI 作为候选方案评估：先比较现有 JNI 的维护成本、类型安全、异步/回调能力、生成绑定流程和 CI 复杂度，再决定是否局部采用。

## 3. 当前代码布局

| 路径 | 当前用途 | 目标定位 |
|---|---|---|
| `app/` | Android Compose 应用 | UI、ViewModel、Repository、用户配置 |
| `core-gateway/` | Android library、JNI Kotlin 门面、Service 等 | Android Runtime/Platform Adapter |
| `agate-server/` | Rust 网关 crate，含 JNI bridge | 平台无关 Rust Core + Android 薄适配入口 |
| `scripts/build-android-so.sh` | Android Rust `.so` 构建 | 可复现的 Rust Android 构建入口 |
| `.github/workflows/android-build.yml` | CI | Rust、Android、APK 产物与依赖校验 |
| `docs/` | 架构与迁移文档 | 决策记录、API 契约、接手说明 |

Rust crate 当前基于 `edition = 2021`，同时产出 `cdylib` 和 `rlib`；Android JNI 模块按 `target_os = "android"` 编译。Android `.so` 当前通过脚本放入 `core-gateway/src/main/jniLibs/arm64-v8a/`。

## 4. 已完成工作与提交

以下提交存在于本工作副本的本地分支：

| Commit | 内容 | 状态 |
|---|---|---|
| `e5fb279` | 从用户提供 ZIP 导入的基线 | 已提交 |
| `aeca960` | 第一轮 Rust 环境变量与桌面路径依赖审计 | 已提交；风险已定位，修复待实施 |
| `f19d946` | 串行化 Rust Gateway 生命周期、修复 Native 加载与构建防护 | 已提交 |
| `9f67679` | 串行化 Android Service 命令、补齐失败 tick 收尾 | 已提交 |
| `6ffef6d` | Android API Key 显式运行时覆盖，避免动态修改进程环境变量 | 已提交；待 Rust 编译/并发验证 |
| `90d419a` | Native Bridge 错误不再被吞成 null，CoreTransport 保留链路错误和协程取消 | 已提交；待 Kotlin 编译/单测 |
| `cc7c868` | 新增 RuntimeController，收口 App、Service、调度器入口；修复内部触发任务读取 Android runtime API Key；增加测试 | 已提交；待构建/测试 |
| `d6004e3` | 合并重复 START/TICK 命令，增强 WakeLock 获取/释放异常收尾 | 已提交；待 Android 编译/真机验证 |
| `1d1cd1f` | 更新交接文档中的 RuntimeController 与 Service 阶段状态 | 已提交 |
| `3cc6bf3` | 克隆并持有 JNI 日志回调 GlobalRef，消除撤销监听时的裸句柄竞争 | 已提交；待 Rust 编译/并发验证 |
| `ebd298a` | 修复 RuntimeController 等待线程中断语义；Tile/Settings/控制页/Service 状态查询移出主线程 | 已提交；独立 Kotlin smoke 通过，Android 编译待验证 |
| `c9ec0f1` | 更新本交接文档和本轮验证记录 | 已提交 |
| `38f13c9` | 修复 Service 命令去重跨越 STOP 屏障导致新 START/TICK 被吞的问题 | 已提交；纯 Kotlin coalescer 测试通过，Android Service 编译待验证 |
| `a68aede` | 澄清 Receiver 路由与验证状态 | 已提交 |
| `1fec029` | 记录 Service 命令顺序屏障修复 | 已提交 |
| `ddf4627` | 修复 Service startId 停止语义、前台通知竞态与 Alarm 注册降级；增加 startId 队列键测试 | 已提交；7 项独立 Kotlin 测试通过，Android 构建/真机验证待执行 |
| `2038a4e` | 新增 `checkinRunAndWait` JNI 契约；签到任务真实结束前持续持有 WakeLock，busy 时等待现有签到结束 | 已提交；Rust/Android 构建与真机验证待执行 |
| `c5f203d` | 记录签到真实完成等待与 WakeLock 生命周期修复 | 已提交 |
| `ba252ef` | Android 数据目录改为 `OnceLock<PathBuf>` 显式注入；配置迁移同步识别该覆盖，避免 `set_var` 并发风险 | 静态检查通过；Cargo 编译/测试待执行 |
| `3406bb1` | 加固 `onDestroy()` 清理任务入队失败处理；清理旧签到 timeout 通知分支 | 静态检查通过；Android/Rust 构建与真机验证待执行 |

### 4.1 第一批：Gateway 与 Native 边界

- Rust `start_gateway/stop_gateway` 增加操作锁，防止并发启动/停止竞争。
- 避免重复 start 在检查 `already_running` 前先清除正在使用的 LAN API Key。
- `already_running` 状态返回补齐 `bind_lan`、`version`。
- 修复 `nativeVersion()` 返回 JSON 引号字符串而 Kotlin 期望普通字符串的问题。
- Kotlin Native API 调用前保证 `.so` 已加载。
- `Gateway.apiCall()` 不再捕获所有 `Throwable` 并返回 `null`；改为向上传递异常，由 `CoreTransport` 将 Native 链路错误转换为明确的 `Reply.Down`，并保留协程取消语义（`90d419a`）。

### 4.2 第二批：Android Service 并发与失败收尾

- `ACTION_START`、`ACTION_STOP`、`ACTION_TICK` 统一进入单线程 executor，避免裸线程并发调用 Native Core。
- tick 失败、Core 初始化失败等路径统一执行前台服务清理。
- `onDestroy()` 将 Native stop 与 WakeLock 释放排入同一执行队列。
- Service START/STOP/TICK 进入单线程 executor；相邻重复命令合并，但不同命令构成顺序屏障。
- STOP/TICK 改用独立 startId 队列键，避免命令合并后 `stopSelfResult(startId)` 使用过期 startId；停止动作回主线程执行，避免与 `onStartCommand()` 并发移除新前台通知。
- `stopSelfResult(startId)` 只在当前命令仍是最新 start 请求时停止服务；较新的 START/TICK 已到达时，旧 STOP/TICK 不得销毁服务。
- **仍需验证**：上述 Android Service 改动尚未通过 Gradle 编译和设备验证；executor shutdown/onDestroy 排队竞态、长 tick 期间 STOP 的 FIFO 等待、WakeLock 生命周期仍需真机回归。

### 4.3 第三批：Android API Key 显式运行时覆盖（`6ffef6d`）

正在处理的修改涉及：

- `agate-server/src/server/config/parse.rs`
- `agate-server/src/jni_bridge.rs`

问题背景：原 Android JNI `start_gateway()` 使用 `std::env::set_var/remove_var("AGATE_PROXY_API_KEY", ...)`。但请求鉴权路径会在并发请求中读取 `env_api_key_value()`。`GATEWAY_OP` 只串行化 start/stop，不会串行化请求线程，因此进程环境变量被当作动态运行配置并不合适；另外 start 中途失败时旧逻辑还可能留下 Key。

当前修改设计：

- 用进程内 `OnceLock<RwLock<Option<Option<String>>>>` 保存 Android 覆盖。
- 外层 `None` = Android 没有启用覆盖，回落到原有环境变量行为。
- `Some(Some(key))` = Android 明确启用该 Key。
- `Some(None)` = Android 明确不注入 Key（例如只监听 `127.0.0.1`），不可回落到系统环境变量。
- `env_api_key_value()` 先读 Android 覆盖；未启用覆盖时再按原优先级读取 `AGATE_PROXY_API_KEY` / `WORKBUDDY_PROXY_API_KEY`。
- 配置快照构建仍只读取真实进程环境变量，不把 Android 运行时覆盖误当成持久配置或旧版单 Key 字段；鉴权热路径才读取运行时覆盖。
- Android start 失败时清理覆盖；stop 发出 shutdown 后等待端口释放，完成/超时后清理覆盖。

**注意：代码已提交，但尚未通过 Rust 编译或并发测试。** 需要重点复核 JNI 启动失败的每个分支、stop 超时语义、配置快照构建时机和桌面/headless 环境变量兼容性。不得把当前代码视为已验证完成。

### 4.4 第四批：Native Bridge 错误透传（`90d419a`）

- `Gateway.apiCall()` 返回非空 `String`，不再捕获 `Throwable` 后返回 `null`。
- `CoreTransport` 捕获 `LinkageError` 和普通 `Exception` 并转换为 `Reply.Down`，不再把 Native 加载/调用失败伪装成 `core_not_ready` 重试。
- 显式重新抛出 `CancellationException`，避免协程取消被错误转换成普通链路失败。
- Rust 端返回的结构化 `core_not_ready` 仍保留原来的有限重试行为。

**验证状态：** 尚未通过 Kotlin 编译和 Android 单元测试；后续需要确认 Native 异常文案、JNI 返回空字符串/空指针边界，以及所有调用方对 `Reply.Down` 的处理。

### 4.5 RuntimeController、JNI 回调与内部鉴权修复（已提交）

本轮继续审计后发现 JNI 日志回调还有一个全局引用生命周期风险：`forward_log()` 原先从 `GlobalRef` 取出裸 `jobject` 后释放锁，再调用 Java；如果另一个线程同时撤销监听，原 `GlobalRef` 可能被释放，回调线程仍使用旧句柄。已改为在锁内克隆 `GlobalRef`，并持有到本次 Java 回调结束，移除了这段不必要的裸句柄使用。

本轮还发现一个由 API Key 存储方式调整引入的回归：`trigger_task()` 的内部 HTTP 请求仍直接读取 `AGATE_PROXY_API_KEY` 环境变量，而 Android LAN Key 已迁移到 `env_api_key_value()` 的运行时覆盖中。因此外部请求可通过鉴权，但 JNI 内部闹钟触发请求可能缺少 Authorization 并得到 401。

已修改 `agate-server/src/jni_bridge.rs`：内部触发请求改为调用与鉴权中间件相同的 `env_api_key_value()` 入口。桌面/headless 环境变量行为仍通过该函数的 fallback 保留。

新增 `parse.rs` 测试覆盖 Android override 的三种语义：正常 Key、空白值明确关闭 Key、`None` 参数明确无 Key。测试会清理 override，不修改进程环境变量。

同时新增 `core-gateway/.../RuntimeController.kt`：

- 使用单线程生命周期执行器串行化 Native Core 状态变更。
- 对同一控制线程调用提供重入保护，避免嵌套操作自我等待。
- `AgateApp` 的 `coreInit`、Foreground Service 的 `start/stop/coreInit/onDestroy stop`、设置页的 `schedulerSet` 已接入。
- 设置页调度器开关改为 IO 调用，完成后回主线程更新 UI，避免同步 JNI 阻塞主线程。
- `apiCall`、带超时的 `checkinRunNow` 和只读 `status` 暂不进入生命周期队列，避免长耗时业务调用阻塞 start/stop。
- 新增 FGS 专用 `checkinRunAndWait` JNI 契约：等待 Tokio 签到任务真正结束；若 `fire()` 返回 busy，还会等待当前在途签到归零。定时 tick 在整个真实执行期间持有无超时 WakeLock，finally 才释放。原因是 `checkinRunNow(120s)` 的 timeout 只结束 JNI 等待，不会取消 Rust 后台任务。
- 新增 `RuntimeControllerTest`，覆盖并发串行、嵌套重入、异常透传，以及等待线程被中断后已入队操作仍完成并恢复中断标记；JUnit 尚未通过 Gradle 执行，独立 smoke harness 已覆盖关键中断语义。
- Service 增加 START/TICK 在途标记，合并重复投递，限制等价命令在队列中堆积（`d6004e3`）。
- tick 的 WakeLock 先保存句柄再 acquire，并把释放放在 `finally`，覆盖获取阶段异常。
- 调度器开关通过 `submitSerialized` 按 UI 事件到达顺序入队，回调再切回主线程更新界面。

这只是 Phase 2/3 的第一步，不是完整状态机：BootReceiver 只负责将开机/升级事件转成 FGS start，AlarmReceiver 只负责重排闹钟并转成 FGS tick；两者没有直接调用 JNI，Native 生命周期仍由 Service → RuntimeController 收口。后续仍需验证 STOP 优先级、Service 队列关闭、Receiver 的异步工作时限与状态可观察性。尚未实现 `StateFlow` 状态机。

**验证状态：** `git diff --check`、Rust 结构平衡脚本和行尾检查通过；Cargo 不存在，Gradle Wrapper 下载 Gradle 8.11.1 时 DNS 失败，因此 Rust/Kotlin 编译与测试均未执行。


### 4.7 Service 命令去重必须保留顺序屏障（`38f13c9`）

继续检查 `AgateForegroundService` 时发现，上一版 `startPending/tickPending` 原子标记存在命令顺序 bug：

- 队列中已有 START，随后用户发 STOP，再发新的 START；
- 因第一个 START 尚未完成，`startPending` 仍为 true；
- STOP 已排在队列中，但第三个 START 被误判为重复并丢弃；
- 最终队列执行 START → STOP，服务停下，而用户最后一次明确意图是 START。

现新增纯 Kotlin `ServiceCommandCoalescer`：只合并队列尾部连续相同的命令，不跨越不同命令形成的顺序屏障。因而 START → START 合并；START → STOP → START 保留三个动作；TICK → STOP → TICK 同样保留最后一次 TICK。使用 token 防止较早命令完成时清除较新队列尾部的去重状态。

`AgateForegroundService` 的 START/STOP/TICK 均改走 `enqueueCommand()`，并对 executor 已关闭时的 `RejectedExecutionException` 做显式日志与 token 清理。新增 `ServiceCommandCoalescerTest`，覆盖连续重复合并、START/STOP 和 TICK/STOP 顺序屏障、旧 token 不清理新命令状态。

**验证：** 用独立 `kotlinc` 编译 coalescer 与测试源码（JUnit API 使用临时 stub，仅用于编译/调用测试方法），两组逻辑测试通过。该方式不等于 Gradle/JUnit 测试，也不等于 Android Service 编译或设备验证。

## 5. 改造路线图与验收条件

按顺序推进；若发现依赖关系变化，应先更新本文件和对应 ADR，再调整实施顺序。

### Phase 0 — 建立可靠基线与可复现构建

任务：

- 保留导入基线和每阶段独立提交。
- 检查 Gradle、Rust toolchain、NDK、SDK、ABI、Cargo.lock 的一致性。
- CI 必须验证 Rust tests、Android unit tests、Debug/Release APK、`.so` 打包结果。
- 构建失败必须可定位，不能自动修改并 push Cargo.lock。

验收：

- 干净环境可按文档构建 arm64-v8a `.so` 和 APK。
- CI 在 PR 和 push 上运行，权限最小化。
- APK 内存在预期 ABI 的 `libagate_server.so`。

当前状态：CI workflow 已调整，但本地没有 Cargo，Gradle 下载依赖时遇到 DNS 失败；GitHub Actions 尚未真实运行。因此本阶段是**代码已准备、构建验证未完成**。

### Phase 1 — Native Bridge 契约与生命周期安全

任务：

- 完成 API Key override 修改及测试。
- 审计所有 JNI 入口：参数校验、异常捕获、panic 边界、返回结构、线程限制。
- `Gateway.apiCall()` 的异常吞噬已修复；后续仍需梳理其他 JNI 方法是否存在相同问题，并完善统一错误码/状态契约。
- 定义 `NativeResult` / 错误码 / 状态对象和 Native API 版本。
- 明确 `initialize/start/stop/release/status/apiCall/SSE` 的幂等性、线程安全和释放语义。

验收：

- JNI 所有公开入口均有契约测试或 Kotlin 单元测试。
- 错误不再被静默转成 null；UI 能区分 Core 未初始化、网关未启动、鉴权失败、超时和 Native 异常。
- 重复 start/stop、并发 start/stop、stop 后 restart 均有测试。

### Phase 2 — Kotlin RuntimeController 收口

状态：**第一版串行执行器已提交并接入主要入口；独立 Kotlin smoke 测试通过。主线程 JNI 状态读取已进一步收口；完整状态机/状态流、Boot/Alarm 全入口收口和 Android Gradle/真机验证尚未完成。**

任务：

- 新建单一 `RuntimeController`，作为 Android 侧运行时生命周期协调者。
- UI、Repository、ForegroundService、BootReceiver、AlarmReceiver 不再各自拼接 Native 生命周期操作。
- 定义状态机：`Stopped → Starting → Running → Stopping → Stopped`，另有 `Failed`；状态迁移和错误原因可观测。
- 区分 Core 生命周期与 Gateway 监听生命周期：Core 可持续运行而 Gateway 可独立启停（以现有产品语义为准）。
- 所有命令支持幂等、取消和过期请求防护；避免旧的 tick/start 结果覆盖新状态。
- 明确已入队生命周期操作的取消语义：调用线程中断不得造成“调用方认为取消、队列仍执行”的假取消。当前实现选择等待已提交操作完成并恢复 interrupt 标记。

验收：

- 所有启动入口（App、Boot、Alarm、用户操作）汇入 RuntimeController。
- 生命周期状态只有一个权威来源。
- 自动化测试覆盖重复点击、快速 start-stop、Service 重建、App 进程重启。

### Phase 3 — Foreground Service / Scheduler 可靠性

任务：

- 复核现有单线程 executor：队列容量、重复 tick 合并、STOP 优先级、拒绝任务处理、onDestroy 关闭顺序。队列尾部相邻命令合并已实现；STOP 是否需要优先于正在执行的长 tick，仍需产品语义与真机验证后决定。
- 将周期调度与 Android 系统调度分层：Rust Core 业务调度不应无意依赖 Activity；Android Alarm/WorkManager/FGS 的使用需符合产品实际运行要求和系统版本限制。
- WakeLock 获取/释放必须成对；tick 改为按 Rust 任务实际生命周期持有并在 finally 释放。后续需增加进度/最长执行时间观测，避免异常卡死时无限持锁。
- 明确通知权限、Android 12+ 前台服务启动限制、Android 14+ FGS 类型/权限声明。
- 处理电池优化、Doze、重启、进程被杀、网络切换等恢复路径。

验收：

- Service 所有退出路径都释放 WakeLock、停止/保留 Core 的行为符合状态机。
- 无重复 tick 堆积；tick 超时或 Core 不可用时能自愈。
- 真机测试覆盖冷启动、后台启动、屏幕关闭、网络切换、强制停止/进程回收后的恢复策略。

### Phase 4 — Rust Core 平台解耦

任务：

- 列出 `agate-server` 中所有 Android/JNI/平台特定依赖和调用点。
- 将 Android 专属行为限制在 `jni_bridge` 或独立 platform adapter。
- 抽象 Core Runtime 的启动配置、数据目录、事件通知、取消和 shutdown。
- 将环境变量读取收口为启动配置/配置 Provider；Android 专属动态配置显式注入，不在请求热路径修改进程环境。**本轮已把 Android 数据目录从 `std::env::set_var("AGATE_PROXY_HOME", ...)` 改为 `paths::set_config_dir_override()` 的 `OnceLock<PathBuf>`；`config_migration` 也改为统一查询覆盖状态。**
- 审计后台任务、定时器、文件路径、网络监听、系统时间、信号处理是否隐式依赖 Android。
- 保持 desktop/headless 环境变量、配置文件和命令行入口兼容。

验收：

- Rust 核心逻辑可在非 Android target 独立编译/测试。
- Android 平台能力没有渗透到 Provider/Protocol/业务模块。
- 桌面/headless 的启动、鉴权和配置优先级回归测试通过。

### Phase 5 — Native Bridge 技术评估（JNI vs UniFFI）

任务：

- 盘点 JNI 当前函数数量、参数复杂度、异步/事件流、错误处理和维护成本。
- 做小型 UniFFI spike：只选 DTO/简单同步 API，不迁移全部接口。
- 比较生成绑定、Kotlin 类型映射、异步任务、回调/SSE、取消、生命周期、CI 和版本升级成本。
- 形成 ADR 决策：保留 JNI、局部 UniFFI，或迁移部分稳定接口。

验收：

- 有可运行的对比样例和决策记录。
- 选择方案有明确收益，不以“新技术”作为迁移理由。
- 不影响 Rust Core 平台无关性。

### Phase 6 — 可观测性、压力测试与发布门禁

任务：

- 增加统一日志关联 ID、Native 调用耗时、Core/Gateway 状态、Service 重启原因。
- 对 Native Bridge 做并发、重复调用、长时间运行和内存/线程泄漏测试。
- 对 Gateway 做启动/停止压力、并发请求、SSE 断线重连和 graceful shutdown 测试。
- 建立 Android 真机回归清单和 Release 检查清单。

验收：

- 关键故障可从日志定位到阶段与错误码。
- 长时间运行没有线程/FD/WakeLock 持续增长。
- Debug、Release、冷启动、升级覆盖安装均通过。

## 6. 当前发现的优先问题清单

| 优先级 | 问题 | 风险 | 建议动作 |
|---|---|---|---|
| P0 | Android API Key override 尚未经过真实 Rust 编译与并发测试 | 运行时鉴权行为尚未被构建/测试证实 | 已补 override 单元测试；完成 Rust 编译、内部触发鉴权及启动失败/停止回归测试 |
| P0 | `trigger_task()` 曾直接读取环境变量，未读取 Android runtime override | LAN 模式内部闹钟触发可能 401 | 已在工作区改为统一 `env_api_key_value()`；待测试验证 |
| P0 | 其他 Native 入口的错误边界尚未统一审计 | 仍可能出现异常吞噬、错误状态不一致 | 审计 start/stop/status/task/log callback 并统一错误契约 |
| P0 | Rust/Android 构建未在当前环境真实跑通 | 无法确认源码修改可编译 | 配好 Rust + Android SDK/NDK，执行 CI |
| P1 | Service executor 的销毁和排队边界尚未真机验证 | 任务丢失、Native 并发、WakeLock 泄漏；STOP 等待当前签到真实结束 | 已修复 startId 停止语义和旧命令误停新服务；新增真实任务完成等待，需真机验证长签到、停止等待与进程回收；后续增加执行时长观测/卡死保护 |
| P1 | 闹钟注册失败的降级路径不完整 | Receiver 可能被 AlarmManager 异常中断；UI 可能显示未注册的 nextAlarmAt | 已增加精确闹钟失败降级、最终失败清零和 Receiver 隔离；待 Android 版本/ROM 真机验证 |
| P1 | Android 各启动入口可能分散管理运行时 | 状态竞争、重复启动 | RuntimeController 已接入 App/Service/调度器主要入口；继续收口并实现状态机 |
| P1 | Rust Core 平台边界尚未完整审计 | Android 逻辑渗透核心 | 全量平台依赖清单与 adapter 分层 |
| P2 | JNI/UniFFI 选择尚未有实证 | 可能过度迁移或长期维护成本偏高 | 做小型 spike + ADR |
| P2 | 端到端可观测性和长稳测试不完整 | 故障难复现 | 增加日志、压力和真机回归 |

## 7. 开发者接手指南

### 7.1 获取当前工作副本

解压本项目工作副本 ZIP 后，在项目根目录执行：

```bash
git status --short --branch
git log --oneline --decorate -8
```

当前交接包 ZIP 不包含 `.git` 历史；Git patch 可用于查看/应用已有修改。若需要完整提交历史，应直接接收当前工作目录的 Git bundle 或由维护者从本地仓库导出 bundle。

### 7.2 当前分支与提交策略

- 分支：`architecture/kotlin-rust-core`
- 本地基线：`e5fb279`
- 最近代码改造提交：`3406bb1`（`fix(android): harden service destroy cleanup`）；此前 Android 数据目录修复：`e852b95`；平台隔离基础改造：`ba252ef`；平台依赖审计提交：`aeca960`。当前本地分支领先于导入快照对应的远端跟踪分支，尚未推送；具体 ahead 数以 `git status --short --branch` 为准。
- 关键提交：`f19d946`、`9f67679`、`6ffef6d`、`90d419a`、`cc7c868`、`d6004e3`、`3cc6bf3`、`ebd298a`
- 接手时仍先查看 `git status --short` 与 `git log --oneline -10`。
- 已提交 API Key override：`6ffef6d`。
- 已提交 Native 错误透传：`90d419a`。
- 每个阶段单独提交，建议格式：

```text
feat(android): introduce runtime controller
fix(native): define stable bridge error contract
refactor(core): isolate platform runtime adapters
 test(android): cover foreground service lifecycle
```

不要把未经验证的代码与“已完成”文档一起提交。提交信息中写清验证情况。

### 7.3 本地验证命令

```bash
# Rust 格式与测试
cargo fmt --all -- --check
cargo test --locked --lib

# Android 构建（项目 Wrapper）
bash gradlew testDebugUnitTest
bash gradlew assembleDebug
bash gradlew assembleRelease

# 检查补丁空白错误
git diff --check

# 检查 APK 内 native 库（示例，按实际产物路径调整）
unzip -l app/build/outputs/apk/release/app-release.apk | grep 'lib/arm64-v8a/libagate_server.so'
```

Android `.so` 构建按项目脚本执行：

```bash
bash scripts/build-android-so.sh
```

### 7.4 真机测试最低要求

- 首次启动、Core 初始化、Gateway 启动/停止。
- 连续快速 start/stop、重复 start、重复 stop。
- LAN 模式启用 Key；回环模式明确不启用 Key；LAN → 回环切换。
- 错误端口/端口占用导致 bind 失败后再次启动。
- Service tick 期间 stop、Service 销毁期间有排队命令。
- 屏幕关闭、后台运行、网络切换、进程重建。
- Release APK 安装后 Native `.so` 可加载。

## 8. 本次工作环境限制与真实性说明

本轮输入是 `agate-android.zip`，该 ZIP 不包含 `.git`。因此当前 `architecture/kotlin-rust-core` 是在导入快照上建立的本地 Git 分支，不代表已从用户远端仓库检出，也没有推送到 GitHub。

此前本地环境检查结果：

- Cargo/Rust 工具链不可用，Rust 编译与测试未执行。
- Gradle Wrapper 可启动，但下载 Gradle 8.11.1 时因 DNS 无法访问镜像站失败。
- Git 静态检查、行尾检查、Rust 源码括号平衡脚本、CI YAML 结构检查已执行；这些不能替代真正的编译和测试。
- GitHub Actions 尚未真实运行。
- 本轮发现本机有 `kotlinc`，已单独编译 `RuntimeController.kt`，并运行独立 smoke harness，覆盖并发串行、嵌套重入、异常透传、异步回调，以及等待线程被中断时已提交操作仍完成并恢复中断标记，结果通过。该验证不等于 Android/Gradle 模块编译，也不覆盖 Service / Compose / TileService 代码。
- 本轮再次尝试 `bash gradlew --offline :core-gateway:testDebugUnitTest`，仍需下载 Gradle 8.11.1，因 `UnknownHostException: mirrors.cloud.tencent.com` 失败。Cargo 不可用，Rust 编译/测试未执行。
- 本轮 `ServiceCommandCoalescer` 与其测试源码通过独立 `kotlinc` 编译，并直接运行测试方法通过；这不替代 Gradle/JUnit/Android 构建。
- 本轮在独立 Kotlin/JUnit stub harness 下重新编译并执行 `RuntimeControllerTest` 与 `ServiceCommandCoalescerTest`，共 7 个测试通过。该方式验证纯 Kotlin 并发/队列逻辑，不代表 Gradle/JUnit 或 Android Service/Alarm 代码已编译。
- 本轮进一步修复 Service 的 `startId` 停止语义：旧 STOP/TICK 不会因 `stopSelf()` 无条件终止较新的 start 请求；`stopSelfResult` 与 `stopForeground` 被投递回主线程，避免和 `onStartCommand()` 交错。STOP/TICK 使用带 `startId` 的唯一队列键，不会因 coalescing 导致 `stopSelfResult` 永远拿到旧 id。代码已提交 `ddf4627`。
- 本轮修复闹钟注册状态：精确闹钟调用失败时降级为非精确闹钟；两种方式都失败时清除 nextAlarmAt 并返回 null；AlarmReceiver 即使接力排程失败仍继续触发当前 tick，避免丢掉当前签到。
- 后续发现 `checkinRunNow(120s)` 超时后 Rust Tokio 签到任务仍继续，而原 Service 会释放 WakeLock 并可能退出 FGS。新增 `Gateway.checkinRunAndWait()` / `nativeCheckinRunAndWait()`：JNI 等待任务真实完成；若本次 `fire` 因已有任务返回 busy，则继续等待 `AutoCheckin.is_running()` 归零。Service tick 改用此入口，WakeLock 改为无固定超时并由 `finally` 成对释放。该改动已提交为 `2038a4e`，但尚未 Cargo/Gradle 编译或真机验证。
- 对新 JNI/Rust 改动执行了 `rust_balance.py`、`check_eol.py`、`git diff --check`，均通过；当前环境无 Cargo/rustfmt，未执行 Rust 编译、格式化或测试。
- 本轮 Rust 平台边界审查发现 Android `ensure_core()` 仍通过 `std::env::set_var("AGATE_PROXY_HOME", data_dir)` 修改进程全局环境；该写入可能与 Tokio worker/日志线程的环境读取并发。已改为 `paths::set_config_dir_override()`（进程级 `OnceLock<PathBuf>`，相同路径幂等、不同路径拒绝），`config_dir()` 优先使用显式覆盖，桌面/headless 仍沿用环境变量；`config_migration::env_dir_override_set()` 同步调用 `paths::has_config_dir_override()`，避免 Android 显式目录被误判为没有覆盖。该改动尚未 Rust 编译/测试。
- 本轮重新编译并运行 `RuntimeControllerTest` 与 `ServiceCommandCoalescerTest` 的独立 Kotlin/JUnit stub harness：当前源码共 7 个测试方法，全部通过。该 harness 不代表 Gradle/JUnit 或 Android Service/Alarm 代码已编译。
- 本轮再次执行 `bash gradlew --offline :core-gateway:testDebugUnitTest`，Wrapper 仍需下载 Gradle 8.11.1，因 `UnknownHostException: mirrors.cloud.tencent.com` 失败。
- 本轮新增 `e852b95`：`paths::set_config_dir_override()` 在写入进程级 `OnceLock` 前校验路径非空、绝对路径且不含 NUL；非法输入不会污染一次性初始化状态。新增两个 Rust 单元测试覆盖绝对/空/相对路径与 NUL 拒绝。该改动通过 `rust_balance.py`、`check_eol.py`、`git diff --check`；当前环境仍无 Cargo/Rustc/rustfmt，因此测试尚未执行。
- 本轮重新使用独立 `kotlinc` + 临时 JUnit stub harness 编译并执行 `RuntimeControllerTest` 与 `ServiceCommandCoalescerTest`，7 个测试方法全部通过；这不是 Gradle/JUnit/Android 模块构建。

因此当前进度只能描述为：**已完成若干代码改造并提交；部分静态检查通过；构建、自动化测试和真机回归仍待执行。**

## 9. 最新平台依赖审计（2026-10-31）

新增 `docs/ANDROID_PLATFORM_DEPENDENCY_AUDIT.md`，完成第一轮 Rust 环境变量与桌面路径分类。重要发现：`account_bootstrap::run()` 在正常 Core bootstrap 路径运行，并触发 Raccoon / CatPaw / AutoClaw 启动期旧数据导入；其中部分 `home_dir()` / `legacy_directory()` 在 `HOME` 与 `USERPROFILE` 缺失时回退到相对路径 `.`。这属于平台边界风险，尚未证明 Android 设备运行时必然缺少 HOME。下一步应先将“无用户主目录”显式表示为 `None`，在对应自动导入来源中安全跳过且不写一次性导入标记；不要直接把所有 Provider 路径替换为 `config_dir()`。

环境变量需分为 Provider 显式凭证/端点配置、宿主运行配置、桌面路径探测、测试变量四类；不能批量删除 Provider 的 `*_TOKEN` / `*_COOKIE` / `*_BASE_URL` 等兼容入口。具体调用点和实施顺序见该审计文档。

## 10. 接手时的第一优先事项

1. 先检查工作树和最近两个提交，确认 API Key override 的所有失败分支和清理时机，以及 `Gateway.apiCall()` 异常透传行为。
2. 安装/配置 Rust 1.88+、Android SDK、NDK 和 Gradle 所需依赖，运行真实构建与测试。
3. 为 API Key override 增加单元/并发测试，验证 `Some(None)` 不会回落到系统 env，验证桌面/headless 未启用 override 时仍读环境变量。
4. 审计其他 JNI 方法和 `AgateForegroundService` 中的 `Throwable` 捕获，区分可恢复异常与必须向上传递的致命错误。
5. 先在可联网/具备 Android SDK 的环境运行 Gradle 全量编译和单元测试；特别验证本轮 TileService、Compose 与 Service 改动。
6. RuntimeController 下一步需实现可观察状态机；Service 已修复 startId 保护，但仍需验证 executor 的 STOP 等待策略、onDestroy 排队与关闭竞态。BootReceiver/AlarmReceiver 目前只转发为 Service 命令，不直接调用 JNI。
7. 在设备上验证 START → STOP → START、TICK → STOP → TICK、重复 TICK、tick 执行中 STOP，以及 Service 销毁时队列中仍有命令；特别检查旧 STOP/TICK 不会移除新前台通知。
8. 继续审计所有 `Gateway.status()` / Native 调用是否在主线程执行；当前已处理本轮定位到的主要 UI 入口，但仍需以全仓调用清单为准。
9. 验证精确闹钟授权撤销、非精确降级、AlarmManager 两种注册都失败、Receiver 当前 tick 继续执行等场景。
10. 对本轮 `paths::set_config_dir_override()` 做 Rust 编译与回归：Android 显式路径优先于 env、重复同路径幂等、不同路径拒绝、桌面/headless env 优先级保持、配置迁移检测 Android override。

---

文档维护原则：每次阶段性改造完成后，更新「已完成工作与提交」「路线图状态」「问题清单」「验证结果」四部分，并把实际 commit hash 写入本文件。

## 11. 启动期旧账号导入路径防护（最新代码变更）

- 新增 `paths::user_home_dir()`：USERPROFILE 优先、HOME 次之；拒绝空白、相对路径和 NUL，不回退到当前工作目录 `.`。
- `account_bootstrap::run()` 在 Raccoon / CatPaw / AutoClaw 三家旧数据自动导入之前检查该目录；无有效绝对目录时跳过整组三家导入，且不调用导入器，因此不写 `raccoonImported` / `catpawImported` / `autoclawImported` 标记。Agate 自身的旧 auth.json 迁移及前序数据库迁移不受此门禁影响。
- `paths.rs` 新增测试源码，覆盖 USERPROFILE、HOME fallback、缺失、空白和相对路径拒绝。
- 该修复仅保护启动期自动导入入口；Provider 内部手动导入路径函数仍存在 `.` fallback，不能宣称所有桌面路径已平台隔离。显式 Provider 旧目录覆盖也未扩展。
- 静态验证：`git diff --check`、`python3 tools/rust_balance.py`、`python3 tools/check_eol.py` 通过。Cargo/Rustc/rustfmt 当前不可用，Rust 测试未执行；Android 构建与真机验证未执行。

## 12. 本轮后续增量：启动导入门禁与 JNI 参数读取

- `7e1c2e7 fix(core): guard legacy imports without user home`：增加 `paths::user_home_dir()` 与 `account_bootstrap::run()` 启动期门禁。无有效绝对 HOME / USERPROFILE 时，Raccoon / CatPaw / AutoClaw 旧数据自动导入整体跳过，不写一次性导入标记。静态检查通过；Rust 测试未运行（工具链缺失）。
- `81b89bd fix(native): preserve JNI argument read errors`：修复 `nativeApiCall` 把 `get_string()` 错误通过 `unwrap_or_default()` 静默折叠为空字符串的问题；增加统一字符串读取/错误响应辅助函数，并保留 Kotlin `body: String?` 的 nullable 契约（null 与空字符串都表示无请求体）。
- JNI 变更仍未经过 Rust 编译或 Android 真机验证。下一步需继续检查 JNI 导出签名、可空参数、锁持有范围、阻塞等待和错误返回契约；不要将本轮静态审查描述为完整 JNI 安全认证。

## 13. JNI 字符串错误边界统一（`NEXT`）

本轮继续审计全部 `Java_dev_agate_core_Gateway_*` 导出入口，覆盖 `nativeStart`、`nativeStop`、`nativeStatus`、`nativeTriggerTask`、`nativeCoreInit`、`nativeSchedulerSet`、`nativeCheckinRunNow`、`nativeCheckinRunAndWait`、`nativeVersion`、`nativeSetLogCallback`、`nativeApiCall` 共 11 个导出方法。

发现此前虽然 `nativeApiCall` 已尝试把字符串读取错误转换为 JSON，但 JNI `get_string()` 失败可能留下 pending Java exception。此时继续调用 `json_reply()` 中的 `new_string()` 可能再次失败，导致 Kotlin 收到 null，而不是结构化错误。并且 `nativeStart`、`nativeCoreInit`、`nativeTriggerTask` 仍各自直接调用 `get_string()`，错误字段格式也不统一。

本轮修改：

- `read_jstring()` 统一处理 `get_string()`；读取失败时先保存原始错误文本，再检查并清理 pending Java exception，最后返回 Rust `Err`。
- `nativeApiCall`、`nativeStart`、`nativeCoreInit`、`nativeTriggerTask` 全部通过该 helper 读取 Java String。
- 参数读取错误统一返回 `{ok:false, stage:"jni", argument:<name>, error:<message>}`，使 Kotlin 侧可稳定解析错误，而不是因 pending exception 得到空 JNI 返回值。
- `nativeApiCall.body` 仍保留 nullable 语义：null 和空字符串表示无 body。

本轮仅处理字符串参数读取边界，不代表所有 JNI 风险已消除。剩余重点：

1. `nativeStop()` 最多同步等待端口释放约 5 秒；`nativeCheckinRunAndWait()` 无超时等待签到完成；`nativeApiCall()` 最多同步等待 120 秒。Kotlin 调用方必须保证这些入口不运行在主线程，仍需全仓调用点审计和 StrictMode/设备验证。
2. `nativeStart()` / `nativeCoreInit()` 会同步执行 Core bootstrap；必须确认所有调用都在后台生命周期执行器中。
3. `nativeSetLogCallback()` 的回调替换/撤销、GlobalRef 保活已做基础处理，但还需要验证 JVM shutdown / callback 抛异常 / 注册失败时的行为。
4. `json_reply()` 在 JVM 分配失败时仍返回 null 指针；这是 OOM 等极端场景，Kotlin 当前解析入口没有统一处理 null native return 的保护。应评估是否改为抛出 Java 异常或统一错误字符串，但不能在 pending exception 状态下继续分配字符串。
5. 当前 JNI 是静态导出符号绑定，没有 `JNI_OnLoad/RegisterNatives`；需要通过 APK/so 构建验证包名、类名、方法签名一致性。

验证：`git diff --check`、`python3 tools/rust_balance.py`、`python3 tools/check_eol.py` 通过。当前容器仍无 Cargo、rustc、rustfmt，未执行 Rust 编译、测试或格式化；Gradle/Android SDK 和设备验证也未执行。因此本轮是代码修复 + 静态检查，不是编译通过。

### 13.1 JNI 日志回调异常隔离补充

继续沿 `forward_log()` → `call_on_log()` 检查发现：当 Kotlin `LogListener.onLog()` 抛出异常时，`env.call_method()` 返回错误，但此前代码仅忽略该错误。若回调发生在已由 Java 附着的线程，pending exception 可能残留在该线程，污染后续 JNI 调用。现新增 `clear_pending_exception()`，并在日志回调失败时清理异常；日志回调是尽力而为的旁路，不允许 listener 异常反向破坏 Native 控制面。`nativeSetLogCallback()` 的 `new_global_ref()` 失败分支也先清理 pending exception，再生成结构化错误。

该修复与上一节的字符串读取修复共用异常清理辅助函数。仍需在 Android/JVM 环境验证 listener 抛异常、并发撤销回调、回调线程附着/脱离等场景。

## 13.2 Service 销毁收尾与签到通知契约清理（本轮）

- `AgateForegroundService.onDestroy()` 对清理任务入队增加 `RejectedExecutionException` 防护。正常路径仍将 `Gateway.stop()` 排在已接收的 START/STOP/TICK 命令之后，并使用 orderly `shutdown()` 排空队列，不中断正在执行的 JNI/Rust 操作。
- 若 executor 已关闭、清理任务无法入队，不在主线程补调可能阻塞的 `Gateway.stop()`；记录明确日志并同步释放本地 WakeLock/WifiLock，避免异常中断 Service 销毁流程。该拒绝分支只能保证 Android 本地锁释放，不能保证 Native Core 已停止，属于防御性降级，不应描述为完整清理成功。
- `notifyCheckinResult()` 现在只按 `checkinRunAndWait()` 的同步完成契约处理 `busy` / 结果 / 错误，删除旧 `checkinRunNow(120s)` 的 `timeout` 通知分支，修正相关注释，避免 UI 通知暗示任务已转后台继续。
- **验证状态**：`git diff --check` 与静态源码检查通过；本环境没有 Gradle/Rust 工具链，未执行 Android/Rust 编译、JUnit/ Cargo 测试或真机验证。
- **语义边界仍保留**：STOP 在单线程队列中等待当前长 tick 完成；本轮没有引入 JNI 线程中断或 Rust 任务强制 abort。后续若产品要求“立即停止”，必须先设计 Provider 网络请求的合作式取消、部分结果落盘和幂等收尾契约，再改 STOP 行为。

## 14. Android 签到生命周期与 Provider 超时审计（2026-10-31）

新增 `docs/ANDROID_CHECKIN_LIFECYCLE_AUDIT.md`。本轮沿 `nativeCheckinRunAndWait()` → `AutoCheckin::fire_for()` → `billing::checkin::run_checkin()` → 各 Provider 的请求链审计了任务收尾语义与请求超时。

关键结论：

- `run_checkin()` 按账号串行执行，但没有整轮 deadline；各 Provider 的单请求上限并不等于 Provider 整体或整轮上限。
- 已确认 WorkBuddy Billing 20s、WorkBuddy 国际版活跃请求/读体 20s、Raccoon 20s（账单核对 25s）、AutoClaw 20s、Qoder API 15s / UMID 子进程 25s、Trae 20s、Loomy 20s、KukuAI 30s、OfficeAce 20s、CodeArts 20s 等具体阶段上限；多阶段流程总耗时可明显更长。
- `egress` 的默认 read timeout 与 SSE 长连接兼容，默认配置 300s；它不是整轮签到 deadline。不能因为某个 Provider 的一发请求没有 `.timeout()` 就推断其永远无超时，必须沿 `send_raw` 和 Client 配置继续核对。
- CodeArts / Trae / AutoClaw 等签到流程有多阶段写入与确认，给整个 Future 粗暴套 `tokio::time::timeout()` 会在远端已接受写请求后 drop 本地 Future，存在远端成功、本地未确认/未落账的风险。
- 当前 `AutoCheckin::stop()` 只停止 scheduler loop，不打断正在执行的签到；这是源码明确的既有语义。Android Service STOP 排在 TICK 后面，优先保证 Native 操作完整收尾，代价是 STOP 可能等待较久。

下一步安全方向已记录在专项审计文档：新增与 scheduler `stopped` 分离的每轮 stop-request；Android STOP 可先通过轻量 JNI 入口设置停止标志；当前账号的多步 Provider 流程自然收尾；在账号与 schedule 边界阻止启动下一项；保留已完成账号结果并明确呈现停止状态。当前仅完成审计与设计约束，没有为追求响应速度而引入危险的全局 timeout / abort。

验证：本轮为源码阅读与文档变更；`git diff --check`、`rust_balance.py`、`check_eol.py` 待本轮执行。Rust/Gradle 工具链仍不可用，未运行编译、测试或真机验证。

## 14. Android 签到合作式停止（2026-10-31）

本轮从生命周期审计进入了运行时代码实现，新增 `docs/ANDROID_CHECKIN_COOPERATIVE_STOP.md`。

- `AutoCheckin::Inner` 新增独立 `cancel_current` 原子标志，与调度器 `stopped` 分离。
- `AutoCheckin::request_cancel_current()` 可从 JNI 轻量置位，不等待 Service 命令队列。
- `run_checkin()` 保留兼容入口，内部转调 `run_checkin_cancellable()`；合作式停止只在账号边界检查，不中断当前 Provider 的多阶段请求。
- 部分执行结果、账号台账与签到历史正常落地，并增加 `cancelled` 字段区分完整批次与提前停止。
- Kotlin 新增 `Gateway.requestCheckinStop()` 和独立 `AgateCheckinStop` executor；STOP 仍通过原命令队列完成网关停止与 Service 生命周期收尾。
- FGS 对 `cancelled=true` 结果发送“签到已停止”通知；WakeLock 仍等待真实任务结束后在 `finally` 释放。

验证：`git diff --check`、`tools/rust_balance.py`、`tools/check_eol.py` 通过；Kotlin Native 声明与 Rust JNI 导出名称 12/12 匹配。当前环境没有 Cargo/Rustc/Rustfmt/Gradle，因此未执行 Rust 测试、Android 构建或真机回归。实现尚需重点验证 STOP 与最后一个账号收尾、调度器正在运行时 STOP、Service 销毁时独立停止 executor 的行为。

## 15. 当前工作树与交付状态（2026-10-31）

- 当前 HEAD：`4bb8141 feat(android): add cooperative checkin stop`。
- 分支：`architecture/kotlin-rust-core`；相对导入 ZIP 时创建的 `origin/architecture/kotlin-rust-core` 跟踪点 ahead 20；该 origin 不是用户原始 GitHub 仓库历史，当前仍未推送。
- 工作区：干净（提交前后均检查）。
- JNI：Kotlin `Gateway` 的 12 个 `native*` 声明与 Rust 12 个 JNI 导出名称完全匹配。
- 静态检查：`git diff --check`、`python3 tools/rust_balance.py`、`python3 tools/check_eol.py` 通过。
- 编译/测试：本环境没有 `cargo`、`rustc`、`rustfmt`、`gradle`，所以 Rust 测试、Android Gradle 构建和真机测试仍未执行。
- 合作式停止边界：只覆盖 `AutoCheckin` 当前已进入 `running` 的轮次；如果 STOP 信号恰好先于轮次取得 running，或当前业务是 `api::accounts` 直接执行的独立签到，则不会由该标志取消。当前 Provider 请求仍自然收尾，不做强制中断。

## 16. STOP/TICK 启动窗口竞态修复（2026-10-31）

在合作式停止实现后的二次审查中发现：STOP 可能早于 Rust `AutoCheckin.running=true` 到达，第一次 `nativeRequestCheckinStop()` 会返回 `requested=false`，若不重试，已经排队的 TICK 仍会启动完整签到。

本轮修改：

- 新增 `PendingTickRegistry`，以 Android `startId` 跟踪已入队且尚未退出的 TICK。
- STOP 快照当时已存在的 TICK；独立停止 executor 每 100ms 重试 JNI 请求，直到快照内所有 TICK 退出。STOP 之后新入队的 TICK 不在快照中，不会被旧 STOP 误取消。
- TICK 的正常完成与 executor 拒绝路径都会注销对应 `startId`，防止停止线程永久等待。
- 新增 3 个纯 Kotlin Registry 测试：STOP 快照隔离后续 TICK、多 TICK 全部完成判定、完成项不残留。

验证：`PendingTickRegistry.kt` 使用本机 `kotlinc` 单独编译通过；通过临时 JUnit stub 执行测试方法 3/3 通过；`git diff --check`、`python3 tools/rust_balance.py`、`python3 tools/check_eol.py` 通过。没有 Gradle、Android SDK、Cargo、Rustc，因此完整 Service 编译、Gradle 测试、Rust 编译和真机回归仍未执行。该测试结果不能等同于 Android 构建通过。

仍需真机验证：TICK 已排队但尚未执行时 STOP；TICK 正在 Provider 请求时 STOP；多个旧 TICK 排队后 STOP；STOP 后新 TICK 排队；重复 STOP；Service 销毁与停止 executor 关闭的交错。

## 17. 当前状态更新（2026-10-31，STOP/TICK 竞态修复后）

- 最新功能提交：`a716e96 fix(android): retry stop signal for queued ticks`；最新 HEAD：`01c13ea docs(android): record queued tick stop race fix`。
- 分支：`architecture/kotlin-rust-core`；当前相对导入 ZIP 时创建的本地跟踪点 ahead 23；未推送 GitHub。
- 改动文件：`AgateForegroundService.kt`、新增 `PendingTickRegistry.kt`、新增 `PendingTickRegistryTest.kt`、合作式停止专项文档与本交接文档。
- 本轮纯 Kotlin Registry 测试 3/3 通过（临时 JUnit stub 驱动）；Registry 单文件 kotlinc 编译通过。静态脚本通过。
- 仍未执行 Gradle/Android build、Cargo/Rust build、真实 JUnit Gradle 测试与设备测试。当前容器无 Android SDK、Gradle、Cargo、Rustc。
- 交付物应以本次刷新生成的 ZIP / Patch / Bundle 为准；Bundle 是本地导入仓库历史，不是用户原始 GitHub 仓库历史。

## 18. 2026-10-31：签到请求超时链路审计与响应体错误修复

本轮新增 `docs/ANDROID_CHECKIN_TIMEOUT_AUDIT.md`，沿 `billing::checkin_for` 实际分派逐家追踪签到请求链，区分单请求超时与整账号/整批次耗时。结论是：多数 provider 已有明确的请求级超时，但多阶段签到仍可能由多次顺序请求叠加；不能用一个包住整个 provider future 的 `tokio::time::timeout` 粗暴中断，尤其 CodeArts 与 Trae 的写入/确认链。

本轮修复两个真实错误处理缺陷：
- `providers/officeace/subscription.rs`：响应体读取失败不再吞成空字符串；超时映射 504，其它传输读取失败映射 502。
- `providers/codearts/welfare.rs`：同样保留响应体读取错误；发送阶段和读取阶段均区分 504/502。

这两处修复不改变成功响应解析、上游业务码、CodeArts 幂等键预写或 claim → confirm → delivery 回读顺序。

**验证状态：** `git diff --check`、`tools/rust_balance.py`、`tools/check_eol.py` 已通过；当前环境仍没有 `cargo` / `rustc` / `rustfmt` / Gradle / Android SDK，因此本轮 Rust 编译、provider mock 测试、Android 构建和设备验证不能宣称通过。

## 19. 2026-10-31：响应体读取错误修复提交

- Commit：`2ececca fix(checkin): preserve reward response read failures`
- 当前本地分支：`architecture/kotlin-rust-core`，相对导入 ZIP 时创建的本地 tracking ref ahead 25 commits。
- 本轮新增 `docs/ANDROID_CHECKIN_TIMEOUT_AUDIT.md`，记录各 provider 的请求级超时、顺序阶段与取消风险。
- OfficeAce / CodeArts 的 HTTP send 与 response-body read 均将 timeout 归类为 504，其他 transport error 归类为 502；response-body read 不再吞错。
- `git diff --check`、`python3 tools/rust_balance.py`、`python3 tools/check_eol.py` 通过。
- 当前容器没有 Cargo / Rustc / Rustfmt / Gradle / Android SDK；本轮未执行 Rust 编译、Rust provider 测试、Gradle 构建或设备验证。
- 本地导入仓库的 `origin/architecture/kotlin-rust-core` 仍是 ZIP 导入基线的 tracking ref，不是用户 GitHub 原始分支；本轮没有 push。

## 2026-10-31 follow-up: Trae response-body timeout handling

A further pass over the check-in chain found that the shared Trae HTTP helper (`providers/trae/http.rs`) discarded response-body read errors and returned an empty string. This could make the mandatory same-device verification after a claim look like an invalid/empty upstream response when the real cause was a timeout or mid-body disconnect.

The helper now propagates send/read transport errors with `504` for timeouts and `502` for other transport failures, preserves ordinary HTTP status responses for the existing provider decision logic, and reads chunks incrementally with a 1 MiB cap (instead of buffering the full body before truncation). A pure helper test covers multi-chunk accumulation and cap behavior.

Validation remains bounded by the environment: `git diff --check` and repository static scripts can run here, but Rust compilation/tests, Gradle/Android build, and device verification still require the external toolchain.

## 20. Qoder 刷新后签到重放结果归一（2026-10-31）

继续审查 Qoder campaign 签到发现一个边界不一致：第一次 `claim_campaign()` 后会把 `409` / `ALREADY_CLAIMED` 归一成 `alreadyCompleted=true`；但若第一次明确返回 `401`，刷新凭证并重放领取后，第二次响应直接进入 `auth::payload()`，没有重复执行幂等结果判定。并发场景下，另一客户端可能在刷新窗口内完成领取，导致第二次返回 `ALREADY_CLAIMED`，原代码会把正常的「今日已领取」报成 HTTP 错误。

本轮修复：
- 抽出 `already_claimed()` 与 `already_claimed_result()`，第一次领取与刷新后唯一一次重放共用判定。
- 同时识别 HTTP 409 与业务标记 `ALREADY_CLAIMED`；业务标记即使包在 HTTP 200 也按幂等完成处理。
- 不增加重试次数，不重试网络超时/响应体读取错误，不改变 token refresh 或 campaign claim 的顺序。
- 增加 3 个纯单元测试：HTTP 409、HTTP 200 + `ALREADY_CLAIMED`、普通上游失败不误判。

验证：`git diff --check`、`python3 tools/rust_balance.py`、`python3 tools/check_eol.py` 通过。当前环境无 Cargo / Rustc / Rustfmt，新增 Rust 测试尚未执行，不能宣称 Rust 编译通过；Gradle/Android 构建与设备验证也未执行。

## 21. 最新分支与验证状态（2026-10-31，Qoder 重放结果修复后）

- 最新 HEAD：`e28cbf6`（`fix(qoder): normalize refreshed claim replay result`）。
- 分支：`architecture/kotlin-rust-core`；相对导入 ZIP 时创建的本地 tracking ref `origin/architecture/kotlin-rust-core` ahead 28 commits。该 tracking ref 不是用户原始 GitHub 仓库历史，本轮没有 push。
- 本轮改动：Qoder 首次领取与 401 刷新后重放共用 `ALREADY_CLAIMED` 判定；新增 3 个纯 Rust 单元测试；同步更新签到超时审计与本交接文档。
- 本轮静态检查：`git diff --check`、`python3 tools/rust_balance.py`、`python3 tools/check_eol.py` 通过。
- 编译/测试边界：当前容器没有 Cargo / Rustc / Rustfmt / Gradle / Android SDK；本轮新增 Rust 测试未执行，未执行 Rust 编译、Gradle/Android 构建或设备验证。
- 当前工作树应保持干净；后续交付 ZIP / Patch / Bundle 以此 HEAD 为准。Bundle 仍是本地导入仓库历史，不代表用户 GitHub 原始历史。

## 22. Trae 签到响应解析防假成功（2026-10-31）

继续沿签到链路审查时发现 `providers/trae/checkin.rs::round_trip()` 将 JSON 解析失败降级成 `{}`，而 `status_of()` 将缺失 `code` 默认成 0。空响应、HTML 错误页或结构错误的 JSON 因此可能伪装成业务成功；尤其 claim 响应无效后，后续状态回查若恰好观察到另一端并发完成，可能让当前操作被误认为成功。

本轮修复：
- 新增 `parse_checkin_payload()`，要求响应是 JSON 对象且包含整数 `code`。
- `round_trip()` 在业务码分派前强制验证；无效 JSON / 缺少或错误类型的业务码返回 502，不再进入成功/回查路径。
- 新增 3 个纯 Rust 单元测试，覆盖有效响应、空/非 JSON 响应、缺失/字符串/非对象业务码。
- 不增加重试，不改变 JWT/Bearer 方案回退，不改变 claim 后同设备号回查策略。

验证：`git diff --check`、`python3 tools/rust_balance.py`、`python3 tools/check_eol.py` 通过。当前容器没有 Cargo / Rustc / Rustfmt，新增测试与 Rust 编译未执行；Gradle/Android 构建和设备验证未执行。

## 23. 最新状态（2026-10-31，Trae 响应解析修复后）

- 最新功能提交：`32db3f4 fix(trae): reject malformed checkin responses`。
- 本轮涉及：Trae check-in response validation、签到超时审计、迁移交接文档。
- 分支：`architecture/kotlin-rust-core`；本轮功能提交后相对 ZIP 导入时创建的本地 tracking ref ahead 30 commits；当前未推送 GitHub。
- 工作区：本次交付刷新前已确认干净；交付包应以最终文档刷新提交后的 HEAD 为准。
- 静态检查：`git diff --check`、`python3 tools/rust_balance.py`、`python3 tools/check_eol.py` 通过。
- 新增 Trae Rust 测试尚未运行；当前环境没有 Cargo / Rustc / Rustfmt。Gradle/Android 构建和设备测试也未运行。

## 24. AutoClaw user-info 业务成功判定（2026-10-31）

继续检查 AutoClaw 活动福利触发链时发现，`trigger_login_active()` 原先只要 `userapi_post()` 返回 `Ok(_)` 就认为登录触发成功。但共享 HTTP helper 对 2xx 非 JSON body 会返回 `Value::Null`，上游也可能在 HTTP 200 中返回非零业务码。原逻辑会因此误报触发成功，并额外等待 1.5 秒、执行流水复核。

修复内容：
- 新增 `user_info_trigger_result()`，要求响应为 JSON object 且 `code` 是整数 `0`。
- 非零业务码优先记录上游 `msg`，没有可读消息时记录业务码；缺失/错误类型/非对象响应记录为未确认。
- 只有明确业务成功才等待结算并复核活动流水。
- 这是附加福利链路，触发失败仍不影响已经完成的每日签到；不增加重试、不改变主签到流程。
- 新增 3 个纯单元测试，覆盖成功、非零业务码、缺失/错误业务码。

验证边界：静态检查通过不代表 Rust 编译通过；当前环境没有 Cargo / Rustc / Rustfmt，新增测试未执行。Gradle/Android 构建和设备验证仍未执行。

## 25. 当前分支状态（2026-10-31，AutoClaw user-info 判定修复后）

- 最新功能提交：`a99ce5a fix(autoclaw): validate user-info trigger result`。
- 分支：`architecture/kotlin-rust-core`；相对 ZIP 导入时创建的本地 tracking ref `origin/architecture/kotlin-rust-core` ahead 32 commits。
- 本轮涉及：AutoClaw user-info 触发结果严格判定、签到超时审计、迁移交接文档。
- 工作区：功能提交后干净；交付刷新后以最终文档提交对应的 HEAD 为准。
- 静态检查：`git diff --check`、`python3 tools/rust_balance.py`、`python3 tools/check_eol.py` 通过。
- 新增 AutoClaw Rust 单元测试未执行；当前环境没有 Cargo / Rustc / Rustfmt。Gradle/Android 构建与设备测试未执行。
- 未推送 GitHub。`origin/architecture/kotlin-rust-core` 是 ZIP 导入基线创建的本地 tracking ref，不是用户 GitHub 原始历史。
- Bundle 是本地导入仓库历史的可恢复快照，不代表用户原始 GitHub 仓库历史。

## 2026-10-31 follow-up: global check-in entrypoint serialization

A source audit found a real concurrency gap: the scheduler's `AutoCheckin::running` guard did not cover the direct manual API route, so a manual batch could overlap a scheduled batch despite each individual batch iterating accounts serially. Added a process-wide async mutex gate at `billing::checkin::run_checkin_cancellable()`. The gate is non-queuing (`try_lock`): a competing manual API request receives 409 rather than running later as a stale duplicate batch. `AutoCheckin::fire_for()` treats this 409 as contention, clears the schedule's prewritten `lastFiredDate`, does not overwrite `lastResult`, and allows a later scheduler tick to retry. The lock spans the whole batch and releases with the future guard.

This is a targeted serialization fix, not a durable transaction/outbox solution. The existing per-account `checkinAt`, per-account `checkin_records`, batch `checkin_history`, and `autoCheckin.lastResult` remain separate writes; a process crash can still occur between upstream success and one or more local writes. That crash-consistency design needs a separate idempotent ledger/reconciliation proposal rather than pretending the current writes are atomic.

Validation: `git diff --check`, `tools/rust_balance.py`, and `tools/check_eol.py` passed. Rust compile/tests, Gradle build, and device tests were not run (toolchains unavailable).

Implementation commit: `475bacb fix(checkin): serialize all execution entrypoints`. The local branch is now 34 commits ahead of its imported baseline tracking ref; this ref is not the user's original GitHub history and no push has been performed.

## 2026-10-31 final delivery snapshot (supersedes earlier status snapshots)

- Latest HEAD: `cbde30d docs(android): record global checkin serialization`.
- Latest functional fix: `475bacb fix(checkin): serialize all execution entrypoints`.
- Branch: `architecture/kotlin-rust-core`; ahead 35 commits of the ZIP-import baseline tracking ref `origin/architecture/kotlin-rust-core`.
- Worktree clean at delivery; no GitHub push.
- ZIP / Patch / Bundle are refreshed from this HEAD. Bundle contains local imported history and is not a reconstruction of the user's original GitHub history.
- Static checks: `git diff --check`, `tools/rust_balance.py`, and `tools/check_eol.py` passed. ZIP integrity and Git Bundle full-history verification passed.
- Cargo / Rustc / Rustfmt / Gradle / Android SDK unavailable; Rust compile/tests, Android build, and device tests not run.

## 2026-10-31 follow-up: check-in crash consistency design

After the global serialization fix, the next reliability gap is not concurrent execution but crash consistency. The current `checkin_records` table is a post-result display history; it is not a durable write-ahead execution journal. A process can still exit after an upstream claim but before `checkinAt`, per-account history, batch history, or auto-check-in summary has been persisted.

Added `docs/ANDROID_CHECKIN_RUN_LEDGER_DESIGN.md` with a concrete two-table design (`checkin_runs`, `checkin_run_accounts`), states (`in_flight`, `result_saved`, `projected`, `uncertain`), ordering constraints, idempotent projection requirements, startup recovery policy, integration points, and phased implementation gates. The key safety rule is that an `in_flight` operation found after restart is **uncertain** and must not be automatically replayed. A durable local journal cannot provide exactly-once remote execution without provider-side idempotency or a reliable reconciliation endpoint.

Source findings: regular manual and scheduled check-in converge at `run_checkin_cancellable()`; the account store and check-in history share the same `Db` connection handle, but `AccountStore::mark_checkin()` has its own account-domain guard and read/modify/write path, so atomic multi-projection must be designed explicitly rather than inferred from sharing a DB. Bootstrap constructs the account store before the auto-check-in scheduler is started, allowing recovery to run before scheduling begins. The independent `run_activity` path is excluded from the first implementation slice.

This commit is design/audit only: no schema or runtime behavior changed. The next code slice should add schema v16 and tested ledger-store primitives, then integrate regular check-in and startup recovery incrementally. Rust compile/tests and Android/device crash-injection remain unverified in this environment.

## 2026-10-31 latest snapshot: durable check-in ledger design gate

- HEAD: `8aafa1b docs(checkin): design durable run ledger recovery`.
- Functional check-in serialization fix remains `475bacb fix(checkin): serialize all execution entrypoints`.
- New design artifact: `docs/ANDROID_CHECKIN_RUN_LEDGER_DESIGN.md`.
- This round changed documentation only; no schema, provider workflow, API contract, or runtime behavior changed.
- Static validation: `git diff --check`, `python3 tools/rust_balance.py`, and `python3 tools/check_eol.py` passed.
- Rust compilation/tests, Gradle/Android build, and device crash-injection were not run; toolchains are unavailable.
- Branch remains `architecture/kotlin-rust-core`; no GitHub push. The local `origin/architecture/kotlin-rust-core` ref remains the ZIP-import baseline, not the original GitHub history.

## 2026-10-31 CheckinRun 第一阶段实现

- Schema 已从 v15 升至 v16，新增 `checkin_runs` 与 `checkin_run_accounts`。
- 新增 `core::checkin_run_ledger`，提供批次创建、账号写前占位、结果快照保存、
  投影完成、uncertain 隔离和批次关闭原语。
- 状态写入采用事务 / 条件更新；相同结果与投影重复提交幂等，不同结果禁止覆盖。
- 本阶段没有改动 Provider 执行链，也没有接入启动恢复，故不会改变现有签到行为。
- 新增 Rust 单测，但 Cargo / Rust 工具链当前不可用，测试未运行；不可宣称编译通过。

下一阶段：接入 `run_checkin_cancellable()`，为每轮生成稳定 run_id，账号请求前
提交 `in_flight`，Provider 返回后先提交完整结果，再按 run_id 做历史幂等投影。
`AccountStore::mark_checkin(id, at)` 可复用台账的 `result_saved_at`，恢复时写入相同
时间戳。随后实现启动恢复；任何遗留 `in_flight` 都只能隔离为 `uncertain`，不得自动重放。

## 2026-10-31 latest snapshot: CheckinRun ledger primitives

- Latest implementation commit: `b7a7270 feat(checkin): add durable run ledger primitives`; the latest documentation snapshot is the current HEAD.
- Branch: `architecture/kotlin-rust-core`; 40 commits ahead of the local ZIP-import baseline ref `origin/architecture/kotlin-rust-core`.
- Schema v16 adds `checkin_runs` and `checkin_run_accounts`; `core::checkin_run_ledger` exposes transactional state-transition primitives.
- This commit does not yet wire the ledger into `run_checkin_cancellable()` and does not perform startup recovery. Existing provider execution behavior is unchanged.
- Added Rust unit tests for idempotent result/projection writes, immutable saved results, unresolved-account close guard, and uncertain quarantine. They have **not** been compiled or run.
- Validation passed: `git diff --check`, `python3 tools/rust_balance.py`, `python3 tools/check_eol.py`, plus a Python SQLite smoke test of the v16 DDL, CHECK constraints, and guarded state transitions.
- Cargo / Rustc / Rustfmt / Gradle / Android SDK remain unavailable; Rust tests, Android build, and device crash-injection were not run.
- Worktree clean at this snapshot. No GitHub push. The local `origin/*` ref and bundle history are based on the ZIP-imported source, not the user's original GitHub history.

Next implementation gate: add a stable execution key to `checkin_records` and a timestamp-stable projection API before wiring provider execution. Do not mark a run account `projected` until both account `checkinAt` and the idempotent history projection have been verified. Then add startup recovery before auto-check-in scheduler startup.


## 2026-10-31 CheckinRun 第二阶段：展示历史幂等投影键

- Schema 从 v16 升至 v17：`checkin_records` 新增可空 `run_id`，并建立部分唯一索引 `(run_id, account_id) WHERE run_id IS NOT NULL`。
- 新增 `checkin_records::record_row_for_run()`；原 `record_row()` 与独立活动记录仍不带 run_id，保持兼容。
- 批次结果投影使用 SQLite `ON CONFLICT ... DO NOTHING`，重复投影同一 `(run_id, account_id)` 不会产生重复展示记录。
- 新增 schema 迁移 Rust 单测；另以 Python SQLite 实际验证 v16→v17 的旧行保留、重复批次投影幂等和 NULL 路径不受唯一索引影响。
- **执行链尚未接入**：`run_checkin_cancellable()` 仍未生成 run_id / 写入 ledger / 调用 `record_row_for_run()`；启动恢复也未实现。
- 下一步必须先解决 `AccountStore::mark_checkin()` 的稳定时间戳与幂等更新，再接入 Provider 前 `in_flight` 和 Provider 返回后 `result_saved`，最后实现启动恢复。不能在账号时间与历史投影都确认前标记 `projected`。
- 静态检查通过；当前无 Cargo/Rustc/Rustfmt/Gradle/Android SDK，Rust 编译、Rust 测试、Android 构建与真机测试均未运行。


## 2026-10-31 latest snapshot: CheckinRun projection groundwork

- HEAD: `ae609c7 docs(checkin): clarify recovery projection ordering` (functional schema/API commit: `a36b607`).
- Branch: `architecture/kotlin-rust-core`, 42 commits ahead of the local ZIP-import baseline ref. No GitHub push.
- Schema v17 adds nullable `checkin_records.run_id` plus partial unique index `(run_id, account_id)` for non-null run IDs. `record_row_for_run()` now supports idempotent display-history projection.
- `run_checkin_cancellable()` is still not wired to the ledger; startup recovery is not implemented. Provider behavior remains unchanged.
- `AccountStore::mark_checkin(id, at)` accepts an explicit timestamp; use ledger `result_saved_at` for both first projection and recovery. Account timestamp and history are separate writes and must both succeed before `projected`.
- Validation passed: `git diff --check`, `python3 tools/rust_balance.py`, `python3 tools/check_eol.py`, and a Python SQLite v16→v17 migration/UPSERT smoke test. A Rust schema migration unit test was added but not run.
- Cargo/Rustc/Rustfmt/Gradle/Android SDK unavailable; no Rust compile/test, Android build, or device validation is claimed. Worktree is clean at this snapshot.


## 2026-10-31 CheckinRun 第三阶段：常规签到执行链接入

本轮功能改动：

- `AccountStore::db()` 暴露共享数据库句柄，确保执行台账与账号表使用同一 Db。
- `run_checkin_cancellable()` 在任何 Provider 请求前创建 run / account intent；Provider
  返回后先保存完整结果快照，再按稳定 `result_saved_at` 投影 `checkinAt` 与明细历史。
- `checkin_records::record_row_for_run_at()` 返回严格写入结果；历史插入失败不再被吞掉，
  也不会错误推进 `projected`。
- 启动时在 AutoCheckin 初始化前恢复 `result_saved`、隔离 `in_flight`，并关闭中断批次。
- 每轮新签到前先做本地恢复；同一北京时间自然日的 uncertain 账号不会再次触发 Provider。
- 新增 uncertain 当日隔离 / running 批次保护单测。

本轮尚未完成：批次历史与自动调度摘要的 ledger 派生、uncertain 人工核对入口、
provider-specific reconciliation 和设备崩溃注入。

验证：`git diff --check`、`tools/rust_balance.py`、`tools/check_eol.py` 与本轮
SQLite v16/v17 DDL / 状态转换 / 幂等投影冒烟测试通过。Cargo、Rustc、Rustfmt、Gradle、
Android SDK 不可用；Rust 编译 / 单测、Android 构建和真机测试均未运行。

本阶段属于常规签到可靠性链路，不代表 Kotlin/Rust 整体迁移项目已完成相同比例。

## Latest snapshot — 2026-10-31 CheckinRun execution recovery integration

- HEAD: `049f9bc docs(checkin): record execution recovery integration`.
- Functional commit: `f26abf5 feat(checkin): integrate durable run recovery`.
- Branch: `architecture/kotlin-rust-core`, 46 commits ahead of the local ZIP-import baseline ref; worktree clean; no GitHub push.
- Regular manual/scheduled check-in now writes the durable run ledger around every Provider call. Startup recovery runs before AutoCheckin initialization; the same local-only recovery is also attempted before each new check-in.
- `result_saved` recovery replays `checkinAt` with the original timestamp and keyed history projection. `in_flight` becomes `uncertain`; same-Beijing-day uncertain accounts are skipped in batch and rejected with 409 in single-account mode.
- The run account is marked `projected` only after both local projections succeed. An unclosed `running` batch blocks further Provider calls until recovery succeeds.
- Static validation passed: `git diff --check`, `tools/rust_balance.py`, `tools/check_eol.py`, plus Python SQLite v16/v17 DDL, write-ahead transition, idempotent history insert, and uncertain quarantine smoke test.
- Rust compile/unit tests, Gradle/Android build, and device crash-injection are not run because Cargo, Rustc, Rustfmt, Gradle, and Android SDK are unavailable.
- Remaining: batch `checkin_history` and auto-check-in summary convergence, separate `run_activity` ledger, uncertain-account manual reconciliation UI, provider-specific reconciliation, and real crash-injection tests.


## 2026-10-31 CheckinRun 第四阶段：国际版活动入口纳入统一执行保护

本轮将 `POST /api/checkin-activity` 从旧的 best-effort 独立记账路径接入同一套
CheckinRun 可靠性协议：

- 新增可复用的 `try_acquire_checkin_gate()`，常规签到和独立活动入口共享同一把
  全局互斥门；并发活动请求立即返回 409，不排队、不与常规签到重叠。
- 活动请求调用 Provider 前创建批次并提交 `in_flight`；Provider 返回后先保存完整
  结果快照，再按稳定 `result_saved_at` 写 `checkinAt` 与活动明细，最后标记
  `projected` 并关闭批次。
- 活动明细新增严格、幂等的 `record_activity_for_run_at()`；旧兼容入口仍保留。
- 恢复时识别 `source=activity`，从结果快照中的 `mode` 恢复 `full / claim / keepalive`
  判据，避免把 keepalive 误按普通 claim 记成失败。
- 活动入口也执行恢复预检和当日 uncertain 隔离；同一账号有不确定请求时返回 409。

本轮验证：`git diff --check`、`tools/rust_balance.py`、`tools/check_eol.py` 通过。
当前环境无 Cargo / Rustc / Rustfmt，Rust 编译与单测未运行；Android 构建和真机活动流程
测试未运行。该切片尚需在具备工具链的环境执行完整编译与故障注入。

## 项目阶段总览（2026-10-31）

原架构路线图共 7 个阶段（Phase 0–6），当前不应描述为“已完成第 6 阶段”或“只剩发布”：

| 阶段 | 当前状态 | 尚需完成的验收重点 |
|---|---|---|
| Phase 0 构建基线 | 代码准备，验收未完成 | 干净环境 Rust/Android 构建、CI 实跑、APK ABI 检查 |
| Phase 1 Native Bridge | 部分实现 | 全 JNI 入口契约审计、错误码/API 版本、并发与重启测试 |
| Phase 2 RuntimeController | 第一版已接入主要入口 | 完整状态机/状态流、全部启动入口收口、Android 测试 |
| Phase 3 Service/Scheduler | 可靠性修复已做一批 | STOP/长任务/WakeLock/Doze/进程回收真机验证与观测 |
| Phase 4 Rust Core 解耦 | 正在推进 | 平台依赖清单、配置/路径/事件/shutdown 抽象、desktop 回归 |
| Phase 5 JNI vs UniFFI | 尚未开始 | 小型 spike、成本对比、ADR 决策 |
| Phase 6 可观测性与发布门禁 | 尚未开始 | 压测、泄漏检查、崩溃恢复、Release 门禁 |

因此项目当前处于 **Phase 4 的实施阶段**，但 Phase 0–3 仍有未完成的验收门槛，
不能按阶段编号简单认为前四阶段全部验收通过。按路线图仍有 Phase 4 收尾、Phase 5、
Phase 6 三个主要阶段要推进；此外还要补齐 Phase 0–3 的构建与真机验收，这些是横向
验收工作，不另算新的架构阶段。

CheckinRun 是 Phase 4 Rust Core 可靠性改造中的专项，目前已覆盖常规签到与国际版
活动入口。该专项还剩三组主要工作：

1. **摘要一致性**：将批次时间线与 AutoCheckin `lastResult` 改成从持久化 run 结果
   幂等派生，修复“批次已关闭但摘要尚未写入时进程退出”的缺口。
2. **uncertain 人工处置与 Provider 核实**：提供只读诊断和明确的人工核对/解除流程；
   只有 Provider 有可靠查询接口时才允许自动核实，不得盲目重放领取请求。
3. **验证收尾**：Rust 单测、Android 构建、模拟各持久化边界崩溃、真机恢复与长时间运行。

阶段数是路线图的工作包数量，不代表固定工期。当前最大的排期不确定性是能否恢复
Rust/Android 工具链并跑通 CI，以及设备侧崩溃注入是否暴露新问题。

## Latest snapshot — CheckinRun activity-path integration

- Functional changes: unified check-in gate is now shared by regular and manual WorkBuddy Intl activity entrypoints.
- `POST /api/checkin-activity` now writes `in_flight` before Provider, saves result JSON before local projections, writes a stable timestamped activity record with run_id, and marks `projected` only after local writes succeed.
- Startup recovery dispatches `source=activity` snapshots through the activity-specific projection function using the persisted `mode` field; this preserves keepalive outcome semantics.
- Static checks passed: `git diff --check`, `tools/rust_balance.py`, `tools/check_eol.py`.
- Rust compile/tests, Android build, and device crash-injection remain unrun due missing toolchains.

## 2026-10-31 CheckinRun 汇总时间线恢复（schema v18）

- schema v18 为 `checkin_runs` 增加 `summary_json` / `history_projected`。
- 常规批量签到将精简汇总快照与批次终态原子提交；`checkinHistory` 使用 `runId`
  幂等投影，启动 / 新签到前恢复可补写尚未完成的时间线。
- 时间线写入失败保留 SQLite pending 标记，不影响已成功的 Provider 结果；不重复保存
  每账号完整结果 JSON。
- 当前只解决签到中心 `checkinHistory`。AutoCheckin 的顶层与 schedule `lastResult`
  仍在执行返回后写入，尚未统一到同一恢复协议；下一切片应持久化 schedule 身份与
  `lastResult` 快照，再补恢复投影。
- 本轮新增 schema / 状态单测尚未由 Cargo 执行；静态与 SQLite 冒烟结果见本轮交接。


## 2026-10-31 CheckinRun 批次汇总持久化与时间线恢复（schema v18）

- `checkin_runs` 新增 `summary_json` / `history_projected`；创建批次时持久化
  `target_count` / `skipped_count`，关闭批次时与终态原子保存精简汇总。
- `checkinHistory` 新增按 `runId` 幂等投影；时间线写入成功后才置投影标记。若进程
  在批次关闭后、时间线写入前退出，下一次恢复会补写；若在时间线写入后、标记前退出，
  重复投影也不会重复产生条目。
- 若进程在批次关闭前退出，恢复从每账号 `result_json` 重建 `interrupted` 摘要；
  `in_flight` 仍先转为 `uncertain`，只显示不确定状态，不重放 Provider。
- 汇总快照只保存计数与前五条失败摘要，不重复复制每账号完整 JSON。活动和单账号
  入口不生成批量时间线摘要。
- AutoCheckin 顶层 / schedule `lastResult` 仍未纳入此快照协议；下一步需持久化
  schedule 身份与摘要，再补幂等恢复。
- SQLite v16→v18 DDL、状态转换、终态汇总保存、幂等历史索引冒烟验证通过；
  `rust_balance.py`、`check_eol.py`、`git diff --check` 通过。Cargo/Rustc/Rustfmt
  不可用，Rust 编译与单测未运行；Android 构建及真机崩溃注入未运行。

## Latest snapshot — 2026-10-31 CheckinRun durable batch summary

- HEAD: `061a478 feat(checkin): persist recoverable batch summaries`.
- Branch: `architecture/kotlin-rust-core`, 49 commits ahead of the local ZIP-import baseline ref; worktree clean after the functional commit; no GitHub push.
- Schema v18 adds `checkin_runs.summary_json` and `history_projected`. Batch plan counts are written at run creation; normal batch closure and compact summary snapshot commit in one SQLite transaction.
- `checkinHistory` projection now includes stable `runId` and is idempotent. Startup / pre-check-in recovery replays pending summaries and marks projection only after config persistence succeeds.
- If a process dies before batch closure, recovery rebuilds an interrupted summary from per-account saved result JSON and stored plan counts. `in_flight` is quarantined as `uncertain` first; no Provider is replayed.
- Summary snapshot stores only aggregate counters and up to five failure strings; it does not duplicate all account result JSON. Single-account and activity sources do not create batch timeline entries.
- Validation: `git diff --check`, `tools/rust_balance.py`, `tools/check_eol.py`, and Python SQLite v16→v18 migration / state / summary / idempotency smoke test passed.
- Rust compile and Rust unit tests remain unrun (Cargo/Rustc/Rustfmt unavailable); Gradle/Android build and device crash-injection remain unrun.
- Remaining CheckinRun work: persist schedule identity and AutoCheckin `lastResult` in the same durable summary protocol; add uncertain-account diagnostics/manual reconciliation; run full Rust tests and device crash-injection.

## 2026-10-31 CheckinRun 第七阶段：AutoCheckin 摘要恢复（schema v19）

本轮完成 AutoCheckin 摘要持久化投影的恢复协议：

- schema v19 在 `checkin_runs` 增加 `schedule_id`、`auto_checkin`、
  `auto_result_projected`，创建批次时保存定时条目身份和触发来源类型。
- `run_checkin_cancellable()` 从 AutoCheckin 入口接收 schedule ID；普通手动 API
  和独立活动批次不会误进入 AutoCheckin 摘要投影。
- 新增 `project_pending_auto_results()`：从已关闭批次 `summary_json` 恢复顶层
  `autoCheckin.lastResult` 与条目级 `lastResult`；两处配置持久化均成功后才标记完成。
- schedule 被删除时不重建条目；顶层兼容摘要仍可恢复。
- 配置写入失败会保留 `auto_result_projected = 0`，下次启动或签到前继续重放本地写入。
- `recover_startup()` 和 AutoCheckin 正常成功路径调用同一投影器。

验证：`git diff --check`、`tools/rust_balance.py`、修改文件的 `tools/rust_depth.py`、
`tools/check_eol.py` 及 Python SQLite v16→v19 迁移/元数据冒烟通过。Cargo、Rustc、
Rustfmt、Gradle 和 Android SDK 不可用，Rust 编译/单测、Android 构建、真机和 config
写盘故障注入均未执行。当前工作区快照应以本轮最终提交和交付包为准。

## Latest snapshot — 2026-10-31 AutoCheckin result recovery

- Functional commit: `7951344 feat(checkin): recover auto-checkin result projections`.
- Documentation commit: `b97bf0d docs(checkin): record auto-result recovery projection`.
- HEAD: `b97bf0d`; branch `architecture/kotlin-rust-core`; ahead 52 commits of the local ZIP-import baseline; worktree clean after snapshot commit.
- Schema v19 now records AutoCheckin origin, schedule ID, and result-projection completion. The recovery projection writes top-level and schedule `lastResult` from the durable batch summary and only clears its pending marker after checked config persistence succeeds.
- Validation: `git diff --check`, `tools/rust_balance.py`, `tools/rust_depth.py` for all changed Rust files, `tools/check_eol.py`, source-extracted SQLite V16→V19 DDL, and the `checkin_runs` create/pending/projection SQL smoke test passed.
- Not run: Cargo/Rustc/Rustfmt are unavailable, so Rust compilation/unit tests are not verified. Gradle/Android SDK and device tests remain unavailable. Config persistence fault injection remains outstanding.
- No GitHub push has been performed. Bundle history is based on the local ZIP-import repository, not the original remote Git history.


## 2026-10-31 CheckinRun uncertain 诊断快照

- 新增受保护只读接口 `GET /api/checkin-runs/uncertain?limit=&offset=`。
- 返回待核对记录的 run/account/provider/source/reason/时间与隔离原因；不暴露凭证、请求体或结果 JSON。
- 不提供手动清除、强制成功/失败或 Provider 重放接口；后续必须基于 Provider 专属可靠证据设计核对状态转换。
- 存储层和 API 层均限制分页上限为 500；新增查询单测。
- 修复批次终态校验中重复判断 `RUN_PARTIAL` 的冗余条件。
- 当前新增 Rust 单测未运行；静态检查通过；Rust/Android 构建与真机验证仍待具备工具链后执行。

## 2026-10-31 CheckinRun Provider 对账能力审计

- 新增 `docs/ANDROID_CHECKIN_PROVIDER_RECONCILIATION_MATRIX.md`，基于当前源码审查 WorkBuddy 国内/国际版、Qoder 两地区、Trae、AutoClaw、Raccoon、KukuAI、OfficeAce、Loomy、CodeArts。
- Qoder 是首个优先候选：只读 campaign 列表具备 `CLAIMED` 状态和 `startAt/endAt` 窗口，可设计严格正向证据；国际版必须复用 uid + 风控头重拉。`CLAIMABLE` 不能直接证明先前请求未生效。
- WorkBuddy 两地区状态接口作为第二批候选。Trae 的 `did_checked_in` 依赖每轮随机 device id，当前未持久化；Raccoon/OfficeAce 余额证据可能混入其他奖励；CodeArts `CONFIRMED` 缺少确认日期；其余暂不满足严格自动对账条件。
- 本轮只增加审计文档，不改 Provider 写行为，不提供自动重试或清除 uncertain。对账器首版应只产生 `confirmed/unknown`，不把“未查到”解释成“未执行”。
- 本轮验证：`git diff --check`；尚未运行 Rust 编译或单测（工具链缺失）。

## Latest snapshot — 2026-10-31 Provider reconciliation audit

- Documentation commit: `b4b3584 docs(checkin): audit provider reconciliation evidence`.
- HEAD: `b4b3584`; branch `architecture/kotlin-rust-core`; 56 commits ahead of the local ZIP-import baseline; worktree clean after this documentation snapshot.
- Added provider-by-provider capability matrix with evidence semantics and blockers. Qoder is the first candidate for a read-only, positive-only reconciler; WorkBuddy CN/Intl are second candidates.
- No Provider write behavior changed. No automatic retry, force-clear, or manual-success endpoint was introduced.
- Validation for this docs-only slice: `git diff --check` passed. Rust compile/unit tests were not applicable to changed files and remain unavailable in this environment; Android/device tests remain unrun.
- No GitHub push. Local imported baseline is not the original GitHub history.

## 2026-10-31 CheckinRun / Qoder 对账推进快照

- 新增 Qoder 专用只读对账探针：按 `checkin_run_accounts.started_at` 匹配 campaign 活动窗口，唯一 `CLAIM_BENEFIT` + `CLAIMED` 才返回 `confirmed`。
- 新增受保护管理接口 `GET /api/checkin-runs/reconcile?runId=&accountId=`。目标记录必须仍为 uncertain；非 Qoder Provider 返回 unknown。
- 国际版探针沿用 uid → UMID 风控头 → 重拉。探针不调用 claim，不改写 uncertain，不自动重试。
- `CLAIMABLE`、查询失败、活动窗口缺失/歧义等全部返回 unknown。
- 新增纯函数测试：CLAIMED 正向确认、CLAIMABLE 保持 unknown、窗口不匹配、多个活动歧义。
- 当前证据尚未持久化，尚未实现 uncertain → projected 的事务迁移；这是下一阶段，不允许人工直接清除原始记录。
- 验证：`git diff --check`、Rust 括号/深度静态检查、行尾检查通过；Cargo/Rustc/Rustfmt 不可用，Rust 单测/编译、Gradle/Android 构建与真机验证均未运行。
- 本轮功能提交：`052cd7a feat(checkin): add read-only Qoder reconciliation probe`；工作区提交后保持干净。当前分支相对导入 ZIP 基线领先 58 个提交；该基线不是原 GitHub 历史，本轮未推送 GitHub。

## Latest snapshot — 2026-10-31 Qoder reconciliation evidence persistence

- Schema v20 adds append-only `checkin_reconciliation_evidence`, plus `reconciled_at` / `reconciliation_evidence_id` on account execution rows.
- The original execution state remains `uncertain`; resolution is a separate audit overlay. Only a complete Qoder CN/Intl `CLAIMED` campaign evidence record with campaign ID, valid business window covering the persisted account-start timestamp, and observation timestamp can release same-day isolation; the storage layer independently revalidates this window.
- Evidence insert and resolution marker update are atomic. SQLite triggers reject ordinary evidence UPDATE/DELETE (an application-level append-only guard, not cryptographic tamper-proof storage). Unknown observations are retained but do not release isolation. Later observations cannot overwrite the first successful resolution evidence.
- Reconciliation is now `POST /api/checkin-runs/reconcile?runId=&accountId=` because it performs a local evidence write; `GET /api/checkin-runs/evidence?runId=&accountId=&limit=&offset=` exposes the paginated immutable audit trail.
- Added tests for unknown retention, confirmed resolution, append-only enforcement, and malformed confirmed evidence rejection.
- Validation still requires toolchain: Rust compilation/unit tests, Android build, and device crash/concurrency tests have not been run. No GitHub push.

- Functional commit: `c52f171 feat(checkin): persist Qoder reconciliation evidence`.
- Schema v20 was validated with an independent Python SQLite smoke covering migration DDL, unknown evidence retention, transactionally setting the resolution marker, append-only triggers, and protection against replacing the first resolution evidence.
- Static checks passed: `git diff --check`, `tools/rust_balance.py`, and `tools/check_eol.py`. `tools/rust_depth.py` was also run on all modified Rust files.
- Not run: Rust compilation/unit tests (Cargo/Rustc/Rustfmt unavailable), Gradle/Android build, and device tests. The SQLite smoke does not substitute for Rust tests.

- Documentation commit: `ed83653 docs(checkin): document durable reconciliation evidence`.
- Final snapshot for this slice: HEAD is the delivery-snapshot commit immediately following `ed83653`; branch `architecture/kotlin-rust-core`; 62 commits ahead of the local ZIP-import baseline; worktree clean. ZIP integrity passed; Git Bundle verification passed and an independent clone reproduced the exact HEAD. Patch contains 62 commits from the imported baseline.
- No GitHub push was performed. The imported baseline refs are not the original GitHub repository history.


## 2026-10-31 — Agent2Providers 来源审计与 WorkBuddy 对账推进

- 用户上传 `Agent2Providers-main.zip`，归档根提交标识 `a5697f1ca9ac7188964914c21349dc8e62fd2940`。已解压并审查 Qoder、WorkBuddy、Trae、AutoClaw、CodeArts、KukuAI、Loomy、ChatGLM、OfficeAce、Raccoon 等 Provider 签到实现。
- 该来源仓库包含 Provider SDK/协议层和多家 Provider 业务实现；Agate 当前已有 18 个同名 Provider，来源仓库还额外包含 AtomCode、LobsterAI、ModelScope 等实现，可供后续差异审计。
- Qoder 的现有 Agate 对账逻辑与来源项目协议一致；来源项目明确国际版必须先取 uid，再附加 UMID 风控头重拉活动列表。
- 新增 WorkBuddy CN/Intl 正向对账：复用现有 `BillingService::get_checkin_status(Some(session))`；CN 要求 `checkedIn=true`，Intl 要求 `statusAvailable=true && todayCheckedIn=true`；运行日与观察日必须同属北京时间自然日。
- 新增 WorkBuddy evidence 契约与 SQLite 事务校验；false、缺字段、查询失败、跨日均保持 unknown。状态确认只表示该账号当日业务完成，不声称由本次请求造成。
- 新增 Rust 单测但尚未执行（Cargo/Rustc/Rustfmt 缺失）。`git diff --check` 与可用静态检查将随本轮提交记录。Android/Gradle/真机测试未执行。
- 来源仓库声明 AGPL-3.0。本轮未直接复制其源文件或 SDK；后续如移植/改编代码，必须先做许可兼容审查。

## Latest snapshot — 2026-10-31 Agent2Providers source audit + WorkBuddy reconciliation

- User source archive `Agent2Providers-main.zip` inspected; archive root commit `a5697f1ca9ac7188964914c21349dc8e62fd2940`, license declared AGPL-3.0.
- Added `docs/AGENT2PROVIDERS_SOURCE_AUDIT.md` with source architecture, provider overlap, Qoder/WorkBuddy/Trae/AutoClaw/CodeArts/KukuAI/Loomy/ChatGLM/OfficeAce/Raccoon findings, and licensing constraints.
- Added WorkBuddy CN/Intl read-only reconciliation using existing `BillingService::get_checkin_status(Some(account_session))`; no source SDK or provider file was copied.
- WorkBuddy positive evidence is same-Beijing-day only: CN `checkedIn=true`; Intl `statusAvailable=true && todayCheckedIn=true`. Storage transaction independently checks provider, status fields, run day, observation day, and observation ordering.
- Added tests for CN/Intl positive resolution, Intl availability gate, false/cross-day unknown, and storage rejection of forged cross-day or incomplete Intl evidence.
- Functional commit: `6b511b2 feat(checkin): reconcile WorkBuddy daily status`.
- Rust static structure / EOL checks and `git diff --check` passed. Attempted `cargo test -p agate-server checkin_run_ledger --no-fail-fast`, but Cargo is unavailable (`command not found`); Rust compile/tests remain unverified. Gradle/Android/device tests remain unrun.
- No GitHub push. Imported baseline still does not represent original GitHub history.

## Final handoff snapshot — 2026-10-31

- Current branch: `architecture/kotlin-rust-core`.
- Local imported ZIP baseline: `e5fb279` (`baseline: imported agate-android source snapshot`). At the previous handoff snapshot the branch was 86 commits ahead of that baseline; this is not a count relative to the user's original GitHub history.
- Worktree is intended to be clean after the snapshot commit. No GitHub push was performed.
- Latest functional commit: `6b511b2 feat(checkin): reconcile WorkBuddy daily status`.
- Latest source-audit documentation commit: `8445025 docs(checkin): audit Agent2Providers source`.
- Agent2Providers source archive is a separate user-provided reference snapshot (`a5697f1ca9ac7188964914c21349dc8e62fd2940`), not merged into the Agate repository.
- Validation: `git diff --check`, Rust structural/depth and EOL scripts passed. Rust compile/unit tests remain unrun because Cargo/Rustc/Rustfmt are unavailable; Gradle/Android/device tests also remain unrun.


### WorkBuddy region guard addendum

- Follow-up code review found that account lookup by ID should not assume the current account edition still matches the historical CheckinRun provider. The reconciliation API now checks `session.edition == region.id()` before querying; mismatched CN/Intl sessions return `unknown` and are not queried cross-region.
- Fix commit: `da3e63e fix(checkin): validate WorkBuddy reconciliation region`.
- This addendum snapshot commit brings the branch to 88 commits ahead of imported baseline `e5fb279`; this count is local-import history only.


## 2026-10-31：CheckinRun 对账历史投影

- Schema 升至 v21：`checkin_run_accounts.reconciliation_projected_at`、
  `checkin_records.reconciliation_evidence_id` 与唯一索引。
- Qoder / WorkBuddy 已确认的对账证据会写入独立 `reconciliation` 历史行，
  UI 标签为“恢复核实”；不伪装成实时领取响应。
- 启动恢复与对账 API 都会调用同一幂等投影函数；账号签到时间和历史时间使用
  原始执行时间，避免跨午夜对账污染业务日期；`checkinAt` 只向前更新，避免旧批次
  恢复覆盖较新的签到时间；历史行、台账完成标记按可重试顺序执行。
- 新增 evidence-bound 投影标记单测；Rust / Android 编译仍需在有工具链环境执行。
- Agent2Providers AGPL-3.0 来源只用于协议审计，未直接复制其代码。项目当前
  主要自用、不计划分发；若未来分发或对外提供网络服务，需重新评估许可义务。

## 2026-10-31：v21 对账历史投影交付快照

- 功能提交：`d56f488 feat(checkin): project reconciled results into history`。
- 文档提交：本快照提交记录 Provider 来源审计、v21 投影设计与自用许可范围。
- Schema v21：`checkin_records.reconciliation_evidence_id`、`checkin_run_accounts.reconciliation_projected_at`。
- 对账历史投影在写入事务内重新验证 evidence_id / run_id / account_id / provider / confirmed / resolved；同一 run/account 幂等，错误证据不能替换已有历史。
- 投影时间使用原始签到执行时间；`checkinAt` 只向前更新，延迟恢复不能覆盖更新的签到事实。
- 对账 API 立即尝试投影；启动恢复补齐中断投影。历史来源显示为“恢复核实”，并暴露 evidence_id。
- 验证通过：SQLite v20→v21 DDL、证据绑定、历史 UPSERT 幂等冒烟；`git diff --check`、`rust_balance.py`、`rust_depth.py`、`check_eol.py`。
- 未验证：Rust 编译与单测（环境无 Cargo/Rustc/Rustfmt）；Gradle/Android 构建、模拟器和真机测试。
- Agent2Providers 是 AGPL-3.0 来源，但本轮只参考协议语义，没有复制其代码。用户确认项目仅个人自用、无分发计划；未来若分发或向其他用户提供网络服务，再重新评估许可义务。
- 本地 ZIP 导入基线 `e5fb279`；当前分支本轮后 90 个提交领先该基线。该计数不代表用户原始 GitHub 历史；未执行 GitHub push。

## 2026-10-31：CheckinRun Android 异常核实 UI

- 新增 `CheckinRecovery` 数据层，封装待核实列表、Provider 只读对账和证据分页查询。
- 新增 `CheckinRecoveryPage`，从签到中心「异常核实」快捷入口进入；显示执行元数据、失败原因、每次对账证据及历史投影结果。
- UI 不提供重试、强制成功或清除 uncertain 的动作；unknown 仍保持隔离。已解决记录在当前页面保留到用户刷新，便于查看本次证据。
- 路由 `TASK_RECOVERY` 已接入 `AgateNavHost`。
- 本地 `git diff --check`、Rust 结构检查和行尾检查通过。Kotlin 编译尝试未能进入 Gradle 编译：Wrapper 需要下载 Gradle 8.11.1，当前环境 DNS 不可用；裸 `kotlinc` 缺少 Android/Compose/Coroutine/JSON classpath，不能作为项目编译结果。

### 路线图完成度快照（粗估，非测试覆盖率/工期）

| 阶段 | 估算完成度 | 当前判断 | 最大剩余门槛 |
|---|---:|---|---|
| Phase 0 构建基线 | 55% | 构建/CI 配置已有准备，未验收 | 干净环境构建、CI 实跑、APK ABI 检查 |
| Phase 1 Native Bridge | 65% | 11 个 JNI 导出入口已审计，部分错误边界已修复 | 统一契约、错误码/API 版本、并发/重启测试 |
| Phase 2 RuntimeController | 65% | 第一版串行执行器与主要入口已接入 | 完整状态机/状态流、启动入口收口、Android 测试 |
| Phase 3 Service/Scheduler | 65% | 多项服务、闹钟、WakeLock 修复已落地 | Android 版本/ROM 真机、Doze、进程回收与长任务测试 |
| Phase 4 Rust Core 解耦 | 60% | 进行中；配置路径抽象与 CheckinRun 可靠性已推进 | 全量平台依赖清单、Runtime/事件/shutdown 抽象、desktop 回归 |
| Phase 5 JNI vs UniFFI | 0% | 未开始 | 小型 spike 与 ADR |
| Phase 6 可观测性/压力/发布门禁 | 10% | 只有零散日志与局部静态检查 | 统一关联 ID、压力/泄漏测试、Release 门禁 |

按七个阶段等权粗略折算，路线图整体约 **46%**。这不是阶段验收比例：Phase 0–3 均有源码实现，但尚未通过完整构建/设备验收；因此不能把“代码已写”视为阶段完成。当前主线仍在 Phase 4，Phase 5/6 尚未启动。

CheckinRun 专项另行估算：功能实现约 **85–90%**（执行台账、恢复、Qoder/WorkBuddy 证据、历史投影和基础 UI 已有）；验收验证明显落后，Rust/Android 构建、进程崩溃注入、真机跨日/并发验证仍未完成。不要把专项百分比替代整个架构迁移百分比。

## Latest snapshot — 2026-10-31 CheckinRun recovery UI delivery

- Functional commit: `1232857 feat(android): add check-in recovery diagnostics page`.
- Documentation commit: `c2e7913 docs(android): record recovery UI and roadmap progress`.
- Android 签到中心已增加「异常核实」入口与独立 Compose 页面；用户可浏览 uncertain 记录、主动触发 Qoder/WorkBuddy 只读核实、查看持久化证据。无 Provider 重试或强制解除操作。
- 路线图按阶段任务清单粗估约 46%；Phase 4 仍在实施，Phase 0–3 有未完成构建/真机验收，Phase 5/6 尚未开始。CheckinRun 功能实现约 85–90%，但编译和故障注入验收仍欠缺。
- 验证：`git diff --check`、Rust balance/depth 静态检查、EOL 检查通过；`CheckinRecovery` 纯数据层使用临时 Coroutine/JSON/CheckinOps stub 通过 kotlinc 编译（仅验证 Kotlin 语法与该层类型关系，不代表真实依赖集成）。Gradle Wrapper 下载 Gradle 8.11.1 时 DNS 失败；UI 与整个 Android 工程未完成项目编译。Rust 工具链不可用。
- 当前导入基线 `e5fb279`，分支领先 95 个提交；相对本地 `origin/architecture/kotlin-rust-core` ahead 7。以上均为 ZIP 导入历史计数，不代表原始 GitHub 历史。未推送 GitHub。

- Follow-up fix `5392d72`：对账解决后将当前卡片标为已解决并保留至用户刷新，避免立即消失导致无法查看刚生成的证据；手动刷新后以服务端待核实列表为准。

- 最终交付快照：ZIP、Patch、Bundle 均按本节所对应的最新提交树重新生成并验证。


## 2026-10-31：CheckinRun 故障窗口回归用例补强

- 在 `checkin_run_ledger.rs` 新增两组状态机测试：Provider 响应已持久化但尚未本地投影时的恢复门/不可提前关闭/首次结果时间戳稳定；以及 uncertain 隔离后拒绝迟到结果回填、拒绝直接投影、拒绝重新打开执行状态。
- 这两组测试通过直接构造 SQLite 持久化状态模拟崩溃窗口，不发起 Provider 网络请求，也不等同于真实进程 kill/restart。
- 静态验证：`git diff --check`、`rust_balance.py`、`rust_depth.py`、`check_eol.py` 通过。Rust 测试未执行（环境缺 Cargo/Rustc/Rustfmt）；Android/Gradle/真机测试同样未执行。
- 下一验收门槛：在具备 Rust 工具链的环境运行 `cargo test`；之后增加独立进程重启恢复测试，并覆盖 DB 写入/投影中断点。


## 2026-11-01：Round 3 CI-Fix8 验证与 Batch 3 并行推进

### Round 3 验证分支执行基线（与本地架构分支历史分开记录）

- 验证分支：`verify/arch-8faa5f5 @ 385cc04ed96d3d88f29b29e2c70f75f2a8416c90`。
- 第 10 轮 GitHub Actions Run `41268816939`：Rust tests + Android build 双绿；真实 x86_64 Emulator Instrumentation 2/2 通过。
- 第 9 轮失败的 `foregroundServicePreservesStartAfterStopOrderingBarrier` 在第 10 轮通过。CI-Fix8 的进程级启动意图标记修复了已观测到的旧 `onDestroy()` 清理晚于新 START 的竞态。
- 该结果只证明 R10 两条 Instrumentation 用例和当轮构建通过，不代表 Phase 1/2/3 已整体验收；更复杂的重复命令、Service 重建、进程回收、长 tick/STOP 与真机 ROM 行为仍待验证。
- R10 stage 阶段出现双实例竞争；最终 750 文件 SHA1 对账为 0 mismatch / 0 missing / 0 extra，证明该产物无损，但 stage 流程仍需进程级互斥锁。该流程问题不得归入 Android CI 门禁修复。

### Batch 3 小批量并行工作（基于 385cc04 快照）

- Phase 3（历史尝试，已被 R13 纠正）：Batch 3 曾将 `AtomicBoolean` 升级为 `GatewayIntentTracker` 代次快照，但 R11 红跑证明「捕获快照后比较相等」仍无法防止迟到的 `onDestroy()` stop。最终修复见本文末尾 R13 记录：废弃快照相等守卫，改为执行时刻检查最后意图。
- Phase 2：静态盘点 `AgateApp`、`AgateForegroundService`、`GatewayControlPage` 的生产生命周期入口，当前可见 `coreInit/start/stop/schedulerSet` 均经过 `RuntimeController`。Boot/Alarm 通过 Service 转发，不直接调用 JNI。完整状态机、进程重建与动态压力验收仍未完成。
- Phase 1：Kotlin `Gateway.kt` 的 11 个 external 声明与 Rust `jni_bridge.rs` 的 11 个 JNI 导出对应；CI 已有 JNI export 对照门禁。空 Java String 分配失败时返回 null、统一 NativeResult/API version、panic containment 与长任务释放契约仍需继续审计。
- Phase 4：Android/JNI 适配主要集中在 `jni_bridge.rs`、`lib.rs` Android cfg、`server/logging.rs` Android 日志适配和 `paths.rs` 数据目录覆盖；未发现 Rust Core 直接持有 Android Context/Service。环境变量、HOME/USERPROFILE、文件系统、网络和信号语义仍需逐项审计。
- 本批新增代码和文档尚未通过 Gradle/Android CI。当前执行环境没有 Cargo/Rustc；Gradle Wrapper 下载 Gradle 8.11.1 时 DNS 失败。只有静态检查通过，不得将本批新改动标为构建或测试通过。
- 新批次交付以 `Agate_Verification_Source_8faa5f5_CI-Fix9_Batch3` 源码快照为准；它基于 Round 10 验证快照，不代表原 `architecture/kotlin-rust-core` 分支的 Git 历史，也不包含 `.git`。


## 2026-11-01：Round 3 R11–R13 根因闭环、Phase 3 迟到 stop 修复与交接更新

### 1. 当前验证分支基线

- 验证分支：`verify/arch-8faa5f5`。
- 最新验证提交：`72ed23baca4c5458c851d82a2dfcbe694d2c6029`（短 SHA `72ed23b`）。
- GitHub Actions Run：`41293598053`。
- 结果：`rust-tests` 与 `android-build` 双绿；真实 x86_64 Emulator Instrumentation 2/2 通过；`:core-gateway:testDebugUnitTest` 通过。
- 本节只代表该提交与该轮 CI 覆盖范围，不代表 Phase 1/2/3 整体验收，也不代表真机、Doze、进程回收或多 ROM 验收完成。
- 对应源码归档：`Agate_Verification_Source_8faa5f5_72ed23b.zip` SHA256 `c0b28b87b96ce73f38197b9c5245284f01b24a069d5dbb50a8b4a99471ec5fe1`；`Agate_Verification_Source_8faa5f5_72ed23b.tar.gz` SHA256 `cb0c9b2f2d7a07d617bd7bd32c473a464cbd1e874d4c67721807a417353d2fa8`。

### 2. R11–R13 根因与修复结论

- R11（`a8ecd9d`）重跑时，`foregroundServicePreservesStartAfterStopOrderingBarrier` 失败；失败不是 Emulator 基建问题，而是旧 STOP 在新 START 已投递后，仍可能由 `onDestroy()` 兜底清理再次执行 `Gateway.stop()`。
- R12（`8c6d8d6`）只增加诊断日志，没有改变逻辑。R12 双绿不能证明守卫有效，因为当轮销毁清理分支没有执行。
- 红跑日志闭环显示：旧 STOP 的 `stopSelfResult(startId)` 先成功触发销毁；新 START 随后到达并将进程级意图更新为 shouldRun=true；新 START 命令先拉起 Gateway；旧实例的销毁清理任务随后执行，快照相等判断仍成立，最终把新 Gateway 停掉。
- R13（`72ed23b`）废弃「捕获意图快照—执行时比较快照相等」模式。`GatewayIntentTracker.mayStopNow()` 在 `RuntimeController.runSerialized` 串行区内，于实际执行 STOP 的时刻读取最新意图；只有最新意图为停止时才执行 `Gateway.stop()`。STOP 命令体与 `onDestroy()` 兜底清理共用该守卫。
- 跳过 STOP 时保留 Gateway；旧 Service 自身持有的 KeepAwake 锁仍由该实例销毁收尾释放，新 Service 如已接管则持有自己的锁句柄。
- 新增/重写 `GatewayIntentTrackerTest`：初始 idle 可停止；新 START 阻止迟到 STOP；每次接受的意图都递增 generation。
- R13 的 Instrumentation 2/2 通过，ordering 用例 40.9s；单测通过。实际日志序列为 1 UP + 1 DOWN。

### 3. 证据边界与未完成风险

- R13 双绿期间竞态窗口没有再次触发（服务未销毁、清理分支未执行）。因此不能把两次连续绿灯当作竞态被真实重现后通过的证明。
- 当前修复的主要证据是：执行时刻守卫的单元测试，以及报告对两个实际 `Gateway.stop()` 调用点的穷尽审查；Instrumentation 仍提供端到端顺序回归，但没有每轮都触发销毁竞态。
- `mayStopNow()` 必须继续在 `RuntimeController.runSerialized` 内与 `Gateway.stop()` 连续执行。任何新增的 Gateway stop 调用点都必须纳入同一守卫和串行契约；禁止在守卫外直接 stop。
- `GatewayIntentTracker` 是进程级意图，不等同于完整 RuntimeController 状态机。它不能替代 Phase 2 的 `StateFlow`/状态权威来源、Core/Gateway 双生命周期建模、取消和 Service/进程重建验收。
- R12/R13 临时诊断日志曾保留在服务路径。本次后续清理批次仅删除带快照/代次的竞态诊断输出，保留启动失败、Native 异常、签到结果、资源拒绝等业务诊断日志；该清理属于无逻辑变更，仍需单独 CI 验证。

### 4. Phase 0–6 当前状态（以原始主线为准）

| 阶段 | 最新状态 | 已有证据 | 主要剩余门槛 |
|---|---|---|---|
| Phase 0 构建基线 | 基本建立，仍非最终验收 | 多轮 GitHub Actions Rust/Android 双 Job 通过；APK/Native 检查；R13 双绿 | staging 进程互斥；干净环境可复现；依赖/ABI/Release 门禁完整性 |
| Phase 1 Native Bridge | 部分完成 | JNI 导出对照；部分错误透传；API Key override 与 Rust 测试进入 CI | 全入口错误/空值/panic/线程/释放契约；Native API 版本和统一错误码；并发与重启契约测试 |
| Phase 2 RuntimeController | 第一版已接入主要入口，未收口 | 单线程生命周期执行器、主要 Native 生命周期调用串行化；Boot/Alarm 转发到 FGS | 状态机与 StateFlow、唯一状态来源、Core/Gateway 分离、取消/过期请求、Service/进程重建测试 |
| Phase 3 Service/Scheduler | 迟到 STOP 逻辑修复已由 R13 构建/单测/仪器测试验证，阶段未完成 | R13 双绿；ordering + native lifecycle 两条 Instrumentation；mayStopNow 单测 | 临时诊断日志清理后复验；重复/快速命令矩阵；executor 关闭与拒绝；长 tick/STOP；WakeLock；Doze/重启/进程回收；真机 ROM |
| Phase 4 Rust Core 解耦 | 持续实施中 | Android API Key 与数据目录运行时覆盖已有抽象；CheckinRun 多项可靠性实现 | 全量平台依赖清单；Runtime/config/path/event/cancel/shutdown 边界；desktop/headless 构建和行为回归 |
| Phase 5 JNI vs UniFFI | 未开始 | 无 spike/ADR 证据 | 先盘点 JNI，再做最小 DTO/同步 API spike 和 ADR；不预设迁移 |
| Phase 6 可观测性/压力/发布门禁 | 局部基础 | CI 报告、局部生命周期日志和测试 | 关联 ID、耗时/状态指标、并发与长稳、FD/线程/WakeLock 泄漏、SSE、真机与 Release 清单 |

阶段完成度不能仅按 CI 绿灯或提交数量计算。R13 只提升 Phase 3 中一个高风险竞态项的证据等级，不据此把 Phase 3 标成完成，也不将整体架构迁移百分比大幅上调。此前 46% 为粗估快照，后续应按验收项逐条计数并注明证据等级，而非把它当成精确工程度量。

### 5. 下一批小批量并行计划（Batch 4）

**Batch 4A — Phase 3：临时竞态诊断日志清理（本快照实施）**

- 删除 R12/R13 用于定位特定时序的快照、generation、stopSelfResult 诊断日志和仅供日志使用的快照变量。
- 保留真实运行故障日志、签到结果日志、Native 异常日志和资源拒绝日志。
- 不改变 `mayStopNow()`、`RuntimeController.runSerialized`、Gateway.stop 调用顺序或 WakeLock 所有权。
- 验收：diff 证明无行为代码变化；`git diff --check`；GitHub Rust/Android 双 Job 与 Instrumentation 2/2 通过。

**Batch 4B — Phase 2：RuntimeController 状态机设计/实现切片**

- 先定义状态模型与事件来源：Core 与 Gateway 分开表达，明确 Starting/Running/Stopping/Stopped/Failed 的转换和错误原因。
- 状态必须由 RuntimeController 单一发布；Service/UI/Receiver 只能提交命令或观察状态，不各自维护“权威运行状态”。
- 先增加纯 JVM 状态转移测试，再分批接入调用方；不得在同一批同时重构所有 UI、Receiver 和 Rust Core。

**Batch 4C — Phase 1：Native Bridge 契约盘点**

- 对 12 个 JNI 导出逐项列出输入校验、返回类型、空值、错误、线程、panic containment、资源释放与调用者。
- 先形成契约表和测试缺口，不在本批贸然更改 JNI ABI。
- 下一批再选择一个低风险入口实现统一错误表达/版本能力探测。

**Batch 4D — Phase 4：Rust 平台依赖审计**

- 搜索 `std::env`、HOME/USERPROFILE、路径、时间、网络监听、后台线程/定时器、信号、Android cfg/JNI 引用。
- 按 Core / platform adapter / CLI 启动层分类，给出每项的实际调用路径和 desktop/headless 回归测试。
- 仅先做清单与风险排序，避免把跨模块重构和 Phase 1/2 混在一起。

**Batch 4E — Phase 0：staging 互斥设计**

- 第 10 轮记录的双实例 stage 竞争仍是工具链流程风险；事后全量 SHA 对账不能替代互斥。
- 先审查实际 stage 脚本/工作流，确定互斥锁覆盖快照解码、复制、校验、打包的完整临界区，再独立实施。
- 不与 Android Service 逻辑修改放在同一提交。

### 6. 执行纪律

- 继续使用 `verify/arch-8faa5f5` 验证分支；不动 main、不合并。
- 每个 Batch 都基于用户最近上传并由 GitHub 验证的源码快照；不把本地导入 ZIP 的提交计数冒充 GitHub 历史。
- 代码变更、文档更新、静态检查、Gradle/Rust 构建、Instrumentation、真机验证分别记录；没有实际执行的检查必须标为未执行。
- 每批小范围独立交付；文档更新必须同步写入本文件的阶段状态、验证证据、未完成项和下一步，不改变 Phase 0–6 的原始目标。


## 2026-11-01：R14 Batch 4A 收官与 Batch 4B/4C/4D 源码审计

### 1. 最新验证基线

- 分支：`verify/arch-8faa5f5`；R14 HEAD：`53758d9`。
- GitHub Actions Run：`41305086716`；`rust-tests` 与 `android-build` 双 Job 成功。
- Android Instrumentation：2/2 通过，0 failed；`foregroundServicePreservesStartAfterStopOrderingBarrier` 与 `nativeLibraryCoreRouterAndGatewayLifecycleWork` 均通过。
- R14 删除 Batch4A 临时诊断日志后仍通过，Batch 4A 正式关闭。`GatewayIntentTracker.mayStopNow()` 两个调用点、tracker、测试与 workflow 未改。
- 证据限制：R14 是一次 CI 观测，不能保证迟到 STOP 竞态窗口实际触发；它为 R13 的修复补充无诊断日志条件下的独立证据。
- R14 报告记录的源码归档 SHA256 前缀：ZIP `539235e3f4ed33da`，TAR.GZ `8e18110ee83f515a`；本次用户上传 tar.gz 作为源码审计输入，不含 `.git`。

### 2. Batch 4B：RuntimeController 审计结论（设计阶段，未改运行代码）

- 当前 `RuntimeController` 是单线程串行执行器，不是完整状态机；已有重入、中断等待、异常透传与异步提交。
- Core 与 Gateway 在 Rust 侧是两个独立生命周期；scheduler 又是 Core 内独立开关。Kotlin 当前没有对应的统一快照/状态发布，多个 UI/Repository/Tile 仍直接读取 `Gateway.status()`。
- `GatewayIntentTracker` 仅负责最新 START/STOP 意图防止过期 STOP，不等同于 RuntimeController 状态机，必须保留 `mayStopNow()` 在同一串行区内与 `Gateway.stop()` 连续执行的要求。
- 推荐先增加纯 Kotlin `RuntimeSnapshot` 与状态转移器、JVM 测试，再分批迁入 `coreInit/start/stop/schedulerSet/status` 具名入口；暂不整体改造 Service/UI。
- `core-gateway` 当前未显式依赖 kotlinx-coroutines；不能假设 App 的协程依赖自然成为 core 模块 API 契约。状态发布方案需先决定 listener/snapshot 或显式新增依赖。
- 详细字段、状态迁移规则、调用面与测试缺口见 `docs/BATCH4B_4C_4D_SOURCE_AUDIT_20261101.md`。

### 3. Batch 4C：JNI 入口数量更正与审计

- 更正旧计划：当前 `Gateway.kt` 有 **12 个** `private external fun`，`jni_bridge.rs` 有 12 个同名 JNI 导出；`verify-jni-exports.py` 会从 Kotlin 声明动态提取并与 `.so` 导出符号精确比较。旧文档“11 个入口”是过时计数。
- 12 个入口为 `nativeStart`、`nativeStop`、`nativeStatus`、`nativeTriggerTask`、`nativeCoreInit`、`nativeSchedulerSet`、`nativeCheckinRunNow`、`nativeCheckinRunAndWait`、`nativeRequestCheckinStop`、`nativeVersion`、`nativeSetLogCallback`、`nativeApiCall`。
- 横切风险：JNI 导出没有统一 panic containment；Release profile 为 `panic=abort`，不能承诺 `catch_unwind` 能恢复；`json_reply`/`nativeVersion` 在 Java String 分配失败时可能返回 null；Kotlin `setLogListener()` 忽略 `nativeSetLogCallback()` 返回的 JSON；错误结构与线程契约尚未统一。
- 本轮只完成源码盘点，未改 JNI ABI，也未运行 `.so` 符号核验脚本。

### 4. Batch 4D：Rust 平台依赖审计扩展

- 本轮对 `agate-server/src` 检索平台相关调用，得到 296 条匹配行（含测试/注释，非生产依赖数量）。按语义分类为 CLI 启动参数、数据目录、Provider env、桌面导入、Windows 系统命令、子内核进程管理、TCP 监听、Core 后台任务、线程与时间/临时目录。
- 新增优先风险：`sub_kernel/runtime.rs` 实际 spawn 外部二进制并调用平台杀进程命令；Android 是否支持子内核必须独立确认 ABI、执行权限、SELinux、存储与进程监督。桌面账号导入手动 API 也需检查 Android unsupported 语义。
- Tokio 定时器/provider worker/自动签到属于 Rust Core 业务能力，原则上保留 Rust；后续重点审计其启动者、取消句柄、shutdown 等待与 Gateway stop 的边界。
- 未修改 Rust 平台逻辑，未运行 Cargo/Gradle/真机验证。

### 5. Phase 0–6 状态修订（以原始主线为准）

| 阶段 | 当前状态 | 新证据/修订 | 主要剩余门槛 |
|---|---|---|---|
| Phase 0 构建基线 | 基本建立，未最终验收 | R14 双 Job；JNI export 校验步骤在 CI 通过 | staging 互斥、干净环境可复现、Release/ABI 门禁完整性 |
| Phase 1 Native Bridge | 部分完成 | 入口数量更正为 12；契约表完成源码盘点 | 统一错误/版本、null、panic 风险、回调错误反馈、每入口测试 |
| Phase 2 RuntimeController | 串行执行器 + 已验证纯 Kotlin 状态模型；状态快照开始由 RuntimeController 持有 | R16/R17 验证状态 Reducer 与 NativeStatusObserved；Batch 4B-2B-1 增加只读快照和 worker-confined 更新入口（待 CI） | 具名生命周期 API、命令/观测接线、订阅/统一观察、重建恢复、过期结果保护 |
| Phase 3 Service/Scheduler | R13 STOP 竞态修复 + R14 清理后复验已完成对应切片，阶段未完成 | R14 2/2 instrumentation；Batch4A 关闭 | 多轮竞态压力、executor 关闭/拒绝、长 TICK/STOP、WakeLock、Doze、进程回收、真机 ROM |
| Phase 4 Rust Core 解耦 | 持续实施中 | 平台依赖分类扩展，识别子内核进程管理与 Core worker 生命周期边界 | 子内核 Android 能力决策、桌面导入 unsupported、任务取消/shutdown 抽象、desktop/headless 回归 |
| Phase 5 JNI vs UniFFI | 未开始 | 先有 12 入口 JNI 契约盘点 | 选取 DTO/同步 API 做 spike 与 ADR；不预设迁移 |
| Phase 6 可观测性/压力/发布门禁 | 局部基础 | R14 CI 与 instrumentation 报告 | 长稳/多轮竞态、关联 ID、Native 耗时、线程/FD/WakeLock 泄漏、真机与 Release 门禁 |

### 6. 下一批执行顺序

1. **Batch 4B-1：** 已在 R16 由 GitHub CI 验证并关闭；新增纯 Kotlin Core/Gateway 双状态模型与 8 项状态转移单测。
2. **Batch 4B-2A：** Native `Gateway.Status` 事实到 RuntimeSnapshot 的显式 reconcile 事件与纯 JVM 测试；只建立状态对账规则，不接入调用方。**Batch 4B-2B：** RuntimeController 具名生命周期 API 与状态发布；再分批迁移 `AgateApp`、FGS、scheduler toggle，保持 `mayStopNow()` + `Gateway.stop()` 原子串行关系。
3. **Batch 4C-1：** 优先处理 `nativeSetLogCallback` 返回值被忽略的问题，并补 Kotlin 侧契约测试；其余 JNI 入口先逐个补测试，不一次性改 ABI。
4. **Batch 4D-1：** 审查 `sub_kernel` Android 能力与桌面导入 API 的平台语义；Core worker 清单另列，不与 RuntimeController 改造混批。
5. **Batch 4E：** staging 双实例互斥仍独立处理，先审查真实 stage 脚本/工作流临界区。

### 7. 执行与验证纪律

- 继续 `verify/arch-8faa5f5`；不动 `main`、不合并。
- 本轮仅更新文档与审计记录，没有 Kotlin/Rust 运行代码改动。
- 本轮未执行 Cargo、Gradle、Instrumentation 或真机测试；静态计数不是 `.so` 符号核验。
- 下一批状态模型必须先通过 GitHub CI，再接入调用方；未执行的测试不得标为通过。

## 2026-11-01：R15 Batch 4B-1（纯 Kotlin 双生命周期状态模型）

### 1. 基线与批次边界

- 基线：验证分支 `verify/arch-8faa5f5`，R15 HEAD `9182025`（用户提供 GitHub archive；归档本身不含 `.git`）。
- R15 GitHub Actions：Run `41309768641`，attempt 2；`rust-tests` + `android-build` 双绿，Instrumentation 2/2。attempt 1 在 Emulator boot 阶段 `Broken pipe (32)` / adb exit 224，未执行 app 测试；同 commit failed-job 重跑通过。该基线验证结果见第 15 轮报告。
- 本批只新增纯 Kotlin 状态模型与 JVM 单元测试，并更新文档；不改 `RuntimeController`、`Gateway`、`AgateForegroundService`、JNI、Rust、Gradle 依赖或 workflow。
- 本批没有把状态模型接入运行路径。当前 RuntimeController 仍是串行执行器；状态唯一发布、Native reconcile、UI 订阅均未实现。

### 2. 新增状态模型

新增 `core-gateway/src/main/java/dev/agate/core/RuntimeSnapshot.kt`：

- `RuntimeSnapshot` 是不可变快照，分开表达 `core`、`gateway` 与 nullable `schedulerEnabled`；`null` 明确表示尚未观测到 Native 调度器状态，不把 SharedPreferences 期望值伪装成实际值。
- Core 状态：`NotInitialized`、`Initializing`、`Ready`、`Failed(stage, code, message)`。
- Gateway 状态：`Stopped`、`Starting`、`Running(port, bindLan, version)`、`Stopping`、`Failed(stage, code, message)`。
- `revision` 仅在快照实际变化时递增；`updatedAtMillis` 由调用方传入，Reducer 不直接读取系统时钟，便于确定性测试。
- `RuntimeStateEvent` 表达命令执行过程的状态事件/结果，不直接调用 JNI，也不是命令 API。
- `RuntimeSnapshotReducer.reduce(current, event, nowMillis)` 为纯函数；非法/过期完成事件和重复状态事件不改变快照，不增加 revision。

### 3. 测试覆盖

新增 `RuntimeSnapshotReducerTest.kt`，包含 8 项测试：

1. Core 与 Gateway 生命周期独立；
2. Core 失败保留 stage/code/message 且允许重试；
3. Gateway start/stop 状态转换保留 Native 运行信息；
4. Scheduler 初始未知且独立观测；
5. 无对应进行态的乱序完成事件被忽略；
6. 重复 start/stop 事件幂等；
7. revision/timestamp 只在状态变化时更新；
8. Gateway 失败保留结构化原因。

### 4. 重要设计边界

- Reducer 不负责命令串行化、不负责发布观察者、不负责 Native `status()` reconcile，也不负责 Service/进程重建恢复。
- 乱序完成事件忽略只能保护纯状态模型；真正的异步旧结果防护仍需由后续 RuntimeController 在控制队列内实施，不能用 `revision` 取代 `GatewayIntentTracker.mayStopNow()`。
- 当前 `Gateway.status()` 仍是 Native 运行事实来源；后续应增加显式 reconcile/observed 事件，支持 Application/Service 重建后从 Native 事实恢复快照。
- Core 初始化成功不隐式将 Gateway 标记 Running；Gateway stop 不将 Core 标记 NotInitialized；scheduler 状态独立。
- 本批没有新增 StateFlow/coroutines 依赖，也没有向 RuntimeController 添加具名 API。

### 5. 本地验证与 CI 状态

- `kotlinc` 1.9.0 对 `RuntimeSnapshot.kt` 独立编译：通过。
- `./gradlew :core-gateway:testDebugUnitTest --no-daemon`：未能启动 Gradle 构建。Wrapper 需要下载 Gradle 8.11.1，但当前执行环境 DNS 无法解析 `mirrors.cloud.tencent.com`（`UnknownHostException`）；这不是编译/测试失败，也不能记为通过。
- JUnit 测试源码已新增，但本地未执行 JUnit；GitHub CI 尚待用户上传本批快照触发。
- Cargo、Android APK、Instrumentation、真机测试均未执行；本批没有 Rust/Android 运行代码改动。

### 6. 下一批

- Batch 4B-2A：定义 Native `Gateway.Status` 到 `RuntimeSnapshot` 的显式 reconcile 映射与纯 JVM 测试，先不接 UI。
- Batch 4B-2B：在 RuntimeController 控制队列中增加具名 `coreInit/start/stop/schedulerSet` 包装与状态事件更新；保持 `mayStopNow()` + `Gateway.stop()` 在同一 `runSerialized` 区间连续执行。
- Batch 4B-2C：再选择单一调用方（优先 Application 初始化或 FGS start）接入；每个入口独立 CI。
- Batch 4C-1 / 4D-1 仍独立排期，不与本批混改。

### 7. Batch 4B-1 本地实现快照补记（待 GitHub 验证）

- 在 R15 基线 `9182025` 上形成了一个未提交的工作快照：新增 `RuntimeSnapshot.kt` 与 `RuntimeSnapshotReducerTest.kt`；源码归档不含 `.git`，因此本工作快照不宣称有 Git commit SHA。
- 纯模型编译：`kotlinc` 1.9.0 独立编译通过；另用临时 JUnit API 兼容 shim 编译并反射执行新增 8 个测试，8/8 通过。该 shim 只用于本地逻辑/测试源码快速验证，**不等价于项目 Gradle/JUnit 执行**。
- Gradle `:core-gateway:testDebugUnitTest` 未启动：Gradle Wrapper 下载 8.11.1 时 DNS `UnknownHostException`。项目真实 JUnit、Android 构建、Rust tests、Instrumentation 均待 GitHub CI 验证。
- 本批新增代码未接入任何生产调用路径；Phase 2 仍未完成。GitHub CI 通过前不得将 Batch 4B-1 标为关闭。
- 下一批依赖本批 CI 结果：先做 Native `Gateway.Status` → 快照 reconcile 映射及测试，再接入 RuntimeController 具名生命周期 API；不可跳过验证直接迁移 Service/UI。


## 2026-11-02：R16 Batch 4B-1 验证收官与 Batch 4B-2A

### 1. R16 验证收官

- 验证分支：`verify/arch-8faa5f5`；HEAD `73f781a`。
- GitHub Actions Run：`41329110670`；`rust-tests` 与 `android-build` 双绿。
- `:core-gateway:compileDebugUnitTestKotlin`、`:core-gateway:testDebugUnitTest`、`:app:testDebugUnitTest` 通过；新增 8 项 RuntimeSnapshot 单测由 CI 正式编译和执行。
- Android Instrumentation 2/2 通过，0 skipped / 0 failed。
- R16 源码对账确认：新增 `RuntimeSnapshot.kt` 与 `RuntimeSnapshotReducerTest.kt`；修改两份文档；无缺失或意外代码改动。
- Batch 4B-1 正式关闭。状态模型仍未接入 RuntimeController，不代表 Phase 2 完成。
- CI 纪律：速断/取消前必须查询同 workflow 最近成功运行的 per-step timing；不得用其他项目或印象中的整轮耗时设定 Instrumentation 超时。step 运行中无法取得日志时，只轮询状态，不把正常长耗时误判为卡死。

### 2. Batch 4B-2A：Native Status 对账事件

- 基线：R16 HEAD `73f781a`（用户提供的 GitHub archive；归档无 `.git`）。
- 本批在 `RuntimeStateEvent` 增加 `NativeStatusObserved`，承载 Native `Gateway.Status` 的三个独立事实：`coreReady`、`gatewayRunning`（含 port/bindLan/version）和 `schedulerEnabled`。
- Reducer 收到该事件时以 Native 观测事实对账三个状态维度：Core 映射为 Ready/NotInitialized；Gateway 映射为 Running/Stopped；scheduler 使用 Native 布尔值。Core 与 Gateway 仍独立表达，不从 Gateway stop 推导 Core shutdown。
- 该事件是**权威观测/恢复事件**，不是生命周期命令。调用方必须在 RuntimeController 串行控制边界内、且没有未完成生命周期命令时使用；禁止将任意轮询结果与正在执行的 START/STOP 交错写入快照，否则旧观测可能覆盖 Transitional 状态。
- 新增 4 项测试：三维状态联合对账；Gateway stopped 不重置 Ready Core；Native Core/Gateway 均未就绪的映射；相同 Native 状态不增加 revision。连同原有 8 项，目标 CI 测试数为 12。
- 本批仍未把状态发布接入 RuntimeController、Application、FGS、Repository 或 UI；没有新增协程/StateFlow 依赖，没有改变 JNI ABI/Rust 代码。

### 3. 验证状态与限制

- 已完成源码级变更与文档更新。
- 当前执行环境未提供可用 `kotlinc` / Gradle 命令，Gradle Wrapper 也未在本轮执行；因此 12 项测试、Gradle 编译、Rust tests、APK、Instrumentation 均**待 GitHub CI**。
- 未运行真机测试。
- 源码归档不含 `.git`，本地工作快照不宣称 Git commit SHA。
- 下一步：上传本批快照，以 `73f781a` 为父提交触发 CI。通过后进入 4B-2B，先增加 RuntimeController 状态持有/快照读取与具名生命周期 API 的最小切片，再逐入口接入；不同时迁移所有调用方。


## 2026-11-02：R17 Batch 4B-2A 验证收官与 Batch 4B-2B-1

### 1. R17 验证收官

- 验证分支：`verify/arch-8faa5f5`；R17 HEAD `414dd67`（完整提交 `414dd6706d2f889f913068b907b3dc26aca651ab`）。
- GitHub Actions Run：`41360505254`，一次过双绿；`rust-tests` 约 2 分钟，`android-build` 约 26.6 分钟。
- `:core-gateway:compileDebugUnitTestKotlin`、`:core-gateway:testDebugUnitTest`、`:app:testDebugUnitTest` 成功；RuntimeSnapshot 相关测试源码计数 12。
- Android Instrumentation XML：2 tests、0 failures、0 errors、0 skipped；双 APK 与 `.so` 校验产物正常。
- R17 源码对账：基线 `73f781a`，756 文件；新增/删除 0，修改 4（RuntimeSnapshot、RuntimeSnapshotReducerTest、两份文档），与声明一致。
- Batch 4B-2A 正式关闭。其 NativeStatusObserved 仍是调用方责任的串行 reconcile 事件；没有接入 RuntimeController 前，不代表真实状态对账链路已经启用。

### 2. Batch 4B-2B-1：RuntimeController 快照持有与读取（本轮工作快照，待 CI）

基线：R17 HEAD `414dd67`。本批只修改 `RuntimeController.kt`、`RuntimeControllerTest.kt` 和两份交接/审计文档，不迁移 Application、FGS、scheduler toggle，不调用新的 JNI，不改 Rust/Gradle/CI。

实现内容：
- RuntimeController 新增进程内 `AtomicReference<RuntimeSnapshot>`，作为 Kotlin 运行状态快照的唯一存储位置。
- 新增线程安全只读 `snapshot()`；读取不进入生命周期队列，不阻塞控制命令。
- 新增 `internal recordStateEvent(event, nowMillis)`；只允许 RuntimeController 的生命周期 worker 调用，其他线程调用立即失败，避免多个调用方并发写状态。
- 状态更新仍由既有纯函数 `RuntimeSnapshotReducer` 完成；不在本批增加 StateFlow、协程或观察者订阅。
- `NativeStatusObserved` 的串行边界仍需调用方遵守：只有在生命周期控制队列内、没有未完成命令时才能记录。当前尚无具名 API，因此本批不主动调用该事件。

新增 3 项 RuntimeController 测试：
1. 快照可从控制队列外读取，重复读取返回同一不可变快照实例；
2. 控制 worker 外的状态写入被拒绝；
3. 在 `runSerialized` 内记录 NativeStatusObserved 后，快照 revision、时间戳和三个状态维度正确发布。

**边界：** 本批建立唯一快照存储和受控写入边界，但还没有把 `Gateway.coreInit/start/stop/schedulerSet/status` 接入，也没有 UI 订阅、Repository 统一观察、进程恢复或 stale-result token。`RuntimeController` 的具名生命周期 API 留到后续切片。

### 3. 本批验证状态

- 本地环境未提供 `kotlinc` / `gradle`，本批未执行 Kotlin 编译或 JUnit。
- 归档文件对账、源码静态审查与归档完整性检查完成；Gradle、Rust、APK、Instrumentation、真机均待 GitHub CI。
- 本批工作快照不含 `.git`，不得将其称为已提交 Git commit。
- 上传目标：`verify/arch-8faa5f5`，父提交必须是 R17 `414dd67`；不动 main、不合并。

### 4. 下一批建议

- Batch 4B-2B-2：增加 `coreInit()` 与 `refreshStatus()` 最小具名入口；`coreInit` 的开始/成功/失败事件与 Native status reconcile 必须在同一个控制队列任务内完成。
- 明确 `refreshStatus()` 仅是显式受控 reconcile，不提供任意线程轮询写入口；若存在 START/STOP 在途操作，status 观察必须排在命令之后。
- 再独立实施 start/stop/schedulerSet；STOP 的 `mayStopNow()` 与 `Gateway.stop()` 必须继续在同一个 `runSerialized` 区间内连续执行。


## 2026-11-02：R18 Batch 4B-2B-1 验证收官与 Batch 4B-2B-2 实施记录

### 1. R18 验证收官

- 验证分支：`verify/arch-8faa5f5`；R18 HEAD `238d9f39eeea1ea475db7a8516ccef10b3a51c94`（short `238d9f3`）。
- GitHub Actions Run：`41366079448`，`rust-tests` 与 `android-build` 一次过双绿。
- `:core-gateway:testDebugUnitTest`、`:app:testDebugUnitTest` 成功；`RuntimeControllerTest` 源码 `@Test` 计数 7（既有 4 + Batch 4B-2B-1 新增 3）。
- Instrumentation XML：2 tests、0 failures、0 errors、0 skipped；双 APK artifact 正常。
- R18 源码快照 756 个文件；相对 `414dd67` 基线 ADD/DEL 0、UPD 4；patch dry-run、实跑和逐 blob 对账均匹配。
- Batch 4B-2B-1 正式关闭。其快照写入尚无生产调用方，真实生命周期状态链路仍待具名 API 接入。

### 2. Batch 4B-2B-2：Core 初始化与显式状态刷新入口（当前工作快照，待 CI）

基线：R18 HEAD `238d9f3`。本批目标是建立两个最小具名入口，不迁移 Application/FGS/Repository/UI 调用方，不接入 start/stop/schedulerSet。

实现内容：
- `RuntimeController.coreInit(dataDir, port, schedulerOn)`：在一个 `runSerialized` 控制任务中记录 CoreInitializationStarted，调用 `Gateway.coreInit()`；成功和 Native `ok=false` 两条结果路径都会在同一任务中读取 `Gateway.status()` 并应用 NativeStatusObserved。成功路径随后记录 CoreInitializationSucceeded；失败路径先对账 Native 三维事实，若 Core 仍未就绪，再记录结构化 CoreInitializationFailed，最后原样返回 Native JSONObject。JNI/初始化调用抛异常时记录失败事件后原样抛出。
- `RuntimeController.refreshStatus()`：显式调用 `Gateway.status()` 并在同一个串行任务中应用 NativeStatusObserved；因此观察请求排在此前已入队的生命周期命令之后，不提供任意线程直接写快照的入口。
- `coreInitWith()` / `refreshStatusWith()` 是 internal 测试缝，允许 JVM 测试注入 Native 操作与时钟，不加载 JNI；生产 API 仍固定调用 Gateway 门面。
- Native status 映射保持单一函数：`core`→Core Ready/NotInitialized，`running`→Gateway Running/Stopped（携带 port/bindLan/version），`scheduler`→schedulerEnabled。
- `Gateway.Status.parse()` 现在显式要求 Native status envelope 的 `ok=true`；若 Rust 返回 `{ok:false,error:...}`（例如状态锁 poisoned），抛出明确异常，不再通过 `optBoolean` 默认值伪造“Core/Gateway/Scheduler 全部关闭”。
- 没有修改 Gateway JNI ABI、Rust、Gradle、CI workflow；没有新增协程/StateFlow；没有改 `GatewayIntentTracker.mayStopNow()` 与 STOP 清理区间。

### 3. 关键契约与边界

- `coreInit()` 是同步 API，调用方必须在后台线程调用；它不持有 Android Context，也不启动 Android Service。
- `coreInit` 的 Native 初始化调用、开始/成功/失败状态事件，以及成功/Native `ok=false` 两条结果路径的 Native status reconcile，均在同一个 `runSerialized` 任务内完成。status 不会插队覆盖该任务中间状态。
- `refreshStatus()` 是显式串行 reconcile，不是高频 UI 轮询 API。控制队列已有的 START/STOP 命令先执行，刷新再读取 Native 状态；后续不得从任意线程直接调用 `recordStateEvent(NativeStatusObserved)`。
- 当前本批不迁移既有 `AgateApp.onCreate()` 和 FGS `runTick()` 中的 `runSerialized { Gateway.coreInit(...) }` 调用；它们仍串行，但暂时不写入 RuntimeSnapshot。下一批单独迁移一个生产调用入口，再逐个迁移其他入口，避免本批扩大改动面。
- 当前 `refreshStatus()` 只返回 Native `Gateway.Status` 并更新 RuntimeSnapshot；它不负责 UI 订阅、轮询节奏、Repository 聚合或进程重建通知。
- 若 `coreInit` Native 返回 `ok=false`，本实现仍在同一控制任务中读取 Native status：若 Native Core 已就绪（例如 scheduler 对齐失败），快照以 Native 实际状态为准；若 Core 未就绪，则在对账后保留结构化 CoreInitializationFailed。若 status 读取本身失败，则记录初始化失败并传播 status 异常。
- 若 `Gateway.status()` 在成功或失败结果路径返回错误 envelope 或 JNI 异常，`Gateway.Status.parse()` 会抛出，不会把错误误记为 `core=false/running=false/scheduler=false`；记录对应初始化失败信息后，status 异常向调用方传播。成功路径中 CoreInitializationSucceeded 已记录，status reconcile 未完成。
- `RuntimeSnapshot` 仍是进程内 Kotlin 快照，不替代 Native 状态；revision 也不替代 `GatewayIntentTracker.mayStopNow()`。

### 4. 新增测试（目标 CI）

- `coreInitRunsAndReconcilesNativeStatusInOneSerializedTask`：验证初始化成功与 status 读取在同一串行任务按顺序发生，并验证三维快照。
- `failedCoreInitReconcilesNativeStatusAndRetainsBootstrapFailure`：验证 Native 失败结果仍执行 status reconcile，并在 Core 仍未就绪时保留结构化失败原因。
- `RuntimeSnapshotReducerTest` 新增 `coreFailureCanFollowAnAuthoritativeNotInitializedObservation`，覆盖 Native 对账后保留本次初始化失败原因。
- `refreshStatusWaitsBehindPreviouslyQueuedLifecycleWork`：用队列阻塞任务证明显式 status reconcile 不会越过此前生命周期任务。
- 原有 `RuntimeControllerTest` 7 项保留；本批目标为 10 项 `RuntimeControllerTest`。RuntimeSnapshotReducerTest 在 R18 基础上新增 1 项；全项目其他测试数量以 CI 实际报告为准。

### 5. 本批验证状态

- 已完成源码修改、静态审阅、文件范围对账与归档生成。
- 当前执行容器没有 `kotlinc` / `gradle`，本批 Kotlin 编译、JUnit 均未执行。
- Rust tests、Android APK、Instrumentation、JNI 导出脚本、真机测试均未执行；本批未修改 Rust/JNI/Gradle。
- 本工作快照不含 `.git`，不得宣称为 Git commit。必须以 R18 `238d9f3` 为父提交上传 `verify/arch-8faa5f5` 后触发 CI。
- 不动 `main`，不合并。

### 6. 下一批建议

- Batch 4B-2B-3：只迁移一个生产 Core 初始化调用方（优先 `AgateApp.onCreate()`），由 `RuntimeController.coreInit()` 接管；FGS `runTick()` 暂时保持旧调用，独立下一批迁移。
- Batch 4B-2B-4：接入 `Gateway.start()` 及其 transitional/success/failure 状态事件；保留 Service 现有命令队列和意图追踪。
- Batch 4B-2B-5：接入 stop，必须把 `mayStopNow()` 判断与 `Gateway.stop()` 保持在同一个 `runSerialized` 原子区间。
- Batch 4B-2B-6：接入 schedulerSet；确认偏好值、Native 实际值和状态快照之间的失败恢复语义。


## 2026-11-02：R19 Batch 4B-2B-2 验证收官与 Batch 4B-2B-3 实施记录

### 1. R19 验证收官

- 验证分支：`verify/arch-8faa5f5`；R19 HEAD `40801fcaf9c8e35cb61d6ce57618c6083bdd65b3`（short `40801fc`），父基线 `238d9f3`。
- GitHub Actions Run：`41387453485`，`rust-tests` 与 `android-build` 双绿。前一次推送因 `RuntimeControllerTest.kt` 缺少 `assertFalse` import 编译失败；验证方仅补入该 import 后再次运行双绿。下一批必须保留 `import org.junit.Assert.assertFalse`。
- `RuntimeControllerTest` 10 项、`RuntimeSnapshotReducerTest` 13 项；Instrumentation `NativeRuntimeInstrumentedTest` 2/2，无 failures/errors/skipped；双 APK 与 instrumentation report artifacts 正常。
- 相对 R18 `238d9f3`：756 blobs，ADD/DEL 0，UPD 7；文档、diff、patch 与关键代码文件 blob 对账一致。
- Batch 4B-2B-2 正式关闭。`coreInit()` / `refreshStatus()` 已成为 RuntimeController 具名 API，但在本批结束时尚无生产调用方；本轮 Batch 4B-2B-3 才开始迁移第一个生产入口。
- R19 报告指出 HANDOFF 正文早期方案分支记录容易与验证分支现实混淆。本次在本文标题后增加状态基准声明，明确两套历史的适用范围。

### 2. Batch 4B-2B-3：仅迁移 AgateApp.onCreate()（本轮工作快照，待 CI）

基线：R19 HEAD `40801fc`。本批只迁移 `AgateApp.onCreate()` 中的 Core 初始化调用，不迁移 FGS `runTick()`，不改 Service 生命周期调用、JNI、Rust、Gradle、CI workflow。

实现：
- 原生产调用：`RuntimeController.runSerialized { Gateway.coreInit(dataDir, port, schedulerOn) }`。
- 新生产调用：`RuntimeController.coreInit(dataDir, port, schedulerOn)`。
- 保留 `AgateApp.onCreate()` 的后台 `Thread`，因此 JNI/Core 初始化仍不在 Android 主线程执行。
- 数据目录仍为 `filesDir/agate`；port 与 schedulerOn 仍从 `GatewayPrefs` 读取；Application 异常仍由原有 catch 记录 logcat。
- 移除 `AgateApp` 对 `Gateway` 门面的直接依赖；Core 初始化及其状态事件、Native status reconcile 现在由 RuntimeController 的同一串行任务统一执行。
- `AgateForegroundService.runTick()` 仍直接调用 `RuntimeController.runSerialized { Gateway.coreInit(...) }`（旧路径），保留给下一独立批次；Instrumentation 测试中的直接 Gateway 初始化调用属于测试夹具，不是生产调用方迁移目标。
- 保留 R19 验证修复的 `import org.junit.Assert.assertFalse`，未改动 `RuntimeControllerTest`。

### 3. 本批边界与验证状态

- 仅修改 `app/src/main/java/dev/agate/android/AgateApp.kt` 与两份交接/审计文档；预期 3 个文件修改，0 新增、0 删除。
- 不新增状态订阅、StateFlow、协程、线程池或 Application 生命周期机制；不改变初始化参数、后台线程、错误处理及“Gateway 仍由用户手动启动”的产品行为。
- 本批没有迁移 FGS `runTick()`，没有迁移 start/stop/schedulerSet，没有改变 `GatewayIntentTracker.mayStopNow()`。
- 本地环境未执行 Gradle/Kotlin/Android 编译、JUnit、Rust、APK、Instrumentation 或真机测试；这些均待 GitHub CI/后续设备验证。静态源码核对不能替代构建验证。
- 源码快照不含 `.git`，不宣称为 Git commit。下一批验证基线必须是 R19 `40801fc`，上传到 `verify/arch-8faa5f5`；不动 `main`，不合并。

### 4. 下一批建议

- Batch 4B-2B-4：只迁移 FGS `runTick()` 的 Core 初始化调用到 `RuntimeController.coreInit()`；保留签到等待在生命周期队列之外，不让长签到阻塞 Core/Gateway 生命周期命令。
- 随后独立接入 Gateway `start()`、`stop()`、`schedulerSet()`；STOP 的 `mayStopNow()` 与 `Gateway.stop()` 必须继续处于同一个串行执行区间。


## 2026-11-02：R20 Batch 4B-2B-3 验证收官与 Batch 4B-2B-4 实施记录

### 1. R20 验证收官

- 验证分支：`verify/arch-8faa5f5`；R20 HEAD `44b8be8af228...`，父基线 R19 `40801fc`。
- GitHub Actions Run：`41406380716`，`rust-tests` 与 `android-build` 一次过双绿。
- 相对 R19 基线：756 blobs，ADD 0 / DEL 0 / UPD 3；仅 `AgateApp.kt` 与两份交接/审计文档修改。
- `AgateApp.onCreate()` 已通过 `RuntimeController.coreInit()` 进入统一 Core 初始化/状态对账入口；后台 Thread、初始化参数和错误日志边界保持不变。
- R20 正式关闭 Batch 4B-2B-3。CI 证明本批代码编译与工作流通过，不等同于真机后台长期运行验证。
- 更正 R19 测试计数：`RuntimeControllerTest` 为 10 项，`RuntimeSnapshotReducerTest` 实测为 13 项。该更正依据 R20 验证方对 `44b8be8` 对象中 `@Test` 的计数。

### 2. Batch 4B-2B-4：仅迁移 FGS runTick() 的 Core 初始化调用（当前工作快照，待 CI）

基线：R20 HEAD `44b8be8`。本批仅迁移 `core-gateway/src/main/java/dev/agate/core/AgateForegroundService.kt` 中 `runTick()` 的 Core 初始化调用，并更新本文及 Source Audit；不迁移 FGS start/stop/schedulerSet，不改变 Service 队列或 Rust 签到业务执行。

实现：
- 原调用：`RuntimeController.runSerialized { Gateway.coreInit(dataDir, port, schedulerOn) }`。
- 新调用：`RuntimeController.coreInit(dataDir, port, schedulerOn)`。
- 初始化参数保持不变：`filesDir/agate`、`GatewayPrefs.port(this)`、`GatewayPrefs.schedulerOn(this)`。
- `RuntimeController.coreInit()` 内部仍通过同一个 `runSerialized` 任务完成 CoreInitializationStarted、Native `Gateway.coreInit()`、成功/失败事件和 Native status reconcile；FGS 不再自行组合底层初始化调用。
- `runTick()` 原有 `try/catch(Throwable)`、失败通知、签到结果通知和错误日志保持不变。

### 3. 必须保持的 FGS / 业务边界

- WakeLock 仍在 Core 初始化前获取，并在 `finally` 中释放；不改变其覆盖整个签到轮次的语义。
- `Gateway.checkinRunAndWait()` 仍在 `RuntimeController` 生命周期队列之外执行。它是可能长时间运行的业务等待，不得放进 `runSerialized` 阻塞 Core/Gateway 生命周期命令。
- `pendingTicks.complete(startId)`、`finally` 中的 Gateway 状态读取及 `stopIfCurrentStart(startId)` 保持原顺序。
- 不改变 `GatewayIntentTracker.mayStopNow()`；不改变 STOP 命令和 `Gateway.stop()` 的原子区间；不修改 `onDestroy()` 清理。
- 不增加网关自动启动行为；Tick 仍只确保 Core 可用并执行签到，不因本批改变 Gateway running 状态。
- 本批不改 JNI ABI、Rust、Gradle、CI workflow、测试文件，不引入 StateFlow/协程。

### 4. 静态审计与验证边界

- 静态检索应确认 `AgateForegroundService.runTick()` 不再直接出现 `Gateway.coreInit()`；RuntimeController 内部 `Gateway.coreInit()` 是封装实现，Instrumentation 中的直接调用属于测试夹具。
- 本批源码修改、文档更新、基线文件对账和归档完整性检查完成后，仍须以 GitHub CI 验证 Kotlin/Gradle 编译、Rust tests、APK、Instrumentation。
- 本地未执行的编译、JUnit、Rust、APK、Instrumentation 和真机测试不得标为通过。
- 本工作快照不含 `.git`，不宣称 Git commit；下一轮上传必须以 R20 `44b8be8` 为父提交，目标分支 `verify/arch-8faa5f5`，不动 `main`、不合并。

### 5. 下一批建议

- Batch 4B-2B-5：单独接入 Gateway `start()` 的状态转换；保留现有 Service 命令队列和意图追踪。
- Batch 4B-2B-6：单独接入 Gateway `stop()`；`mayStopNow()` 判断与 `Gateway.stop()` 必须在同一个 `runSerialized` 区间内连续执行。
- Batch 4B-2B-7：单独接入 `schedulerSet()`，审计偏好值、Native 实际值和 RuntimeSnapshot 失败恢复语义。
- 完成上述切片后，再做生产调用入口总审计；不得将本批描述为 Phase 2 已完成。
