# WPS 灵犀（lingxi）移植方案

> 状态：**P0 / P1 / P2 / P3 全部落地（`329ba90`），仅差真机 2xx 复验** · 2026-11-13
> 已拍板：先 SSE（WS 作可选通道，设置页切换）· run 生命周期上报默认开启 ·
> 首版含额度与签到 · 任务中心四项 / 智点明细 / 网页登录 / cowork WS 全部要实现
>
> 基准说明：本仓惯例以上游 agent2api 公开插件包为 ground truth，但已核 3.5.0-beta.1
> 插件包的 `crates/providers/src/providers/`（26 家）与顶层 `/upstream/` 快照，
> **都没有 lingxi / wps_sid**。所以本轮以三个互相独立的公开实现交叉验证为准。
>
> ⚠️ **两轮调研的关键教训**：第一轮只读三份 **README**，以为签到/余额/目录的
> 上游 URL 全部缺失（只给了 `daily_check_in`、`status=claimed`、`total_balance`
> 这类语义字段），于是按语义猜了一批占位路径并用开关把签到写请求 gate 住。
> 第二轮改读**实现源码**（`ketion/lingxi2api-panel::internal/upstream/lingxi.go`
> 的 `LX*` 常量表 / `lxPutExecutionStatus` / `lingxiCheckin`，
> `RobbsLuo/Coding2API::src/provider/lingxi/client.py` 的 `EP_*` / `checkin()` /
> `_frame_of`），发现两个独立实现的端点、三请求 body、执行状态字段、目录与
> 余额形状**逐字一致**。据此把占位路径全部换成真值、修正 SSE 分帧与请求头，
> 并把 `CLAIM_ENDPOINT_VERIFIED` 打开（签到真正提交，判据仍保守）。
> **结论：README 不给全 ≠ 源码不给全；下次先读实现文件。**
>
> 现在唯一未闭环的是**真机 2xx 复验**：代码能编译 ≠ 上游此刻还认这套协议。
> `adb devices` 为空，P0 的 5 条真机验收尚未执行。

---

## 0. 一句话结论

能移植，但**不是「再加一家 provider」的常规量级**：本仓 36 家 provider 里已经有
Cookie 类凭证、私有 SSE 改写、策略 200 折错、签到幂等台账这些现成零件，缺的是
「按账号 × 模型缓存上游 session/run」这一层，以及**凭证在 Android 上天然活不久、
且多账号互斥**。
**已定稿**：P0 = 粘贴凭证 + SSE 通道 + 真值口径 + **智点额度与签到**（用户要求
一并做完）；run 生命周期上报**默认开启**；WebView 登录原留 P1、cowork WebSocket
原留 P3 —— **2026-11-13 两项都已做完**（用户明确否掉「按拍板排除」的口径，
见 §6 末「P1/P2/P3 落地状态」）。WS 仍**不是默认通道**：它以「设置 → 灵犀通道」
的开关启用，默认走 SSE。

**P0 已全量落地**（本地 `15dea1f` → `253787a` → `70ceffd` → `80f9c46`，
远端同树 `ee41d19` → `c5efb3a` → `96a4b93` → `fc9f1ff` → `0283a06`）：
三轮共 26 个文件、71 条灵犀单测，`ProviderKind::Lingxi` 已成为第 37 家，
从粘贴凭证到三请求、私有 SSE 翻译、额度、签到台账全部接通，且协议形状
已按两个公开实现的源码校正（§1 末）。
**唯一未闭环的是真机 2xx 复验** —— `adb devices` 为空，§6 的 5 条验收
尚未执行：代码能编译 ≠ 上游此刻还认这套协议。

---

## 1. 公开渠道调研

| 项目 | 语言 / 定位 | 对本仓的价值 | 可信度 |
| --- | --- | --- | --- |
| `ketion/lingxi2api-panel`（用户给的参考） | Go，灵犀单渠道网关 + Web 面板；基于 `workbuddy2api-panel` 架构，**流量治理层原样保留、只换上游协议** | 「已知限制」7 条的原始出处；三请求协议、UA/设备头、任务中心、计费开关 | 高，README 逐条给了实测对照实验 |
| `RobbsLuo/Coding2API` | Python/FastAPI，10 渠道（CodeBuddy 国内/国际、TRAE、Zen、Kilo、Qoder、CodeArts、OfficeAce、小浣熊、**灵犀**）OpenAI/Responses/Anthropic 兼容 | **与本仓 provider 集合高度重合**，协议细节最全：工具 schema 压平、usage 口径、run 缓存、签到假成功、同浏览器互斥、ToS 登记 | 最高，且 `issues/38` 是灵犀落地记录 |
| `DGZSbot/workbuddy2api-panel` / `linguo2625469` fork / `Sliverkiss/workbuddy2api` | Go，CodeBuddy 账号池网关（灵犀版架构母本） | 账号池 / 熔断冷却 / 会话粘性 / 加权选号的工程范式 | 中；**母项目已于 2026-10-21 停维护**，理由是「官方正在收紧对非官方客户端的识别」——这条本身是风险信号 |
| `xfyang728/dsh-router-wpslingxi` | TS，DSH 路由插件（WPS 灵犀供应商），走 **cowork WebSocket** 通道 | 第二个协议候选；真机实测的能力矩阵（思考 / 工具 / 沙箱执行）、帧协议、`lingxi.kdocs.cn` host、额度字段口径 | 高（真机实测），但它是 Agent 插件不是网关 |
| `OdysseyFather/lingxi` | 灵犀 Web 端（禁用功能清单） | 非同类网关，只作「哪些能力在 Web 端被禁用」的边界参考 | 低，不进实现依据 |
| 本仓上游 agent2api 3.5.0-beta.1 | Rust 插件包 | **否定证据**：无 lingxi，本轮不能照抄上游架构，需原生移植 | — |

### 三份实现互相印证的事实（可直接当判据）

- 凭证只有 `wps_sid`（`V02S` 前缀 Cookie），**无 OAuth、无 refresh 接口**，失效只认上游 401。
- 一次对话三请求：`POST /api/aioffice/v1/cowork/sessions` → `POST /api/public/v1/credits/runs` → `POST /api/public/v1/agent-runtime/chat/completions`（私有 SSE）。
- 出站必须带 `x-kso-*` 设备头，UA 对齐桌面客户端 `Lingxi Claw/1.3.36`；设备 ID 按账号稳定派生。
- 正文在 `choices[].text`（不是 `delta.content`），**不发 `[DONE]`**，`usage.total_tokens` 是当前上下文大小。
- 上游**按 run 计费**：一个 run 内多次调用只扣一次智点 → 会话 + run 按「账号 × 模型」缓存 1 小时、不滑动续期、并发单飞。
- 签到重复领取回 **200 假成功**（`result=ok` 但余额不动）。
- 内容审核命中时上游回 **HTTP 200 兜底话术**。

### 来源冲突 → **读源码后收敛为已定事实**（2026-11-13 第二轮）

| 冲突点 | 最初 A 说 | 最初 B 说 | 源码定论（Go / Python 两处逐字一致） |
| --- | --- | --- | --- |
| API host | `lingxi.wps.cn`（登录页） | `lingxi.kdocs.cn`（API） | **主域 `wps.cn`，回落域 `kdocs.cn`**；401/403/404 也回落（kdocs 账号的 cookie 只在回落域有效）。聊天必须沿用「建 run 成功的那个域」 |
| 额度字段 | `total_balance` 直接进余额 | 累加 `bonus_credits[]+purchased_credits[]` | **`data.total_balance` 是主口径**，批次只是「积分构成」明细；解析仍兼容批次回落。`expire_time` 是 UTC+8 **字符串** `YYYY-MM-DD HH:mm[:ss]`，不是毫秒数 |
| 模型目录 | `/v1/models` 的 `key` | `sessions/plans` 的 `data.models` | **`GET /api/aioffice/v1/sessions/plans` 的 `data.plans` + `data.models` 两组**，按此顺序合并去重 |
| 协议通道 | `agent-runtime` 私有 SSE | `cowork/.../ws` WebSocket | 见 §4，已定先 SSE。**每帧是单行 `data:`**，Go 用 `bufio.Scanner`、Python 明说不能用空行分帧 |
| 签到/任务/余额/目录 URL | README 未给出 | README 未给出 | **源码全给了**：见下表 |

### 源码给出的完整端点表（`lingxi/endpoints.rs` 的唯一来源）

| 动作 | 方法与路径 |
| --- | --- |
| 建 cowork 会话 | `POST /api/aioffice/v1/cowork/sessions` |
| 领 credits run | `POST /api/public/v1/credits/runs` |
| 私有 SSE 对话 | `POST /api/public/v1/agent-runtime/chat/completions` |
| run 终态上报 | `PUT /api/public/v1/sessions/{session_id}/execution_status`（⚠️ 用 session_id，不是 run_id） |
| 智点余额 | `GET /api/public/v1/credits/balance`（无 query） |
| 模型目录 | `GET /api/aioffice/v1/sessions/plans` |
| 任务中心状态 | `GET /api/public/v1/tasks` → `data.tasks[]` 按 `task_key` 找、`status == "claimed"` |
| 领取签到 | `POST /api/public/v1/tasks/daily_check_in/claim?date=YYYY-MM-DD`，body `{}` |
| 领取任意任务（P2） | `POST /api/public/v1/tasks/{task_key}/claim`，body `{}`（只有签到带 `?date=`） |
| 智点明细流水（P2） | `GET /api/public/v1/credits/transaction/list?type=&income_cursor=&expense_cursor=&limit=` |
| 会话消耗明细（P2） | `GET /api/public/v1/credits/transaction/expense?session_id=&cursor=&limit=` |
| 建群 / 解散（P2 群聊任务） | `POST /api/imchat/v1/channels`（`{session_id, expect_model:"plan_pro"}`）、`DELETE …/{channel_id}` |
| cowork WS（P3 对话通道） | `wss://lingxi.kdocs.cn/api/aioffice/v1/cowork/sessions/{id}/sandbox/completions/ws` |
| 群聊前置 WS（P2） | `wss://lingxi.kdocs.cn/api/aioffice/v1/sessions/{id}/completions/ws`（建群前该会话必须收过一次 `user.input`） |

三请求 body、执行状态字段（`group_id`/`status`/`run_token`/`lease_seconds`/
`terminal_reason`）、`x-cc-session-id`/`x-cc-run-id` 头、统一包络
`{result:"ok", data:{…}}` 也全由源码给全；**第一批按语义猜的那批形状已作废**。

⇒ 验收边界只剩一条：**代码能编译 ≠ 上游此刻还认这套协议**。真机抓到 2xx
且字段对得上仍是必须补的一步；在此之前，界面与日志只读真算过的数值，
读不出就降级，签到失败只回「待确认」不写「已领取」。

---

## 2. 本仓现状（已核，路径:行号）

**没有灵犀相关代码**：全仓 grep `lingxi|wps_sid|灵犀` 只命中 `atomcode/usage.rs`、
`lobsterai/usage.rs`（与 WPS 无关的其它词），`upstream/` 与 `updates/` 快照也无命中。

新增一家 provider 的接线点（以 `quwork` 为模板，其目录 11 个文件：
`adapter/chat/checkin/client/credentials/endpoints/mod/models/models.json/refresh/usage`）：

| 层 | 文件 | 现状 |
| --- | --- | --- |
| 注册表 | `core/providers/mod.rs:215`（`ProviderKind`）、`:723`（`PROVIDERS`）、`:924`（`kind_from_id`）、`:982`（`kind_id`） | 分类唯一判据是插件包 `provider_category`；本轮上游无该包 → 按「Agent」对称归类并在文档注明依据 |
| 适配器 | `core/providers/adapter.rs:384`（`ProviderAdapter` trait）、`:1252`（`adapter_for`） | trait 的 `list_models` **同时喂路由候选链与广告清单**，不能在这里按偏好过滤 |
| 账号存储 | `core/account_store/*_accounts.rs` | schema v16，`AccountStore::load()` 返回 `Arc<AccountState>`，写路径须 `(*state).clone()`，失效判据 `accountsRevision` |
| 凭证归一 | `core/providers/doubaowork/login.rs:77`（`normalize_import`）、`core/account_transfer/normalize.rs:71` | 粘贴 Cookie 三种形态（裸 sid / 单键 / 整段）直接复用这套 |
| API / 登录 | `api/accounts.rs`、`core/login.rs`、`http.rs`（路由只认 `{id}`） | 三段式 `login/start|wait|cancel` 是通用通道 |
| 客户端预设 | `data/PresetRegistry.kt:563`（quwork 范例）、`AuthWebViewSheet.kt:96-129`（Cookie 收尾） | `LoginKind.WEBVIEW` + `webLoginCookies` + `sessionCookieNames` 现成 |
| 图标 / 预设分类 | `res/drawable-nodpi/provider_*.png`、`PresetRegistry.kt` | 需新增 256×256 图标；**不要**把上游那张 48×48 放大 |

**可直接复用的零件**（这是本轮最大的便宜）：

- SSE 帧改写：`core/upstream/sse.rs`（`FramePolicy` / `push` / `finish` / `sse_frame`）、`aggregate.rs`。
- 工具 schema 压平：`core/protocol/antigravity_schema.rs:43`（`$ref/anyOf/oneOf/allOf` 展开）—— 只在本家出站启用，别改公共行为。
- 策略 200 折错：`core/upstream/images.rs:266,315,370`、`provider_loop.rs:1375`（`ContentBlocked` 换中性提示词重试一次）、`:1786`（拒答不采纳）。
- 会话粘性：`core/upstream/route_state.rs:88` 的 `sticky: 会话键 → (账号 id, 时刻)` —— **这是账号级粘性，不等于上游 session_id 缓存**，后者要新增。
- 签到幂等台账：`core/providers/doubaowork/checkin.rs:25,105,415`、`rewards.rs:135` —— 「先落盘 pending 再发 claim；只有明确拒绝才清；到账以官方状态为准」整套判据可直接照搬。
- 额度摘要：走现有 `UsageCache`-类快照口径，**不把 `/api/accounts/usage` 塞进 5s 快照**（真打上游撞风控）。

**必须新增的零件**：按「账号 × 模型」缓存 `(session_id, run_id)` 的 1h 不滑动缓存 +
并发单飞 + 失效重建一次；`x-kso-*` 头族与设备 ID 稳定派生（复用 `core::hostinfo`，
别写第二份指纹来源）。

---

## 3. 已知限制逐条处置（重点）

图例：**放大** = Android 上比桌面更严重；**缓解** = Android 上比桌面更容易；**不做** = 平台不适用。

| # | 限制（出处） | Android 上的变化 | 本仓处置 | 验收判据 |
| --- | --- | --- | --- | --- |
| L1 | 无 OAuth；`wps_sid` 静态 Cookie；**寿命约半天~一天，重新登录必换新值，无续期接口**（panel 已知限制 1、2） | 缓解：WebView `CookieManager` 读得到 HttpOnly，比桌面 CDP 抓 Cookie 库简单 | 失效判据**只认上游 401**，不按时间猜过期（与豆包台账同一纪律）；账号行显示「需重新登录」而非「已过期 N 小时」 | 同一 sid 换头族打 `/credits/balance` 全 401 才标死号；能 200 的不许停用 |
| L2 | **同一浏览器只保留最近一次登录**：同 profile 登录另一个 WPS 账号，上一个 `wps_sid` 立刻 401（Coding2API 实测） | **放大（头号硬约束）**：Android `CookieManager` 是进程级全局、无法像桌面 Chrome 独立 profile 隔离；且 `AuthWebViewSheet.kt:232` 每次登录主动 `removeAllCookies()` | 多账号**只能**靠粘贴，或「逐个登录 → 立刻落盘 → 立刻清空」；账号页必须如实标注「本家多账号互斥」，不做「一键验证全部」的假象 | 连续导入两个不同账号后，前一个账号下一次探活必须 401 且被标「需重新登录」，不许继续参与调度 |
| L3 | 浏览器登录仅限本机部署，远程服务器上浏览器弹在服务器端（panel 已知限制 3） | **缓解**：本仓网关与 WebView 同在手机回环，天然满足 | 可以做 WebView 登录；但受 L2 约束，只作为「单账号便捷入口」 | 登录成功 → 落盘 → `/credits/balance` 200 |
| L4 | 客户端独占 Cookie 库；`v20` app-bound 加密不支持（panel 已知限制 4、5） | **不做**：Android 无 WPS 桌面客户端 Cookie 库，也没有 DPAPI | 移植时明确不搬 `scripts/import_lingxi.py` / CDP 登录；`AuthChannel` 只留 `PASTE` + `EMBEDDED` | 预设里不出现 `IMPORT_FILE` 这类死入口 |
| L5 | 提示词超约 **320KB** 直接 `400 AI服务异常`，不提示「太长」（panel 已知限制 6） | 同 | 出站前按序列化字节数预检，超限本地返 400 并写清「请求体过大（约 320KB 上限）」，**不发上游、不换号** | 构造 330KB 请求 → 本地 400，日志里没有上游请求 |
| L6 | 内容审核命中时上游 **HTTP 200 兜底话术**（panel 已知限制 7） | 同 | 帧改写层识别兜底话术 → 折 `ContentBlocked` / `finish_reason: content_filter`；**换号无用**，不进轮换 | 200 兜底响应 → 下游拿到明确错误，且 `tried_ids` 不增加 |
| L7 | 计费按 run，必须上报 run 终态才结算；参考项目 `report_lifecycle` **默认 false（不扣费）** | 同 | **用户已定：默认开启上报**（§8）。与官方客户端行为一致，避免网关表现得像「不计费」而与上游结算口径不一致；界面如实写「已上报 run 生命周期」 | 开启前后各跑一轮，官方流水出现 `-1`；关闭时界面不得暗示已计费 |
| L8 | 会话 + run 按账号 × 模型缓存 1h、不滑动续期、并发单飞；`SQLNoRows`/会话不存在时丢弃缓存**重建一次** | 同 | 新增 `lingxi/session.rs`；缓存键 `(账号 id, 模型)`，TTL 固定、单飞用 `tokio::sync::Mutex` 或等价；重建只一次，第二次失败交 `OutboundFailure` | 单测钉住：同账号同模型并发 10 次只建 1 次 session；缓存失效重建一次后不再重试 |
| L9 | 私有 SSE：正文 `choices[].text`、无 `[DONE]`、`usage.total_tokens` 是上下文大小、`cached_tokens/reasoning_tokens` 扁平且方向相反 | 同 | 复用 `sse.rs` 的 `FramePolicy` 做逐帧改写；usage **按 prompt + completion 重算**，否则报表虚高几十倍 | 单测给出上游原始帧 → 断言下游帧形态、补 `[DONE]`、usage 数值 |
| L10 | 工具 schema 含 `anyOf/oneOf/$ref`/数组型 `type` 会让整轮「AI服务异常」；`max_tokens` 进 `base_llm_arguments`；`reasoning_effort` 顶层和 `llm_arguments` 各一份 | 同 | 出站前只对本家启用 `antigravity_schema.rs` 的压平；思考档位复用 `model_rules::reasoning`，别另抄一份档位表 | 带 `$ref` 的 tools 请求能正常返回，不出现「AI服务异常」 |
| L11 | 签到重复领取回 200 假成功（`result=ok` 余额不动） | 同 | 照搬豆包台账：**先落盘 pending 再发 claim；奖励按领前/领后余额差算，差 0 时读任务状态 `daily_check_in` 判「已签」；绝不看 HTTP 200**；待确认文案不许含「已领取」 | 单测覆盖「200 但余额不变」「明确拒绝」「传输失败保留台账」三态 |
| L12 | 错误语义：401 凭证死、402 额度尽、404 短冷却、429 **模型级**、`AuditFailed` 换号无用 | 同 | 映射到本仓 `OutboundFailure` 现有分类；429 顺延池按模型收窄（既有判据），审核类**不换号** | 各类错误各一条单测 |
| L13 | 模型目录字段是 `key` 不是 `id`，含 `multiplier/max_context_tokens` | 同 | 在 `lingxi/models.rs` 归一到 `list_item` 同一形态；倍率直接当 `credits`（参照 `lobsterai/models.rs:235`） | `/v1/models` 条目与目录逐条对得上，不重复广告 |
| L14 | **ToS 灰区**：复用网页/桌面登录态 + 伪装客户端头 `X-Client-Type: lingxi_claw`；母项目因官方收紧识别而停维护 | 同 | 合规红线，**不替用户做判断**：仅本机自用、不做公网部署 / Key 分发 / 额度售卖；`x-kso-*` 头族只按公开实测值如实设置，不做反检测对抗 | 文档与界面明示「非官方接口，可能随时失效」 |
| L15 | Anthropic `cache_control` 透传在灵犀**未验证**；任务中心 4 任务含群聊 WS（建群前须收到 `user.input`，`expect_model=plan_pro`） | 同 | 不给灵犀透传 `cache_control`；4 个任务放 P3，首版只做 `daily_check_in`，群聊任务不做 | 首版无 WS 依赖；Anthropic 出口对灵犀不带 `cache_control` |

---

## 4. 协议选型（已定：先 SSE）

| 通道 | 能拿到什么 | 代价 | 与本仓契合度 |
| --- | --- | --- | --- |
| A. `agent-runtime/chat/completions` 私有 SSE（两个网关项目用） | 正文、思考增量、usage | 三请求 + run 缓存 + 逐帧改写 | **高**：本仓已有 `sse.rs` 帧改写链、`provider_loop` 轮换/冷却、`ContentBlocked`，改动收敛在一个新 provider 目录里 |
| B. `cowork/.../ws` WebSocket（DSH 插件用） | 思考 + 工具调用 + 真沙箱执行、产出文件回传 | 新建 WS 帧状态机（`run.start`/`component.*`/`run.end`）、队列/唤醒/只 close 一次，插件作者明确列了 4 个流控坑 | 中低：本仓是**网关**不是 Agent 运行时，工具执行/沙箱文件回传不在职责内，收益换不来复杂度 |
| C. `aigc/expert_completion` SSE | — | 会被劫持成 deep research 流水线（DSH 实测） | 不选 |

**推荐 A**，B 留作 P3 可选。理由是「不为对齐参考项目上无收益改动（判据：规模同量级）」：
B 的能力矩阵很漂亮，但本仓下游是 OpenAI 兼容客户端，不需要把沙箱执行渲染成 Markdown 正文。

**决议（2026-11-13）**：先做 A；B（cowork WS）留 P3，届时按「是否真需要工具调用 /
真执行」重新评估，不进首版验收。

---

## 5. 落点清单（已按实际落地校准）

**Rust 内核（新建 `core/providers/lingxi/`，12 文件）**
`endpoints.rs`（**源码核实的完整端点表 + 双域回落 + 头族 + `CLAIM_ENDPOINT_VERIFIED`
一键停发开关**）、
`credentials.rs`（`wps_sid` 三形态归一 + 按 uid 稳定派生 device id）、
`models.rs`（`key` / `multiplier` 归一、小写去重、`remote_refreshed` 读数）、
`stream.rs`（`text→delta.content`、靠 `finish_reason` 补 `[DONE]`、usage 重算、
流收尾上报 `completed`）、`classify.rs`（401/402/404/429/审核、320KB 预检）、
`session.rs`（账号×模型 1h 不滑动缓存 + 失效重建一次）、
`usage.rs`（`total_balance` 与批次累加两种形状）、
`checkin.rs`（台账状态机 + `claim_for_account` 编排）、
`chat.rs`（三请求 + `running`/`completed` 五步 body、工具 schema 压平）、
`client.rs`（统一出站、三请求编排、**tokio Mutex 并发单飞**、任务领取/回查）、
`adapter.rs`（`ProviderAdapter`：`query_usage` / `refresh_models` / `prepare_body` …）、
`mod.rs`。

**Rust 内核（修改）**
`providers/mod.rs`（枚举 / `PROVIDERS` / `kind_from_id` / `kind_id`）、
`providers/adapter.rs`（`UpstreamResponse::LingxiSse` + `adapter_for`）、
`providers/catalog.rs`（`refresh_meta` 的 Lingxi 分支 —— 新增变体必须补这个穷尽 match）、
`upstream/provider_loop.rs`（响应协议分派）、`upstream/translate.rs`（`LingxiToChatStream`）、
`auth_http.rs`（**修 `send_raw` 只认 POST 的 bug**，PUT 不再被静默降级成 GET）、
`account_store/lingxi_accounts.rs`（新建：粘贴落库 + 公开形态 + `lingxiCheckin` 台账）、
`account_store/mod.rs` / `store_view.rs`（模块声明与公开视图分派）、
`api/accounts.rs`（粘贴导入分支，明确拒绝桌面态导入）、
`core/billing/checkin.rs`（签到分派）、`core/auto_checkin.rs`（`CHECKIN_PROVIDERS` 21→22）。

**Kotlin 客户端（修改）**
`data/PresetRegistry.kt`（`LoginKind.PASTE` + 只开 `AuthChannel.PASTE`，分类 Agent，
如实写「非官方接口 / 凭证约半天失效 / 多账号互斥」）、
`data/AccountOps.kt`（整段 Cookie 交内核归一）、
`ui/components/AgateProviderAvatar.kt`（图标表）、
`res/drawable-nodpi/provider_lingxi.png`（新 256×256 图标）。
`auth/AuthWebViewSheet.kt` **一行未改**（首版不做 WebView 登录）。

**与最初规划的差异**（都已按判据收口，不是遗漏）：
· 预设从 `WEBVIEW` 改成 `PASTE` —— 用户拍板首版只做粘贴；
· 没建 `refresh.rs`，探活并入 `client.rs::probe`（不另造一个文件）；
· `provider_loop.rs` / `translate.rs` **改了**（当时规划以为能完全复用，实际新增
  响应协议必须加一个变体 + 一台翻译器）；
· `core/login.rs`、`http.rs` 没动 —— 粘贴走 `api/accounts.rs` 通用分支即可。

---

## 6. 分阶段与验收

| 阶段 | 内容 | 验收判据 |
| --- | --- | --- |
| **P0 协议 / 凭证 / 额度 / 签到**（首版，用户已定） | 粘贴 `wps_sid` → 三请求（含 **run 生命周期上报默认开启**）→ SSE 改写 → usage 重算 → 401/402/429 分类 → 320KB 预检 → 审核 200 折错；session/run 缓存 + 单飞；模型目录；**智点余额（两种形状容错）+ `daily_check_in` 幂等台账 + 任务状态回查** | `cargo test --lib` 新增 ≥14 条纯函数单测（帧改写、usage、缓存单飞、三形态凭证、长度预检、错误分类、额度两形状、台账三态）；离线编译客户端；CI 出包；真机 5 条：能对话 / 多轮不丢上下文 / 关掉账号后 401 不换号误判 / 额度读得出（读不出写「暂不可用」不拿 0 冒充）/ 签到 200 假成功不写「已领取」 |
| **P1 登录与治理**（已落地 2026-11-13） | 网页登录抓 `wps_sid`（`supports_web_login` + `build_login_url` + `exchange_login_code`：Cookie 载荷 → 归一 → **上游探活** → 落账号；L2「本家多账号互斥」的 401 文案随探活一起给出）、账号探活「验证全部」（既有链路）、429 顺延池按模型收窄（既有链路） | 真机：网页登录 → 落盘 → 余额 200；二次登录不同账号后旧号被标「需重新登录」 |
| **P2 观测增强**（已落地 2026-11-13） | 智点明细流水对账（`ledger.rs` + `GET /api/accounts/{id}/lingxi/ledger`，双向游标；上游延迟归账，与余额两套口径分开显示）、任务状态看板（`tasks.rs` + `/lingxi/tasks`，4 个任务 + 汇总 + 领取回读）、代跑一次性任务（专家 / 客户端 / 群聊） | 明细缺失时降级说明（`upstreamStatus` + `error`），不拿估算值冒充真实流水；领取后**只有** `status == "claimed"` 才写「已领取」 |
| **P3 可选**（已落地 2026-11-13，默认关闭） | `ws.rs` cowork 通道（工具执行折 Markdown、多轮压平、无 token 用量）+ `channel.rs` 设置在「设置 → 灵犀通道」里切；默认 `sse`，切换后下一条请求即生效 | 见 §6 末「P1/P2/P3 落地状态」 |

### P0 落地状态与唯一剩余验收项（2026-11-13 第二轮）

**代码侧全量落地**：两轮共 26 个文件；灵犀模块 71 条单测钉住端点常量、
三请求 body、执行状态字段、双域回落、逐行 SSE 分帧、目录/余额/任务解析、
签到台账三态。Kotlin 离线编译与单测通过；CI（行尾体检 → `cargo test --lib`
→ 交叉编译 → gradle 单测 → assembleDebug）通过。

**协议形状已不是猜测**：第二轮把两个独立公开实现的**源码**逐处复核，替换了
第一批按 README 语义猜的占位路径、请求体、`execution_status` 字段与 SSE 分帧
（差异清单见 §1 末与 `lingxi/*` 各文件模块头）。`CLAIM_ENDPOINT_VERIFIED`
已打开，签到会真正提交领取请求；Go 的 `autotask.go` 与 Python 的 `checkin()`
都是真实在跑的同一条链。

**唯一剩余的是真机 2xx 复验**：`adb devices` 为空，上表 5 条真机验收一条都
没执行。代码能编译 ≠ 上游此刻还认这套协议，所以当前包的诚实口径是：

- 粘贴 `wps_sid` 后能建会话、发聊天、走私有 SSE 翻译；主域 `wps.cn` 不通时
  自动回落 `kdocs.cn`，并沿用「建 run 成功的那个域」发聊天；
- 额度读不出一律 `available: null`（界面「暂不可用」），**不会**出现 0；
- 签到先读 `data.tasks[]`，`status == "claimed"` 直接回报「今日已签到」，
  否则 `POST .../daily_check_in/claim?date=…`；**200 假成功不算到账**，
  只有领前/领后余额差或官方任务状态能推进台账；只有明确拒绝才清台账；
- 真机按上表 5 条逐项复验，尤其「200 假成功不写已领取」与「同一 run 内
  多次调用只扣一次智点」（并发单飞 + 缓存 TTL + `running`/`completed`
  上报的联合结果）。若上游已改协议，把 `CLAIM_ENDPOINT_VERIFIED` 改回
  `false` 即可一键停发写请求，链路与判据都不用动。

每阶段按本仓既有节奏：先集中改完 → 本地 Kotlin 编译 + 单测 → 推送 → CI
（`workflow_dispatch`，`cargo test --lib` + 交叉编译 + gradle 单测 + assembleDebug）
→ 逐条目比对 APK 确认改动范围 → 单次批量提交。

### P1 / P2 / P3 落地状态（2026-11-13 第三轮，`329ba90`）

上一版把 P1/P2/P3 一起标成「范围外」，用户明确否掉了这个口径 —— 三项都按
原方案落地，**没有一项是「按拍板排除」**。三个新增拍板与落点：

| 拍板 | 落点 |
| --- | --- |
| P3 用**新增「灵犀通道」设置项**启用（不是按请求自动判据、不是账号级开关） | `providers::lingxi::channel`（kv 固定键 `lingxiChannel`，已登记 `RESERVED_KV_KEYS`）+ `GET/POST /api/lingxi/settings` + 设置页「5b. 灵犀通道」 |
| P2「灵犀 4 个任务」看板放**签到中心页内新增分组** | `CheckinCenterPage.kt` 的 `LingxiCard`（手点「查询才打上游；可领→「领取」、`auto && !claimable`→「代跑」） |
| P2「智点明细流水」放**账号详情页内** | `ProviderDetailPage.kt` 的 `LingxiLedgerDialog`（all/income/expense 三档、双向游标「加载更多」、`amount >= 0` 红、负数绿） |

代码侧：Rust 新增 4 个模块（`tasks.rs` 544 行 / `ledger.rs` 313 行 /
`ws.rs` 824 行 / `channel.rs` 169 行）+ `api/lingxi_api.rs` 448 行，接线 6 处
（`api/accounts.rs` 四条子路径、`api/mod.rs`、`http.rs` 一条路由、`server/mod.rs`
的 `channel::install`、`db/schema.rs` 保留键、`core/login.rs` 的 Lingxi 分支、
`adapter.rs` 的四个覆写）；新增 25 条单测（tasks 6 / ledger 7 / ws 9 /
channel 3 / api 4）。

三条与 P0 一脉相承的判据（都写进了模块头，改代码时别绕过）：

1. **领取不看 HTTP 200**：`claim()` 只回「上游怎么说」，`lingxi_api` 领取后
   重新 `GET tasks` 回读，`status == "claimed"` 才写「已领取」；否则按
   `explicitReject` / `sent` 分三种诚实话术。
2. **读不出就说读不出**：任务 / 明细读失败仍 200，但带 `upstreamStatus` +
   `error`，界面写「暂不可用」，不拿空表或 0 冒充。
3. **代跑必须产生真实行为**：专家 / 客户端任务走「独立会话 → 领 run →
   真实对话（自然语言 + `max_tokens=4096`）→ `running`/`completed` 上报」；
   群聊任务走「`cowork_web` 会话 → completions WS 发一次 `user.input` →
   建群 → 解散」（缺 WS 前置动作时上游必 400「参数错误」）。

**仍未验证（阻塞项，与本轮代码无关）**：`adb devices` 为空，所以下列真机
验收一条都没跑 —— ① 网页登录 → 落盘 → 余额 200；② 旧号被顶后探活 401；
③ 看板 4 行 + 领取回读；④ 群聊代跑建群/解散；⑤ 明细翻页；⑥ 切 WS 后请求
真的走 `wss`。其中③④⑤⑥都是**新的写/读端点**，理论上仍可能被上游改过。

**一处如实说明的取舍**：WS 通道拿不到 token 用量（`run.end` 只给智点消耗），
所以那条通道的请求日志 token 记 0，**不拿估算值冒充**；界面在设置页写明了
「多轮压平 + 无 token 用量」是上游限制。

---

## 7. 明确不做（别重复论证）

- 桌面 CDP 浏览器自动登录、从 WPS 桌面客户端 Cookie 库导入、`v20` app-bound 解密（平台不适用）。
- 公网部署、HTTPS 反代、Key 分发 / IP 白名单 / 额度售卖（本仓是本机回环网关，且是合规红线）。
- ~~群聊任务、专家模式 / 客户端任务~~ —— **2026-11-13 已推翻**：三项都实现了
  （`tasks.rs::complete` + `ws.rs::complete_group_task`），用户明确要求落地。
  仍在「不做」里的只剩**沙箱产物回传**（把 `c_full_content` 里的文件取回本地）：
  那需要按 id 再打一次上游下载端点，本仓没有证据链，且 `c_execute_code` 的
  `output` 已经能折成正文给人看。
- 生图通道接入（灵犀生图不在本轮证据链里；本仓生图链有独立准入判据，别顺手塞）。
- 为对齐参考项目而搬整套 Go/Python 的账号池 / 面板架构 —— 本仓 `provider_loop` +
  `route_state` 已覆盖轮换、冷却、退避、熔断、粘性，重复实现等于两套事实源。

---

## 8. 已拍板的三件事（2026-11-13）

1. **协议通道：先 SSE，WS 留 P3**。首版只实现 `agent-runtime` 私有 SSE + 三请求；
   cowork WebSocket 不与首版混做，P3 按「是否真需要工具调用 / 真执行」重新评估。
2. **run 生命周期上报：默认开启**。与官方桌面客户端结算行为一致；不沿用
   `lingxi2api-panel` 的 `report_lifecycle=false` 默认。界面如实标注「已上报 run
   生命周期」；关闭时（若日后提供开关）**不得**暗示已计费。
3. **首版范围：粘贴凭证 + 额度 + 签到**。P0 一次做完 `usage.rs` / `checkin.rs`，
   不先做 WebView 登录（留 P1）。因此 L2「本家多账号互斥」的界面提示也顺延到 P1，
   首版只需在预设文案里如实写清「凭证约半天失效、多账号互斥」。

**由决策派生的三条约束**

- 首版**不含** WebView 登录 ⇒ `AuthWebViewSheet.kt` 一行都不用改；
  `PresetRegistry.kt` 的预设只开 `PASTE`，别放出点不动的 `EMBEDDED` 入口。
- 计费默认开启 ⇒ `chat.rs` 的三请求编排必须把 `PUT execution_status {running}` 与
  流结束后的 `{completed}` 都做进去，**结算触发点不能漏**；`session.rs` 的
  session/run 缓存因此不只是省请求，还直接决定扣几次智点。
- 首版含额度与签到 ⇒ P0 就要处理两处来源冲突（额度字段两种形状、签到 200 假成功），
  探针端点与台账单测不能拖到 P1。

> **2026-11-13 更正**：上面第 1 条（WS 留 P3）与第 3 条（登录留 P1）**已被推翻**。
> 用户原话是「没有排除，继续推进实现」—— P1/P2/P3 一并做完，只是 WS 保持
> **非默认**（设置项开关）。保留本节是为了记住当时的取舍理由，不是当前范围。

---

## 9. 来源

- https://github.com/ketion/lingxi2api-panel （README「已知限制」「核心机制」「计费」）
- https://github.com/RobbsLuo/Coding2API （README「灵犀(lingxi)」、issues/38）
- https://github.com/DGZSbot/workbuddy2api-panel 、https://github.com/linguo2625469/workbuddy2api-panel
- https://github.com/xfyang728/dsh-router-wpslingxi （README「工作方式 / 帧协议 / 实测能力」）
- https://github.com/OdysseyFather/lingxi/blob/main/docs/web-disabled-routes.md
- 本仓上游快照：`.workbuddy/tmp/updates/prov-v350b1/Agent2Providers-3.5.0-beta.1/crates/providers/src/providers/`（无 lingxi）
