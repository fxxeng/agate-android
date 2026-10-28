# C-MCP 工作量评估（内核侧）

> 只回答问题：**「内核侧到底要写多少」**。不含前端页面，那部分是纯 Compose，参照
> `ui/assistant/AssistantPage.kt` 的成熟套路即可。
>
> 评估日期 2026-10-28，对应提交 `a0af63d`。

## 结论先说

**内核侧不需要新写业务逻辑，只需要一层协议翻译。** 12 个工具里有 11 个能直接
调用已有的 handler 或 db 函数，真正「新写」的只有 MCP 协议本身（JSON-RPC 分发
+ 会话 + 配对鉴权），约 **600～800 行**，其中协议骨架占大头。

这不是「再做一个 AI 助手」那样的从零功能 —— 助手页的 8 只工具是在**客户端**执行、
走 HTTP 打回管理 API 的；MCP 只是把这套暴露面换成 JSON-RPC 给外部客户端。业务
能力两侧共享同一个内核。

## 一、现有地基：可以直接挂，不用改架构

| 事实 | 位置 | 对 MCP 的意义 |
| :--- | :--- | :--- |
| 内核是 **axum** | `agate-server/Cargo.toml`（`axum` 已在依赖里） | MCP 的 Streamable HTTP 就是一个 `POST /mcp` 的 handler，`.route("/mcp", post(...))` 即可 |
| 路由分 `public` / `protected` 两组，`protected` 统一挂 `middleware::from_fn(require_api_key)` | `server/http.rs:79`, `:728` | MCP 端点自带配对 token 鉴权，**不进 protected**（它是另一套凭证体系），单独挂一层 |
| 已有 55 个 `api/*.rs` handler，签名统一 `(State(state), Query/Bytes/Request) -> Response` | `server/api/` | 工具实现可以**直接调 handler**，把 JSON-RPC 的 `arguments` 拼成 Query/body 塞进去，不用重写查询 |
| SSE 已有现成设施（`events_api`、`api/assistant.rs`） | `server/api/events_api.rs` | Streamable HTTP 的 SSE 分支可以复用，不必引新依赖 |

**不需要新增任何 crate 依赖**。这不是小事：`Cargo.toml` 是从 Tauri 桌面 crate
摘出来的精简体，加依赖意味着重新评估 Android 交叉编译（rustls/ring + rusqlite
bundled 这条链已经很挑）。能在零依赖下完成，风险面小得多。

## 二、12 个工具的实现代价，逐个对账

上游 v44 基线里的 12 只工具（`docs/Agate-UI设计基线-v44.html:3124` 起）：

| # | 工具 | 类型 | 实现路径 | 代价 |
| :--- | :--- | :--- | :--- | :--- |
| 1 | `get_call_logs` | 读 | 调 `stats_api::stats_requests` | 极低 |
| 2 | `get_request_logs` | 读 | 调 `logs_api` 查询函数 | 极低 |
| 3 | `get_account_status` | 读 | 调 `accounts_usage::accounts_usage_snapshot` | 极低 |
| 4 | `get_usage_stats` | 读 | 调 `stats_api::stats_summary` | 极低 |
| 5 | `analyze_errors` | 分析 | **纯内核聚合**：查失败请求 → 按 provider/status 分组 → 输出计数与根因摘要 | 中（~120 行，要设计输出结构） |
| 6 | `diagnose_route` | 分析 | 读 `db/schema.rs:730` 那条决策 JSON（`{strategy, sticky, picked, ordered[]}`） | 中（~90 行，把决策链路翻成人话） |
| 7 | `trigger_checkin` | 操作 | 调 `checkin_schedules::entry` | 极低 |
| 8 | `run_model_test` | 操作 | 调 `model_test::run_model_test` | 低（它是 `Bytes` 入参，需要包一层参数校验） |
| 9 | `set_provider_enabled` | 操作 | 调 `custom_providers` 的启用开关 | 极低 |
| 10 | `set_route_strategy` | 操作 | 调 `routing_api` 的策略写入 | 低 |
| 11 | `clear_backoff` | 操作 | 调 `retry_api` 的清理 | 低 |
| 12 | `export_diagnostic` | 操作 | 组装现有 `backup_api` + 日志快照，**剔除凭据** | 中（~100 行，脱敏是重点） |

读数：**8 只「极低 / 低」等于薄封装，4 只「中」合计约 400 行的真实逻辑。**
没有任何一只是「内核里现在完全没有这个能力」—— 助手页做的时候已经把 8 类工具
的内核侧跑通了。

## 三、真正要新写的东西（约 600～800 行）

这部分是 MCP 协议本身，跟 Agate 的业务无关，但绕不过去：

1. **JSON-RPC 2.0 分发**（~200 行）
   `initialize` / `tools/list` / `tools/call` / `notifications/initialized` 四个方法。
   错误码要守规范（`-32601` method not found、`-32602` invalid params），不能
   自造一套 —— 客户端（Claude Code、RikkaHub）按规范判错。

2. **工具 schema 声明**（~250 行）
   12 只工具的 `name` / `description` / `inputSchema`。**听起来是填空，但这
   恰恰是 MCP 好不好用的全部** —— 模型只有 description 判断该不该调。写得太
   简（「查询日志」）模型不会用；写太啰嗦浪费 context。

3. **会话与配对鉴权**（~150 行）
   这是**协议里最容易被低估的一段**。MCP 客户端要拿 token 才算「授权」，配对
   流程是：客户端请求 → 内核生成 6 位码并挂起 → APP 弹窗显示码 → 用户确认 →
   内核发 token。涉及一个有状态的待配对表 + 超时清理。

4. **「操作需确认」拦截**（~80 行）
   设计基线要求：读类直接放行，操作类先弹窗。**但 MCP 是被动应答模式** ——
   服务端不能主动弹窗，只能返回一个「待确认」的中间态，让客户端轮询/重试。
   这是在协议约束下做取舍，不是抄个开关就行。

## 四、三个必须现在拍板的决策

1. **监听端口：独立还是并入主端口？**
   基线写 `http://127.0.0.1:18083/mcp`，是**独立端口**。并入主端口（一样走
   18080）能少一套监听的启停逻辑，但「关掉 MCP 服务」这个开关就没法真的把
   端点摘掉（只能返回 404），安全性靠的是路径藏起来而非端口不通。
   **建议独立端口**：这一页有个明确的启停开关，用户的理解是「关了就没了」。

2. **`set_provider_order` 要不要做？**
   助手页当时**刻意剔除**了这只工具（理由：本内核已下线 `providerRoute`，
   账号是全局一条队列、先后由账号优先级决定，留着等于一个改不动东西的按钮）。
   而基线里 MCP 的第 10 只是 `set_route_strategy`，不是 `set_provider_order`
   —— 两者不冲突，可以照做。**但要确认 `set_route_strategy` 在新内核里真的
   有对应字段**（`db/schema.rs` 的决策 JSON 里有 `strategy`，看起来是有的，
   实做前需要核对写入点）。

3. **「AI 分析演示」这一块做不做？**
   基线页面底部有「试跑一次错误分析」。它纯粹是给用户看效果的按钮，走
   `analyze_errors` 同一套逻辑。**建议做** —— 不然用户配好 MCP 端点，在手机
   上看不到任何反馈，只能去别的客户端里试，体验断层。

## 五、风险：一个真实的未知

**Streamable HTTP 的 SSE 分支要不要真做？**

MCP 2025-06 规范里 Streamable HTTP 的双向流是可选特性。但实际客户端
（Claude Code、RikkaHub）在 `tools/call` 时普遍只用「POST → JSON 响应」
这条简单路径，只有 `notifications` 和长任务才用 SSE。

**建议先只做 POST → JSON**，SSE 留成 stub 返回 405。理由：`run_model_test`
是唯一可能跑很久的工具（测速），但它本来就有超时收敛；为它一个人背下 SSE
的复杂度不划算。**代价**：如果某个客户端坚持要 SSE，会连不上 —— 但这个风险
可以在实测时发现并补，而不是一开始就为它付出全部成本。

## 六、总结：如果要做，顺序是这个

1. `initialize` + `tools/list`（**先把端点跑通，工具清单可以是空的**）——
   这一步做完就能拿真客户端验证握手，是最快的「活不活」判据。
2. 4 只只读工具（`get_*`）—— 薄封装，一天能完成，验证「调用链通」。
3. 配对鉴权 + 「操作需确认」—— 安全边界，必须在开放操作类工具**之前**做完。
4. 4 只操作工具。
5. `analyze_errors` / `diagnose_route` / `export_diagnostic` —— 输出结构最费
   心思的三只，放最后。
6. 前端页面。

**不建议连做**：第 1 步做完就该拿真客户端连一次。MCP 的坑几乎全在握手细节
（`protocolVersion` 协商、`Content-Type` 要求、`Accept` 头），纯读代码看不出
来，必须实测。
