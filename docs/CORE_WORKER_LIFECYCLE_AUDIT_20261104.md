# Core Worker 生命周期审计与实施方案

- 日期：2026-11-04
- 源码基线：R33 `82e27c41e8`
- 分支：`verify/arch-8faa5f5`
- 状态：第一轮源码审计完成；生产 Worker 改造尚未开始
- 关联：`ANDROID_KOTLIN_RUST_MIGRATION_HANDOFF.md` 第 24.3 节

## 1. 结论

Agate Rust Core 不是单一 Worker。当前代码至少包含四种所有权范围：

1. **Core scope**：随 Rust Core bootstrap 创建，Gateway stop 不应终止。
2. **Gateway scope**：随 Gateway start/stop 创建和优雅关闭。
3. **Request scope**：由单次请求、登录或 Provider 会话创建，完成/取消后释放。
4. **Sidecar scope**：拥有 mihomo 子进程与其串行命令队列；应由 Core/产品能力决策驱动，不应与 Gateway HTTP listener 混为一谈。

当前实现是多种模式并存：`spawn_task` / `tokio::spawn`、`OnceLock`、丢弃的 `JoinHandle`、`watch` / `oneshot`、局部 `abort()`。不能简单统一成“全部 abort”，也不能把长任务放进 Kotlin `RuntimeController` 的生命周期串行队列。

## 2. 关键源码事实

### 2.1 Core scope

`ServerState::bootstrap` 末尾启动：
- `core::sub_kernel::spawn_worker()`；
- `core::scheduled_tasks::spawn(...)`；
- `core::providers::zcode::config_sync::spawn(...)`；
- `core::usage_query::spawn_sweeper(...)`。

这些任务不是 Gateway HTTP listener 的子任务。代码注释明确其进程/Core 生命周期与 Gateway start/stop 解耦。前三类循环主要通过 `spawn_task` 后丢弃 JoinHandle；usage_query 与 zcode config_sync 用模块级 OnceLock 防重复，但没有显式 stop 接口。

### 2.2 Sidecar worker

`core/sub_kernel/mod.rs`：
- `SENDER: OnceLock<mpsc::UnboundedSender<Command>>`；
- `spawn_worker()` 创建 unbounded channel，启动 `while let Some(command) = rx.recv().await { execute(command).await }`；
- JoinHandle 未保存；
- `send()` 忽略 `sender.send()` 失败；
- OnceLock sender 没有关闭/替换入口，因此正常生命周期中 receiver 不会因 sender drop 而结束；
- `shutdown()` 当前仅调用 `runtime::kill_child()` 并写日志；在 R33 归档中未发现其生产调用点。它不关闭 worker、不 drain/drop 队列、不等待 worker 退出，也不能阻止正在执行的 Restart 在 kill 之后继续 spawn。

`core/sub_kernel/runtime.rs`：
- 当前子进程句柄在 `static Mutex<Option<tokio::process::Child>>`；
- 当前句柄停止使用 `start_kill()` + `wait().await`；
- stale PID 清理先验证本地控制器 secret，再按平台命令杀 PID，避免 PID reuse 误伤；
- `restart_with_config()` 使用 `kill_on_drop(true)`，启动后将 Child 放入全局 Mutex；
- `shutdown()` 的进程停止与命令 worker 停止尚未组成一个原子关闭协议。

### 2.3 Runtime owner

`lib.rs::GLOBAL_RUNTIME` 是 `OnceLock<tokio::runtime::Runtime>`。`spawn_task()` 优先使用当前 Tokio Handle；无运行时上下文时转入全局 fallback Runtime。该 Runtime 没有显式 shutdown API，实际持有到进程退出。很多 `spawn_task` 调用方丢弃返回的 JoinHandle，因此调用方无法统一等待任务结束。

这不等于当前一定有运行时泄漏：进程退出会回收资源；问题是 graceful shutdown 时缺少可枚举、可等待、可观测的任务所有权。

### 2.4 已存在的可复用模式

- Gateway HTTP serve：`server/mod.rs` 用 oneshot 接收停机请求，再转成 watch 广播给多个 Axum serve；通过 `with_graceful_shutdown` 等待在途请求完成。
- AutoCheckin：保存 JoinHandle + AtomicBool；stop 先置停止标志，再 abort 循环任务。
- `task_state::RunGuard`：任务级持有 heartbeat JoinHandle，在 finish/Drop 中 abort，Drop 同时释放租约。
- Trae / AutoClaw callback server：使用 watch channel 表达关闭。
- 这些是不同 scope 的局部模式，不应机械复制为全局方案。

## 3. Worker/task 分类表

| 模块 | Scope | 当前创建/停止 | 下一步 |
|---|---|---|---|
| `sub_kernel::spawn_worker` | Sidecar | OnceLock + unbounded mpsc；无 worker stop/join | 优先改造：关闭闩、队列策略、worker JoinHandle、sidecar stop 顺序 |
| `sub_kernel::runtime::CHILD` | Sidecar | static Mutex Child；start_kill/wait；stale PID secret guard | 与 worker 关闭协议统一，不能只 kill child |
| `scheduled_tasks::spawn` | Core | 长循环，JoinHandle 丢弃 | 注入 Core cancel receiver，退出后可等待 |
| `zcode::config_sync::spawn` | Core | OnceLock + 长循环，JoinHandle 丢弃 | 同上，保留单实例语义 |
| `usage_query::spawn_sweeper` | Core | OnceLock + 长循环，JoinHandle 丢弃 | 同上 |
| AutoCheckin scheduler | Core feature | 局部 JoinHandle + stop flag + abort | 明确是否由 Core supervisor 管理，保留用户级开关 |
| `task_state::RunGuard` heartbeat | Request/task | RunGuard 持有句柄，finish/Drop abort | 保持局部 RAII，不纳入全局无差别取消 |
| Gateway main/panel Axum serve | Gateway | oneshot → watch + graceful shutdown | 保留现有模型，后续可登记 Gateway scope |
| OAuth callback servers | Request/session | watch 取消，部分 JoinHandle 管理不一致 | 统一记录 session owner 与 join timeout |
| JNI background operations | Request/Core API | 多处 `spawn_task`，有些 detached | 逐接口确定是 request 还是 Core scope，不统一强制取消 |

## 4. 推荐最小设计（待 ADR / spike）

不建议一开始引入大型通用 Actor 框架或 `tokio-util::CancellationToken` 依赖。先用现有 Tokio primitives 做一个小型、可测的 supervisor：

```text
CoreTaskSupervisor
  ├── Core scope
  │    ├── scheduled_tasks
  │    ├── usage_query
  │    └── zcode_config_sync
  ├── Gateway scope
  │    └── main/panel serve (现有 graceful shutdown)
  ├── Request scope
  │    └── login/session/provider operation (局部 owner)
  └── Sidecar scope
       ├── command worker
       └── child process
```

建议最小契约：
- 每个注册任务有稳定名称、scope、取消信号、JoinHandle 与启动时间。
- `spawn` 返回可观测的 registration/handle；注册失败要显式返回，不静默丢任务。
- shutdown 先禁止该 scope 接收新任务，再广播合作式取消。
- 按依赖顺序停止：Sidecar command intake → drain 或拒绝队列 → 停 child → worker 退出；Core 周期任务停止；Gateway serve 仍由 Gateway stop 单独处理。
- 等待采用明确 timeout；超时记录任务名和状态，再按策略 abort；禁止无日志地直接 detach。
- shutdown 幂等；重复 shutdown 返回稳定结果。
- Request scope 默认由 request/session owner 负责，不应因为 Gateway stop 或 Core 停止而误取消已定义为独立的业务操作。
- `Core shutdown` 与 `Gateway stop` 是两个不同操作。Android Activity/Service `onDestroy` 不是可靠的进程退出通知，不能直接映射为 Core shutdown。

## 5. 推荐实施顺序

### Batch 4D-2A：契约与 supervisor spike
- 设计 `TaskScope` / `TaskRegistration` / shutdown result。
- 纯 Rust 单测覆盖重复注册策略、重复 shutdown、取消传播、join timeout、任务 panic/JoinError 记录。
- 不迁移 Provider `tokio::spawn`，不改 JNI ABI。

### Batch 4D-2B：迁移低耦合 Core loops
- 优先 `zcode::config_sync` 与 `usage_query`，再迁 `scheduled_tasks`。
- 保留 OnceLock 或将其替换为 supervisor 的按 scope 去重；不能双重启动。
- 每个循环在 sleep/IO 边界检查取消信号；不要只靠 abort。

### Batch 4D-2C：Sidecar worker 关闭协议
- 用 bounded mpsc 或显式队列容量；满队列时 API/调用方可观察到 busy/rejected。
- 关闭时先关闭 command intake；明确 pending Restart/Download/Refresh 是 drain 还是 cancel。
- 对正在下载的命令复用现有合作式 cancel；对正在 restart 的命令使用 generation/shutdown guard，避免 shutdown 后继续 spawn。
- 停止 child 并 wait；最后 join worker。
- `shutdown()` 必须有实际宿主调用点，且调用方等待/记录结果。

### Batch 4D-2D：宿主接线
- Desktop/headless graceful shutdown 调用 Core shutdown。
- Android 仅在明确的 Native Core 释放/退出协议中调用；不绑定 Activity 或 FGS `onDestroy`。
- Gateway stop 仍只停 Gateway scope，不停 Core loops / Sidecar。
- 本批完成后集中验证 Rust tests、Android ABI build、Desktop/headless build 与启动/关闭回归。

## 6. 不在本轮做的事

- 不把所有 `tokio::spawn` 全部替换为 supervisor。
- 不在 Gateway stop 时停止 Core scope。
- 不把签到/Provider 网络长任务放进 Kotlin `RuntimeController` 单线程生命周期队列。
- 不依赖 Android `onDestroy` 做唯一清理保障。
- 不因 Rust 使用 `std::process::Command` 就把它一律搬到 Kotlin。
- 不宣称 Android mihomo sidecar 已支持；当前没有 Android 专用已验证资产。
