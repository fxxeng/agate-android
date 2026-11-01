//! 豆包工作的模型目录：**账号级在线目录** + 内置兜底 + 落盘缓存。
//!
//! ── 上游长什么样（`/alice/slot/action_bar_v3/brief_list` 一族）────────
//! 本家**没有** `GET /models` 那样的目录接口。[二进制] 里与目录有关的键是
//! 这一串（连排出现，因此它们属于同一份响应）：
//! ```text
//!   conversation_mode_list  support_models  entry_list
//!   active_switch  confuse_menu_conf_v2  menu_conf_v2
//!   mode_list  item_list  model_list  menu_conf  menu_type2
//!   default_select_mode_map  new_chat  agent-  default_mode
//!   agent_mode  mode_id  mode_name  deep_think  fetched_at  credential  bot
//! ```
//! 读法（这是本模块最重要的一段，写清楚了别处就不用猜）：
//!   - 本家的「模型」在协议里叫 **mode**（模式）：`mode_id` / `mode_name` /
//!     `deep_think` 三件套；
//!   - `default_select_mode_map` 是「客户端选哪个模型 → 上游给哪个 mode」的映射，
//!     `default_mode` 是默认那一个；
//!   - `entry_list` / `item_list` / `model_list` 是三种粒度的清单（模式 → 条目 → 模型）；
//!   - `agent-` 前缀 + `agent_mode` 说明**Agent 模式**是其中一类 mode；
//!   - `support_models` 与 `conversation_mode_list` 是「当前账号可用的模式清单」。
//! 因此本模块的远程目录拉的是 [`endpoints::action_bar_brief_list_url`]（`brief_list`
//! 就是「模式简报」），并按 `mode_list` / `entry_list` / `model_list` 三级
//! **宽容解析**（[`parse_catalog`]）。
//!
//! ╔══════════════════════════════════════════════════════════════════╗
//! ║ ⚠️ **需要接线方补：`catalog_cache::ALL_SCOPES` 登记**              ║
//! ╚══════════════════════════════════════════════════════════════════╝
//! 本文件的 [`SCOPE`] 是 `"doubaowork"`。它必须在
//! `providers/catalog_cache.rs` 的 `ALL_SCOPES`（以及该文件测试里那个
//! 「条数」断言 `all_scopes_size_matches_declared_scopes`）里登记，否则：
//!   - 启动日志的「从缓存恢复的清单」那一行**不会有本家**（缓存其实恢复了，
//!     只是没人报告 —— `cached_scopes()` 按 `ALL_SCOPES` 遍历）；
//!   - 那条断言测试会继续通过（它数的是声明条数，不是运行时行为），
//!     所以这个漏登记**不会在 CI 里暴露**，只会表现为排障时的信息缺失。
//! 任务约定：`catalog_cache.rs` 是既有文件，本切片不改它 —— 因此这里
//! **显式声明需要接线方补**，而不是在 `models.rs` 里自己绕过。
//!
//! ── 内置兜底清单：**刻意的取舍** ────────────────────────────────
//! 本家上游会返回「当前账号可用的模式」，因此目录**是账号级**的。但：
//!   1. 进程刚起来、还没添加账号、或上游抖动时，`list_models()` 会返回空；
//!   2. 上游的目录接口一旦口径变化（`brief_list` 这名字本身就在变），
//!      解析失败也会返回空；
//!   3. 「清单为空的家不进 `/v1/models`」（见 `catalog` 的可用性判据）——
//!      意味着空清单会让整家**从客户端视野里消失**（用户会以为网关坏了）。
//! 因此保留一份兜底清单。**它的内容来自任务书给出的、二进制里确实出现过的
//! 名字**（见下面 [`FALLBACK_MODELS`] 的逐条注释）。
//!
//! ⚠️ **一个必须写下来的坑：那串模型名的归属不是本家**（已核实）。 ──
//! 任务书列出的 `MiniMax-M2.7` / `qwen3.7-max` / `kimi-k2.7-code` /
//! `doubao-seed-2-1-pro-260628` … 那一串，在二进制里的**归属是 LobsterAI**，
//! 不是 doubaowork。核实方法（可复现）：在 `agent2api-providers.exe` 里定位
//! 那串名字的偏移（`MiniMax-M2.7qwen3.7-max` 附近），然后找**最近的两个
//! 编译期文件路径字符串** —— 得到的是
//! `crates\providers\src\providers\lobsterai\credentials.rs`（前）与
//! `crates\providers\src\providers\officeace\signer.rs`（后）：那串名字夹在
//! **lobsterai 与 officeace 两家的字符串块之间**，与 doubaowork 的文件标记
//! 相距十几万字节（doubaowork 的标记在 1081xxxx，那串名字在 1108xxxx）。
//! 而本仓的 `providers/lobsterai/models.rs` 里**逐字就有这些名字**
//! （第 132~143 行），佐证同一个结论。
//! ⇒ 把它们当本家的兜底清单，就是**把另一家的模型名广告给用户**
//! （客户端点了必然 400）。因此本家的兜底只收「形态上确实是豆包自家模式」的名字
//! （`doubao-*`），并在 [`FALLBACK_MODELS`] 里逐条注明出处。
//!
//! ── 诚实标注：兜底清单是**形态推断**，不是实测 ──────────────────
//! 二进制里本家唯一的 `doubao-*` 名字是 `doubao-seed-2-0-code-preview-260215`
//! （但那也落在 lobsterai 的名字串里），**没有**任何一处能证明本家的模式名
//! 长什么样。因此：
//!   - 兜底清单用**本家用得上的档位名**（`doubao-fast` / `doubao-think` /
//!     `doubao-expert`，来自 [Go参考] `registry.Builtin` 的逐字定义 —— 那是
//!     一份**真的打过上游**的实现，它的模式名就是这三个）；
//!   - 名字里带 `doubao` 前缀，与上游 `mode_name` 的形态一致；
//!   - 远程目录一旦拉到，就以远程的为准（兜底只在没有远程清单时生效）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::server::core::auth_http::send_raw;
use crate::server::core::providers::adapter::ModelRefreshOutcome;
use crate::server::core::providers::catalog_cache;

use super::credentials::Credentials;
use super::endpoints;

/// 本家在 `catalog_cache` 里的 scope 名（**单一 scope**：本家没有地区之分）。
///
/// ⚠️ 见模块头的接线说明：这个常量要在 `catalog_cache::ALL_SCOPES` 里登记。
pub const SCOPE: &str = "doubaowork";

/// 自动路径的缓存 TTL（与别家的 5~10 分钟同档；本家目录是账号级的，
/// 没必要拉得更勤）
const TTL_MS: i64 = 10 * 60 * 1000;

/// 目录查询超时（维护动作，见 [`endpoints::CATALOG_TIMEOUT_MS`]）
const REQUEST_TIMEOUT_MS: u64 = endpoints::CATALOG_TIMEOUT_MS;

/// 内置兜底清单：`(模型 id, 展示名, 思考档位, 上下文窗口)`。
///
/// ── 逐条出处（都来自 [Go参考] `internal/registry/registry.go` 的
/// `Builtin`，那是**真的打过上游**的那份实现）──────────────────────
/// ```text
///   doubao        豆包·快速   ThinkOff    128000   「默认模式，响应最快，无思维链」
///   doubao-think  豆包·思考   ThinkOn     128000   「思考模式，返回思维链」
///   doubao-expert 豆包·专家   ThinkExpert 256000   「专家模式，深度推理」
/// ```
/// ⚠️ 为什么不用二进制那串 `MiniMax-M2.7 / qwen3.7-max / …`：见模块头
/// （**那是 LobsterAI 的清单**，拿它当本家的兜底会把别家的模型广告给用户）。
/// 上文那条 `doubao-seed-2-0-code-preview-260215` 也不收 —— 它同样落在
/// lobsterai 的名字串里，无法证明是本家的模式名。
///
/// `supportsImages` 给 **`false`** —— 这一条是**修正过的**，改动理由必须写清：
///
/// ```text
///   上游确实收图片（[二进制] image / image_ori / attachment_block 一族 +
///   [Go参考] 的 imageBlock），**但本仓当前传不上去**：
///   图片必须先 POST /alice/resource/prepare_upload 换成上游认的资源 uri
///   （见 super::upload），而这一步是**异步**的，build_chat_request 是
///   **同步**的 —— trait 里没有任何「能同时拿到 body 的异步钩子」
///   （ensure_access_token 没有 body 参数；带 body 的 forward_conversation
///   属于有状态路径，本家不该走那条，见 chat.rs 的模块头）。
///   于是 chat::prepare_attachments 写好了却**没有调用点**，附件只能降级成
///   文本标记 [Attached image 1: a.png]。
/// ```
///
/// 广告成 `true` 而实际传不上图，是**给下游一个假承诺**：客户端会据此把
/// 图片塞进请求，用户以为图生效了，而模型只看到一句 "[Attached image 1: …]"。
/// 按本仓口径（「算不准就降级说法」），这里如实报 `false`。
///
/// 要真正开启，缺的是**一处结构改动**（不是补丁）：给 `ProviderAdapter` 加
/// 一个异步钩子，例如
/// `fn prepare_body<'a>(&'a self, account: &'a Value, body: &'a Value)
///    -> Pin<Box<dyn Future<Output = Result<Option<Value>, GatewayError>> + Send + 'a>>`
/// （默认返回 `Ok(None)` = 不改），由 `provider_loop` 在
/// `ensure_access_token` 之后、`build_chat_request` 之前 await；
/// doubaowork 覆写它去调 `prepare_attachments` 并把 `ref_file_ids` 放进 body。
/// 那才是「这一类点的唯一入口」，也顺带解决将来别家的同类需求。
/// **在那之前，这里的 false 是当前唯一诚实的答案。**
///
/// 收敛成一个常量：本文件有**两个**构建器会产出能力位
/// （`to_manifest_value` 与 `fallback_models`），两处各写一份字面量必然漂移
/// （一个改了另一个忘改，症状是「远程目录来的模型说支持图、兜底清单说不支持」）。
pub const SUPPORTS_IMAGES: bool = false;

/// `supportsReasoning` 按档位给（`off` 那一档为 false）。
/// `supportsToolCall` 给 **false**：本家是**文本桥接**的工具调用
/// （插件文档原话「Tool calls use a text bridge, not native function calling」），
/// 不是原生 function calling ⇒ 广告成原生支持会让客户端发 `tools` 字段去换一个
/// 「上游不认识」。如实报 false 更准。
const FALLBACK_MODELS: [(&str, &str, &str, i64); 3] = [
    ("doubao-fast", "豆包·快速", "off", 128_000),
    ("doubao-think", "豆包·思考", "on", 128_000),
    ("doubao-expert", "豆包·专家", "expert", 256_000),
];

/// 默认模型（客户端没指定 model 时的兜底；与 [Go参考] `Resolve` 的回落一致）
pub const DEFAULT_MODEL: &str = "doubao-fast";

/// 进程内目录状态
struct CatalogState {
    /// 聚合层形态的条目
    models: Vec<Value>,
    /// 落地时刻（毫秒）
    fetched_at: i64,
    /// 这份清单是不是**远程拉到**的（落盘缓存装回来的也算 —— 它当初就是远程的）
    remote: bool,
}

static STATE: OnceLock<Mutex<Option<CatalogState>>> = OnceLock::new();

/// 取状态句柄（毒锁按「继续用」处理：这份缓存是可重建的展示数据）
fn state() -> &'static Mutex<Option<CatalogState>> {
    STATE.get_or_init(|| Mutex::new(None))
}

/// 在锁内执行一段取值，并在**首次访问时**把落盘缓存装回内存。
///
/// 只在首次查一次库：`list_models()` 在每个请求的清单构造路径上，不能每次都查库。
fn with_state<R>(f: impl FnOnce(&mut Option<CatalogState>) -> R) -> R {
    let mutex = state();
    let mut guard = mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.is_none() {
        if let Some(cached) = catalog_cache::load(SCOPE) {
            if !cached.models.is_empty() {
                *guard = Some(CatalogState {
                    models: cached.models,
                    fetched_at: cached.fetched_at,
                    remote: true,
                });
            }
        }
    }
    f(&mut guard)
}

/// 把一个上游模式条目映射成聚合层认的形态
/// （键名对照 `core/models/shape.rs::list_item`）。
fn map_entry(row: &Value) -> Option<Value> {
    // 模式 id 的三个候选名（[二进制] `mode_id` / `model_list` / `entry_list` 一族）
    let id = ["mode_id", "modeId", "model_name", "modelName", "id", "name"]
        .iter()
        .find_map(|key| row.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())?;
    let name = ["mode_name", "modeName", "model_name", "display_name", "displayName", "name"]
        .iter()
        .find_map(|key| row.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(id);
    // 思考档位：上游用 `deep_think` 表达（整数或布尔都见过）
    let deep_think = row
        .get("deep_think")
        .or_else(|| row.get("need_deep_think"))
        .map(deep_think_level);
    let reasoning = deep_think.map(|level| level != "off" && level != "0");
    let mut entry = json!({
        "id": id,
        "name": name,
        // 本家是**文本桥接**的工具调用，不是原生 function calling ⇒ false
        // （见 FALLBACK_MODELS 的说明；这一条对所有条目一致，不逐条判）
        "supportsToolCall": false,
        // 图片：上游收图，但本仓**传不上去**（同步/异步接缝）
        // ⇒ 如实报 false，不给假承诺（依据与彻底接线方案见 SUPPORTS_IMAGES）
        "supportsImages": SUPPORTS_IMAGES,
    });
    if let Some(reasoning) = reasoning {
        if let Some(object) = entry.as_object_mut() {
            object.insert("supportsReasoning".to_string(), Value::Bool(reasoning));
        }
    }
    // 上下文窗口（上游给了才写 —— 「没给」与「给了一个 0」是两件事）
    for key in ["context_window", "contextWindow", "maxInputTokens", "max_input_tokens"] {
        if let Some(value) = row.get(key).and_then(Value::as_i64).filter(|value| *value > 0) {
            if let Some(object) = entry.as_object_mut() {
                object.insert("maxInputTokens".to_string(), Value::from(value));
            }
            break;
        }
    }
    Some(entry)
}

/// `deep_think` 值 → 档位字符串（整数、字符串、布尔三种形态都认）
fn deep_think_level(value: &Value) -> String {
    match value {
        Value::Number(_) => {
            let number = value.as_i64().unwrap_or(0);
            match number {
                0 => "off".to_string(),
                1 => "on".to_string(),
                3 => "expert".to_string(),
                other => other.to_string(),
            }
        }
        Value::Bool(flag) => {
            if *flag {
                "on".to_string()
            } else {
                "off".to_string()
            }
        }
        Value::String(text) => {
            let lowered = text.trim().to_ascii_lowercase();
            match lowered.as_str() {
                "0" | "false" | "off" => "off".to_string(),
                "1" | "true" | "on" | "think" | "thinking" => "on".to_string(),
                "3" | "expert" | "deep" => "expert".to_string(),
                _ => lowered,
            }
        }
        _ => "off".to_string(),
    }
}

/// 解析目录响应（**宽容三级**：`mode_list` → `entry_list` → `model_list`）。
///
/// ── 为什么是三级而不是一级 ────────────────────────────────────
/// [二进制] 里 `menu_conf_v2` / `mode_list` / `item_list` / `model_list` /
/// `entry_list` 是**同一份响应里不同层级的键名**（那份字符串表把它们连排）。
/// 上游到底在哪一级放清单，我们**没有真实账号可核实**，因此按「哪一级有货
/// 就用哪一级」宽容解析：三级都试，取第一个非空结果。这不是「随便写」——
/// 每一级都是二进制里逐字存在的键，且顺序与字符串表里的出现顺序一致。
///
/// `support_models` / `conversation_mode_list` 是**当前账号可用清单**，
/// 若它们是字符串数组（形态 B），则当作「白名单」作用于 mode 列表。
pub fn parse_catalog(payload: &Value) -> Vec<Value> {
    // ── 白名单（`support_models` / `conversation_mode_list`，字符串数组）──
    let whitelist: Vec<String> = ["support_models", "conversation_mode_list"]
        .iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_array))
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    // ── 三级载体 ──
    let carriers: Vec<Vec<Value>> = [
        "/data/mode_list",
        "/data/entry_list",
        "/data/model_list",
        "/mode_list",
        "/entry_list",
        "/model_list",
        "/data/conversation_mode_list",
    ]
    .iter()
    .filter_map(|pointer| payload.pointer(pointer).and_then(Value::as_array).cloned())
    .collect();

    let mut out: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for carrier in carriers {
        for row in carrier {
            // 条目可能再包一层：`{"mode": {…}}` / `{"model": {…}}` / `{"item": {…}}`
            let inner = ["mode", "model", "item", "entry"]
                .iter()
                .find_map(|key| row.get(*key).filter(|value| value.is_object()))
                .unwrap_or(&row);
            let Some(entry) = map_entry(inner) else { continue };
            let Some(id) = entry.get("id").and_then(Value::as_str).map(str::to_string) else {
                continue;
            };
            if seen.contains(&id) {
                continue;
            }
            if !whitelist.is_empty() && !whitelist.contains(&id) {
                continue;
            }
            seen.push(id);
            out.push(entry);
        }
        if !out.is_empty() {
            // 第一级有货就用它（见函数文档）
            break;
        }
    }
    out
}

/// 兜底清单的聚合层形态
fn fallback_models() -> Vec<Value> {
    FALLBACK_MODELS
        .iter()
        .map(|(id, name, level, context)| {
            json!({
                "id": id,
                "name": name,
                "supportsToolCall": false,
                "supportsReasoning": *level != "off",
                "supportsImages": SUPPORTS_IMAGES,
                "maxInputTokens": context,
            })
        })
        .collect()
}

/// 当前模型清单（内存 → 落盘缓存 → 内置兜底；**永远非空**）
pub fn list() -> Vec<Value> {
    with_state(|slot| {
        slot.as_ref()
            .map(|state| state.models.clone())
            .filter(|models| !models.is_empty())
            .unwrap_or_else(fallback_models)
    })
}

/// 这份清单是否来自**远程拉取**（`catalog::refresh_meta` 的「来源」列判据）
pub fn remote_refreshed() -> bool {
    with_state(|slot| slot.as_ref().map(|state| state.remote).unwrap_or(false))
}

/// 最近一次成功刷新的时刻（毫秒；没刷过为 0）—— 界面「更新日期」列读它
pub fn last_refreshed_at() -> i64 {
    with_state(|slot| slot.as_ref().map(|state| state.fetched_at).unwrap_or(0))
}

/// 目录最后一次落地至今的年龄（毫秒）
pub fn age_ms() -> i64 {
    with_state(|slot| {
        slot.as_ref()
            .map(|state| crate::server::logging::now_ms() - state.fetched_at)
            .unwrap_or(0)
    })
}

/// 清单是不是空的（界面「刷新目录」按钮的提示用）。
///
/// 本家有兜底，因此这个函数**几乎恒为 false** —— 留着是为了让调用点不必
/// 自己判断「兜底算不算清单」（`catalog` 的可用性判据读的是 [`list`] 的长度，
/// 兜底会算进去，这是刻意的：见模块头第 3 条）。
pub fn is_empty() -> bool {
    list().is_empty()
}

/// 客户端给的名字 → 本家内部的模型名。
///
/// ── 为什么这里是「恒等 + 白名单校验」而不是「映射」──────────────
/// 上游的 `mode_id` 就是网关下发的名字（本家不做名字改写 —— 与 Command Code
/// 那种「目录 id 原样透传」同一处境）。因此这个函数只做两件事：
///   1. 在清单里**找得到**就返回原名（说明这个名字是本家认得的一个模式）；
///   2. 找不到返回 `None`，由调用方决定「原样发」还是「报 400」。
/// 注意**不要**在这里做模糊匹配（`contains` 那种）：客户端点的是 `/v1/models`
/// 里的名字，模糊匹配会把一个「名字没映射」的问题变成一个静默的错误请求。
pub fn upstream_id_for(requested: &str) -> Option<String> {
    let target = requested.trim();
    if target.is_empty() {
        return None;
    }
    let items = list();
    let hit = items
        .iter()
        .find(|item| {
            item.get("id").and_then(Value::as_str) == Some(target)
                || item.get("name").and_then(Value::as_str) == Some(target)
        })
        .and_then(|item| item.get("id").and_then(Value::as_str))
        .map(str::to_string);
    hit
}

/// 拉一次模型目录并落地（内存 + 持久化缓存）。
///
/// `force = false`（自动路径）时 [`TTL_MS`] 内早退为 `unchanged()`；
/// `force = true`（用户手动刷新）真打上游。
///
/// 失败时**保留现有清单**（内存里的优先，其次兜底），返回 `failed(原因)`
/// 让手动路径如实汇报 —— 自动路径不看返回值，只打日志。
pub async fn refresh(credentials: &Credentials, force: bool) -> ModelRefreshOutcome {
    let now = crate::server::logging::now_ms();
    if !force {
        let fresh = with_state(|slot| {
            slot.as_ref()
                .map(|state| state.remote && now - state.fetched_at < TTL_MS)
                .unwrap_or(false)
        });
        if fresh {
            return ModelRefreshOutcome::unchanged();
        }
    }
    // 风控 query 必须带上（上游对目录类接口同样校验公参）
    let query = endpoints::encode_query(&endpoints::security_params(&credentials.fingerprint));
    let url = format!("{}?{}", endpoints::action_bar_brief_list_url(), query);
    let mut headers = endpoints::base_headers();
    headers.extend(endpoints::auth_headers(
        &credentials.cookie_header(),
        credentials.csrf_token().as_deref(),
    ));
    let response = match send_raw("GET", &url, None, &headers, None, Some(REQUEST_TIMEOUT_MS)).await
    {
        Ok(response) => response,
        Err(error) => {
            return ModelRefreshOutcome::failed(format!(
                "豆包工作模型目录查询失败：{error}（保留现有清单）"
            ))
        }
    };
    if response.status == 401 || response.status == 403 {
        return ModelRefreshOutcome::failed(
            "豆包工作登录态已失效（HTTP 401/403）：请在账号页重新登录后刷新（保留现有清单）",
        );
    }
    if !response.ok {
        return ModelRefreshOutcome::failed(format!(
            "豆包工作模型目录返回 HTTP {}（保留现有清单）",
            response.status
        ));
    }
    let payload = response.payload.unwrap_or(Value::Null);
    let models = parse_catalog(&payload);
    if models.is_empty() {
        return ModelRefreshOutcome::failed(
            "豆包工作模型目录为空或格式不认识（三级载体 mode_list / entry_list / \
             model_list 都没解析出条目；保留现有清单）",
        );
    }
    let count = models.len();
    with_state(|slot| {
        *slot = Some(CatalogState {
            models: models.clone(),
            fetched_at: now,
            remote: true,
        });
    });
    catalog_cache::save(SCOPE, &models, now);
    crate::server::logging::log(
        "[DoubaoWork]",
        &format!("模型目录刷新完成：{count} 个模式"),
    );
    ModelRefreshOutcome::refreshed(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 兜底清单**永远非空**（清单为空的家不进 `/v1/models`，见模块头第 3 条）
    #[test]
    fn fallback_is_never_empty() {
        let items = fallback_models();
        assert_eq!(items.len(), FALLBACK_MODELS.len());
        for item in &items {
            assert!(item.get("id").and_then(Value::as_str).is_some_and(|id| !id.is_empty()));
            assert!(item.get("name").and_then(Value::as_str).is_some_and(|name| !name.is_empty()));
        }
        // 默认模型必须在兜底清单里（否则「客户端不指定 model」会路由到一个不存在的名字）
        assert!(items
            .iter()
            .any(|item| item.get("id").and_then(Value::as_str) == Some(DEFAULT_MODEL)));
    }

    /// ⚠️ **别把 LobsterAI 的清单抄进来**（这是本文件最容易犯的错，见模块头）
    #[test]
    fn fallback_does_not_contain_lobsterai_model_names() {
        let ids: Vec<&str> = FALLBACK_MODELS.iter().map(|(id, ..)| *id).collect();
        for foreign in [
            "MiniMax-M2.7",
            "qwen3.7-max",
            "qwen3.7-plus",
            "kimi-k2.7-code",
            "kimi-k2.7-code-highspeed",
            "kimi-k2.6",
            "doubao-seed-2-1-pro-260628",
            "doubao-seed-2-1-turbo-260628",
            "doubao-seed-2-0-code-preview-260215",
        ] {
            assert!(
                !ids.contains(&foreign),
                "{foreign} 属于 LobsterAI 的清单（已核实），不能出现在本家兜底里"
            );
        }
        // 兜底里只允许出现形态确实是豆包自家模式的 `doubao-*` 名字
        assert!(ids.iter().all(|id| id.starts_with("doubao-")));
    }

    /// 工具调用能力**如实为 false**（本家是文本桥接，不是原生 function calling）
    #[test]
    fn tool_call_is_declared_unsupported() {
        for item in fallback_models() {
            assert_eq!(
                item.get("supportsToolCall"),
                Some(&Value::Bool(false)),
                "本家是文本桥接，广告成原生支持会让客户端发 tools 去换一个上游不认识"
            );
            // 上游收图但本仓传不上去（同步/异步接缝，见 SUPPORTS_IMAGES）：
            // 广告成 true 会让客户端塞图进请求、用户以为生效了
            assert_eq!(item.get("supportsImages"), Some(&Value::Bool(false)));
        }
    }

    /// 三级载体解析：`mode_list` 优先；条目可以再包一层（`mode` / `model` / `item`）
    #[test]
    fn catalog_parses_three_carrier_levels() {
        let payload = json!({"data": {"mode_list": [
            {"mode_id": "doubao-fast", "mode_name": "豆包·快速", "deep_think": 0},
            {"mode": {"mode_id": "doubao-expert", "mode_name": "豆包·专家", "deep_think": 3}}
        ]}});
        let parsed = parse_catalog(&payload);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].get("id").and_then(Value::as_str), Some("doubao-fast"));
        assert_eq!(
            parsed[0].get("supportsReasoning"),
            Some(&Value::Bool(false)),
            "deep_think=0 是关闭档"
        );
        assert_eq!(
            parsed[1].get("supportsReasoning"),
            Some(&Value::Bool(true)),
            "deep_think=3 是专家档"
        );
        // 第二、三级载体（mode_list 没有时）
        let entry_level = json!({"data": {"entry_list": [{"model_name": "doubao-think"}]}});
        assert_eq!(parse_catalog(&entry_level).len(), 1);
        let model_level = json!({"model_list": [{"name": "doubao-fast"}]});
        assert_eq!(parse_catalog(&model_level).len(), 1);
        // 三级都没有 → 空（调用方据此报「格式不认识」而不是静默）
        assert!(parse_catalog(&json!({"unexpected": true})).is_empty());
    }

    /// 白名单（`support_models` / `conversation_mode_list`）确实收窄结果
    #[test]
    fn whitelist_narrows_the_catalog() {
        let payload = json!({
            "support_models": ["doubao-fast"],
            "data": {"mode_list": [
                {"mode_id": "doubao-fast"},
                {"mode_id": "doubao-expert"}
            ]}
        });
        let parsed = parse_catalog(&payload);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].get("id").and_then(Value::as_str), Some("doubao-fast"));
    }

    /// 去重：同一个 id 只出一条（上游可能同时出现在两级里）
    #[test]
    fn duplicate_ids_are_deduplicated() {
        let payload = json!({"data": {"mode_list": [
            {"mode_id": "doubao-fast"},
            {"mode_id": "doubao-fast", "mode_name": "重复"}
        ]}});
        assert_eq!(parse_catalog(&payload).len(), 1);
    }

    /// `deep_think` 三种形态（整数 / 布尔 / 字符串）都要认
    #[test]
    fn deep_think_accepts_three_shapes() {
        assert_eq!(deep_think_level(&json!(0)), "off");
        assert_eq!(deep_think_level(&json!(1)), "on");
        assert_eq!(deep_think_level(&json!(3)), "expert");
        assert_eq!(deep_think_level(&json!(true)), "on");
        assert_eq!(deep_think_level(&json!(false)), "off");
        assert_eq!(deep_think_level(&json!("expert")), "expert");
        assert_eq!(deep_think_level(&json!("3")), "expert");
        // 表外的自定义值原样保留（不猜）
        assert_eq!(deep_think_level(&json!(7)), "7");
    }

    /// 上下文窗口（上游给了才写 —— 「没给」与「给了一个 0」是两件事）
    #[test]
    fn context_window_is_omitted_when_absent_or_zero() {
        let with = parse_catalog(&json!({"data": {"mode_list": [
            {"mode_id": "a", "context_window": 200000}
        ]}}));
        assert_eq!(
            with.first().and_then(|item| item.get("maxInputTokens")).and_then(Value::as_i64),
            Some(200_000)
        );
        let without = parse_catalog(&json!({"data": {"mode_list": [
            {"mode_id": "b", "context_window": 0}
        ]}}));
        assert!(without
            .first()
            .and_then(|item| item.get("maxInputTokens"))
            .is_none());
    }

    /// `upstream_id_for`：名字或 id 都能命中；认不出返回 None（**不做模糊匹配**）
    #[test]
    fn upstream_id_resolution_is_exact_only() {
        // 兜底清单生效时（本单测进程里没有远程清单）
        assert_eq!(upstream_id_for("doubao-fast"), Some("doubao-fast".to_string()));
        assert_eq!(upstream_id_for("豆包·快速"), Some("doubao-fast".to_string()));
        assert_eq!(upstream_id_for("no-such-mode"), None);
        assert_eq!(upstream_id_for("   "), None);
        // 模糊匹配必须**不**命中（否则「名字没映射」会变成一次静默的错误请求）
        assert_eq!(upstream_id_for("doubao"), None);
    }

    /// `list()` 恒非空（兜底会兜住），`remote_refreshed()` 在没远程清单时 false
    #[test]
    fn list_is_never_empty_and_remote_flag_is_honest() {
        assert!(!list().is_empty());
        assert!(!is_empty());
        // 本单测进程没拉过远程目录 → 来源标记必须是 false（不能谎报「远程」）
        assert!(!remote_refreshed());
    }
}
