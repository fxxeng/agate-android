# Android 签到合作式停止机制

## 目标

让 Android FGS 的 `ACTION_STOP` 能及时通知正在运行的 Rust 签到轮次停止继续启动账号，同时不通过线程中断或 `JoinHandle::abort()` 杀掉当前 Provider 流程。

## 调用链

```text
Android ACTION_STOP
  ├─ AgateCheckinStop executor（独立后台线程）
  │    └─ Gateway.requestCheckinStop()
  │         └─ JNI nativeRequestCheckinStop
  │              └─ AutoCheckin.request_cancel_current()
  │                   └─ cancel_current = true
  │
  └─ Service command queue（仍保持 START/STOP/TICK 顺序）
       └─ 等当前 TICK 自然结束
            └─ checkinRunAndWait 返回
                 └─ Rust run_checkin_cancellable 在账号边界停止后续账号
```

## Rust 语义

- `Inner.stopped` 仍只负责调度循环停机；新增的 `cancel_current` 专门表示当前签到轮次停止请求，两者不混用。
- 新一轮 `fire_for` 成功取得 `running` 后重置 `cancel_current=false`。
- `request_cancel_current()` 只在 `running=true` 时置位，并返回是否接收请求。
- `run_checkin()` 保留原有签名与行为，转调 `run_checkin_cancellable(..., None)`；现有手动 API 不受影响。
- `run_checkin_cancellable()` 在每个账号开始前读取停止标志。已进入的 `checkin_for()` 不被取消，完整完成后才检查下一个账号。
- 汇总增加 `cancelled` 字段；部分结果、每账号记录与历史仍正常保存。`total` 表示本轮实际处理的账号数，`cancelled=true` 表示不是完整批次。

## Android 语义

- 紧急停止信号使用独立 `AgateCheckinStop` executor，避免被正在等待 JNI 的 `AgateServiceCommands` 队列阻塞，也避免主线程加载/调用 Native。
- STOP 仍按原有命令队列处理 Gateway 停机与 Service 生命周期；本改动不改变命令屏障与 `stopSelfResult(startId)` 语义。
- FGS 收到带 `cancelled=true` 的结果后发送“签到已停止”结果通知。
- `checkinRunAndWait()` 仍等待实际 Rust 任务退出，WakeLock 仍由 `runTick.finally` 释放；没有提前释放锁或伪装任务完成。

## 安全边界与限制

- 这是**账号边界合作式停止**，不是立即中断。当前账号请求若超时策略本身失效，停止仍需等待该 Provider 自然返回。
- 不保证 Provider 内部多阶段操作可以回滚；因此不使用 Tokio abort、线程 interrupt 或丢弃任务句柄。
- 当前取消标志挂在 `AutoCheckin` 实例上，覆盖通过该实例运行的自动/调度签到轮次；不覆盖 `api::accounts` 中独立调用 `run_checkin()` 的单独业务请求。
- 运行中请求在最后一个账号执行期间到达时，结果也会标记 `cancelled=true`，表示请求被接受且当前轮次自然收尾。

## 验证状态

已完成静态检查：`git diff --check`、`tools/rust_balance.py`、`tools/check_eol.py`，以及 Kotlin Native 声明与 Rust JNI 导出名称逐项对照（12 对，完全匹配）。

尚未完成：Cargo/Rust 编译与测试、Gradle/Android 构建、真机 STOP/TICK/多账号/Provider 多阶段回归。当前执行环境没有 `cargo`、`rustc`、`rustfmt`、`gradle`，不得将静态检查描述为构建通过。

## 补充：修复 STOP 早于 Rust running 的启动窗口（2026-10-31）

专项复查发现一个竞态：TICK 已进入 Android 命令队列，但 `runTick()` 尚未让 Rust `fire_for()` 抢到 `running` 时，STOP 的第一次 JNI 请求会返回 `requested=false`。如果只发一次请求，停止信号会丢失，排队中的 TICK 随后仍可能启动完整签到。

本轮 Android 侧修复：

- 新增 `PendingTickRegistry`，按 `startId` 记录已经成功入队、尚未退出的 TICK。
- STOP 到达时只快照当时已经入队的 TICK；STOP 之后新入队的 TICK 不会被旧 STOP 误取消。
- 独立 `AgateCheckinStop` executor 每 100ms 重试轻量 JNI 请求，直到 STOP 快照中的所有 TICK 都退出。Rust 仍只在 `running=true` 时接受请求，因此不会把一个空闲 STOP 留成影响未来签到的陈旧标志。
- TICK 在 `runTick.finally` 中按 `startId` 注销；命令入队被 executor 拒绝时也会注销，避免 STOP 线程等待一个永远不会执行的 TICK。
- 新增 `PendingTickRegistryTest`，覆盖 STOP 快照不捕获后续 TICK、多条既有 TICK 全部退出才结束、已完成 TICK 不残留。

本轮本地验证：PendingTickRegistry 单独由 kotlinc 编译通过；使用轻量 JUnit stub 执行上述 3 个测试，3/3 通过；`git diff --check`、`tools/rust_balance.py`、`tools/check_eol.py` 通过。该测试不是 Gradle/JUnit 工程测试；Android Service 完整编译和真机队列竞态回归仍待工具链/设备验证。
