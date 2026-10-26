# 两级内核重构 · 实施记录（P1/P2 已完成）

> 2026-10-26 更新。配套方案：《Agate两级内核重构-方案v2.0拍板版.md》（本目录，
> 开工前拟定）。方案中部分条目在实施时被修正，**接手以本文为准**；冲突处见 §2。
>
> 背景（用户定调，2026-10-26）：APP 只是管理器，打开 APP 绝不自动激活网关；
> 网关只在真正需要时手动运行/停止；管理操作不得以「网关已运行」为前置条件。
> 推进原则：「给出最佳方案，不是最省事的方案——项目还在开发初期阶段」。

## 1. 状态总览

| 阶段 | 内容 | 状态 | commit |
|---|---|---|---|
| 立即小修包 | F4 登录前清 Cookie / F5 明文收紧 networkSecurityConfig / F12 Kuku 置灰 / F13 主题档持久化 / F16 SSE 零订阅者跳过 等 7 项 | ✅ | `cf92dd4` |
| P1 Rust 两级化 | Idle→Core→Gateway 三态、任务分级、JNI 生命周期导出 + `nativeApiCall` | ✅ | `3c07629`（+B2 修正） |
| P2 Kotlin 通道层 | 管理面 9 个数据文件底层切内核直调（CoreTransport） | ✅ | `aadca49`（CI 40292123078 绿） |
| P3 生命周期/UI | 开机自启默认 false（D6）、端口设置页、D5 三家登录 UI 明示 | 未开始 | — |
| P4 真机五场景验收 | 方案 §验收清单 | 未开始 | — |

当前验收口径（P2）：**不启动网关** → 打开「添加提供商」→ 浏览器登录应可用
（内核直调，不再依赖 8787 端口）。真机验证待装 APK（artifact `agate-m0-arm64`）。

## 2. ★ 语义修正（B2）：网关启停 = 纯端口服务开关

**对方案 v2.0 最重要的偏离，方案 §1.3 的任务分级表就此作废。**

- 用户澄清的正确语义：网关启停只控制端口服务（`/v1` 转发）。
  **余额查询、订阅同步、签到等一切后台功能照常跑，完全不碰。**
- 方案原定「sub_kernel / zcode config_sync / usage_query 三件抽为 Gateway 级、
  spawn 挪到 gateway_start」**作废**；三件**升格为 Core 级**——挪到 bootstrap
  尾部拉起（时序等价、时机提前到进程启动），与调度器同级，跟网关启停彻底解耦。
- `stop_gateway` 一行未改：它本来就只停端口，正是目标语义。
- 实现方式：三件任务挪 bootstrap + 模块自防重入（补齐 worker 同款 OnceLock
  守卫），不做「Gateway 级抽取」临时分组。

需知悉的副作用：Core 态下余额 sweeper 与 config_sync 按各自既有开关
（账号级「自动查询」+ 存在代表账号）开始工作——这是语义修正的自然结果，
非回归。

## 3. 顺带根治的存量 bug（读码发现，随 B 包/B2 修复）

1. **`start_gateway_tasks` 守卫写反**：`if STARTED.set(()).is_ok() { return; }`
   ——OnceLock set 成功＝第一次调用，却直接 return 什么都不拉起；从第二次起
   才拉，且 config_sync/sweeper 无防重入，每多启动一次网关就叠加一层轮询
   循环。已随三件任务挪 bootstrap 根治。
2. **`status_json` 锁内 TCP 探活**：`controller_alive` 带 300ms connect_timeout
   且在 `with_status` 闭包内调用 → 最坏持锁 300ms，期间所有状态读取排队。
   已抽公共函数、探活移到锁外。

## 4. 架构现状（接手者需要知道的）

- **Core 态**（APP 进程常驻）：bootstrap 建全部服务句柄 + 内存 Router，
  跑调度器与 Core 级后台任务（凭证维护 / 模型刷新 / auto_checkin /
  sub_kernel / zcode / usage）。零端口、零转发。
- **Gateway 态**（手动「运行」才进入）：bind 端口（默认 8787，port/host 经
  ServerState 字段开放可配置）+ `/v1` 转发 + 前台服务。停止只撤端口服务，
  Core 态一切保留。
- **管理面调用**：`nativeApiCall(method, path, body, apiKey)` → CORE 全局取
  ServerState → `server::http::router(state)` 组 Router（OnceLock 缓存）→
  `Router::oneshot` 走完整中间件栈（鉴权、错误信封与 HTTP 完全一致），
  spawn_task + mpsc 同步等结果，上限 120s。
- **Kotlin 侧**：`data/CoreTransport.kt`（`Reply.Ok{status,text}` /
  `Reply.Down{message}`）；AccountOps / CustomProviderOps / ModelOps /
  TaskOps / TaskCenter / ScheduledTaskOps / LoginClient / GatewayRepository /
  KernelOps 共 9 个数据文件的 get/post/patch 底层全切 CoreTransport，
  **上层解析零改动**。

### 通道选择指引（新代码）

- 管理面请求一律走 CoreTransport（内核直调）。
- 仅两处刻意保留 HTTP：
  1. `KernelOps.isUp()` 端口活性探测——oneshot 永远「通」，测不了端口死活；
  2. SSE `/api/events` + 轮询降级（O9）——流式不适合 oneshot。

## 5. UI 文案语义对齐（P2 随附）

- 连接失败文案「无法连接内核（网关未运行？）」→「无法连接内核」（Core 态
  下直调不会因网关没跑而失败，旧文案误导）。
- AddProviderPage 提示卡改「网关未运行（不影响登录与添加）」+ 内核直调说明。
- ConsolePage 4 处 `?: 8787` 展示兜底改读 prefs（配合端口可配置）。

## 6. 遗留（P3/P4 待办）

- D6 开机自启默认值 true→false（对齐手动按需哲学，开关保留）。
- D5 三家（Accio / CodeArts-OAuth / Antigravity）登录依赖真实 loopback 端口
  收 OAuth 回调，Core 态没端口收不了——需 UI 明示「这几家需先运行网关」，
  不硬解。
- 端口设置页（可写性已备好，UI 未做）。
- P4 真机五场景验收（方案 §验收）。

## 7. 踩坑附录（P2 五轮 CI 换来的，下次写 oneshot 直调必看）

1. Rust 闭包里用 `?` 必须显式标注返回类型（如 `|| -> Option<ServerState>`），
   否则推断失败 E0277。
2. CORE 全局里是 `Core { state: ServerState, .. }`——取 `state.clone()`，
   Core 本身非 Clone。
3. `Router::oneshot` 不是 axum 固有方法，是 tower 的 `ServiceExt` trait 方法：
   需 `use tower::ServiceExt;`，且 **tower 必须在 [dependencies]**（上游只在
   [dev-dependencies] 给测试用；移入主依赖同版本无产物增量）。
4. **http 1.x 的 HttpBody impl 严重收窄**：`String` / `Vec<u8>` /
   `bytes::Bytes` 全不满足（http 0.2 时代可以），报错 help 列表有误导——
   唯一可靠写法＝`axum::body::Body::from(...)` / `Body::empty()`。
5. Kotlin 非表达式体函数的最后表达式**不自动返回**，`return if (...) {...}
   else {...}` 要显式 return。
6. OnceLock `set()` 返回 `Result` 不是 `Option`（P1 首轮 CI 失败点）。
