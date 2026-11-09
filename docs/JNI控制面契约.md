# JNI 控制面契约（v1 · R46 定稿）

> 本文是 Kotlin 门面（`core-gateway/src/main/java/dev/agate/core/Gateway.kt`）
> 与内核桥（`agate-server/src/jni_bridge.rs`）之间 12 个入口的稳定契约。两侧
> 文件头各有指回；字段/语义变更必须同提交改两侧并 bump
> `BRIDGE_CONTRACT_VERSION`。历史沿革见 docs/《Agate两级内核重构-实施记录.md》。

## 0. 总纲

- **双通信面**：数据面全部业务交互走 `127.0.0.1:8787` HTTP（或 Core 态的
  `nativeApiCall` 内存路由）；本契约只管「电源开关 + 闹钟 + 日志回调 + 管理
  直调」，桥不碰任何业务数据结构。
- **两级内核**：Core（随进程，bootstrap 全部基础设施，不监听端口）与网关
  （手动 start/stop，绑 8787）。调度器是 Core 级独立开关，不随网关启停。
- **异常模型（R46/A 案）**：release 保持 `panic = "abort"`（与上游一致）。
  全库 panic-free by construction 纪律 + CI「Panic discipline gate」机械守护
  （deny unwrap/expect/panic/unreachable/todo/unimplemented，测试代码豁免）。
  桥可达路径零 panic 是硬约束；panic 在本契约下没有「穿越 JNI」的合法形态
  ——它等于进程死亡，靠纪律与审计预防，不靠运行时兜底。
- **契约版本**：`nativeStatus()` 信封携带 `bridge` 字段 =
  `jni_bridge::BRIDGE_CONTRACT_VERSION`（首版 1）。Kotlin 读到 0 = 旧内核
  （无该字段），按 R44 `kind` 的 null 兼容先例降级；≥2 = 未知新内核，消费方
  只允许取已知字段。字段变更（增/删/改语义）必须 bump 本号。

## 1. 入口一览

| # | 入口 | 线程要求 | 阻断上限 | 返回形态 |
|---|---|---|---|---|
| 1 | `nativeStart(configJson)` | 后台线程（bind 阻塞） | bind + 引导，秒级 | 信封 JSON |
| 2 | `nativeStop()` | 后台线程 | ≤5s（端口释放探测） | 信封 JSON |
| 3 | `nativeStatus()` | 任意（UI 5s 轮询） | 锁等待，μs 级 | 信封 JSON |
| 4 | `nativeTriggerTask(task)` | 后台线程 | 即返（派发即回） | 信封 JSON |
| 5 | `nativeCoreInit(dir,port,on)` | 后台线程（Application/FGS） | bootstrap，秒级 | 信封 JSON |
| 6 | `nativeSchedulerSet(on)` | 任意 | 锁等待 | 信封 JSON |
| 7 | `nativeCheckinRunNow(ms)` | 后台线程（闹钟） | ≤ms（默认 120s） | 信封 JSON |
| 8 | `nativeCheckinRunAndWait()` | 后台线程 + 必须持 WakeLock | 不封顶（等本轮真实结束） | 信封 JSON |
| 9 | `nativeRequestCheckinStop()` | 任意 | 锁等待 | 信封 JSON |
| 10 | `nativeVersion()` | 任意 | 即返 | **裸字符串**（唯一非信封） |
| 11 | `nativeSetLogCallback(cb\|null)` | 任意（@Synchronized） | 锁等待 | 信封 JSON |
| 12 | `nativeApiCall(m,p,body,key)` | Dispatchers.IO 级 | 120s | 信封 `{ok,status,body}` |

## 2. 错误信封规范

所有信封入口返回 `{ok: bool, ...}`。失败时必带 `error`（人读），可带：

- `stage`：`"jni"`（参数读取/分配失败）| `"config"`（start 配置解析）|
  `"bootstrap"`（Core 引导失败）| `"bind"`（端口冲突，附 `kind`:
  occupied/reserved/other——R44 引入）。
- **null 兜底链（R46/T3）**：Rust `json_reply` 分配失败先试最小信封
  `{ok:false,stage:"jni",error:"reply alloc failed"}`，连它都失败才返回
  null；Kotlin `Gateway.envelope(raw)` 把 null / 空串 / 非法 JSON 一律收敛为
  `{ok:false,stage:"jni",error:"malformed native reply"}`。任何一层都不允许
  把失败伪装成 null 静默抛 NPE——9 个解析点全部经 envelope 收口。

## 3. 关键语义（逐条有桥内注释背书，此处只列不变量）

1. **start 幂等语义**：已运行返回 `already_running` 且不触碰鉴权覆盖（LAN
   会话 Key 不能被本机模式重复 start 摘掉）。
2. **stop 释放语义**：shutdown 信号 + 5s/100ms×连续 2 命中端口探测；超时也
   返回 ok=true + released=false。只停网关，Core/调度器不受影响。
3. **apiCall 直调**：内存 `Router::oneshot`，完整鉴权/CORS/错误信封栈；body
   上限 4MB；API Key 走 Bearer；Core 未就绪返回 `core_not_ready`。
4. **checkin 三入口**：RunNow 超时不断任务（fire 已后台跑，返回 timeout +
   ok=true）；RunAndWait 等 CAS 抢锁的 busy 轮真实结束（FGS WakeLock）；
   RequestStop 合作式（当前账号自然收尾，不中断在途 Provider 请求）。
5. **日志回调**：单 GlobalRef；网关运行期不换监听；Kotlin 侧固定在 AgateLog
   HandlerThread 执行，回调内禁重活；Rust 侧回调异常一律清理不留 pending
   exception；增补平面字符转 U+FFFD（NewStringUTF 是 modified UTF-8）。
6. **锁纪律**：Rust 侧 Mutex 全部带 poisoned 分支（abort 语义下不可达，纯
   防御；若未来翻 unwind 则变活代码）。
7. **回复兜底链异常清理（R47/T3.2）**：`json_reply` 入口与第二层兜底分配
   失败处一律 `clear_pending_exception` —— JNI 规范下未清 pending 再调 JNI
   属未定义行为，且带 pending 返回时 JVM 会在调用点抛异常，破坏「失败以
   结构化信封 / null 交付、异常绝不逃逸到 Kotlin」契约。
8. **信封结构契约（R47/T3.3，Kotlin `envelope()` 从严）**：ok 必须存在且为
   布尔（optBoolean 无法区分缺字段与 false）；ok=false 必带非空 error；
   违者与畸形回复同路收敛（{ok:false,stage:"jni",error:"malformed native
   reply"}）且不保留原内容。只捕 JSONException，Error 诚实上抛。
9. **stop 四态消费（R47/T3.4 起，R49/T1 收口）**：wire 不变（released 自
   F6 起存在）。Kotlin `StopExecutionResult` 把回复分成四态：
   - `SkippedByNewerIntent`：守卫读到更新的 START，未执行 STOP；
   - `Stopped`：Native `ok=true`（`not_running` 是**唯一**被接受的幂等成功）；
   - `StopTimedOut`：`ok=true && released=false` → `GatewayRuntimeState.StopTimeout`；
   - `Failed(code,message,cause)`：`ok=false` 或 JNI 异常（异常不再上抛）。
   **只有 `Stopped` 才允许释放 WakeLock/WifiLock 并退出前台**；超时/失败保留
   FGS 与保活锁，按 10s × 3 复核（每轮先校验新 START 未到达，探测走
   `refreshStatus`），复核用尽仍保留服务并给出可诊断通知。R48 的「超限退出
   前台」语义废止。

10. **网关启动顺序（R49/T3）**：`nativeStart` 先 `server::start`（bind），
    成功后才提交 `start_gateway_tasks()`（当前唯一内容 = Sidecar 自动拉起）。
    bind 失败原样返回 `port_conflict`，不产生任何 Sidecar 副作用；顺序由
    `server::run_after_successful_bind` 固定，jni_bridge 与 headless bin 同源。

## 4. 版本沿革

- **v1（R46，bridge=1）**：定稿本文；status 增 `bridge`；envelope 收口；
  panic 纪律门上线。
- **pre-v1**：无 bridge 字段（Kotlin 读 0）；R44 增 `kind`（null 兼容）；
  R45-T3 剪 web_shim 桌面桥残留。
- **v1 增补（R47/T3，bridge 仍 =1）**：wire 零变更；补 json_reply 兜底链
  异常清理、Kotlin envelope 结构契约、released 三态消费语义；android
  target clippy 门与 verify-jni-exports 签名级互检上线（§3.5 流程外注）。
- **v1 增补（R49/T1，bridge 仍 =1）**：wire 零变更；stop 结果四态化（JNI
  异常收敛为 `Failed.cause`、`not_running` 唯一幂等）、Service 只在 Stopped
  时放锁/退出、复核用尽保留前台并区分启动/停止失败卡片。
- **v1 增补（R49/T3，bridge 仍 =1）**：wire 零变更；`nativeStart` 改为 bind
  成功后才提交随网关副作用，端口冲突不再误拉起 Sidecar。

## 5. 变更流程

1. 改 `jni_bridge.rs` 与 `Gateway.kt` 同一提交，字段增删对称；
2. `BRIDGE_CONTRACT_VERSION` +1，本文 §1/§2 表格与沿革同步；
3. 两侧测试同批更新（Rust：信封形状断言；Kotlin JVM：parse/envelope）；
4. 本文档 diff 进同一提交，CI 全绿才合并。
