# Round 3 · Batch 3 架构并行审计（基于 CI-Fix8 / 385cc04）

> 本文只记录本轮源码快照中的可证事实与下一步验收项。CI-Fix8 的真实验证结果见第 10 轮报告；本地新增改动尚未经过 Gradle/Android CI。

## 基线

- 分支快照：`verify/arch-8faa5f5 @ 385cc04ed96d3d88f29b29e2c70f75f2a8416c90`
- 第 10 轮：Rust tests + Android build 双绿；真实 x86_64 Emulator 的 2/2 Instrumentation 通过。
- 本轮输入归档 SHA-256：`9e100704d0acdc72488cbfe7cb4f09327d290dbd08982a93e76e58ad6b2f2b7b`

## Batch 3A — Phase 3：Service 销毁清理代次保护

### 基线审查

`AgateForegroundService` 原实现使用进程级 `AtomicBoolean gatewayStartRequested`。它已通过 R10 的 STOP→START 测试，但布尔状态没有区分不同请求代次：旧实例捕获销毁状态后，如果随后发生新的 START/STOP，无法证明旧快照仍然有效。

### 本轮小改动

- 新增 `GatewayIntentTracker`，每个 START/STOP 都生成递增 generation，并以不可变快照保存 `shouldRun`。
- `onDestroy()` 捕获快照；在 `RuntimeController.runSerialized` 内，仅当快照仍是当前代次且状态为 stopped 时，才执行兜底 `Gateway.stop()`。
- 新的 START 或 STOP 都会使旧销毁快照失效；显式 STOP 仍由 STOP 命令自身执行。
- 新增 3 个纯 JVM 单元测试，覆盖新 START、新 STOP、重复同向意图对旧快照的失效。

### 尚待验证

- Gradle 单元测试、Android Debug/Release 构建。
- Instrumentation 原 STOP→START 用例。
- 增加 START→STOP、START→STOP→START 快速连发，以及旧实例销毁延迟的压力用例。
- 验证 Service 被系统重建（null intent）时 tracker 默认状态与恢复行为。

## Batch 3B — Phase 2：RuntimeController 入口盘点

本快照可见的 Android 生命周期变更入口：

| 入口 | 操作 | 当前路径 |
|---|---|---|
| `AgateApp.onCreate` | `Gateway.coreInit` | `RuntimeController.runSerialized` |
| `AgateForegroundService.runStart` | `Gateway.start` | `RuntimeController.runSerialized` |
| `AgateForegroundService.ACTION_STOP` | `Gateway.stop` | `RuntimeController.runSerialized` |
| `AgateForegroundService.onDestroy` | 兜底 `Gateway.stop` | `RuntimeController.runSerialized` |
| `AgateForegroundService.runTick` | `Gateway.coreInit` | `RuntimeController.runSerialized` |
| `GatewayControlPage` | `Gateway.schedulerSet` | `RuntimeController.submitSerialized` |

测试代码中的直接生命周期调用也均包在 `runSerialized` 内。静态搜索未发现上述方法在生产 Kotlin 源码中绕过 RuntimeController 的直接调用。

**判断：RuntimeController 已基本成为生命周期控制面入口；但尚不能据此标记 Phase 2 完成。** 仍需检查 Receiver/Worker/Alarm 的间接启动链、所有 Native 入口线程要求、并发压力与错误回传；此外 `submitSerialized` 的回调运行在控制线程，调用方必须自行切回 UI（当前 GatewayControlPage 已显式 post 到主线程）。

## Batch 3C — Phase 1：Native Bridge 契约初审

本快照中 Kotlin `Gateway.kt` 声明 11 个 `external` 方法；Rust `jni_bridge.rs` 可见对应 11 个 `Java_dev_agate_core_Gateway_*` 导出，CI 已运行 `verify-jni-exports.py` 并在 R10 双绿前通过。

已具备的保护：
- 字符串参数经 `read_jstring` 读取；失败走 `jni_argument_error`，而不是默默替换为空字符串。
- `apiCall` 对 method/path/body/apiKey 做显式处理；nullable body 与空 body 语义一致。
- 日志回调使用 `GlobalRef`，调用时克隆引用并处理 pending Java exception。
- Kotlin `Gateway.ensureLoaded()` 使用双重检查和锁，避免并发重复加载。

待补审计：
- `json_reply` / `nativeVersion` 在 JNI 分配 Java String 失败时返回 null；需明确 Kotlin 层如何将该边界转换为稳定错误，不能让平台类型 NPE 成为隐式契约。
- 所有 JNI 导出目前未见统一 panic containment wrapper；需结合 `panic` profile、`extern "system"` ABI 与 release 构建策略确认 panic 不会跨 JNI 边界。
- 统一 API version、错误码分类、线程约束和 release/重复调用契约。
- 对 `nativeCheckinRunAndWait` 的无限等待语义及取消/释放边界进行专项验证。

## Batch 3D — Phase 4：Rust Core 平台边界初审

本快照中 Android/JNI 代码主要集中在：
- `agate-server/src/jni_bridge.rs`（Android JNI 控制面）
- `agate-server/src/lib.rs` 的 `#[cfg(target_os = "android")]` 模块挂载
- `agate-server/src/server/logging.rs` 的 Android 日志适配
- `agate-server/src/paths.rs` 的一次性数据目录覆盖适配

桌面/headless 路径仍通过 `AGATE_PROXY_HOME` / `WORKBUDDY_PROXY_HOME` 等环境变量解析配置目录；JNI 路径通过显式目录覆盖注入。当前未发现 Rust Core 直接持有 Android Context/Service 对象。

这只是边界初审，不代表平台解耦完成。下一批应逐项检查环境变量、HOME/USERPROFILE 路径假设、进程信号、文件系统与网络接口等行为在 Android/headless 的差异，避免误把“没有 JNI import”当成完全平台无关。

## CI stage 并发风险（流程级）

R10 报告记录 stage 阶段曾出现双实例竞争，最终 750 文件 SHA1 对账为 0 mismatch / 0 missing / 0 extra。该结果证明本轮产物无损，不证明 stage 流程具备并发安全性。

下一轮外部 staging 工具必须增加**同一目标目录的互斥锁**（进程级 lockfile / OS 文件锁），并在锁内完成清理、复制、stage manifest、全量 hash 校验；锁释放后才允许 push。不得仅靠计数或事后 hash 作为并发控制替代品。

## 本轮状态

- R10 基线 CI：已验证双绿。
- 本轮新增代次保护与单测：代码已写，尚未运行 Gradle/Emulator。
- Phase 2：生产生命周期入口静态盘点基本完成，动态验收未完成。
- Phase 1：JNI 对应关系已有 CI 门禁，错误/异常/版本契约仍未完成。
- Phase 4：平台边界初审完成，系统依赖审计未完成。
