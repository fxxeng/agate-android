# Round 3 · Batch 2 — Android Runtime / JNI / Service Instrumentation

## Scope

This batch adds runtime verification only. It does not change Rust business logic,
SQLite schema/migrations, Provider behavior, or production Service command semantics.

### Instrumented coverage

`app/src/androidTest/java/dev/agate/android/NativeRuntimeInstrumentedTest.kt`

1. Load the real `libagate_server.so` inside Android Runtime.
2. Call `Gateway.version()` and `Gateway.coreInit()` through JNI.
3. Repeat Core initialization and assert the Core remains ready, scheduler remains off,
   and the Gateway listener is not started implicitly.
4. Call `GET /health` through `Gateway.apiCall()` / in-memory Router while the TCP
   Gateway is stopped; require a successful JNI envelope and HTTP 200 response.
5. Start and stop the actual Gateway listener on a free loopback port.
6. Start the real `AgateForegroundService`, enqueue `STOP` immediately followed by
   `START`, and verify the new start intent survives the older `startId` stop path.
7. Stop the service and restore the temporary port/LAN preferences in `finally`;
   keep the scheduler disabled in the disposable emulator to prevent background check-in.

All runtime state is stored in the app's private sandbox on a disposable emulator.
The test explicitly disables the background scheduler; no user data or external
Provider credentials are installed or used.

### JVM coverage

`PendingTickRegistryTest` verifies that a STOP snapshot only targets TICK commands
accepted before that STOP, and that completing one TICK does not remove another.
Existing `RuntimeControllerTest` and `ServiceCommandCoalescerTest` remain enabled.

## CI ABI strategy

The production Debug/Release build remains `arm64-v8a` only. CI additionally builds
an `x86_64` Rust shared library and invokes `:app:connectedDebugAndroidTest` with
`-PagateInstrumentationAbi=x86_64` on an API 35 Google APIs emulator. This temporary
property affects only the instrumentation test build; the arm64 APK artifacts are
uploaded before the emulator build starts.

## Explicitly not claimed by this batch

- No real `ACTION_TICK` is triggered; it could execute a real account check-in.
- No Provider network request or credential is used.
- WakeLock acquisition/release under a real long-running check-in is not yet tested.
- Process kill/restart, Doze, exact-alarm delivery, and crash-recovery ledger behavior
  remain Batch 3 / device-level work.
- CI emulator success is not a substitute for validation on the user's OEM device.

## Acceptance

- Existing unit tests pass.
- `:app:connectedDebugAndroidTest` passes on API 35 x86_64 emulator.
- Normal CI Debug/Release APKs remain arm64-only and pass Batch 1 APK/JNI gates.


## Round 3 · Batch 2 diagnostic follow-up (CI-Fix4)

The first API 35 x86_64 emulator run compiled the Android test APK but the
instrumentation process exited before JUnit reported any tests (`tests=0`,
`Process crashed`). The failure is not classified as a Kotlin assertion failure.

CI-Fix4 intentionally does not change Rust, JNI, Gateway, Service, or test logic.
It adds failure-preserving diagnostics around `connectedDebugAndroidTest`:

- `logcat-all.txt`: complete emulator logcat (`-b all`, threadtime format).
- `logcat-crash.txt`: crash buffer, including native crash records when emitted.
- `emulator-getprop.txt`: emulator build/ABI properties.
- `dropbox-native-crash.txt` and `dropbox-app-crash.txt`: Android DropBox crash
  records when accessible.
- `gradle-exit-code.txt`: original Gradle test exit code.

The capture runs from an EXIT trap, so a non-zero Gradle result does not skip
log collection. The instrumentation artifact upload remains `if: always()` and
now includes this diagnostics directory.

### Diagnostic decision tree

1. `Fatal signal`, tombstone, `libagate_server.so`, or Rust panic evidence:
   investigate native initialization/ABI-specific behavior before changing code.
2. `linker`, `dlopen`, missing dependency, or ELF relocation evidence:
   compare x86_64 `DT_NEEDED`, SONAME, and symbol dependencies with the arm64 build.
3. `AndroidRuntime` Java exception:
   diagnose the first fatal exception and process owner; do not infer that
   `AgateApp`'s background initialization catch covers native signals.
4. No crash evidence:
   retain the logs and add a narrower startup probe in a subsequent batch;
   do not make speculative production-code changes.

### Acceptance for CI-Fix4

- Rust all-targets and normal arm64 APK jobs remain unchanged.
- The x86_64 instrumentation compile/run is attempted as before.
- Whether it passes or fails, the artifact contains the captured diagnostics
  whenever the emulator is available to the runner script.
- The original instrumentation exit code remains the step result.

## CI-Fix7：Emulator 冷启动稳定性门禁修复（Round 8 后）

本轮仅调整 CI Emulator 镜像与启动脚本，不修改应用、Rust、JNI、Gateway、
Service、数据库或 Instrumentation 测试业务代码。

### 变更

- `android-build.yml`：`target: google_apis` 改为 `target: default`（AOSP），
  `emulator-boot-timeout` 从 900 提高到 1200 秒。正式 arm64 Debug/Release
  构建与 ABI 策略不变。
- `run-instrumentation.sh`：启动测试前执行 `adb root`、`adb wait-for-device`，
  并检查 `id -u == 0`。
- PSI 检查改为 fail-closed：不可读、格式异常、avg10 消失均停止本轮，
  不再静默等待到 cap 后继续。
- 要求 CPU PSI `some avg10 < 20` 连续 3 次（每次间隔 5 秒），最长等待 480 秒。
  超时则失败并通过 EXIT trap 上传诊断，不启动 Gradle。
- 保留原始 Gradle/settle 退出码；诊断增加 `adb-devices.txt` 和 `emulator-id.txt`。

### 为什么本轮不加自动重试

Round 8 已明确发现 settle 门失效。先修复镜像与门禁并观察一次真实运行，
避免自动重试掩盖首次启动失败，也避免把基础设施不稳定误报成测试通过。
若 AOSP + root + PSI 门后仍出现纯瞬时系统 ANR，再单独引入一次重试。

### 验收标准

1. 日志出现 `adb root confirmed (uid=0)`。
2. `psi-cpu.txt` 不再是 Permission denied。
3. 出现 3 次连续 `CPU pressure ... quiet 1/3 ... 3/3`。
4. 只有 settle gate 成功后才启动 Gradle。
5. 若 gate 失败，诊断 artifact 仍上传，退出码非零。
6. AOSP Emulator 上 Instrumentation 真正开始运行；测试结果需单独判断，
   不以 Emulator 成功启动代替 JNI/Service 测试通过。



## Batch 3-A：ForegroundService STOP → START 销毁竞态修复（CI-Fix8）

Round 9（run `41263307363`，源码基线 `f8253a6`）首次在 AOSP x86_64
Emulator 上真实运行 Instrumentation：Native Core/Router/Gateway 用例通过；
`foregroundServicePreservesStartAfterStopOrderingBarrier` 失败，表现为新
START 已短暂启动 Gateway，随后 Gateway 又停止。CI 启动、adb root、PSI
settle 均已通过，本轮按应用时序缺陷处理，不再调整 Emulator 基建。

### 根因边界

`ACTION_STOP` 的 Gateway.stop 与同一实例命令队列中的 START 本身是串行的。
但 `onDestroy()` 另有一条无条件兜底 `Gateway.stop()`：旧实例被
`stopSelfResult(startId)` 销毁后，该清理任务可能晚于新实例的 START 执行，
从而停止新实例刚启动的 Gateway。单靠 `RuntimeController` 串行化不能表达
「这个 stop 属于哪个 Service 实例/哪次用户意图」。

### CI-Fix8 修改

- `AgateForegroundService` 增加进程级 `gatewayStartRequested` 原子标记，
  跨 Service 实例记录最近一次 START/STOP 意图。
- 收到 ACTION_START 或 START_STICKY 的 null intent 时置为 true；收到
  ACTION_STOP 时置为 false。
- `onDestroy()` 的兜底线程在真正执行时读取该标记：已有更新的 START 意图
  时跳过旧实例的 Gateway.stop；当前仍是 STOP 意图时才执行兜底 stop。
- 显式 ACTION_STOP 仍在原命令队列中同步执行 Gateway.stop；命令队列、
  RuntimeController、JNI/Rust API、WakeLock 获取/释放逻辑均不改。
- 不引入延时 sleep、重试或全局重构；保留现有 Instrumentation 测试作为
  STOP→START 回归门禁。

### 第 10 轮验收

1. Rust tests 保持全绿。
2. `nativeLibraryCoreRouterAndGatewayLifecycleWork` 通过。
3. `foregroundServicePreservesStartAfterStopOrderingBarrier` 通过，且新 START
   后 Gateway 在断言窗口内持续 running。
4. 检查 logcat：若旧实例销毁清理遇到新 START，应出现
   `跳过旧 Service 实例清理：已有新的 START 意图`；不得出现其后的旧清理
   停止新 Gateway。
5. 正式 Debug/Release 仍只打包 arm64-v8a；x86_64 仅用于 Instrumentation。
6. 若仍失败，保留 per-test logcat 与 service lifecycle 日志，再根据实际顺序
   继续定位；不先调整 CI settle 参数。
