# Agate（Android AI Gateway）架构与移植方案 · v4.0 超越演进版

> **整理日期**：2026-10-25 ｜ **状态**：M0/M1 已真机验收关单，M2 设计系统实施中
> **设计基线**：`123_v44.html`（v44 视觉/状态机规范，不复用 HTML 代码）× agent2api 上游源码（**实证基线 server 3.0.8**，O2 已核实）
> **核心理念**：取上游之精华（26 家官方 Agent 协议/签名/伪装头/脱敏资产 + 25 张中转预置卡），创移动端之最优解（CHAIN 账号级混排路由、SSE 零轮询总线、系统级低功耗调度、内嵌无感抓 Cookie、原生 MCP Server）
> **相对 v3.0**：吸收《8 大超越级架构优化》报告为蓝本，逐项对齐 M0/M1 已实证的工程事实（端口、调度决策、仓库状态），冲突处按实证修正而非照抄

---

## 0. 工程现状快照（报告之外的实证对齐，2026-10-25）

| 项 | 事实 |
| :--- | :--- |
| 仓库 | github.com/fxxeng/agate-android（**已公共**，Actions 免费不占额度）；构建仓库 `~/agate-build/repo`（Termux，SSH push）；CI `workflow_dispatch` 手动触发 |
| 数据面端口 | **8787**（原方案 8080 与本机 Termux MCP 冲突且内核 bind 失败静默，commit `9248bb1` 改定；方案内所有 `:8080` 均按 8787 读） |
| M0 内核闭环 | ✅ 真机关单：JNI 桥 6 函数 / `libagate_server.so` 13.5MB / 前台 Service / `/health` 200（degraded 未登录诚实降级）/ `/v1/models` 空列表 |
| M1 保活壳层 | ✅ 真机关单：FGS 分级持锁（平时无锁）+ AlarmManager 北京时间定点（时区钉 Asia/Shanghai 对齐内核 beijing.rs）+ BootReceiver + 电池白名单 + 局域网 0.0.0.0 **四端点闸门**（health 放行 / models 空列表放行 / `/v1` 无 Key fail-closed 503 / `/api` 401 panel_login_required） |
| M2 设计系统 | 🔄 实施中：v44 双主题 tokens + tnum + 12 通用组件（350ms 长按拖拽+Haptic+置顶、365 热力图、24h 双轴图等）+ 4 主 Tab/22 二级页骨架 + 组件走查页（commit `1a8cd75` + 编译修复 `c2a17d6`，CI 验证中） |
| 已实证踩坑 | 8080 端口冲突静默 bind 失败（running=false/port=0 但 HTTP 有响应 = 打到别的监听者）；明文 HTTP 需 `usesCleartextTraffic`；`termux` 侧工具返回成功≠落盘成功，改后必 grep 验证 |

---

## 1. 定位修正：从「编译移植」到「超越版 Superset」

上游 Agent2API-main 本质为桌面端（PC 浏览器面板 + Tauri 壳）设计，原样照搬会「水土不服」。Agate 的定位：**复用上游 20+ 家 Provider 协议适配资产 + 重塑移动端原生内核与高阶路由引擎**。

### 8 大超越级架构优化（浓缩对照）

| # | 模块 | 上游痛点 | Agate 优化 | 状态 |
| :-- | :--- | :--- | :--- | :--- |
| 1 | 选号与路由 | 两级割裂（先排家顺位再家内选号），不支持跨家账号交错混排 | **CHAIN 扁平账号级候选队列**：`global_queue_pos` 字段 + 顺序优先/负载均衡/同家轮询三算子 + `/api/routing/simulate` 实时选号预演 | M3 |
| 2 | 状态与日志同步 | 1~2s 高频 HTTP 轮询，手机 CPU 无法进低功耗态 | **单条 `/api/events` SSE 增量事件总线**（tokio broadcast），请求进出/自愈熔断/退避归零/签到完成才推送 Diff；平时 0 轮询 0 耗电 | M3 前置 |
| 3 | 后台调度 | tokio 死循环 sleep，Doze 下失效或强持锁发烫 | **调度混合模式（按 M1 实证修正）**：内核循环保留（进程活着时 tokio sleep 零 CPU）+ Android 闹钟负责进程被杀后定点拉起与 Doze 网络窗口兜底；WorkManager 承接 ≥15min 周期任务 | ✅ 部分已实现（闹钟侧），见 §3.3 |
| 4 | 根证书与网络栈 | 硬编码 webpki-roots，抓包工具/自签代理一律 TLS 握手失败 | **rustls-platform-verifier**（信任 Android 系统证书链）+ **NetworkCallback 切网连接池重置**（Wi-Fi↔5G 瞬切不假死） | M3 |
| 5 | 凭据加密 | 桌面 DPAPI / 非桌面退化为明文 | **AndroidKeyStore (TEE) 硬件主密钥**：Kotlin 生成 AES-256-GCM 主密钥经 JNI 注入 crypto.rs，Root/备份外泄无 TEE 不可解 | M3/M5 |
| 6 | 过码与登录 | 每账号开 localhost 临时端口等回调 | **进程内虚拟拦截**：WebViewClient 内存拦截 `localhost:*`/`127.0.0.1:*`/`zcode://*` + CookieManager 自动剥离 BDUSS/PTOKEN/ot_session，零端口零切后台 | M3（v3.0 §6.1 矩阵全保留） |
  > **★ 实证补充（2026-10-25）**：拦截到的回调 URL 无需新增任何接口——上游现成 `POST /api/session/login/callback`（session.rs:496 `submit_login_callback`，接口注释明确「壳侧捕获或远程面板粘贴」通道）即完成凭证交换，**内核零改动**。
| 7 | MCP 服务 | 上游无外部 MCP 服务端（仅内部 assistant.rs 9 工具） | **原生 Streamable HTTP MCP Server**：12 只工具（6 读免确认 + 6 操作挂「操作需确认」弹窗），RikkaHub / Termux Claude Code 可直连排查 | M5（端口形态见 O8） |
| 8 | 系统级快捷交互 | 仅开窗口操作 | **Quick Settings 磁贴 + Glance 桌面微件 + 通知栏快捷动作**：下拉点一下启停网关/一键签到；微件实时看今日 Token 与额度水位 | M5 |

---

## 2. 四层解耦架构

```
┌─ Agate Android App (dev.agate.android · minSdk 26 / targetSdk 35) ──────────┐
│  L1 原生交互与系统集成层（Kotlin + Compose + System UI）                    │
│    · 4 底部 Tab + 侧抽屉 + 22 二级页 + 3 模态 BottomSheet（v44 规范）       │
│    · 系统触点：QS 磁贴 · Glance 微件 · 常驻通知栏（今日 N 请求 · 活跃 X/Y） │
│    · 授权/过码容器：内嵌受控 WebView（内存级 URL 拦截 + Cookie 自动提取）   │
│              │ REST + 单条 /api/events SSE                                  │
│  L2 响应式状态仓库层（GatewayRepository · Single Source of Truth）          │
│    · 全局 StateFlow：CHAIN（混排队列）· OFFQ · Providers · ModelExposure    │
│    · 生命周期感知：退后台自动断开 SSE（后台 0 耗电）；SSE 异常降级轮询      │
│              │ 薄 JNI 控制面（Start/Stop/Status/TriggerTask/Version/Log）  │
│  L3 宿主调度与安全桥接层（core-gateway 模块）                               │
│    · 电源三级跳：轻保活 FGS（平时无锁）→ 流量/局域网态按需持锁 → 闹钟瞬时锁│
│    · 原生调度：AlarmManager 定点签到 + WorkManager ≥15min 周期任务         │
│    · 安全与网络：AndroidKeyStore 主密钥 · NetworkCallback 切网重置         │
│              ▼                                                               │
│  L4 增强版 Rust 网关内核（libagate_server.so · aarch64-linux-android）     │
│    · 【保留复用】26 家官方 Agent 协议/签名/伪装头 + 25 张预置卡 + 脱敏      │
│    · 【重构 1】CHAIN 扁平账号级混排路由器 + 实时选号预演引擎               │
│    · 【重构 2】/api/events broadcast 增量事件总线（替代高频轮询）           │
│    · 【重构 3】原生 MCP 端点（建议复用 8787 /mcp 路径，见 O8）             │
│    · 【重构 4】external_scheduler 可选开关（默认关：保留内核调度循环）     │
└─────────────────────────────────────────────────────────────────────────────┘
```

**双通信面原则（不变）**：数据面走 Loopback HTTP 8787（Compose 用 Retrofit 消费 `/api/*` 与 `/v1/*`），控制面走薄 JNI 6 函数（生命周期归 Android）。

---

## 3. 核心子系统优化设计

### 3.1 CHAIN 账号级混排路由（核心算法升级）

- Rust `account_store` / `routing_api` 引入 `global_queue_pos: Option<u32>`（`None` = OFFQ 未入队；`Some(i)` = CHAIN 全局顺位），实现跨提供商账号自由交错。
- 三种调度算子（完整实装 v44）：
  1. **顺序优先 `seq`**：从 `CHAIN[0]` 向下找第一个未满并发且未冷却账号，榨干前序再溢出；
  2. **负载均衡 `bal`**：全 CHAIN 可用账号间跨家全局 Round-Robin 均摊；
  3. **同家轮询 `prov`**：锁定 CHAIN 中排最前的可用提供商为主家，家内轮转，整家不可用才切下一家。
- 新增 `GET /api/routing/simulate?count=6`：实时输出接下来 6 个并发请求的分派序列（对齐 v44 选号预演条；Kotlin 侧可共享同一套纯函数做离线预演）。

### 3.2 模型暴露引擎：xp-card「客户端所见清单」

`core/models/shape.rs` 原生支持三档：
- **`model`（裸名聚合）**：去重出干净的 `gpt-4o` / `claude-3-5-sonnet`，CHAIN 自动选号跨家故障转移；
- **`provider`（前缀展开）**：全部展开为 `catpaw/gpt-4o`、`codebuddy/gpt-4o`，锁定指定家出战；
- **`hybrid`（混合）**：裸名为主，对勾选的 `pinned_providers` 额外展开前缀条目。
- 转发层 `chat.rs` **同时接受裸名与 `provider/model` 前缀**（无论展示模式）。

### 3.3 后台调度：调度混合模式（★ 对报告的实证修正）

报告原文「彻底关掉 Rust 内部死循环 Timer、全部交 Android 调度」。**M1 实证后修正为混合模式**：

- **内核调度循环保留**：`auto_checkin.rs` 的 tokio sleep 循环自带北京时间到点判定/当天去重/启动补签，进程活着时 sleep 挂起零 CPU 成本；FGS 常驻期间它是最可靠的执行者。
- **Android 闹钟定位 = 兜底而非替代**（M1 已实现）：`setExactAndAllowWhileIdle` 定点（08:00/20:00 Asia/Shanghai）→ 30s WakeLock → `trigger_task("scheduled_tick")` 点名内核执行；负责**进程被杀后的定点拉起**与 **Doze 网络窗口兜底**。
- **全关内核循环（`external_scheduler: true`）列为 M5 可选省电实验**：WorkManager 最小周期 15min 且不精确，只能承接凭证续期/目录刷新类，定点签到不可依赖；内核去重/补签逻辑与循环耦合，拆分成本高，仅在实测 FGS 常驻待机功耗超标时启动。
- 验收基线（报告口径保留）：息屏待机 1 小时耗电 < 1%（M1 实测待验证，列为 M5 回归项）。

### 3.4 AuthWebViewSheet：进程内无感拦截引擎

v3.0 §6.1 登录与验证码矩阵全保留（OAuth 回跳+轮询 / Cookie 自动抓取 / 设备码·短信·填 Key / 阿里 V3 滑块与验证码池双模 / SAF 跨端导入）。核心增强：
1. **OAuth 回跳拦截**：`shouldOverrideUrlLoading` 内存拦截 `http://localhost:*`、`http://127.0.0.1:*`、`zcode://*`，回调参数直传 Rust `login/*` 完成绑定，**全程不开临时端口**（替代上游 callback_server.rs）；
2. **Cookie 自动剥离**：KukuAI（BDUSS+PTOKEN）/ CodeBuddy / MonkeyCode（monkeycode_ai_session）/ OrcaTerm（ot_session），登录成功域名跳转时 `CookieManager.getCookie()` 提取落库；
3. **ZCode 验证码池双模铸造**：离屏 WebView 无痕补货 + 手动滑块 Sheet（通知栏提醒），水位不足触发。

### 3.5 原生 MCP Server（12 只工具）

新增 Streamable HTTP（JSON-RPC 2.0）MCP 端点，**仅绑定 127.0.0.1**：
- **读取分析类（6，免确认放行）**：`get_call_logs` / `get_request_logs` / `get_account_status` / `get_usage_stats` / `analyze_errors` / `diagnose_route`；
- **操作类（6，受「操作需确认」开关管控）**：`trigger_checkin` / `run_model_test` / `set_provider_enabled` / `set_route_strategy` / `clear_backoff` / `export_diagnostic`——开关开启时 MCP 请求挂起，经 `/api/events` 推 UI 确认弹窗，用户允许后执行返回。
- **端口形态（O8）**：倾向复用 8787 的 `/mcp` 路径（少开端口、复用闸门与 CORS 面）；独立 `:18083` 为备选，M5 实施时拍板。

### 3.6 TLS 与网络栈（M3 增强，M0 未做项补账）

- `rustls-platform-verifier` 替换 `webpki-roots` 静态根证书：信任 Android 系统证书链，兼容抓包工具（Reqable/Charles）与企业自签代理池；
- Kotlin `ConnectivityManager.NetworkCallback` 监听 Wi-Fi↔5G 切换 → JNI 通知内核重置 reqwest 连接池，消除切网后 60s 假死超时。

### 3.7 凭据加密：AndroidKeyStore 硬件主密钥（M3/M5）

Kotlin 经 AndroidKeyStore（TEE）生成 AES-256-GCM 主密钥，JNI 注入 `crypto.rs` 加密 SQLite 中 Cookie/Token。Root 或备份文件外泄，无本机 TEE 密钥不可解密。注意 Keystore 主 key 卸载重装后不可用（aigw 项目 v2.21.40 实证），需带「删 key 重建」自愈路径。

### 3.8 系统触点（M5）

- `AgateTileService`（Quick Settings 磁贴）：下拉点一下启停网关 / 一键签到；
- Glance 桌面微件：今日 Token 消耗与额度水位实时展示；
- 常驻通知栏「停止/打开」快捷按钮（M1 已实现，微件/磁贴 M5 补齐）。

### 3.9 Clash / 机场订阅子系统（✅ 拍板保留，2026-10-25）

> **决策反转**：v3.0 曾取「个人 app 无机场场景 → 不引入」；现拍板**原样保留**。实证依据：该子系统（`core/clash.rs` + `core/subscriptions.rs` + `core/sub_kernel/` + `api/proxies.rs` + `api/subscriptions.rs`）与代理池主干（`core/proxy_pool.rs`、`core/proxies.rs`）深度耦合（Clash 快照镜像、`clash_proxy_options`/`CLASH_MIXED_UID` re-export、订阅节点镜像分支），剔除反而伤及代理池；且完整保留成本近零——M0 起一直编译进 `.so`，端点全部存活。

| 层 | 内容 | 代价 |
| :-- | :--- | :--- |
| P0 原样保留（即时生效） | 代码/端点/`ui.proxy` 页全保留，零改动。无内核时 `clash_snapshot()` 诚实返回 `CLASH_UNAVAILABLE`，代理池订阅镜像分支自然降级不可用；直连与「代理不可用回退直连」不受影响 | 0 |
| P1 Android 可用化（M4 可选项） | 内核以 `libmihomo.so` 命名打进 jniLibs（`extractNativeLibs=true` → 系统解压至 nativeLibraryDir，**该目录允许 exec**）；`sub_kernel::kernel_dir` 加 Android 分支（优先 nativeLibraryDir）；CI 增「下载 mihomo android arm64 → 入 jniLibs」步骤 | ~2-3 天；APK +15~25MB |

**Android 硬约束（上游形态照搬会失败处）**：
- 上游「下载内核到数据目录 → spawn 子进程」在 targetSdk≥29 被 SELinux `untrusted_app` 域禁止（W^X：禁止 exec 数据目录文件）→ 必须走 jniLibs / nativeLibraryDir 路径；
- 「订阅内核下载/更新」端点在 Android 语义改为「随 APK 内置」，不做运行时下载（版本策略见 O10）；
- **范围界定**：保留 = 订阅管理 + 内核进程 + 混合端口（HTTP/SOCKS 本地代理，供账号出网走）+ Clash API 镜像；**不做** VPNService/TUN 全局流量接管（权限与常驻复杂度是另一量级，超出网关 app 定位）。

---

## 4. Rust 内核改造清单（相对上游 3.0.8 的全部 diff 面）

**原则不变**：Android 专用逻辑经 `#[cfg(target_os = "android")]` 或独立文件隔离，保持随时可 merge 上游主线。

| 阶段 | 改动 | 状态 |
| :--- | :--- | :--- |
| M0 | `Cargo.toml`（lib 改名 agate_server + cdylib + profile）；`src/jni_bridge.rs`（6 JNI 导出，启动序列对齐桌面壳 backend.rs::ensure_ready）；`AGENT2API_PROXY_HOME` 环境变量整体覆盖数据目录（paths.rs 零改）；logging `console_line` 挂 logcat；bin 11 处 crate 引用改名 | ✅ 已落地（首轮 CI 即过） |
| M1 | 无内核改动（bind_lan / trigger_task M0 已具备；内核自带签到调度） | ✅ 零改动 |
| M3 | CHAIN（`global_queue_pos` + 三算子 + simulate API）；`/api/events` broadcast 事件总线；xp-card shape 三档；（增强）rustls-platform-verifier、KeyStore 注入、NetworkCallback 连接池重置 | 待做 |
| M4 | §3.9 P1（可选）：`sub_kernel::kernel_dir` Android 分支（nativeLibraryDir 优先）+ jni_bridge 暴露 nativeLibraryDir + CI mihomo→jniLibs 步骤 | 待做 |
| M5 | `/mcp` 端点 + 12 工具 + 操作确认联动；（可选）`external_scheduler` 开关 | 待做 |

---

## 5. Android 工程模块结构（v3.0 §4 增补）

```
agate-android/
app/src/main/java/dev/agate/android/
  ui/           # theme(✅M2) + components(✅M2 12件) + home/provider/models/tasks/quota/reports/logs/proxy/routing/assistant/settings
  ui/pages/     # M2 新增：SkeletonPage / GatewayControlPage(M1 迁移) / ShowcasePage(走查)
  navigation/   # AgateNavHost：4 Tab + 侧抽屉 + 22 二级路由（✅M2）
  data/         # M3：Retrofit ApiService + GatewayRepository + SseEventSource(SSE，降级轮询)
  auth/         # M3：AuthWebViewSheet + CaptchaBottomSheet
  mcp/          # M5：MCP 操作确认弹窗联动（PendingIntent → /api/events 订阅）
  tile/         # M5：AgateTileService
  widget/       # M5：Glance 微件
core-gateway/   # ✅M1：Gateway.kt / AgateForegroundService / AlarmScheduler / AlarmReceiver / BootReceiver / BatteryWhitelist / GatewayPrefs
agent2api-server/  # 上游 3.0.8 内核 + jni_bridge（§4 清单）
```

---

## 6. 里程碑 M0–M5（对齐现状，合计约 11.5 周 + 增强项）

| 里程碑 | 内容（v4.0 口径） | 状态 |
| :--- | :--- | :--- |
| **M0 内核闭环** | cargo-ndk 交叉编译 + jni_bridge + 数据目录注入 + 内环 Token 守卫；Demo 跑通 /health、/v1/models、/v1/chat/completions | ✅ 真机关单（verifier/KeyStore 增强转 M3/M5） |
| **M1 保活壳层** | FGS 分级持锁 + 精确闹钟 + 开机自启 + 电池白名单 + 局域网 0.0.0.0 四端点闸门 | ✅ 真机关单（磁贴/微件转 M5） |
| **M2 设计系统** | v44 双主题 tokens + tnum + 12 通用组件 + 4 Tab/22 二级页骨架 + 组件走查页 | 🔄 实施中（`1a8cd75`+`c2a17d6`）；**SSE 总线调整为 M3 前置**（M2 验收不依赖内核改动） |
| **M3 核心链路** | **SSE 总线 + GatewayRepository（先于页面）**；控制台（Hero/自检/向导/KPI/实时队列）；**CHAIN 混排路由页 + 三模式预演 + 自愈演练**；提供商列表/详情/新增（26 官方 + 25 预置卡 + AuthWebViewSheet 无感登录 + SAF 导入）；模型页（xp-card 三模式推演/测速/映射/能力位）；访问密钥；verifier + NetworkCallback + KeyStore | 待做（3 周） |
| **M4 高阶页面** | 任务中心（待办/福利/验证码池+滑块/定时签到/保活链/周期任务/50 轮台账）；数据报表（6 档窗/365 热力图/24h 双轴/三维占比）；额度中心；调用记录 + 出网取证 Diff；应用日志；出网代理池（Clash 订阅管理随页接入，§3.9）；备份恢复中心；AI 运维助手 | 待做（3 周） |
| **M5 打磨发版** | **原生 MCP Server（12 工具 + 确认弹窗，端口见 O8）**；**磁贴 + Glance 微件**；6 语言 i18n；SQLite VACUUM；崩溃栈捕获；CI 签名发布 `Agate-v{version}-arm64-v8a-release.apk`；M1–M4 全量回归 + 待机功耗基线 | 待做（1.5 周） |

---

## 7. 开放问题

| # | 问题 | 状态 |
| :--- | :--- | :--- |
| O1 | 与 aigw fork 的关系 | ✅ 已拍板：**并行**（aigw = Kotlin 原生重写线；Agate = Rust 内核直用线） |
| O2 | 上游版本口径 | ✅ 已核实：以 **server 3.0.8** 为基线（无 3.2/3.3 实证） |
| O3 | OAuth 对内嵌 WebView 风控兼容 | 保留（M3 验证点；降级路径 = 复制链接外部浏览器 + 手动粘贴回调参数） |
| O4 | 息屏 Doze 离屏 WebView 静默过码可行性 | 保留（M4 验证点；不可行则仅前台手动滑块 Sheet） |
| O5 | rustls/ring 交叉编译工具链坑 | ✅ 已过（M0 首轮 CI 即编译通过） |
| O6 | Compose 所需 API 面完整性 | 部分核实（M2 起逐页对路由）；缺口处才动 Rust |
| **O7** | 调度取舍：WorkManager 最小 15min 周期 vs 内核循环 | **已倾向混合模式**（§3.3）：内核循环保留 + 闹钟兜底；全关模式仅作 M5 省电实验 |
| **O8** | MCP 端点形态：8787 `/mcp` 路径 vs `:18083` 独立端口 | 倾向前者（少开端口/复用闸门）；M5 实施拍板 |
| **O9** | SSE 与轮询兼容 | GatewayRepository 需支持 SSE 推送为主 + **降级轮询**（内核旧版本/SSE 异常恢复期），M3 设计数据源时一并落地 |
| **O10** | 内置 mihomo 内核版本策略（§3.9 P1）：CI 锁定版本随 APK 升级 vs 跟随上游 release 动态拉 | 倾向锁版本（体积可控、行为可复现）；P1 启动前拍板 |

---

## 附：整理记录（2026-10-25）

- **蓝本**：《Agate 相比上游 8 大超越级架构优化》报告（CHAIN/SSE/调度/证书/Keystore/WebView 拦截/MCP/系统触点 + 四层架构 + v4.0 里程碑）。
- **实证修正**（报告与现状冲突处）：① 全文端口 8080 → **8787**（Termux MCP 冲突实证）；② 「彻底关掉 Rust 死循环」→ **调度混合模式**（M1 已按此实现并验收，WorkManager 15min 精度是硬约束，全关列 M5 实验）；③ M0/M1 已完成项标注关单，增强项（verifier/KeyStore/磁贴/微件/SSE）重排进 M3/M5；④ MCP 端口 :18083 → 复用 8787 `/mcp` 优先（O8）。
- **外部走查报告裁决（2026-10-25，6 条逐项核实）**：① Rust 裁剪（cfg 隔离桌面模块）——**拒绝**：`port_conflict/selfupdate/upgrade_api` 实际引用面 10+ 文件（lib.rs、account_bootstrap、db/backup.rs 等），「5 行 stub」不成立，cfg 铺开破坏「随时 merge 上游」主线；clash/订阅裁剪部分与 §3.9 拍板直接冲突；「提速 15%+」无依据。② OAuth 回调改内存直注——**方向采纳但修正为现成端点**（§3.4 实证补充），无需新增接口。③ CHAIN 对齐——方案 §1#1 已有，报告「前端已按 CHAIN 构建」与其自述 data/ 仅一个文件自相矛盾，无新信息。④ GatewayRepository 拆三文件（ProviderRegistry / AgateApiClient / GatewayRepository）——**采纳为 M3 开工结构**。⑤ 图标资源缺失——**属实已执行**（ic_agate_logo 通知+磁贴、ic_launcher adaptive 四件套、manifest 接线；minSdk 26 无需 legacy 图）。⑥ M3 三批次路线——与 §6 一致，无新决策。报告「已落实项」复核：磁贴/strings.xml/.gitignore 属实，**「v4.0 已归档 docs/」不实**（本条即补归）。
- **保留自 v3.0**：登录与验证码矩阵、SAF 跨端导入、22 二级页清单、平台适配五处、双通信面原则、开放问题处置记录。
- v3.0 原文存档于本目录 `Agate移植方案-v3.0.md`，不再更新。
