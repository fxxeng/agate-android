# Agate（Android AI Gateway）移植方案 · v3.0 整理稿

> **整理日期**：2026-10-24 ｜ **状态**：定稿待立项（M0 未启动）
> **设计基线**：`123_v44.html`（v44 全模块交互规范，仅作视觉/状态机规范，不复用 HTML 代码） × `agent2api` 上游源码
> **一句话**：Rust `agent2api-server` 内核交叉编译成 `.so` 原样复用，外层套 Kotlin + Jetpack Compose 原生壳；UI 数据面走 HTTP 消费内核自带 API，生命周期控制面走薄 JNI，**Rust 业务代码零改动**。
> **工期**：M0–M5 六个可独立验收里程碑，合计约 **11.5 周**（原文档区间 10–14 周）。

---

## 1. 开放问题（整理时补记，立项前需拍板）

| # | 问题 | 说明 |
| :--- | :--- | :--- |
| **O1** | **与现有 Android-AI-gateway 的关系未定义** | 现有 app（v2.21.91）是 Kotlin 原生重写路线，已落地路由全套、任务中心超集、调用记录、WebView 登录 + 阿里验证码、拖拽排序等约七成 v44 功能。Agate 是**另一条架构路线**（Rust 内核直用，17 家提供商专有逻辑跟随上游、不再 Kotlin 重写）。需拍板：**替代 / 并行新项目 / 仅评估**——这决定仓库、包名占用与现有 Kotlin 资产的去留 |
| **O2** | **上游版本口径待核** | 方案写「agent2api 3.2/3.3 线」；本地已实证的上游快照是 **server 3.0.8**（Agent2API-main）。需确认上游是否真有 3.2/3.3，否则以 3.0.8 为基线修订本方案 |
| **O3** | OAuth 对内嵌 WebView 的风控兼容性 | 部分上游站点有 WebView 检测；需保留「复制链接到外部浏览器 + 手动粘贴回调参数」降级路径 |
| **O4** | 息屏 / Doze 下离屏 WebView 静默过码的可行性 | 不可行则退化为仅前台手动滑块 Sheet；列为 M4 验证点 |
| **O5** | rustls/ring 在 NDK 交叉编译的工具链坑 | M0 首日验证（ring 需要 host 侧 cc/clang） |
| **O6** | Compose 所需 API 面完整性 | 上游 Docker/Web 同构模式（ui-islands 71 岛）已证明 HTTP API 面大体存在，但 v44 的部分交互（拖拽 CHAIN、验证码池补货、保活链编辑等）需逐个对到具体路由，缺口处才动 Rust |

---

## 2. 命名与工程规范

| 维度 | 规范值 |
| :--- | :--- |
| 应用名 | **Agate**（Agent Gateway / AI Gate；桌面图标名 `Agate`，中文环境副标题「随身 AI 网关」） |
| Application ID | `dev.agate.android`（Gradle namespace 统一） |
| JNI 类 | `dev.agate.core.Gateway`（core-gateway 模块独立命名空间；JNI 符号 `Java_dev_agate_core_Gateway_*`） |
| Native 库 | `libagate_server.so`（agent2api-server 编译产物，入 `jniLibs/arm64-v8a/`；x86_64 视 O6 再定） |
| 数据目录 | `/data/data/dev.agate.android/files/agate/`（SQLite 主库、日志库、调试报文库、5 份本地备份快照） |
| 常驻通知 | `Agate 网关运行中 · :8080`；副文案实时 `今日 N 次请求 · 活跃账号 X/Y`；带「停止」「打开」快捷按钮 |
| APK 产物 | `Agate-v{version}-arm64-v8a-release.apk`，主版本号与上游内核同步便于溯源 |
| 系统要求 | minSdk 26 / targetSdk 35 |

---

## 3. 架构总览

```
Agate APK (dev.agate.android · minSdk 26 / targetSdk 35)
│
├─ ① 表现层：Kotlin + Jetpack Compose（100% 对齐 v44 视觉与状态机）
│    · 4 主 Tab（控制台 / 提供商 / 模型 / 任务中心）+ 侧边抽屉
│    · 22 个二级页 + 3 个模态 BottomSheet（提供商选择器 / 日志筛选 / 滑块验证）
│    · Lifecycle-aware Flow：前台 STARTED 轮询刷新，退后台自动挂起 UI 轮询
│
├─ ② 网络与文件桥接层
│    · Retrofit + OkHttp + kotlinx.serialization；Header 注入 X-Internal-Admin-Token
│    · SAF 文件桥：content:// ↔ cacheDir
│
├─ ③ 受控 WebView 引擎（认证 / 过码）
│    · OAuth 回跳内存拦截（localhost / 127.0.0.1 / zcode://）
│    · CookieManager 提取（BDUSS / PTOKEN / session…）
│    · 阿里云 V3 滑块铸造器（离屏静默补货 + 手动 BottomSheet）
│
├─ ④ 系统保活与调度层（core-gateway 模块）
│    · AgateForegroundService（specialUse）+ 常驻通知
│    · AlarmManager 精确闹钟 + 30s 瞬时 WakeLock
│    · WifiLock / PartialWakeLock 按需动态启停
│    └─ 控制面：薄 JNI，仅 6 个导出函数
│
└─ ⑤ 内核层：Rust libagate_server.so（aarch64-linux-android / x86_64）
     · axum + tokio + reqwest(rustls) + rusqlite(bundled)
     · 完整复用 /v1/*（chat / messages / responses）与全部 /api/* 管理模块
     · SQLite 与快照落盘于 context.filesDir/agate
```

**双通信面设计核心原则**

| 通信面 | 通道 | 承载内容 | 设计理由 |
| :--- | :--- | :--- | :--- |
| **数据面** | HTTP（`127.0.0.1:8080`） | 全部 `/api/*` 管理接口、`/v1/*` 转发、SSE 测速与助手流 | server 已有完整 API 面（上游 Docker/Web 同构模式即以此工作），Compose 用 Retrofit 直接消费，Rust 零业务改动 |
| **控制面** | JNI（6 函数） | start / stop / status / triggerTask / version / 日志回调 | 生命周期归 Android 管；JNI 只做电源开关与闹钟触发器，不碰业务数据结构 |

---

## 4. 工程模块结构

```
agate-android/
app/                        # 主应用模块：Compose UI + ViewModel + Repository + Retrofit
  src/main/java/dev/agate/android/
    ui/
      theme/                # 深/浅 ColorScheme + v44 自定义语义色 + tnum 等宽数字排印
      components/           # 12 个通用组件（徽标/并发点/拖拽行/热力图/双轴图/手风琴卡…）
      home/                 # Tab1 控制台 + 紧凑新手向导 + 健康自检面板
      provider/             # Tab2 提供商：列表/拖拽 + 详情三 Tab + 新增提供商 + 选择器 Sheet
      models/               # Tab3 模型：xp-card 客户端所见清单 + 聚合/分组 + 测速 + 映射 + 能力位 + 目录刷新
      tasks/                # Tab4 任务中心：今日待办 + 福利/验证码池/滑块 Sheet + 定时签到/保活链/周期任务 + 50 轮台账
      quota/                # 二级页：额度中心
      reports/              # 二级页：数据报表（概览 + 三维构成）
      logs/                 # 二级页：调用记录（转发请求 + 调用报文/出网取证 Diff + 筛选 Sheet）+ 应用日志
      proxy/                # 二级页：出网代理池
      routing/              # 二级页：路由（选号预演 + 候选队列拖拽 + 自愈演练 + 限制器）
      assistant/            # 二级页：AI 运维助手（快捷诊断 Chips + 工具调用气泡）
      settings/             # 二级页：设置手风琴组 + 访问密钥管理
    navigation/             # AgateNavHost 路由表
    data/                   # Retrofit ApiService + GatewayRepository（Single Source of Truth）
    auth/                   # AuthWebViewSheet（OAuth 拦截 + Cookie 提取）+ CaptchaBottomSheet
core-gateway/               # 核心保活库：JNI 桥接 + libagate_server.so 加载 + AgateForegroundService + AlarmReceiver
agent2api-server/           # 上游 Rust 内核：新增 cdylib 与 src/jni_bridge.rs（< 280 行非侵入式代码）
```

---

## 5. Rust 内核层非侵入式改造清单（预计 < 280 行）

所有 Android 专用逻辑通过 `#[cfg(target_os = "android")]` 隔离，确保随时可 `git merge` 上游主线。

**1）`Cargo.toml` 扩展**

- `[lib] name = "agate_server"`，`crate-type = ["cdylib", "rlib"]`；
- 增加 `[target.'cfg(target_os = "android")'.dependencies] jni = "0.21"`。

**2）新增 `src/jni_bridge.rs`（6 个薄控制面函数）**

| 函数 | 职责 |
| :--- | :--- |
| `nativeStart(configJson)` | 传入 `data_dir`、`port`（默认 8080）、`bind_lan`、`admin_token`、宿主 `machine_id`、宿主局域网 IP；拉起 tokio 多线程 Runtime 与 `server::serve` |
| `nativeStop()` | 触发 shutdown channel 优雅停机 |
| `nativeStatus()` | 返回运行状态、端口、运行秒数、在途流数量 |
| `nativeTriggerTask(task)` | 供 AlarmManager 唤醒时直接点名触发签到/维护协程 |
| `nativeVersion()` | 内核版本号（与 APK 版本同步溯源） |
| `nativeSetLogCallback()` | Rust 日志转发到 Android logcat / 应用日志页 |

**3）5 处平台适配点**

| 位置 | 适配内容 |
| :--- | :--- |
| `paths.rs` | 数据目录由 `GatewayConfig.data_dir` 注入（指向 `/data/data/dev.agate.android/files/agate`） |
| `access.rs`（内环免登守卫） | 校验 `X-Internal-Admin-Token` 请求头，匹配 Kotlin 启动时注入的随机 Token 即豁免 Altcha 面板登录；同时阻止局域网其他设备无 Token 越权访问 `/api/*` |
| `qoder_umid.rs`（W^X 兼容） | Android 禁止执行动态下载的二进制，桩掉子进程调用；UI 在 Qoder 国际版家规中标注「机器级组件限桌面/Linux，Android 端走 PAT 或国内版」 |
| 硬件网卡枚举规避 | Android 10+ 限制 netlink 枚举网卡 MAC；设备指纹 `machineId` 与局域网展示 IP 直接读取 Kotlin 注入的 `GatewayConfig` 值 |
| 桌面专属能力桩化 | `upgrade_api.rs` 与 `port_conflict.rs` 在 Android 下桩掉；版本检查改为弹窗跳转下载新 APK |

---

## 6. Android 系统级专项

### 6.1 登录与验证码矩阵（AuthWebViewSheet，弃用 Chrome Custom Tabs）

放弃无法读取 Cookie 的 Custom Tabs，统一用 `AuthWebViewSheet`（Compose ModalBottomSheet + 受控 WebView）：

| 模式 | 覆盖提供商 | 实现机制 |
| :--- | :--- | :--- |
| **OAuth 回跳 + 轮询** | CatPaw / AutoClaw / CodeArts / Trae / OfficeAce 果办 / ZCode | WebView 重写 `shouldOverrideUrlLoading`：拦截到 `http://localhost:{port}/callback*`、`http://127.0.0.1:*` 或 `zcode://*` 时，在内存中把回调参数注入本机 Rust 回调接口；不切外部浏览器、不惧后台杀进程 |
| **Cookie 自动抓取** | KukuAI（BDUSS+PTOKEN）/ CodeBuddy / MonkeyCode（monkeycode_ai_session）/ OrcaTerm（ot_session） | 登录成功后 `CookieManager.getInstance().getCookie(domain)` 自动提取目标键值对回填保存，免去手机端无法 F12 抓包的痛点 |
| **设备码 / 短信 / 填 Key** | WorkBuddy / Loomy / Qoder (PAT) / Antigravity / CommandCode / 25 张中转预置卡 | 原生 Compose 表单直调 `/api/login/*` 或 `/api/accounts`；设备码模式一键复制 user_code 并拉起内嵌授权页 |
| **阿里云 V3 滑块与验证码池** | ZCode 活动套餐（zcode_captcha_pool）/ Qoder 签到风控 | ① 后台静默补货：池水位 < target 时，Service 持有的离屏 WebView 自动运行无痕验证 JS 铸造令牌入池；② 手动滑块 Sheet：触发滑块拦截或点 v44「过码」按钮时弹 `CaptchaBottomSheet`，验证通过即入池并刷新票据格 |
| **跨端凭证导入（SAF）** | `cc-switch.json` / `auth.json` / `providers.json` / SQLite `Cookies` | 系统 SAF `OpenDocument` 选文件 → 拷入 cacheDir → 传路径给 Rust 解析合并（支持 Windows `enc:` 前缀自动解密） |

### 6.2 三级低功耗保活与精准调度（Doze 兼容）

1. **常驻轻保活（默认本机态）**：`AgateForegroundService`（specialUse）挂常驻通知保进程存活，但**平时不持 PARTIAL_WAKE_LOCK**，息屏正常休眠省电。
2. **按需持锁（流量 / 局域网态）**：
   - 开启「同时监听局域网（0.0.0.0）」→ 自动持 PARTIAL_WAKE_LOCK + `WIFI_MODE_FULL_HIGH_PERF`，保障同网段设备随时接入；
   - 仅本机模式（127.0.0.1）→ 仅在途推理请求期间短暂持锁，流结束即释放。
3. **定点唤醒（定时签到 / 周期维护）**：对应 v44 北京时间签到计划（如 08:00、20:00）注册 `AlarmManager.setExactAndAllowWhileIdle()`；触发时拿 30 秒超时 WakeLock 调 `Gateway.nativeTriggerTask("scheduled_tick")`，跑完签到与临期凭证刷新后立即释放。

---

## 7. v44 UI 规范 → Compose 重点模块映射

1. **单一数据源（GatewayRepository）**：全局维护 `chainAccounts`、`offQueueAccounts`、`providers`、`modelExposure`、`captchaPool` 五大 StateFlow；控制台、提供商页、详情页、路由页任意一处执行拖拽排序 / 本家置顶 / 删除账号 / 切换选号策略，四处视图与 KPI `活跃账号 7/8` 自动同步重组。
2. **模型页「客户端所见清单（xp-card）」实时推演**：对接模型暴露配置，支持「按模型（裸名聚合）/ 按提供商（provider/model 前缀）/ 混合（Hybrid 多选指定家展开前缀）」三模式；实时计算客户端 `GET /v1/models` 最终可见模型数与预览 Chips，条目 > 80 时琥珀色超长预警。
3. **新增提供商与选择器弹层（v44 去重定案）**：表单主卡片保留「最近」3 个快捷 Chips；点「更换 ❯」弹出的 ProviderPickerBottomSheet **不含**「最近使用」分组（v44 拍板冗余），直接展示搜索框、7 大能力筛选胶囊与 26 家官方 Agent / 25 张预置 API 卡（点选自动填名称、协议、Base URL、余额直查方案与无需鉴权开关）。

---

## 8. 里程碑 M0–M5（合计约 11.5 周）

| 里程碑 | 内容 | 工期 | 验收标准 |
| :--- | :--- | :--- | :--- |
| **M0 内核闭环** | cargo-ndk 编译 `libagate_server.so`（arm64-v8a & x86_64）；jni_bridge.rs；paths.rs 沙盒注入；access.rs 内环 adminToken 免登守卫 | 1 周 | 最小 Demo APK 拉起 :8080，OkHttp 跑通 `/health`、`/v1/models`、`/v1/chat/completions` 真实流式对话；记录 .so 体积与冷启动耗时 |
| **M1 保活壳层** | core-gateway 模块：前台服务（常驻通知+快捷停止）、AlarmManager 精确签到唤醒、按需 WakeLock/WifiLock、电池白名单引导、开机自启、局域网 0.0.0.0 模式 | 1.5 周 | 产出「无 UI 纯网关保活 APK」；息屏 30 分钟以上本机/局域网客户端仍可调用；Doze 下定时签到准时触发 |
| **M2 设计系统** | 深/浅双主题 Tokens、tnum 等宽数字、线性图标库、色相 Hash 字母头像 + Logo 槽位；12 个通用组件（350ms 长按抓起拖拽 + Haptic + 置顶、365 天热力图、24h 双轴图）；4 主 Tab + 侧抽屉 + 22 二级页路由骨架 | 1.5 周 | 深浅色跟随系统切换丝滑；组件 Showcase 100% 过 v44 视觉走查 |
| **M3 核心链路** | 控制台（Hero 状态卡、健康自检、3 步新手向导、KPI 四联、实时出战队列、排队水位 Chip）；提供商列表 & 详情（全动态概览、拖拽/置顶/删除真联动、19 家动态专有家规）与新增提供商（26 官方 + 25 预置卡 + 去重选择器 + SAF 导入）；AuthWebViewSheet 登录引擎；模型页（xp-card 推演器、测速、映射、能力位、目录刷新）；访问密钥页 | 3 周 | App 内闭环：网页登录抓 Cookie/Token → 模型暴露推演 → 并发测速 → 外部客户端接入对话 |
| **M4 高阶页面** | 路由页（三策略选号预演、候选队列跨家交错拖拽、自愈故障演练与倒计时回队、限制器）；任务中心（今日待办、福利中心、ZCode 验证码池 + 阿里滑块 Sheet、定时签到、日活保活链编辑、后台周期任务总览、50 轮台账）；数据报表（6 档时间窗、365 热力图、24h 双轴、三维占比）；额度中心；调用记录 & 出网取证 Diff；应用日志；出网代理池；备份与恢复中心；AI 运维助手（4 大快捷诊断 Chips） | 3 周 | v44 全部主页面与二级页 100% 接真 API，无静态假数据 |
| **M5 打磨发版** | 6 语言 i18n、折叠屏/平板自适应、SQLite VACUUM 压缩、崩溃栈捕获横幅；评估 MCP 服务扩展层；GitHub Actions：cargo-ndk + Gradle 一键构建签名 | 1.5 周 | CI 产出正式签名包，M1–M4 全量验收项回归通过 |

---

## 9. M0 启动交付物（原文档承诺）

- Rust 侧：`server/src/jni_bridge.rs` + `Cargo.toml` 补丁；
- Android 侧：core-gateway 模块的 `Gateway.kt` + `AgateForegroundService.kt` 完整源码；
- 本地一键交叉编译脚本 `scripts/build-android-so.sh`。

---

## 附：整理记录（2026-10-24）

- **勘误**：「同時に」→「同时」；应用名/图标名重复表述合并；JNI 类名与 Application ID 混写问题统一为「App ID = dev.agate.android，core-gateway 模块命名空间 = dev.agate.core」。
- **工期核算**：M0–M5 逐项相加 = 11.5 周，与原文「10–14 周」区间相容，取实算值入标题。
- **补充依据**：数据面「Rust 零业务改动」的可行性依据注明——上游 Docker/Web 同构模式（Astro ui-islands 71 岛）本身就消费这套 HTTP API，Compose 壳等价于把 API 消费者从 Web 换成原生。
- **新增第 1 节**开放问题表（O1–O6），其余决策内容未改动。
