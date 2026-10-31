# Agent2Providers 来源源码审计

> 审计日期：2026-10-31  
> 用户附件：`Agent2Providers-main.zip`  
> ZIP 根提交标识：`a5697f1ca9ac7188964914c21349dc8e62fd2940`  
> 来源仓库声明许可证：AGPL-3.0

## 1. 文件性质与对 Agate 的价值

这不是一份单纯的 Provider API 列表，而是一套可构建的 Rust Provider 插件工程，包含：

- `crates/provider-protocol`：Provider 与网关之间的协议类型；
- `crates/provider-sdk`：Provider trait、上下文与服务循环；
- `crates/providers`：多家 Provider 的真实请求实现、鉴权、凭证、归一化、签到、额度与风控逻辑。

Agate 当前是单体 Rust 服务，Provider 由 `agate-server/src/server/core/providers` 与 `core/billing` 承载；Agent2Providers 是独立进程插件模型。两者业务代码有明显重叠，但运行时边界不同。因此来源项目最适合用于**协议核对、行为语义复核、缺口发现和可移植纯逻辑参考**，不应把 `PluginCtx`、Provider trait、插件 serve loop 整套复制进 Agate。

目录名交集：当前 Agate 与来源项目有 18 家同名 Provider。来源项目另含 AtomCode、LobsterAI、ModelScope 等 Agate 当前没有的 Provider；Agate 另有 `catalog`、`custom` 等本地模块。这些差异可用于后续 Provider 覆盖率审计，但不代表应该全部移植。

## 2. 对当前 CheckinRun 对账任务的直接帮助

### 2.1 Qoder：现有对账器协议得到交叉验证

来源文件：

- `crates/providers/src/providers/qoder/checkin.rs`
- `crates/providers/src/providers/qoder/endpoints.rs`
- `crates/providers/src/providers/qoder/risk.rs`

核对结果：

1. 每日权益走 `GET /sash/api/v1/me/campaigns?forceRefresh=true`，而不是 legacy `daily-check-in/status`；
2. 每日权益行通过 `actionType=CLAIM_BENEFIT` 区分，状态为 `CLAIMABLE` / `CLAIMED`；
3. 活动行包含 `campaignId`、`startAt`、`endAt`；
4. 国际版必须先读取顶层 `uid`，再生成 `Cosy-MachineToken` / `Cosy-MachineCode` / `Cosy-MachineType` 风控头并重拉活动列表；
5. 顶层 `claimable` 不能作为签到判据，促销活动可能让它保持 true。

这与 Agate 已有 Qoder 对账器的核心协议相符。来源文件没有替代 Agate 的 schema v20 证据事务，也没有给出可以证明“本次特定 HTTP 请求”归因的 request id。`CLAIMED` 只能证明该账号对应活动窗口已完成，不能声称一定由当前 CheckinRun 的请求造成。

### 2.2 WorkBuddy：提供第二个可复用的正向状态依据

来源文件：

- `crates/providers/src/providers/workbuddy/billing/ops.rs`
- `crates/providers/src/providers/workbuddy/keepalive.rs`
- `crates/providers/src/providers/workbuddy/billing/checkin.rs`

核对结果：

- 国内版签到状态：`POST /v2/billing/meter/checkin-activity-status`，归一化状态含 `checkedIn`；
- 国际版活动状态：`POST /billing/meter/checkin-activity-status`，归一化状态含 `active`、`todayCheckedIn` 与 `statusAvailable`；
- 国际版的 `todayCheckedIn` 与 `active` 是两个不同概念，不能把活动开放误判为签到完成；
- 状态查询失败必须与 `false` 区分。

Agate 已有 `BillingService::get_checkin_status(Some(session))`，本轮直接复用现有业务方法，不复制来源项目网络层，新增 WorkBuddy 专用纯分类器和证据校验。

当前 WorkBuddy 证据契约：

- 国内版：显式 `checkedIn=true`；
- 国际版：`statusAvailable=true && todayCheckedIn=true`；
- 两地均要求 CheckinRun 执行开始日与状态观察日相同，日期统一按北京时间；
- `false`、缺字段、接口失败、跨日均为 `unknown`；
- 存储层事务再次验证 `businessDay` 与原始执行日期、观察日期一致。

该证据确认的是“账号当日业务签到已完成”，不是请求级归因证明。

### 2.3 Trae：来源源码进一步确认设备 ID 约束

来源文件：`crates/providers/src/providers/trae/checkin.rs`。

Trae 的 status → claim → status 三次请求必须复用同一个本轮随机 `device_id`。`did_checked_in` 是设备维度，`checked_in` 是账号维度。Agate 当前随机设备 ID 在本轮执行期间生成，未在 Provider 写请求前持久化到 CheckinRun。因此不能在恢复时新造一个 device ID，再用 `did_checked_in=false` 判定之前的请求未执行。后续若要支持 Trae，必须先把本轮设备 ID 的生成和持久化放在 claim 前的 durable intent 阶段，并重新评估设备指纹与风控风险。

### 2.4 AutoClaw：有任务列表正向线索，但周期绑定不足

来源文件：`crates/providers/src/providers/autoclaw/checkin.rs`。

签到写接口是 `POST /autoclaw-task-complete`，任务 `daily_signin`；辅助只读列表为 `GET /autoclaw-task-list`，任务行含 `completed`。它适合失败归因，但来源实现没有在该字段上建立可供历史恢复使用的明确业务日/周期 ID。当前不据 `completed=true` 直接解除任意历史 uncertain；必须先取得可验证的任务周期字段或可靠的日期边界契约。

### 2.5 CodeArts：读取能力强，但 CONFIRMED 缺少日期

来源文件：`crates/providers/src/providers/codearts/welfare.rs`。

`GET /v1/ops/delivery?channel=IDE` 能回读活动列表，包含 `campaignId`、`type`、`claimable`、`status`。来源实现也明确记录了 `CONFIRMED` 不带确认日期，可能是上一登录周期残留。来源项目通过本地活动台账、idempotent key、claim 后 confirm、再 delivery 回读完成当前写链路，但这套本地台账不自动等于 Agate CheckinRun 的历史请求证据。不能仅因状态是 `CONFIRMED` 就确认历史 uncertain。

### 2.6 其他 Provider

| Provider | 来源源码可见能力 | 当前对账结论 |
|---|---|---|
| KukuAI | `freepoint/homenew` 任务面板 + `taskComplete`；`task_status` 为 `FINISHED/UNFINISHED`，注释说明 `claimable_point` 恒为 0 | 需验证 task_status 是否能绑定明确业务周期；`UNFINISHED` 不能证明写请求未执行 |
| Loomy | `POST /api/v1/points/first-login`，每日首次登录刷新积分 | 没有独立只读签到状态，不适合恢复核验 |
| ChatGLM | `daily_login_score`、积分活动状态和限时活动状态接口 | 各接口对应不同活动，必须拆分“每日登录”与“活动领取”，不能混用余额/活动开关 |
| OfficeAce | `subscription/bonus/claim` 前后权益快照 | 签到与新人礼同一动作，余额增量不能单独证明签到事件归属 |
| Raccoon | 积分账单/每日奖励流程 | 正积分流水可能混入其他奖励，需精确事件类型与日期键 |
| LobsterAI（来源独有） | 活动 slot → context，`state.claimedToday` 与 `actions[]` 两道预检，再使用 idempotencyKey 写入 | 可作为未来新 Provider 的参考；不是当前 Agate Provider 的直接补丁 |
| AtomCode、ModelScope（来源独有） | 各自的计划领取/登录状态业务 | 先做 Provider 需求与协议评估，不应只因来源存在就盲目加入 |

## 3. 本轮 Agate 实际改动

- 新增 `agate-server/src/server/core/providers/workbuddy/reconcile.rs`；
- `workbuddy/mod.rs` 注册 `reconcile` 模块；
- `api/checkin_runs.rs` 对 `workbuddy` / `workbuddy-intl` 复用目标账号 session 查询状态；
- `checkin_run_ledger.rs` 扩展证据白名单与事务校验，支持 `checkin_status`；
- 增加 CN/Intl 正向状态、国际版 statusAvailable 门控、false 与跨日 unknown、事务解决等测试；
- 更新 Provider 能力矩阵、CheckinRun 设计记录和迁移交接文档。

没有增加自动重试、`not_confirmed`、手工清除、强制成功/失败或 Provider 写请求重放。v21 已把正向核实结果投影为独立 `source=reconciliation` 历史行，并关联原始 evidence_id；它不会伪装成实时 Provider 响应。

## 4. 来源代码移植与许可注意事项

来源仓库 `README.zh-CN.md` 声明 AGPL-3.0。本轮没有直接复制来源 Rust 文件或 SDK 代码，只使用其协议字段、端点和状态语义进行交叉审计，并复用 Agate 已有 BillingService。

如果后续要直接移植或改编来源代码，需要先确认 Agate 的整体许可证、分发方式和 AGPL-3.0 的源代码提供义务；应保留来源与版权标记并完成许可兼容审查。不要将来源项目的 `PluginCtx` / Provider SDK 当作 Agate 内部架构直接引入。

## 5. 本轮验证边界

已运行：`git diff --check`、`tools/rust_balance.py`、`tools/rust_depth.py`、`tools/check_eol.py`。

未运行：Rust 编译与单测（当前环境没有 Cargo/Rustc/Rustfmt）、Gradle/Android 构建、模拟器或真机验证。静态结构检查不能代替编译；本轮代码应视为待工具链验证的实现切片。


## 许可与项目使用范围补充

用户说明 Agate 主要用于个人本地自用，不计划分发。若始终是个人私有使用，
不向第三方交付副本，也不向其他用户提供网络服务，通常不会触发 AGPL 面向
接收者提供对应源码的分发义务；但这不等于许可证条款失效。本轮仍只将来源
仓库作为协议行为参考，没有复制其实现文件。若未来分发修改版本，或让其他
用户通过网络与修改后的程序交互，应重新核对 AGPL-3.0 的对应义务。
