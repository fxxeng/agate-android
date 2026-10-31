# Android 签到任务生命周期与上游超时审计

日期：2026-10-31  
范围：Agate Android Kotlin + Rust 迁移分支；只审计 Android FGS 调用的 Rust AutoCheckin / Provider 执行链，不涉及 RapidOCR 项目。

## 结论

当前 `checkinRunAndWait()` 与无固定超时 WakeLock 的组合，解决了“JNI 等待超时、Rust 任务仍在后台执行、Service 却提前释放 WakeLock”的错误配对。但它也意味着：**只要 Rust 签到 Future 尚未结束，JNI 调用、Service TICK 队列和 WakeLock 都会继续等待**。

这不是单纯的 Kotlin 队列问题。`run_checkin()` 对目标账号逐个 `await checkin_for()`，没有整轮 deadline；部分 Provider 由多次请求串成一个事务/流程。不能简单在 JNI 外层增加 timeout，因为 Tokio timeout 会 drop 整个 Future，可能发生在上游写请求已经成功、后续确认/本地台账尚未完成的窗口。

当前判断：不应立即增加“整轮强制超时”或“STOP 立即 abort”。下一步应先增加**合作式停止信号 + 账号边界检查**，并把每个 Provider 的请求阶段与写入确认阶段标注清楚；当前账号正在进行的请求/领取流程自然收尾，停止信号阻止下一个账号/下一条 schedule 启动。

## 已确认的执行语义

1. `jni_bridge.rs::checkin_run_and_wait()` 在 Rust 全局 runtime 中 spawn `AutoCheckin::fire("alarm")`，通过标准 mpsc 等待真实结果。
2. `fire_for()` 用 `Inner.running` 做全局单轮互斥；`RunningGuard::drop()` 负责把 running 复位。
3. `billing::checkin::run_checkin()` 先解析目标，再 `for account in &targets` 串行调用 `checkin_for()`；每个账号结束后落 `checkinAt` / 记录台账，再进入下一个账号。
4. `AutoCheckin::stop()` 当前只停止定时轮询循环（`stopped` + abort scheduler task），注释明确不打断正在执行的一轮签到。
5. Android Service 的 STOP 位于单线程命令队列中，当前 TICK 未返回前不会执行 STOP；这避免了 Kotlin 生命周期先结束而 Native 仍在跑的错配，但 STOP 响应时间受整轮签到时长影响。
6. `checkinRunAndWait()` 的 busy 分支会轮询 `is_running()`，确保另一个入口已启动的签到也结束后再返回。轮询目前每 250ms 一次，没有超时。

## Provider 请求超时盘点（源码口径）

| Provider / 路径 | 已确认的单请求/阶段上限 | 备注 |
|---|---:|---|
| WorkBuddy 国内版 | Billing 请求 20s | `billing/mod.rs::REQUEST_TIMEOUT_MS`；账号串行执行 |
| WorkBuddy 国际版 | 活跃模型请求、读体各 20s；网页会话请求、读体各 20s | 每个模型可继续尝试下一个；因此不是整个 Provider 固定 20s |
| Raccoon | 普通积分请求 20s；账单核对 25s | `balance.rs`；签到可能包含多个读写步骤 |
| AutoClaw | 积分/订阅请求 20s | 签到后还会读活动奖励、触发 user-info、等待 1.5s 并复核；整条链明显大于单请求上限 |
| Qoder | 每次 API 请求 15s；UMID 子进程 25s | 国际版先取 uid、生成风控头、再次拉取活动；必要时刷新后再次拉取；claim 也独立请求 |
| Trae | 单次 POST 20s | 签到前状态、claim、可能的续期重试/复查，多请求串联 |
| Loomy | `send_raw` 请求 20s | `client::token_request` 显式传入 20s |
| KukuAI | 面板请求与每个任务领取 30s | 还可能先换发业务 STOKEN；LOGIN / CHAT 两个任务串行 |
| OfficeAce | 订阅查询/领取 20s | 领取前余额快照失败可容忍，随后 claim；并非单请求总 20s |
| CodeArts | 福利 GET/POST 20s | delivery → claim → confirm → 回读核实，多步写入/确认流程；不能随意中途 drop |

**重要区别：**上述时间是部分具体请求/阶段的上限，不是整家 Provider 或整轮签到上限。`egress::base_builder()` 的 `read_timeout` 是 headers 与 stream idle 两个配置值的较大者；配置默认值为 300s。它是传输层每次读取的后备超时，不等价于整轮任务 deadline。各签到 Provider 是否覆盖该默认值，必须以具体请求构造点为准。

## 当前风险

### 1. 整轮签到没有总时限

账号数量越多，整轮时间越长；每个账号又可能有多次请求。即使所有单请求都有 timeout，也不能推出整轮有固定上限。

### 2. 取消可能落在多步写流程中间

CodeArts 的 delivery/claim/confirm/verify、Trae 的状态/领取/确认、AutoClaw 的签到后活动奖励触发，都存在多个阶段。对整个 `run_checkin()` 使用 `tokio::time::timeout()` 会 drop 当前 Provider Future；如果远端已经接受写请求，客户端取消并不能回滚远端副作用。

### 3. Service STOP 可能等待很久

STOP 在 TICK 后排队，且 TICK 通过同步 JNI 等待 Rust Future。若某个 Provider 的阶段请求迟迟不结束，STOP 和 Service orderly destroy 都会等待。当前设计优先保证操作不被粗暴中断，代价是停止延迟。

### 4. `checkinRunAndWait()` 没有独立 watchdog

这避免了假装任务结束，但如果 Tokio task 卡死或 Provider Future 永不返回，JNI 调用与 WakeLock 可能长期占用。简单给 JNI 加 timeout 并释放锁是错误修复，因为 Rust 任务仍可能继续运行。

## 建议的安全演进顺序

### 阶段 A：先引入合作式停止（不取消当前账号）

- 在 `AutoCheckin` 运行轮次开始时创建独立的 run cancellation state（不要复用现有 `stopped`；它只表示 scheduler loop 停止）。
- Android STOP 收到时，先通过一个**不排队等待 TICK 的轻量 JNI 信号入口**设置 stop-request 标志，再把 STOP 命令按现有队列执行。
- `run_checkin()` 在账号循环的每个账号开始前检查 stop-request；若已请求停止，则不启动下一个账号，保留已完成账号结果并写入明确的 `cancelled` / `remaining` 状态。
- 当前账号的 Provider Future 不 abort，允许当前多步请求/确认链自然收尾。
- `AutoCheckin::fire_for()` 仍由 `RunningGuard` 统一收尾，确保 running 复位。
- scheduler tick 在每条 schedule 的边界也检查停止信号，避免停止后启动下一条 schedule。

### 阶段 B：逐家明确阶段边界

- 将每个 Provider 的“只读准备”“远端写入”“远端确认”“本地落账”标记出来。
- 只有明确可安全取消的只读阶段，才考虑支持阶段级 deadline/cancel。
- 对 claim/confirm 这类写入阶段，优先等请求响应；若响应丢失，下一次执行必须依赖幂等键、上游查询或本地 operation ledger 进行对账，不能直接当作失败重发。

### 阶段 C：再做时间预算与用户可观测性

- 先采集每家 Provider 的单账号耗时分布（p50/p95/p99）、整轮账号数、阶段耗时和停止请求到实际停止的延迟。
- 再根据实测数据设置“软预算”：预算耗尽后不启动下一个账号；不强杀当前账号。
- 通知/UI 区分“停止请求已收到”“当前账号收尾中”“已停止”，不能在仅设置 flag 时显示“已停止”。
- 如果当前账号本身出现超长请求，按具体 Provider 请求超时和阶段语义处理；不允许以释放 WakeLock 掩盖仍在运行的 Rust 任务。

## 暂不实施的做法

- 不在 `nativeCheckinRunAndWait()` 外围简单套 60s/120s timeout 后返回。
- 不通过 `Thread.interrupt()` 取消同步 JNI 调用。
- 不直接 `JoinHandle::abort()` 正在执行的 `fire_for()` / `run_checkin()`。
- 不把 scheduler 的 `stopped` 标志复用成“取消正在进行的签到”标志。
- 不在当前账号执行到一半时把 `running=false`，否则新的手动/调度入口可能并发启动第二轮签到。

## 验证要求

实现阶段 A 后至少需要 Rust 测试覆盖：

1. 未请求停止：全部账号依次执行。
2. 第一个账号完成后收到停止：第二个账号不启动。
3. 当前账号执行期间收到停止：当前账号完成确认与本地落账，随后不启动下一个账号。
4. 空目标列表：正常完成，不留下 running 标志。
5. Provider 返回错误：该账号记录失败，按既有策略继续或停止（需保持原语义，除非显式 stop-request）。
6. stop-request 与新一轮 start 的代际隔离：旧轮次的停止标志不能误伤新轮次。
7. schedule 多条目：停止后不启动后续 schedule。

Android 侧至少验证：TICK 中按 STOP、STOP 后快速 START、Service 销毁时 STOP 已请求但当前账号仍在收尾、WakeLock 只在真实 Native 任务结束后释放。

## 本轮变更状态

本次为源码审计与设计约束记录，没有修改运行时代码。理由是当前 Provider 多阶段写操作的取消边界尚未全部建模，贸然增加总 timeout 或 abort 会引入签到重复领取、部分成功未确认、台账与远端状态不一致等风险。
