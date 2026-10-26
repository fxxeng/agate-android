# Agate 两级内核重构方案 v2.0（拍板融合版）（Core 管理态 / Gateway 数据面）

> 2026-10-26 拟定，待用户批准后实施。**本阶段不改代码。**
>
> 目标语义（用户拍板原话）：
> 1. APP 只是管理器，打开 APP **绝不自动激活网关**；
> 2. 网关（8787 转发服务）只在真正需要时才**手动运行、手动停止**；
> 3. 管理操作（添加提供商等）**不得以「网关已运行」为前置条件**——这是重构要根治的根。

---

## 一、现状调研结论（2026-10-26 实读源码）

### 1.1 最大的好消息：切面天然存在

- `ServerState::bootstrap(port, host)`（`server/mod.rs:292`）：建全部服务句柄
  （AccountStore / AuthService / LoginService / BillingService / ModelCatalog /
  UpstreamService / AutoCheckin / UpdateManager / RequestStats / Db），句柄全部是
  「自带内部锁的轻量 Clone」——**这正是 Core 态需要的全部东西**。
- `server::start(&state)`（`server/mod.rs:937`）：**只做 HTTP bind + serve + oneshot
  停机通道**，不建任何状态。
- jni_bridge 的 `start_gateway()` 把两者串在一起跑——两级化只需把这两步**拆开调度**。

### 1.2 管理面 API 现状

- `server/api/` 53 个文件、约 50 个 `/api/*` 端点，全部是
  `State(ServerState) + Bytes → Response` 的 axum handler，薄壳调用 core 层服务。
- `http.rs:86 panel_router(state)` = `/api/*` 管理面 + 静态 fallback；
  `http.rs:78 router(state)` = panel_router + `/v1/*` 网关面。两路由都是普通
  `axum::Router`（tower Service）——**可以不 bind 端口、直接在内存里调用**（见 D1）。

### 1.3 bootstrap 内 spawn 的后台任务清单（需分级）

| 任务 | 位置（bootstrap 内相对行）| 性质 | 归属判定 |
|---|---|---|---|
| `sub_kernel::spawn_worker()` | ~109 行 | Clash 子内核 worker | Gateway |
| `auto_checkin::init_global` + 后续 `auto_checkin.start()` | ~232 / ~459 行 | 签到调度循环 | Gateway |
| 凭证维护 loop（600s 刷新 token） | ~466 行 | 账号健康 | Core |
| 模型目录启动刷新 | ~467 行 | 目录刷新 | Core |
| `scheduled_tasks::spawn(...)` | ~481 行 | 用户定时任务 | Gateway |
| `zcode::config_sync::spawn(...)` | ~486 行 | 配置同步轮询 | Gateway |
| `usage_query::spawn_sweeper(...)` | ~491 行 | 用量统计清扫 | Gateway |

### 1.4 JNI 桥现状（`agate-server/src/jni_bridge.rs`，424 行）

- 6 导出：nativeStart / nativeStop / nativeStatus / nativeTriggerTask /
  nativeVersion / nativeSetLogCallback。
- `start_gateway`：set env(AGATE_PROXY_HOME/API_KEY) → bootstrap → attach_db →
  local_panel=true → LAN 闸门 → `server::start()` → 存 `RUNNING`。
- `trigger_task`：**经 HTTP 回环打自己 8787 的管理 API，且要求 RUNNING**——
  两级化后此链路断（Core 态无端口），必须改直调（见 D3）。
- `lib.rs` 已有惰性全局 Tokio Runtime（`global_runtime()`，私有 fn）——JNI 同步
  调用可经它 `block_on`，需开放为 `pub(crate)`。

### 1.5 Kotlin 侧现状

- `app/data/` 9 个文件（AccountOps 213 / CustomProviderOps 132 / GatewayRepository
  498 / KernelOps 35 / LoginClient 150 / ModelOps 80 / PresetRegistry 183 /
  ScheduledTaskOps 76 / TaskCenter 175 / TaskOps 84 行）：**各自持 OkHttpClient、
  同构「拼 path + JSON body + Bearer → 解析」模式**，无统一 transport 抽象。
- `GatewayRepository`：okhttp-sse 订阅 `/api/events` + `/health` 5s 轮询降级 +
  控制台快照轮询（5s，挂页面 DisposableEffect）。
- `core-gateway/Gateway.kt`：6 个 native 方法声明 + parse 包装。
- FGS `AgateForegroundService`：ACTION_START/STOP/TICK 三态；BootReceiver 受
  `GatewayPrefs.bootAutoStart`（当前默认 true）控制开机自启网关。
- manifest **没有自定义 Application**（只有 MainActivity）——Core 态需要新建
  `AgateApp.kt` 作为进程级接线点。

### 1.6 登录回调矩阵（决定哪些提供商 Core 态可登录）

| 通道 | 提供商 | Core 态可登录？ |
|---|---|---|
| wait 轮询（UI 轮询登录任务状态） | CatPaw / AutoClaw / CodeArts / Trae / OfficeAce / ZCode 等大头 | ✅ 可 |
| 自定义 scheme 拦截（zcode:// 等 → WebView 拦截转发 callback） | zcode 系 | ✅ 可（oneshot 直调 callback 端点）|
| **真实 loopback HTTP 回调**（`http://localhost:{port}/oauth-callback`，bootstrap 里 `set_loopback_port` 三处） | Accio / CodeArts(OAuth) / Antigravity | ❌ 不可（无端口监听）——需网关运行 |

---

## 二、目标架构

### 2.1 内核状态机（Rust 侧）

```
进程启动（APP 冷启，AgateApp.onCreate）
   │
   ▼
┌─────────────── Core 态（管理面，进程内常驻）───────────────┐
│  ServerState::bootstrap() 全部服务句柄 + DB               │
│  保留后台任务：凭证维护 loop、模型目录刷新                 │
│  内存 Router（panel_router 克隆，不 bind 任何端口）       │
│  行为：登录 / 账号 CRUD / 配置 / 记录查询 / 测速           │
│  零端口、零转发、零定时调度                                │
└───────────────────────────────────────────────────────────┘
   │ 手动「运行网关」（FGS + 磁贴 + 控制页按钮）             │ 手动「停止网关」
   ▼                                                         │
┌─────────────── Gateway 态（数据面，叠加在 Core 上）───────┐
│  server::start()：bind 8787（/v1/* 转发 + /health +      │
│    /api/* 对外管理面，供局域网设备使用）                  │
│  启动 Gateway 级后台任务：签到循环 / scheduled_tasks /    │
│    zcode config_sync / usage sweeper / sub_kernel worker  │
│  停止 = 撤 HTTP serve + 停 Gateway 级任务，Core 保留      │
└───────────────────────────────────────────────────────────┘
```

### 2.2 通道图

```
Compose UI ──JNI nativeApiCall(oneshot)──▶ 内存 Router ──▶ 既有 53 个 handler（零改动）
           （管理面，两级都通，Bearer 在桥内注入）

外部设备 ──HTTP :8787──▶ Gateway 态 serve（仅网关运行时；/v1 转发 + 带 API Key 闸门的管理面）

UI 事件流 ──SSE /api/events──▶ 仅 Gateway 态（Core 态 UI 走轮询降级，徽标区分）
Android 闹钟 ──JNI trigger_task(直调)──▶ Core 态即可跑签到（不再要求网关运行）
```

---

## 三、关键设计决策（D1–D6，★=推荐已定，☆=待拍板）

### D1 管理面 JNI 直调实现：Router::oneshot ★

不抽 service 层、不改 53 个 handler。axum Router 本身是 tower Service：

- Core 态持有 `static CORE: Mutex<Option<CoreState>>`，`CoreState { state: ServerState,
  router: Router }`，router 由 `http::panel_router(state)` 构建一次（克隆廉价）。
- 新 JNI 导出 `nativeApiCall(method, path, bodyJson) -> String`：
  构造 `http::Request`（自动附 `Authorization: Bearer $AGATE_PROXY_API_KEY`）→
  `global_runtime().block_on(router.clone().oneshot(req))` → 读出 status + body →
  JSON 信封 `{status, body}` 返回 Kotlin。
- **鉴权、闸门、错误信封、CORS 全部与 HTTP 面同路径复用**；Kotlin 侧从此不用管
  Bearer（修掉上次 LoginClient 漏带 Key 那类 bug 的根源）。
- 备选（不推荐）：抽 service 层双 API 面——53 文件重构，收益零增量。

### D2 后台任务分级：按 1.3 表归属 ★（归属判定本身 ☆可调���

- Core 态保留：凭证维护 loop、模型目录刷新（账号健康管理，低频）。
- Gateway 态启停：签到循环 / scheduled_tasks / zcode config_sync /
  usage sweeper / sub_kernel worker。
- 「停网关即停」需要给这几个 loop 加停机钩（统一 `tokio::sync::watch` 模式，
  每处 ~10 行）；实现中若发现某 loop 改造成本异常，允许降级为「记录已知边界」，
  但**签到循环必须可停**（定时签到是停网关期间最不可接受的隐性后台行为）。
- ☆可调点：凭证维护 loop 是否也归 Gateway（纯「零后台」洁癖版）——不推荐，
  账号 token 过期会让下次开网关全挂。

### D3 闹钟签到：trigger_task 改直调，不要求网关运行 ★

`trigger_task` 从「HTTP 回环 + 要求 RUNNING」改为 `api_call` 直调
`POST /api/auto-checkin/run`（要求 Core 在）。语义变化：**网关停着，闹钟到点
照样签到**（M1 的 08:00/20:00 闹钟设计不倒退）。Gateway 态运行时内核调度循环
兜底照旧（双轨与 M1 一致）。

### D4 SSE 事件流：一期 HTTP-only（仅 Gateway 态）★

Core 态 UI 走既有轮询降级（徽标文案区分「Core · 轮询」）。二期可选：
`nativeSetEventCallback`（复用 SetLogCallback 的 GlobalRef 模式），Core 态也收
事件。一期不做，控制改动面。

### D5 loopback 回调三家（Accio / CodeArts-OAuth / Antigravity）☆

Core 态登录这几家 = 无端口收 OAuth 回调，不可用。一期方案：UI 在 Core 态对
这几家显示「此提供商登录需先运行网关」提示（PresetRegistry 加 `needsGateway`
标记）。备选：Core 态临时 bind 回环端口收完回调即撤——复杂度不值，不推荐。

### D6 开机自启默认值 ☆

`GatewayPrefs.bootAutoStart` 默认 true → **改默认 false**（对齐「手动按需」哲学，
保留开关给想要的用户）。磁贴/通知栏按钮语义不变（= 运行/停止网关）。

---

## 四、实施计划（四期，每期独立 CI + 可验收）

### P1 Rust 内核两级化（~350 行，核心刀）

| 文件 | 改动 |
|---|---|
| `agate-server/src/lib.rs` | `global_runtime` 开放 `pub(crate) fn block_on_task()` |
| `agate-server/src/server/mod.rs` | bootstrap 后半的 Gateway 级 spawn 移入新 `pub fn spawn_gateway_tasks(&self)` / `pub fn shutdown_gateway_tasks()`；`auto_checkin.start()` 调用点随迁 |
| `core/auto_checkin.rs` `core/scheduled_tasks.rs` `core/providers/zcode/config_sync.rs` `core/usage_query.rs` `core/sub_kernel.rs` | 各加 watch 停机钩（循环体每轮检查；`spawn_*` 返回停机句柄或模块级注册） |
| `agate-server/src/jni_bridge.rs` | ① `static CORE: Mutex<Option<CoreState>>`；② `core_start(data_dir, port, bind_lan)`＝现 start_gateway 前半（bootstrap+attach_db+local_panel+LAN 闸门+build router，**不 bind**），幂等；③ `gateway_start()`＝core 在 → `server::start` + `spawn_gateway_tasks`，存 RUNNING；④ `gateway_stop()`＝oneshot 停 serve + `shutdown_gateway_tasks`，CORE 保留；⑤ `api_call()`（oneshot + Bearer 注入 + JSON 信封）；⑥ `trigger_task` 改走 `api_call`；⑦ 新导出 `nativeCoreStart` / `nativeApiCall`；`nativeStart` 保留＝core_start+gateway_start（兼容磁贴/FGS 老调用，语义=「运行网关」） |

验收（CI + 桌面单测思路）：core_start 后 status `core_ready=true, gateway=false`；
api_call 打 `/health`、`/api/session`（带账号 CRUD）在**无端口**状态全通；
gateway_start/stop 循环 3 次幂等；停网关后 60s 内无签到循环日志。

### P2 Kotlin 通道层（~500 行，机械改造）

| 文件 | 改动 |
|---|---|
| `core-gateway/Gateway.kt` | +`nativeCoreStart` / `nativeApiCall` 声明与包装（`apiCall` 挂 `Dispatchers.IO`） |
| `app/data/CoreTransport.kt`（新） | 统一 `suspend fun call(method, path, bodyJson): CallResult`，解析 `{status, body}` 信封；替代各文件的 OkHttpClient |
| `app/data/` 9 文件 | 底层 call 逐个换 CoreTransport；上层 JSON 解析、DTO、轮询节奏全部不动 |
| `GatewayRepository` | 探活从「/health HTTP」改「nativeStatus 两级状态」；SSE 仅 `gateway==true` 时连接，否则轮询（徽标三态文案更新） |

### P3 Android 生命周期 + UI（~300 行）

| 文件 | 改动 |
|---|---|
| `app/.../AgateApp.kt`（新）+ manifest | `android:name` 注册；onCreate 子线程 `Gateway.coreStart()` |
| `AgateForegroundService` | START＝coreStart(幂等)+gatewayStart；STOP＝gatewayStop；TICK＝直调（去掉「网关没跑就跳过」） |
| `GatewayPrefs` + `BootReceiver` | bootAutoStart 默认 false（D6） |
| 网关控制页 / ConsolePage Hero 卡 | 两级状态显示：「内核就绪」恒亮 +「网关 运行中(:8787)/已停止」；「运行网关」主按钮不变 |
| `PresetRegistry` + `AddProviderSheet` | 三家 loopback 依赖加 `needsGateway` 标记，Core 态置灰 + 提示（D5） |
| `gateway_start` 的 PortConflict | UI 明示「端口 8787 被占用，请检查另一实例」（顺带修上次踩的静默坑） |

### P4 真机验收 + 文档

---

## 五、验收标准（真机五场景）

1. **冷启 APP 不运行网关**：添加 CatPaw 提供商（网页登录）全程成功；账号列表/
   模型/记录可看；`ss`/curl 验证 8787 未监听；logcat 无签到/定时任务行为。
2. **运行网关**：8787 监听；`/v1/models`、`/v1/chat/completions` 转发正常；
   SSE 徽标绿。
3. **停止网关**：8787 释放；UI 仍可增删账号；SSE 徽标切「Core · 轮询」；
   60s 观察无定时行为。
4. **网关停着点「立即触发签到」**：checkin_done 正常（D3 链路）。
5. **幂等**：coreStart 重复调、gatewayStart/stop 交替 3 次，无重复任务、无状态错乱。

## 六、风险与已知边界

- **axum 0.8 oneshot 集成**：需在 Cargo.toml 显式加 `tower`（本就在依赖树）；
  `Body` 构造 / `axum::body::to_bytes` 读响应。集成测试惯用手法，首轮 CI 可能
  有 1–2 轮小修（jni 0.21 同期先例）。
- **block_on 线程模型**：JNI 从 Kotlin 协程线程调，非 tokio worker 线程，
  `Runtime::block_on` 安全；单次调用（登录 token 交换）秒级可接受。
- **后台任务停机钩**是 Rust 侧唯一侵入既有 core/ 模块的改动（5 文件 × ~10 行），
  与上游 diff 增量可控。
- **三家 loopback 登录在 Core 态不可用**（D5，一期明示不硬解）。
- **model_test 测速在 Core 态技术上可用**（UpstreamService 句柄活着），语义建议
  网关运行时测——UI 不拦，文档注明。
- `nativeStart` 旧语义保留（= 全启），磁贴 / BootReceiver / 通知按钮无需同步改。

## 七、工作量评估

P1 ~350 行 Rust（一个 session）→ P2 ~500 行 Kotlin（一个 session）→
P3 ~300 行 Kotlin（半个 session）→ P4 验收（用户配合装 APK）。全程不动 53 个
api handler、不动 core 业务逻辑（除停机钩）。


---

## v2.0 修订记录（2026-10-26 用户拍板，融合 v5.0 决策书）

### 拍板结果
- ① 端口：**可配置**（设置项，单一来源），默认 8787 不变。
- ⑤ 调度器：**不随网关**——独立开关，随 Core 常开（App 进程活着就调度签到），设置里可关。
- ② 局域网网页面板：保留（面板鉴权栈不删）。
- ③ 信封加密：放 P5。
- ④ Kuku 置灰 + 放弃代理订阅：确认执行。
- ⑥ `billing/activity.rs` 会话 ID 前缀 `agate-` 回退上游原值。
- ⑦ AutoClaw / Trae「自带临时监听」排 S2 实机验证后定 Core 态登录性。

### 架构变化（相对 v1.0）
- **三开关取代两级硬归属**：core_init（随 Application，含凭证维护+模型刷新）→ 调度器（独立开关，默认开，`auto_checkin.start` 归此）→ gateway_start/stop（手动，端口+sub_kernel worker+scheduled_tasks+zcode config_sync+usage sweeper）。
- **F2/F3/F6 并入 P1**（同批文件）：tick 不再 `Gateway.start` 拉起网关；WakeLock 等签到完成才释放；gateway_stop 等端口释放再返回。
- **F10 调度单一事实源**（排期 D）：内核 `checkin_schedules` 的下次到点驱动 Android 精确闹钟，取代固定 08:00/20:00。
- 页面级快照起步于 P2（先通道替换，后按页面合并拉取）。
- 不采纳：删面板鉴权栈 / 去日志回调 / 删提供商页拖拽排序（详见《Agate-v5.0决策书对比分析.md》）。

### 路径修正
内核源码实际在 `agate-server/src/server/core/`（本文档正文的 `core/` 前缀均按此理解）；SSE 总线在 `server/events.rs`。

### 排期（最终）
A 立即小修包（F4 清Cookie / F5 networkSecurityConfig / F13 主题持久化 / F16 publish 优化 / .gitattributes / F12 Kuku 置灰）→ B P1 两级内核+三开关 → C P2 通道层+页面级 → D 调度改造（内核驱动闹钟）→ E P5 加固（信封加密/崩溃钩子/CI release）。
