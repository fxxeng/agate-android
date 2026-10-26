# 《Agate 架构决策书 v5.0》对比分析（对照本地 v1.0 方案 + 源码逐条核验）

> 2026-10-26。核验基线：agate-android 实际源码（M3-3R 后状态）。
> 结论先行：**v5.0 是高质量决策书，18 个问题断言核验后 14 属实、2 半实、2 存疑/误读；核心架构与本地 v1.0 不谋而合，另有 6 处比 v1.0 更优应吸收；3 处建议不采纳。**

---

## 一、与本地 v1.0 方案的架构对照

### 1.1 不谋而合（互相印证，可信度增强）

| v5.0 | 本地 v1.0 | 备注 |
|---|---|---|
| D3 管理面 JNI 进程内派发（panel_router oneshot + 假 ConnectInfo） | D1（同手法） | 连实现细节一致；已核实 ConnectInfo 仅 `api/panel.rs` 使用，补假 ConnectInfo 代价极小 |
| D2 Core/Gateway 分离 | 状态机两级 | 同 |
| D4 端口仅手动运行时 bind、单一来源 | P1 | 同；实测 `8787` 字面量在 Kotlin 侧出现 22 处，收敛属实 |
| D5 闹钟直调 Core、不拉网关 | D3 | 同（v5.0 又往前一步，见 1.2-⑥） |
| D10 单一 CoreApi | P2 CoreTransport | 同思路 |
| 开机自启默认关 | D6 | 同 |

### 1.2 v5.0 更优、应吸收的六点

1. **三开关取代两级硬归属（D2）**。v1.0 把「调度器」硬归到 Gateway 态启停，v5.0 改成独立开关（Core / 后台任务 / 网关），用户可选「网关停着只跑定时签到」。这化解了 v1.0 里 D2/D5 两个待拍板点（凭证维护归属、App 前台是否随 Core）——开关化后由用户自定，默认值保守即可。**采纳：Rust 侧 core_init / scheduler_start / gateway_start 三段，成本与两段几乎相同。**
2. **F2 闹钟拉起完整网关（真 bug，v1.0 漏了）**。已核实 `AgateForegroundService.runTick`：网关没跑时直接 `Gateway.start(…, 8787, bindLan)` 并把通知升级为「网关运行中」常驻——旧模型（网关=一切）下合理，新语义下必须去掉，tick 只跑签到即退。
3. **F3 WakeLock 提前释放（真 bug，v1.0 漏了）**。已核实：tickLock 固定 30s；JNI `trigger_task` 异步派发后立即返回 json，不等签到完成。多家账号一轮签到完全可能超 30s → 跑一半设备休眠。改 oneshot 直调后天然同步等待，顺手修复；FGS 侧再加「完成后才释放锁」兜底。
4. **F6 停止等待端口释放**。`stop_gateway` 只发关闭信号立即返回，快速停→启可能绑定冲突。gateway_stop 应等 serve 退出再返回（v5.0 标待验证，风险成立）。
5. **F10 调度单一事实源**。已核实内核 `server/core/auto_checkin.rs` + `checkin_schedules`：多条目、每条目自选时刻、`next_run_at_ms`、fired_today 去重——能力齐全。Android 闹钟固定 08:00/20:00 确是平行时刻表，应改为「内核给下次到点、闹钟跟随」。**比 v1.0 的 D3 直调更进一步，采纳进调度改造。**
6. **F15 的衍生发现（本次核验新抓到）**：`billing/activity.rs:542` 会话 ID 前缀已被改成 `agate-`。这是**发往上游服务器的标识串**，与上游行为分叉可能引入无谓的风控差异。记入巡检：确认是否回退上游原值（前缀不是保密串，上游 agent2api 自己也在用）。

### 1.3 建议不采纳的三点

1. **D8「删除面板鉴权栈」不删**。保留成本为零（路由挂着无人调用），删除却要动 `http.rs` 并砍掉「局域网网页面板」的可能——v5.0 自己在 §7② 也把「是否暴露局域网面板」列为待决，待决项不该先删其依赖。管理走 JNI 后该栈自然闲置。
2. **F8「去掉日志回调」不去**。回调工作正常且有递归保护；「应用日志页读内核 logs 表」是对的（两者并存：logcat 调试用回调、UI 日志页读表）。
3. **F11「删除提供商页排序」先不删**。已核实确实只写 `GatewayPrefs.setProviderOrder`（仅显示顺序、与内核优先级无关）——但拖拽列表（AgateDragList）是 M3-3R 刚建成的资产，路由页要用。处置：加「仅显示顺序」标注，路由页落地后迁移，不删。

---

## 二、18 项断言逐条核验结果

| # | 断言 | 核验 | 证据 |
|---|---|---|---|
| F1 | 管理操作依赖网关（新增页「启动网关」按钮、数据层置「网关未运行」） | ✅属实 | AddProviderPage.kt:153/160、GatewayRepository.kt:199/243 |
| F2 | tick 拉起整个网关并常驻通知 | ✅属实 | runTick 本体：Gateway.start(8787, bindLan) + updateNotification |
| F3 | WakeLock 提前释放 | ✅属实 | tickLock 30s 固定；trigger_task spawn 后立即返回 |
| F4 | WebView 无会话隔离 | ✅属实 | AuthWebViewSheet.kt:174-175 只 setAccept，无清理 |
| F5 | 全局 cleartext | ✅属实 | manifest usesCleartextTraffic=true |
| F6 | 停止不等端口释放 | ✅成立 | stop 只发 oneshot 信号即返回 |
| F7 | 重复调用 set_var 线程安全 | ✅属实 | core_start 幂等化时只首次设置即可（Key 改传参不必——env 机制保留，风险窗口仅首次） |
| F8 | 日志回调 GlobalRef 风险 | ⚠夸大 | 有递归保护与注释约定；处置见 1.3-2 |
| F9 | 11 处 OkHttp / 13 处 Authorization / 7 处端口 | ✅方向属实 | 实测 10 文件持 OkHttpClient、13 处 Authorization、22 处 8787 字面 |
| F10 | 调度两个事实源 | ✅属实且可修 | checkin_schedules 具备 next_run_at_ms 全套能力 |
| F11 | 提供商页排序只写 prefs | ✅属实 | ProviderPage.kt:216；处置见 1.3-3 |
| F12 | Kuku 引擎 Windows DLL | ✅属实 | engine.rs:49 `genflowengine.dll` + 注册表定位，Android 必空 |
| F13 | 主题重启丢失 | ✅属实 | MainActivity.kt:25 rememberSaveable |
| F14 | Key 明文 prefs | ✅属实 | 归 P5 信封加密 |
| F15 | 改名波及 ~100 文件、「5 处差异」记录不符 | ⚠半实 | 波及面属实；但「M0 记录与事实不符」误读时序——M0 记录是 M0 时点，2026-10-25 去上游化是后续动作。衍生发现 agate- 前缀见 1.2-6 |
| F16 | publish 无订阅者仍序列化 | 🔍未定位 | events.rs 实际路径待查（报告路径错）；微优化，方向合理 |
| F17 | CI 无 test/release/.gitattributes | ✅属实 | 仅 :app:assembleDebug；无 .gitattributes |
| F18 | 记录矛盾（M0 验收状态） | ⚠半实 | M0 真机验收 2026-10-24 已完成，个别文档写于验收前；「以真机为准」结论无误 |

**报告自身的错误**：内核路径写错（`core/xxx.rs` 实际在 `server/core/xxx.rs`——kuku/events/billing 全部；本地 v1.0 同错，已一并修正）；「145 个路由」数字口径不明（实测 /api/* 约 50 个）。说明报告部分内容按上游桌面结构推断而非实读本仓。

---

## 三、融合后的排期（取代 v1.0 四期 + v5.0 七期）

**A. 立即小修包**（不碰架构，半天）：F4 登录前清 Cookie、F5 networkSecurityConfig 收紧 127.0.0.1、F13 主题持久化、F16 publish 优化、.gitattributes、F12 Kuku 置灰标注。

**B. P1 两级内核（吸收 v5.0）**：v1.0 P1 原案 + 三开关取代硬归属 + F2/F3/F1 一并修（同一批文件，不另设纠偏期）+ F6 停止等待端口释放 + 路径修正（server/core/）。v5.0 的「P0 纠偏 1-1.5 周」与两级化合并执行，避免同片代码改两次。

**C. P2 通道层**：v1.0 P2 原案（CoreTransport 替换 9 文件）+ 页面级快照起步（v5.0 D10 第二步；先打通道、后按页面合并拉取）。

**D. 调度改造**：三开关中的 scheduler 语义 + F10 内核驱动闹钟（内核给下次到点 → Kotlin 排精确闹钟；tick 短时服务等待完成再放锁）。

**E. P5 加固**：信封加密、崩溃钩子（panic hook + ApplicationExitInfo 横幅）、CI cargo test + release 签名、上游适配器月度巡检（含 agate- 前缀回退决定）。

## 四、待拍板清单（两份合并去重）

| # | 事项 | 推荐 |
|---|---|---|
| ① | 数据端口 8787 维持还是换上游默认 3065 | 维持 8787 |
| ② | Gateway 态是否保留局域网网页面板（决定面板鉴权栈去留） | 保留（不删栈） |
| ③ | 信封加密放 P5 还是提前 | P5（先 JNI 通道后，明文 prefs 风险面已缩小） |
| ④ | Kuku 置灰 + 放弃代理订阅 | 确认 |
| ⑤ | 「后台任务」开关默认值（随网关 / 独立常开 / 默认关） | 默认「随网关」，开关暴露 |
| ⑥ | agate- 会话前缀是否回退上游原值 | 回退（消除无谓上游行为分叉） |
| ⑦ | AutoClaw/Trae「自带临时监听」待实机验证后定 Core 态可登录性 | 排 S2 验证 |
