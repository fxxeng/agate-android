# Core Worker 生命周期审计与实施方案

- 日期：2026-11-04
- 当前已验证基线：R37 修复态 `c1466fedd5`（CI Run `42066887374`；短 SHA 来自第 37 轮验证反馈）
- 分支：`verify/arch-8faa5f5`
- 状态：4D-2A/4D-2B、4D-2C/headless 4D-2D、4D-3A 已完成 CI 验证；4D-3B PathProvider 为当前源码候选，待 CI
- 关联：`ANDROID_KOTLIN_RUST_MIGRATION_HANDOFF.md` 第 29 章

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
- `shutdown()` 当前仅调用 `runtime::kill_child()` 并写日志；在 R34 归档中未发现其生产调用点。它不关闭 worker、不 drain/drop 队列、不等待 worker 退出，也不能阻止正在执行的 Restart 在 kill 之后继续 spawn。

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


## 7. R34 基线上的 4D-2A/4D-2B 首轮实现候选（2026-11-05）

本轮不是外部报告驱动的 CI 专项，而是直接推进 Core 生命周期主线。新增 `agate-server/src/task_supervisor.rs`，当前职责为任务登记、按 scope 去重、取消、限时 join、超时 abort 和 panic 结果报告。`TaskScope` 已定义 Core/Gateway/Request/Sidecar 四类，但本轮仅迁移 Core loops。

迁移范围：
- `scheduled_tasks::spawn` → Core scope / `scheduled_tasks`
- `providers::zcode::config_sync::spawn` → Core scope / `zcode_config_sync`
- `usage_query::spawn_sweeper` → Core scope / `usage_query_sweeper`

迁移保留三个循环原有业务逻辑与节奏。Supervisor 的 Core shutdown API 已提供，但尚未连接宿主。这样可先形成可测试的 Rust Core task ownership，而不错误地把 Gateway stop 或 Android Service onDestroy 映射为 Core shutdown。

未迁移：`sub_kernel::spawn_worker`、mihomo `CHILD`、AutoCheckin、Gateway HTTP serve、Request/session tasks。它们有不同取消/资源语义，分别在 4D-2C/2D/4D-3 等后续批次处理。

### 本轮新增测试意图
- 活跃同名任务重复注册不产生第二份任务；
- Core shutdown 不影响 Gateway scope；
- 取消后等待任务退出；
- 重复 shutdown 稳定；
- 任务 panic 通过 shutdown report 显式返回。

### 验证限制
当前执行环境未安装 Cargo/Rustfmt；本轮没有执行 Rust 编译、Rust 单测或格式化。所有新增测试均为待 CI 验证，不得标记 PASS。Android、模拟器和真机均未在本轮执行。


## 8. R35 验证收官与 4D-2C / headless 4D-2D 当前实施候选（2026-11-05）

### 8.1 R35 已验证事实

- HEAD `9be13754c7797b90f5ac49024195b3451a116022`，基线 R34 `ba8a546d1d`。
- CI Run `42010197461`：rust-tests 与 android-build 双 Job success，无 rerun。
- Rust 746 passed / 0 failed / 11 ignored；新增 Supervisor 4 项逐名通过。
- 源码归档 768 blobs，与远端 tree 768/768 匹配。
- 4D-2A/4D-2B 正式关闭；后续代码以 R35 为唯一基线。

### 8.2 4D-2C 实施：Sidecar worker 资源所有权

当前候选在 `sub_kernel` 中把命令 worker 注册到 `TaskScope::Sidecar`，并把唯一 sender 从 OnceLock 改为可取出的 Mutex Option。关闭流程顺序固定为：

1. 取得 sender 锁后设置永久 `SHUTTING_DOWN` 闩并 take sender；与 `send()` / `spawn_worker()` 使用同一把锁，关闭前已进入临界区的投递先完成，之后的调用只能观察到 stopped。
2. 调用现有下载 cancel 标志，允许下载任务在分片边界合作式退出。
3. supervisor 发取消信号；worker 取消分支优先于命令消费，丢弃尚未开始的命令；`execute()` 入口再次检查关闭闩，防止关闭窗口内已出队的命令继续执行。
4. 在 scope 总超时内 join worker；超时则 abort，并等待最多 1 秒确认 Future Drop。
5. worker 退出/abort join grace 后调用 `runtime::stop_kernel().await`，停止并 wait 当前 Child；再调用 `cleanup_stale_kernel().await`，通过控制器 secret 校验后清理崩溃遗留进程。`run_restart` 与 `restart_with_config` 多点检查永久关闭闩，防止 worker 延迟恢复后重新 spawn。
6. 写回 Sidecar stopped 状态并记录 joined/aborted/timeout/failed 汇总。

`runtime::restart_with_config()` 也改为 `stop_kernel().await`，避免重启流程只发 kill 信号后不等待旧 Child。

注意：运行时队列仍保留 unbounded mpsc；本轮只明确关闭时丢弃 pending commands，不改变 API 的队列满错误契约。容量上限和调用方 busy 反馈应在单独设计后落地。

### 8.3 4D-2D 宿主接线范围

- 新增 `agate_server::shutdown_core(timeout)` 统一入口：停止 AutoCheckin → Sidecar shutdown → Core scope shutdown。
- headless 二进制发送 HTTP graceful shutdown 信号并等待既有 300ms grace interval 后调用该入口并记录报告。`server::start()` 当前没有返回 serve JoinHandle，因此该间隔不是在途请求已全部 join 的证明。
- 当前源码仓库没有 Desktop Tauri backend exit hook 实现；只提供统一 Rust API，需在实际含 Tauri backend 的宿主仓库接入。当前不可标记 Desktop host PASS。
- Android 不将 Activity/FGS/Gateway stop 映射为 Core shutdown；OS kill 不保证回调。未新增 JNI export，未来如需显式 Core release，先定义 RuntimeController / Native release 契约。
- AutoCheckin scheduler loop 由 `stop_global()` abort，但当前运行中的签到轮次不在 Supervisor join 清单内；这符合“真实进程退出”用途，不代表支持进程内可重启式 Core release。

### 8.4 当前候选需要 CI 验证的重点

- Sidecar cancellation 优先级与队列丢弃测试。
- Supervisor spawn/shutdown 并发竞争、timeout abort join grace 与 Future Drop 测试。
- Download / Restart / Refresh 执行中 shutdown，特别是 Child 已存入静态槽与仍在局部变量两种时序。
- `stop_kernel().await` 重启行为与原有端口释放/控制器启动路径回归。
- headless bootstrap fallback Runtime 与 host runtime await 跨 runtime JoinHandle。
- Gateway stop 不调用 Core shutdown；Android JNI ABI 仍保持 12 exports。

本执行环境未提供 Cargo/Rustfmt/Gradle，本候选未执行编译、单测、格式化、Android build、模拟器或真机测试。以上均为待 CI 项，不得提前标记 PASS。


## 9. R36 验证状态与 4D-3A ProcessAdapter 衔接（2026-11-05）

R36（`e5a00f5b4c7d627f5ffa8346b03f2d0ba2593ee8`，CI Run `42031750365`）已通过 Rust/Android 双 Job：Rust 749 passed / 11 ignored，JVM 99/99，Instrumentation 2/2。headless shutdown 与 Sidecar Child 实际退出仍只有编译证据，未有设备/进程运行证据。

R37 候选开始抽取 Sidecar 的 OS 进程差异到 `server/core/sub_kernel/process.rs::ProcessAdapter`，覆盖 command flags、PID 清理和可执行权限；Child 所有权仍留在 `runtime.rs`，生命周期仍由 R36 的 Sidecar supervisor 管理。Unix adapter 被 Android 编译目标复用仅代表 OS API 机制共用，不代表 Android mihomo 资产获得支持。

`stop_kernel()` 的 Child wait 现在有 3 秒上限，超时后依赖 `kill_on_drop(true)`。该路径必须在后续真实运行中确认子进程消失、端口释放；CI 编译/单测不能代替此证据。

下一步平台抽象优先审计路径与数据目录：保留 Desktop/headless 的 `AGATE_PROXY_HOME`、旧名 `WORKBUDDY_PROXY_HOME`、`USERPROFILE` / `HOME` 优先级；Android 的私有目录由既有 JNI/runtime 配置入口提供，不应伪造桌面 HOME。Provider 自有环境变量不纳入同一个全局环境适配器。


## 10. R37 验证收官与 Batch 4D-3B 路径能力边界（2026-11-06）

### 10.1 R37 验证口径

- 原候选 `db6f329f7b` 因 `download.rs` 将 `&File` 传入 `&Path` 参数而编译失败。
- 验证方修复为 `&binary_path` 后提交 `c1466fedd5`，CI Run `42066887374` 双 Job success。
- Rust 749 passed / 11 ignored；Kotlin/JVM 99/99；Instrumentation 2/2。
- R37 新增的 ProcessAdapter 97 行没有新增单测；Rust 测试集合与 R36 逐名相同。
- Windows `#[cfg(windows)]` ProcessAdapter 实现没有 Windows target 编译证据；`configure_std_command` 在 Linux/Android 目标产生一条新增 dead_code warning。
- `stop_kernel()` 3 秒 wait timeout / `kill_on_drop` 仍只有编译证据，没有真实 OS 进程回收证据。

### 10.2 Desktop Host 与共享 Core 的边界

Desktop Host 应拥有 Tauri、窗口/托盘/菜单、桌面 UI、桌面通知、安装包/签名/自动更新与桌面生命周期；共享 `agate-server` 保留 Gateway、Provider、Routing、Protocol、Account、Storage、Authentication、配置迁移与 Core worker。

当前 Android 仓库快照不含 `src-tauri` 或 Tauri backend。`agate_server::shutdown_core()` 仅提供统一 Core 退出 API，Desktop Host 必须在其真实退出 hook 调用；当前不能标记 Desktop hook 已接线。

### 10.3 4D-3B PathProvider 最小切片

- `paths.rs` 增加 crate-private `PathProvider` 与 `EnvironmentPathProvider`，收口配置目录覆盖和用户主目录解析。
- 优先级：Android JNI config override > `AGATE_PROXY_HOME` > `WORKBUDDY_PROXY_HOME` > `{USERPROFILE/HOME}/.agate`。
- `USERPROFILE` 优先于 `HOME`；`user_home_dir()` 只返回有效绝对路径，不回退当前工作目录。
- `.workbuddy-proxy` 旧目录派生迁入 `paths::legacy_config_dir()`；无有效 HOME 时不再把 `.` 当旧桌面目录扫描。
- Clash 配置导入和 cc-switch 数据库定位复用统一 `user_home_dir()`。
- Provider 凭证路径、Provider 专属环境变量和非 UTF-8 路径逻辑暂不机械替换，避免破坏上游 CLI 兼容。

### 10.4 本批验证状态

R38 是基于 R37 修复态的候选。当前环境未运行 Cargo/rustc/rustfmt/Gradle；Rust 编译、单测、Android ABI、Windows target、Gradle、Instrumentation、模拟器和真机均待验证。静态检查不能替代 CI。

### 10.5 后续

先集中验证 4D-3B，再推进 4D-3C 剩余平台能力边界，随后 4E 双实例互斥与资源所有权。Desktop Host 拆分单独规划，不阻塞共享 Core 主线。
