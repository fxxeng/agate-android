# 预置 API 与自定义提供商 —— 对齐上游 3.0.8

> 基线：`agent2api-v3.0.8-main/desktop-tauri`
> 关键文件：`ui-islands/src/islands/add-provider-pick.tsx`（387 行）、
> `add-custom-provider.tsx`（669 行）、`ui/preset-providers.js`（304 行 / 24 条模板）、
> `ui/custom-provider-ui.js`（146 行）、`ui/providers.js`（236 行）、
> `ui-islands/src/islands/balance-query.tsx`（323 行）

## 0. 先纠正一个命名

上游 3.0.8 的添加流程第一步是**四类账号**，不是「官方 / 中转 / 自定义」：

```ts
// add-provider-pick.tsx
export type AccountType = 'proxy' | 'preset' | 'custom' | 'import'
//                            Agent    预置 API   自定义    导入(当前隐藏)
```

- **Agent（`proxy`）**：内置家，来自 providers 摘要
- **预置 API（`preset`）**：24 张**模板卡**，点一张把名称/协议/Base URL/余额方案/上游特判预填进新建表单
- **自定义（`custom`）**：手填；同时列出「已建的非预置同名家」，点它 = 给那家再加账号
- **导入（`import`）**：`IMPORT_SEGMENT_ENABLED = false`，入口收起（实现仍在）

本仓此前把第二段叫「中转 API」，并把它做成「类型 chips（OpenAI 兼容 / NewAPI / 聚合面板）」——
与上游的语义完全不同：上游**没有「中转类型」这个概念**，NewAPI / Sub2API 只是 24 张预置卡里的两张。

## 1. 两个硬 bug（这才是「用不了」的直接原因）

### 1.1 协议取值写错 → 默认路径必然 400

| | 值 |
|---|---|
| 内核 `custom_providers::PROTOCOLS` | `chat_completions` / `responses` / `anthropic` |
| 上游 `providers.js` `PROTOCOL_OPTIONS` | `chat_completions` / `responses` / `anthropic` |
| 本仓旧客户端 `Protocol.OPENAI.wire` | **`openai`** ❌ |

两个表单的默认选项都是「OpenAI 兼容」，于是**默认路径必被 `normalize_protocol` 拒绝**：

```
协议必须是以下之一: chat_completions / responses / anthropic
```

### 1.2 余额方案取值写错 → 选 NewAPI 必 400

| | 值 |
|---|---|
| 内核 `BALANCE_SCHEME_NEW_API` | `new_api` |
| 本仓旧客户端 | **`newapi`** ❌ |

`validate_balance_scheme` 只认内核常量，于是「类型选 NewAPI」同样被拒。

> 两类错的成因相同：**前端猜的拼写与内核契约不一致**。
> 上游那份常量是「一处定义、两个表单共用」；本仓是各写一份，写着写着就漂了。
> 本次把协议/方案集中到 `CustomProviderOps.Protocol` 与 `PresetApiRegistry`。

## 2. 能力对照（实现后）

| 能力 | 上游 3.0.8 | 本仓 |
|---|---|---|
| 新建（名称/协议/BaseURL/Key/无需鉴权） | ✅ | ✅ |
| 三种协议 | ✅ | ✅（取值已对齐） |
| Base URL 提示随协议变 | ✅ | ✅ |
| 预置 API 模板卡（24 张，含图标） | ✅ | ✅ `PresetApiRegistry` |
| 模板卡的余额方案 / 上游特判随建随写 | ✅ | ✅ urlSuffix / headers / anthropicToolType / clientEmulation / balanceScheme / balanceQuery |
| 建过的模板卡仍常驻，再点 = 加账号 | ✅ | ✅ 按**名称相等**判定（同 `matchesPresetName`） |
| 自定义段：新建 + 给已有家加账号 | ✅ | ✅ |
| 余额查询凭证按方案渲染 | ✅ | ✅ new_api(令牌+用户ID) / sensenova(账号密码) / zhipu(令牌)，其余四条无需额外凭证 |
| 列表显示自定义家真实名称 | ✅ `labelOf` 合并 | ✅ 提供商页 + 详情页标题 |
| 改名称/协议/BaseURL（就地） | ✅ 账号设置弹窗 | ✅ 详情页「设置」顶部 |
| 删除（级联 + 账号数 + Key 影响） | ✅ | ✅ `POST /api/custom-providers/remove` |
| 导入 CC-Switch | 入口隐藏 | 未做（与上游一致，入口不露出） |

## 3. 刻意不做的两件事

1. **不做「中转类型」选择器**。上游没有这个概念，NewAPI / Sub2API 是预置卡。
   保留它等于让用户先选一个不存在的分类，再填同一张表单。
2. **不做独立的自定义提供商管理页**。上游 `custom-provider-ui.js` 头注释写明：
   编辑落在账号自己的设置里就地做（「改 Base URL 的动机通常正是这个账号连不上了」）。
   本仓照此，入口在「提供商 → 某家 → 设置」。

## 4. 顺带修掉上游的一个已知缺口

上游文档承认：**名下账号被删光的自定义提供商没有编辑/删除入口**（它仍出现在模型页左栏，
但点不进去）。本仓旧实现更糟 —— 「删除提供商」只批量删账号（`AccountOps.batch`），
**留下一条没有账号的提供商记录**：它从列表消失（列表只列有账号的家），却再也没入口清理。

现在自定义家走内核级联接口 `POST /api/custom-providers/remove`，一次删掉记录 + 账号 +
各 Key 白名单里的这一家，并把 `accountsRemoved` / `keysUpdated` / `keysUnrestricted`
写进成功提示（Key 白名单被放宽属于**权限变化**，必须如实说出）。
