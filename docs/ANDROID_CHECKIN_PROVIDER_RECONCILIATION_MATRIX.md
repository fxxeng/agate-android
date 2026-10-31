# CheckinRun Provider 对账能力矩阵

> 审计日期：2026-10-31  
> 范围：当前仓库 `agate-server/src/server/core/providers` 与 `core/billing` 中实际接入签到链的实现。  
> 目的：判断 `checkin_run_accounts.state=uncertain` 能否通过只读上游查询安全消除不确定性。  
> 本文是源码能力审计，不代表已在真实账号上重新验证上游行为。

## 1. 判定标准

- **可证明已完成**：只读接口能返回账号级、当前业务日/当前活动周期内的明确完成标记，或可归属到目标业务的到账流水；必须能排除历史周期残留。
- **可证明未完成**：只读接口明确返回当前业务日尚未完成，且其一致性/延迟语义足以证明先前写请求未生效。仅仅查不到记录不算。
- **未知**：接口失败、字段缺失、状态含义不明、时间窗口无法绑定、或只存在写接口。未知必须保持 `uncertain`。
- 即使能证明“已完成”，通常也只能证明目标效果已达成，不能证明一定是本次网关请求造成；对账的目标是防止重复执行，而不是归因。

## 2. 矩阵

| Provider | 当前写链路 | 源码中只读状态/回读 | 能否证明当前周期已完成 | 能否证明未完成 | 当前结论 |
|---|---|---|---|---|---|
| WorkBuddy 国内版 | `POST /v2/billing/meter/daily-checkin` | `POST /v2/billing/meter/checkin-activity-status`；归一化为 `checkedIn`，原始响应保留在 `raw` | **已接入正向对账**：同一北京时间自然日且显式 `checkedIn=true` | 否；`false`、缺字段或 `null` 不能证明写请求未生效 | 已接入证据持久化；只解除同日隔离，不归因到具体请求 |
| WorkBuddy 国际版 | 活跃状态查询 `/billing/meter/checkin-activity-status`，条件写 `/v2/billing/meter/daily-checkin` | 状态含 `active`、`today_checked_in`；代码已将 `today_checked_in` 归一为 `todayCheckedIn` | **已接入正向对账**：要求 `statusAvailable=true`、`todayCheckedIn=true` 且执行日与观察日相同 | 否；状态接口失败时当前实现会回落为不可用，不得把默认 `false` 当未完成 | 已接入证据持久化；只解除同日隔离，不归因到具体请求 |
| Qoder / Qoder Intl | 查询 campaign 列表后 `POST /campaigns/{campaignId}/claim` | `GET /sash/api/v1/me/campaigns?forceRefresh=true`；按 `actionType=CLAIM_BENEFIT` 读取 `claimStatus`；国际版先取 uid 并叠设备风控头重拉 | **强候选**：匹配当前活动窗口的每日权益行 `claimStatus=CLAIMED`；领取响应也含 `CLAIMED`、`claimedAt`、`grantedAt` | `CLAIMABLE` 只能表示可领，不能证明之前超时写请求一定未执行（可能存在最终一致性延迟） | 当前最适合做首个 Provider 专用对账器，但必须严格匹配活动窗口/类型/地区，并处理列表刷新延迟 |
| Trae | `POST /trae/api/v2/ug/checkin_credits/claim` | `GET /trae/api/v2/ug/checkin_credits/status`；字段 `checked_in`（账号维度）、`did_checked_in`（设备维度） | **有条件候选**：`checked_in=true` 可作为账号今日已完成的正向证据 | 否；`did_checked_in` 依赖本轮随机 `device_id`，该 ID 目前只在单次 `run()` 内生成，未持久化到 CheckinRun | 不可直接恢复本次请求上下文；不能仅凭新设备号的 `did_checked_in=false` 判未完成 |
| AutoClaw / Intl | `POST /autoclaw-task-complete`，task_id=`daily_signin` | 失败时辅助查询 `GET /autoclaw-task-list`，查 `daily_signin.completed` | **候选**：任务列表 `completed=true` 表示任务已完成，但需确认其按自然日重置以及读取一致性 | 否；`completed=false` 或查不到不能证明先前请求未落地 | 有只读正向线索，但目前只是失败归因，不是严格对账契约 |
| 小浣熊 Raccoon | `GET /api/web/office/v3/setting_info` 触发每日积分发放 | `GET /api/web/points/v1/bills`，当前筛选北京时间当天所有正积分流水 | **不充分**：现有代码汇总所有正积分，没有限定唯一签到事件类型；可能混入其他奖励 | 否；无流水可能是延迟、分页范围或查询失败 | 不能直接用当前 `grantsToday>0` 自动确认签到 |
| KukuAI | `POST /api/genflowpro/freepoint/taskComplete`，分别执行 LOGIN / CHAT | `GET /api/genflowpro/freepoint/homenew`，任务行含 `task_status` | **不充分**：代码注释确认 `FINISHED/UNFINISHED`，但 `claimable_point` 恒 0，且现有流程不把面板状态作为领取判据 | 否；`UNFINISHED` 不等价于领取请求未执行 | 需要真实样本确认 task_status 与领取状态关系后再评估 |
| OfficeAce | `POST /v1/subscription/bonus/claim` | 领取前后 `fetch_subscription` 余额/权益快照 | **不充分**：当前代码只计算前后权益增量；领取前快照失败可缺失，且接口把签到与新手礼合并 | 否；无增量可能是已领、无活动或响应延迟 | 不具备可区分签到的只读证据 |
| Loomy | `POST /api/v1/points/first-login` | 无独立签到状态接口；成功响应仅证明当前请求业务码成功 | **不充分**：成功响应是幂等触发结果，但没有独立读回状态 | 否 | 保持未知；不得在不确定时自动重放 |
| CodeArts | delivery → claim → confirm → delivery 回读 | `GET /v1/ops/delivery?channel=IDE`；条目有 `claimable/status` | **有条件但不能直接用**：代码要求当前执行完整 claim/confirm 后回读；`CONFIRMED/CONSUMED` 可作当前链路证据，但文档明确 CONFIRMED 不带确认日期，可能是上一周期残留 | 否 | 不可把历史 `CONFIRMED` 当今日已完成；重放写操作受每日次数/间隔闸限制，必须独立对账设计 |

## 3. 源码审计结论

### 3.1 首个实施候选：Qoder

选择理由：

1. 活动列表是独立 GET，不需要重放 claim。
2. 每日权益行通过 `actionType=CLAIM_BENEFIT` 与促销 `VIEW_DETAILS` 区分。
3. `claimStatus` 有明确 `CLAIMABLE` / `CLAIMED` 状态。
4. campaign 带 `startAt` / `endAt`，可把状态绑定到活动窗口，而不是只看一个无日期的布尔值。
5. 国际版已有明确的 uid → 风控头 → 重拉顺序；对账器必须复用同一地区协议，不能简化成无风控头请求。

实施前置条件：确认 `startAt/endAt` 的单位（源码样例按 Unix 秒解析）及活动窗口边界；定义当前 CheckinRun 的业务日与活动窗口关联规则；只在唯一匹配到当前窗口的每日权益行且 `CLAIMED` 时输出正向证据。多个匹配、字段缺失、列表拉取失败、活动窗口不覆盖 run 时间，一律 `Unknown`。

### 3.2 WorkBuddy 两个地区（已接入第二个正向对账器）

来源源码确认国内版 `checkin-activity-status` 的 `checked_in` / `checkedIn` 是签到状态，国际版 `/billing/meter/checkin-activity-status` 使用 `today_checked_in`。当前 Agate 已有 `BillingService::get_checkin_status(Some(account_session))`，无需复制来源项目的请求封装即可复用。

新增 `providers::workbuddy::reconcile::classify_status()`：国内版只采纳显式 `checkedIn=true`；国际版必须同时满足 `statusAvailable=true` 与 `todayCheckedIn=true`。两地区都要求台账运行日与查询观察日的北京时间日期相同。`false`、缺字段、查询失败、跨日一律 `unknown`。存储层再次校验 `businessDay` 同时等于原始执行日期和观察日期，并要求观察时间不早于执行开始时间。

该状态只能证明“这个账号在该北京时间自然日已完成签到”，不能证明一定由当前 CheckinRun 的那次请求造成；解决标记只释放当日隔离，原始 `uncertain` 仍保留。

### 3.3 Trae 不应直接套用现有回查

Trae 的 `did_checked_in` 是设备维度，设备号每轮随机生成且目前未写入 CheckinRun。虽然 `checked_in` 是账号维度，但必须先证明该字段在超时/崩溃恢复时仍准确代表当前业务日。不能使用新生成的 device id 查询后把 `did_checked_in=false` 当成“本次没成功”。

### 3.4 不可把“余额增加”当签到证据

Raccoon、OfficeAce 的余额或权益快照可能包含其它奖励；除非流水能精确匹配签到事件类型、账号、业务日和唯一事件标识，否则只能用于辅助诊断。

## 4. 对账器输出契约（建议）

Provider 专用查询只返回三态，不直接修改 CheckinRun：

```json
{
  "verdict": "confirmed | not_confirmed | unknown",
  "provider": "qoder",
  "evidenceType": "campaign_claim_status",
  "businessWindow": {"startAt": 0, "endAt": 0},
  "observedAt": 0,
  "upstreamStatus": "CLAIMED",
  "reason": "明确匹配当前每日权益活动"
}
```

规则：

- `confirmed`：允许后续通过独立的、带证据的事务把 uncertain 解决为已完成；原始 uncertain 记录不得删除。
- `not_confirmed`：只有 Provider 能给出强一致、明确的当前窗口未完成证明时才可产生；首版建议不实现此值，避免把“列表仍显示 CLAIMABLE”误判为可安全重试。
- `unknown`：任何失败、歧义、过期或字段缺失，保持 uncertain。
- 任何 verdict 都必须保存 provider、evidenceType、观察时间、活动窗口、关键上游状态和脱敏后的证据摘要。
- 对账查询是只读的；不能在对账器里调用 claim、刷新会话以外的业务写操作，不能偷偷触发保活或活动上报。

## 5. 本轮结论与下一步

Qoder 和 WorkBuddy 国内/国际版现已具备只读正向对账与 append-only 证据持久化；都不产生 `not_confirmed`，都不触发自动重试。下一步优先完成对账结果到签到历史的独立幂等投影，并为 Qoder/WorkBuddy 增加进程中断与并发故障注入。Trae、Raccoon、KukuAI、OfficeAce、Loomy、CodeArts 仍不满足安全的自动未完成判定。

## 6. Qoder 首版只读探针（已实现，尚未编译验证）

本轮在 Qoder Provider 内增加 `inspect_uncertain_claim()`，并通过受保护的管理 API 暴露：

```text
POST /api/checkin-runs/reconcile?runId=<runId>&accountId=<accountId>
GET  /api/checkin-runs/evidence?runId=<runId>&accountId=<accountId>&limit=&offset=
```

执行约束：

1. 先从 `checkin_run_accounts` 读取该条记录，必须仍为 `uncertain`；不存在或已不是 uncertain 时返回 404。
2. 只对 `qoder` / `qoder-intl` 调用专用探针；其他 Provider 返回 `unknown` / `unsupported_provider`。
3. 使用账号执行开始时间 `checkin_run_accounts.started_at`（毫秒）匹配 campaign 窗口；转换为 Unix 秒后，要求 `startAt <= startedAt <= endAt`。
4. 必须唯一匹配 `actionType=CLAIM_BENEFIT` 的活动，且 campaignId、窗口字段完整。
5. 唯一匹配活动的 `claimStatus=CLAIMED` 才返回 `confirmed`。`CLAIMABLE`、未知状态、无匹配、重复匹配、字段缺失、认证/网络/风控查询失败全部返回 `unknown`。
6. 国际版沿用 uid → 风控头 → 活动列表重拉；探针本身不调用 campaign claim。
7. 不调用 claim、不自动重试。查询结果写入 schema v20 的 append-only 证据表；只有存储层再次验证 Qoder `CLAIMED` 字段完整且活动窗口覆盖原始账号执行开始时间时，才在同一事务中设置 `reconciled_at` 并解除当日隔离。原始 `state='uncertain'` 保留。凭证过期时允许现有 `ensure_fresh` 认证维护流程刷新访问令牌；这不是签到领取操作。

新增纯函数测试覆盖：唯一 CLAIMED、CLAIMABLE 不等于未执行、活动窗口不匹配、多个活动歧义。测试代码已写入，但由于当前环境没有 Cargo/Rustc，尚未实际运行。

**重要限制：**该解决标记只解除同日 uncertain 隔离，不会伪造 `checkin_records` 展示历史，也不会把原始状态改写为 `projected`。管理端可通过 GET evidence 查看不可变证据。后续仍需独立设计“从已核实证据生成兼容签到历史”的投影策略；不能把对账确认与原始 Provider 返回结果混为一谈。

探针提交：`052cd7a feat(checkin): add read-only Qoder reconciliation probe`。证据持久化与事务解决在 schema v20 后续提交中实现。

## 7. 证据持久化实现快照（2026-10-31）

- schema v20 新增 `checkin_reconciliation_evidence`、`reconciled_at`、`reconciliation_evidence_id`。
- 对账接口采用 POST 写入本地证据；证据查询 GET 分页读取。证据表触发器阻止常规 UPDATE/DELETE。
- 只有完整 Qoder `CLAIMED` 证据且活动窗口覆盖台账原始执行开始时间，才能在同一事务内释放当日隔离；原始 `uncertain` 状态不被覆盖。v21 会将其投影为独立 `reconciliation` 历史行，并保留 evidence_id 关联。
- `CLAIMABLE`、未知 Provider、查询失败、窗口不匹配都只追加 `unknown`，不解除隔离。
- 实现提交：`c52f171 feat(checkin): persist Qoder reconciliation evidence`。
- SQLite v20 冒烟与静态检查通过；Rust 编译/单测、Android 构建和真机测试尚未执行。trigger 仅是应用层追加保护，不是密码学不可篡改存储。

## 8. Agent2Providers 来源源码交叉审计与 WorkBuddy 对账接入（2026-10-31）

本轮读取用户提供的 `Agent2Providers-main.zip`（归档根提交标识 `a5697f1ca9ac7188964914c21349dc8e62fd2940`）。该仓库包含 Provider 实现、`provider-protocol`、`provider-sdk`，不是单纯 API 清单。其 Qoder 实现确认了 campaign 的 `CLAIM_BENEFIT` / `CLAIMABLE` / `CLAIMED`、活动窗口和国际版 uid → UMID 风控头 → 重拉顺序；WorkBuddy 实现确认国内版 `checked_in` 与国际版 `today_checked_in` 状态字段。

基于来源协议与 Agate 现有 `BillingService::get_checkin_status(Some(session))`，新增 WorkBuddy 国内/国际版只读对账：

- 新增 `providers/workbuddy/reconcile.rs` 纯分类器；
- 国内版只接受 `checkedIn=true`，国际版要求 `statusAvailable=true` 且 `todayCheckedIn=true`；
- 执行开始日必须与查询观察日相同，统一使用北京时间日期；
- false、缺字段、接口失败、跨日均返回 `unknown`；
- 证据表只保存 `businessDay`、`upstreamCheckedIn`、国际版 `statusAvailable`、`upstreamStatus`、观察时间与原因，不保存原始响应；
- SQLite 事务内重新验证运行日期和观察日期，并检查观察时间不早于执行开始时间；
- 证据只确认“该业务日已签到”，不声称是本次 HTTP 请求造成。

新增测试覆盖国内版正向证据、国际版状态可用性门控、false/跨日保持 unknown，以及两地区证据的事务解决。Rust 测试尚未因工具链缺失而执行。

来源仓库声明 AGPL-3.0。本轮没有直接复制其 Rust 源文件或 Provider SDK；只将协议字段与状态语义作为审计依据，并使用 Agate 已有 BillingService。未来如直接移植或改编其代码，需先完成 AGPL-3.0 许可兼容与分发义务评估。
