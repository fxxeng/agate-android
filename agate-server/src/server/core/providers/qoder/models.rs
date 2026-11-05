//! Qoder 模型目录（移植来源 `Qoder-Proxy/src/models.mjs`）。
//!
//! ── 清单从哪来 ──────────────────────────────────────────────
//!   1. **静态兜底**：`FALLBACK` 里按地区的两个清单。进程启动即可用，
//!      网络不通 / 没账号时 `/v1/models` 仍然有内容。
//!   2. **远程刷新**：`GET {gateway}algo/api/v2/model/list?Encode=1`（COSY 签名，
//!      缓存 1 小时）。失败保留现有清单 —— 与源实现 `refreshModels` 的两级兜底
//!      逐条一致。
//!
//! ── 为什么按地区分开缓存（与其它四家不同）─────────────────────
//! Qoder 分国际版（`api3.qoder.sh`）与中国版（`gateway.qoder.com.cn`），
//! 两边的目录**不是同一份**（套餐档位不同）。缓存按 region 分开。
//! **拆家（2026-10）后两个地区是两家 provider**（`qoder` 中国版 /
//! `qoder-intl` 国际版）：`list_for(region)` 只返回**本地区**的清单
//! （适配器 `list_models` 按自己的地区取）；`list()` 保留两地区的并集
//! （按 id 去重，global 优先）——只供种子与排障使用。
//! 请求时 `resolve` 先在**账号所属地区**的目录里找，再退到另一地区 ——
//! 于是「这个账号能不能用这个模型」由上游说了算，而不是我们猜。
//!
//! ── 条目的键名口径（与另外四家对齐）──────────────────────────
//! 聚合层的 `models::list_item` 认的是 `id` / `name` / `maxInputTokens` /
//! `maxOutputTokens` / `supportsImages` / `supportsReasoning` /
//! `supportsToolCall` / `isDefault` / `kind`（见 `core::models::shape`）。
//! 本模块直接**按这套键名产出**，不再走中间形态；协议层要的 `upstreamKey`
//! 与 `config` 另存两个键 —— 它们不会被 `list_item` 带进 `/v1/models`
//! （那个函数只挑它认识的键），但路由与请求构造要读。
//!
//! ── 进程级句柄 ──────────────────────────────────────────────
//! 与 `raccoon::models` / `core::models` 同一模式（`OnceLock` + `RwLock`）：
//! 适配器是无状态单例，清单必须挂在进程级的共享句柄上，刷新才能对所有调用点
//! （`/v1/models`、路由判定、后台刷新）同时可见。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：零 unwrap/expect/panic；持锁期间绝不做网络请求
//! （刷新先把请求发完、解析完，最后才在锁内做一次整体替换）。

use std::sync::{OnceLock, RwLock};

use serde_json::{json, Value};

use crate::server::core::providers::adapter::ModelRefreshOutcome;
use crate::server::core::providers::catalog_cache;
use crate::server::logging;

use super::context;
use super::cosy::{self, CosyIdentity};
use super::credentials::Credentials;
use super::endpoints::Site;

/// 远程目录缓存有效期（源实现 `CACHE_TTL_MS`：1 小时）
const CACHE_TTL_MS: i64 = 60 * 60 * 1000;

/// 目录请求超时（毫秒）。目录是短响应，给 20 秒足够；
/// 与转发链路共用出网点（`egress::client_for`），这里只叠加一层总超时。
const REQUEST_TIMEOUT_MS: u64 = 20_000;

/// 输出上限固定 32768。
///
/// 源实现取这个值而不是更大：实测超过约 32K 后上游行为会退化
/// （关闭思考时仍输出思考内容，约 49K 起返回空内容甚至直接断连）。
/// 这是上游的实际约束，不是保守取值。
pub const MAX_OUTPUT_TOKENS: i64 = 32_768;

/// 目录条目没给上下文长度时的兜底
const DEFAULT_CONTEXT_WINDOW: i64 = 200_000;

/// 内置兜底清单。
///
/// `enabled` 在这里只是「免费档通常可用」的保守猜测（源实现同款注释）；
/// 真实可用性以运行时从上游拉回的清单为准 —— 拉回来的 `enable` 字段才是
/// 「当前套餐是否可用」。
///
/// **不过滤不可用的模型**：用户需要看到完整清单，否则分不清「模型不存在」
/// 与「没权限」，也看不到升级套餐能解锁什么。
///
/// ── 元组末尾的倍率是什么 ────────────────────────────────────
/// 上游 `price_factor` 的静态快照（2026-09-20 实测）。取**正常价**而不是
/// 折扣价：断网 / 未登录时才用这张表，而折扣是错峰时段的临时态，把它写死
/// 会让离线用户在非折扣时段看到低一档的价 —— 折扣由远程刷新如实带回来。
/// 空串 = 上游也没有这个值（会显示成 `—`）。
fn fallback(site: Site) -> Vec<Value> {
    if site.is_qwenwork() {
        return qwenwork_fallback();
    }
    let global: &[(&str, &str, bool, bool, &[&str], bool, bool, &str)] = &[
        ("Qwen3.8-Flash", "qfmodel", true, true, &["low", "medium", "xhigh"], true, true, "0.1"),
        ("Qwen3.8-Max", "qmodel_38max", true, true, &["low", "medium", "xhigh"], true, true, "0.5"),
        ("Auto", "auto", true, false, &[], true, false, "1"),
        ("Ultimate", "ultimate", true, true, &[], true, false, "1.6"),
        ("Performance", "performance", true, true, &[], true, false, "1.1"),
        ("Efficient", "efficient", false, false, &[], true, false, "0.3"),
        ("Sonus", "smodel", true, true, &[], true, false, "3.2"),
        ("Cantus", "cmodel", true, true, &[], true, false, "3.2"),
        ("Qwen3.7-Max", "qmodel_latest", true, true, &[], true, false, "0.5"),
        // `Qwen3.7-Plus` 带连字符：上游 `display_name` 就是这个形态，而远程
        // 刷新走的是 `display_name` 去空白（见 `parse_catalog`）。少了连字符
        // 会让同一个模型产出两种 id（离线用兜底、在线用远程），客户端缓存里
        // 留下两条记录
        ("Qwen3.7-Plus", "qmodel", false, false, &[], true, false, "0.1"),
        ("Kimi-K3", "kmodel_latest", false, false, &[], true, false, "0.8"),
        ("Kimi-K2.8-Preview", "kmodel", false, false, &[], true, false, "0.3"),
        ("GLM-5.3", "gmodel", true, true, &[], true, false, "0.6"),
        ("GLM-5.3-Flash", "gfmodel", true, true, &[], true, false, "0.1"),
        ("DeepSeek-V4-Pro", "dmodel", true, true, &[], true, false, "0.8"),
        ("DeepSeek-Flash", "dfmodel", true, true, &[], true, false, "0.2"),
        ("MiniMax-M3", "mmodel", false, false, &[], true, false, "0.2"),
    ];
    let cn: &[(&str, &str, bool, bool, &[&str], bool, bool, &str)] = &[
        ("Qwen3.8-Flash", "qfmodel", true, true, &["low", "medium", "xhigh"], true, true, "0.1"),
        ("Qwen3.8-Max", "qmodel_38max", true, true, &["low", "medium", "xhigh"], true, true, "0.5"),
        ("Auto", "auto", true, false, &[], true, false, "1"),
        ("Qwen3.7-Max", "qmodel_latest", true, false, &[], true, false, "0.5"),
        ("Qwen3.7-Plus", "qmodel", true, false, &[], false, false, "0.1"),
        ("DeepSeek-V4-Pro", "dmodel", true, false, &[], false, false, "0.8"),
        ("DeepSeek-Flash", "dfmodel", false, false, &[], false, false, "0.2"),
        ("GLM-5.3", "gmodel", true, false, &[], true, false, "0.6"),
        ("Kimi-K2.8-Preview", "kmodel", true, false, &[], true, false, "0.3"),
        ("MiniMax-M3", "mmodel", false, false, &[], false, false, "0.2"),
    ];
    let rows = if site == Site::QoderCn { cn } else { global };
    rows.iter()
        .map(|(id, key, reasoning, supports_effort, efforts, vision, enabled, factor)| {
            entry(
                id,
                key,
                *reasoning,
                if *supports_effort { efforts } else { &[] },
                *vision,
                *enabled,
                DEFAULT_CONTEXT_WINDOW,
                "system",
                &credits_of_text(factor),
            )
        })
        .collect()
}

/// 千问办公的静态兜底清单（`标准` / `高级` / `Qwen3.8-Max`）。
///
/// 与远程目录条目同一形状（`upstreamKey` 才是上游 key），来源是上游插件
/// `qwenwork_fallback_models`：上游 2026-10-31 复核时只有这三个（社区项目
/// 提到的 `qwork-auto` / `advanced` / `ultimate` 在当前上游不存在）；
/// 远程目录拿得到时以远程为准。
///
/// 上下文按**目录声明值**（131072）填，而不是站点声明值（1.02M）：这个数会
/// 同时出现在 `maxInputTokens` 与发往上游的 `config.max_input_tokens` 里，
/// 两处必须一致 —— 不一致会让「界面写 128K、请求按 1M 发」这种矛盾重新出现。
fn qwenwork_fallback() -> Vec<Value> {
    const ROWS: &[(&str, &str)] = &[
        ("pro", "高级"),
        ("flash", "标准"),
        ("qwen3.8-max-preview", "Qwen3.8-Max"),
    ];
    const CONTEXT: i64 = 131_072;
    ROWS.iter()
        .map(|(key, display)| {
            let mut model = entry(
                &display.replace(' ', ""),
                key,
                false,
                &[],
                false,
                true,
                CONTEXT,
                "system",
                "",
            );
            if let Some(object) = model.as_object_mut() {
                object.insert("name".to_string(), Value::String(display.to_string()));
                if let Some(config) = object.get_mut("config").and_then(Value::as_object_mut) {
                    config.insert("max_input_tokens".to_string(), Value::from(CONTEXT));
                }
            }
            model
        })
        .collect()
}

/// 倍率数值 → `credits` 列的展示文本。
///
/// ── 为什么是 `x{n} credits` 而不是裸数字 ──────────────────────
/// 这一列是**跨 provider 共用**的：WorkBuddy 的 `credits` 就是上游下发的
/// `"x0.16 credits"` 形态（见 `core::models::builtin_models`），前端用同一个
/// 正则 `/x\s*([\d.]+)/` 把它渲染成 `0.16x`。本家用同形文本就自动落进同一条
/// 渲染分支，不必为一个 provider 加特例。
///
/// ── 为什么不用 `format!("{:.1}")` ────────────────────────────
/// Qoder 的倍率有 `0.04` 这一档（Qwen3.7-Plus 的折扣价），一位小数会把它
/// 四舍五入成 `0.0` —— 一个「免费」的错误暗示。这里保留两位并按需裁零：
/// `0.04` → `0.04`、`0.1` → `0.1`、`1` → `1`。
fn credits_of_text(plain: &str) -> String {
    if plain.trim().is_empty() {
        return String::new();
    }
    format!("x{} credits", plain.trim())
}

/// 上游 `price_factor` → 展示文本（远程路径）。
///
/// `price_factor = 0` 是**合法值**（Qwen3.8-Flash 免费档，界面显示 `0x`），
/// 不能当缺失去掉。这里刻意返回**字符串**而非数字：`list_item` 的
/// `credits` 走 `js_truthy` 判定，数字 `0` 会被判成假值而丢掉这个键 ——
/// 于是「免费」在界面上变成「未知」，两者含义正好相反。
///
/// 接受字符串形态（实测上游给的是 JSON 数字，但同一家的其它目录字段就混着
/// 两种形态）—— 只认数字时，上游哪天改成 `"0.5"` 会静默变成「没给」。
fn credits_of(item: &Value) -> String {
    let factor = match item.get("price_factor") {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.trim().parse::<f64>().ok(),
        _ => None,
    };
    let Some(factor) = factor else {
        return String::new();
    };
    if !factor.is_finite() || factor < 0.0 {
        return String::new();
    }
    credits_of_text(&format_factor(factor))
}

/// 倍率数字 → 紧凑文本（整数不带小数点，其余最多两位小数、裁掉尾零）。
fn format_factor(value: f64) -> String {
    let rounded = (value * 100.0).round() / 100.0;
    if (rounded - rounded.trunc()).abs() < 1e-9 {
        return format!("{}", rounded.trunc() as i64);
    }
    let text = format!("{rounded:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// 构造一条目录条目（协议层与聚合层共用同一形状）。
///
/// `config` 是发往上游的 `model_config` 素材（见 `protocol::slim_model_config`）：
/// 只保留上游判定推理链路要用的几个字段 —— 源实现明确剥掉 `thinking_config`，
/// 因为它会**覆盖** `parameters.enable_thinking`，让「关闭思考」失效。
///
/// `credits` 是倍率列的文本（空串 = 上游没给）；它只进清单展示，
/// 不参与 `config`（那是发往上游的请求素材，多一个键都可能被上游校验拒绝）。
fn entry(
    id: &str,
    upstream_key: &str,
    reasoning: bool,
    efforts: &[&str],
    vision: bool,
    enabled: bool,
    context_window: i64,
    source: &str,
    credits: &str,
) -> Value {
    let mut model = json!({
        "id": id,
        "name": id,
        "upstreamKey": upstream_key,
        "enabled": enabled,
        "reasoning": reasoning,
        "supportsReasoning": reasoning,
        "supportsImages": vision,
        // Qoder 的上游是 agent 形态，工具调用是固有能力
        "supportsToolCall": true,
        "efforts": efforts,
        "maxInputTokens": context_window,
        "maxOutputTokens": MAX_OUTPUT_TOKENS,
        "isDefault": false,
        "kind": "chat",
        "config": {
            "key": upstream_key,
            "is_reasoning": reasoning,
            "is_vl": vision,
            "source": source,
        },
    });
    // 倍率键只在有值时插入：`list_item` 对缺失的 `credits` 会输出空串
    // （`js_truthy` 判定），这与插入空串等价；少一个键让「上游没给」与
    // 「上游给了空串」在内部状态里可区分
    if !credits.is_empty() {
        if let Some(object) = model.as_object_mut() {
            object.insert("credits".to_string(), Value::String(credits.to_string()));
        }
    }
    model
}

/// 一个站点的远程目录（清单 + 拉到它的时刻）。
#[derive(Default, Clone)]
struct CatalogSlot {
    models: Vec<Value>,
    fetched_at: i64,
}

/// 内部状态：每个站点一格。
///
/// ── 为什么是数组而不是三个字段 ──────────────────────────────
/// 三个字段写起来要三份 if/match，加站点时每一处都得记得补 —— 那正是这一
/// 类代码漏改的来源。`slot(site)` 是唯一的索引映射，加站点时它编译期就会
/// 提醒（`match` 穷举），其余代码只按索引取。
#[derive(Default, Clone)]
struct CatalogState {
    slots: [CatalogSlot; 3],
}

fn slot(site: Site) -> usize {
    match site {
        Site::QoderCn => 0,
        Site::QoderIntl => 1,
        Site::QwenworkCn => 2,
    }
}

impl CatalogState {
    fn slot(&self, site: Site) -> &CatalogSlot {
        &self.slots[slot(site)]
    }

    fn slot_mut(&mut self, site: Site) -> &mut CatalogSlot {
        let index = slot(site);
        &mut self.slots[index]
    }
}

/// 站点 → 持久化缓存作用域（`catalog_cache` 的键）。
fn cache_scope(site: Site) -> &'static str {
    match site {
        Site::QoderCn => catalog_cache::SCOPE_QODER_CN,
        Site::QoderIntl => catalog_cache::SCOPE_QODER_GLOBAL,
        Site::QwenworkCn => catalog_cache::SCOPE_QWENWORK,
    }
}

/// 进程级目录句柄。首次初始化时**先从持久化缓存恢复**（每个站点各一份，
/// 按站点分开的理由见模块头「为什么按站点分开缓存」），没有再留空 ——
/// 空状态的读取语义就是「回落到静态兜底清单」。
fn catalog() -> &'static RwLock<CatalogState> {
    static CATALOG: OnceLock<RwLock<CatalogState>> = OnceLock::new();
    CATALOG.get_or_init(|| RwLock::new(restored_state()))
}

/// 首次初始化读一次持久化缓存（见 `providers::catalog_cache` 的模块头）。
///
/// 缓存里存的就是 `CatalogSlot` 的形态（`refresh` 落地的那份），
/// 所以这里只做「搬回来」：不重新解析、也不重新归一。
fn restored_state() -> CatalogState {
    let mut state = CatalogState::default();
    for site in Site::ALL {
        if let Some(cached) = catalog_cache::load(cache_scope(site)) {
            let target = state.slot_mut(site);
            target.models = cached.models;
            target.fetched_at = cached.fetched_at;
        }
    }
    state
}

fn read_state() -> CatalogState {
    match catalog().read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// 某站点当前生效的清单（远程优先，否则静态兜底）
fn catalog_for(state: &CatalogState, site: Site) -> Vec<Value> {
    let remote = &state.slot(site).models;
    if remote.is_empty() {
        fallback(site)
    } else {
        remote.clone()
    }
}

/// 当前清单（各站点的**并集**，按 id 去重、注册表顺序优先）。
///
/// 拆家后聚合目录与路由判定读的是**各家自己的** `list_for`（站点是 provider
/// 身份）；并集只留给种子与排障 —— 种子按站点分别种，见 `seed_default_rules`。
pub fn list() -> Vec<Value> {
    let state = read_state();
    let mut out: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for site in Site::ALL {
        for model in catalog_for(&state, site) {
            let id = text_of(&model, "id");
            if id.is_empty() {
                continue;
            }
            let key = id.to_lowercase();
            if seen.iter().any(|known| known == &key) {
                continue;
            }
            seen.push(key);
            let mut model = model;
            if let Some(object) = model.as_object_mut() {
                object.insert("region".to_string(), Value::String(site.id().to_string()));
            }
            out.push(model);
        }
    }
    out
}

/// **本站点**的当前清单（远程优先，否则静态兜底）。
///
/// 拆家后每家 provider 只列本站点（适配器 `list_models` 按 `self.site` 取）：
/// 模型管理、`/v1/models` 广告与「某模型由哪些家提供」的判定都由 provider
/// 身份自动分开了 —— 中国版的账号/清单/启停归 `qoder`，国际版归 `qoder-intl`，
/// 国内版归 `qwenwork`。
pub fn list_for(site: Site) -> Vec<Value> {
    let state = read_state();
    catalog_for(&state, site)
        .into_iter()
        .filter(|model| !text_of(model, "id").is_empty())
        .map(|mut model| {
            if let Some(object) = model.as_object_mut() {
                object.insert("region".to_string(), Value::String(site.id().to_string()));
            }
            model
        })
        .collect()
}

/// 把客户端的模型名解析成「上游标识 + 模型配置 + 站点」。
///
/// ── 匹配顺序（与 `catalog::providers_for_model` 同口径）──────────
/// 先按 `id` 全等（忽略大小写），再按 `name` 全等。源实现还允许按
/// `upstreamKey` 匹配 —— 那是同一条链路上「客户端恰好写了内部 key」的容错，
/// 这里一并保留（客户端真写 `qfmodel` 时不该报「模型不存在」）。
///
/// `site` 是**账号所属站点**：先在自家目录里找，找不到再按注册表顺序看其余
/// 站点 —— 这样「国际版账号请求中国版专属模型」会拿到一个明确的解析结果，
/// 由上游报出真实原因（而不是网关谎报「模型不存在」）。
pub fn resolve(model_id: &str, site: Site) -> Option<Value> {
    let wanted = model_id.trim().to_lowercase();
    if wanted.is_empty() {
        return None;
    }
    let state = read_state();
    let ordered: Vec<Site> = std::iter::once(site)
        .chain(Site::ALL.into_iter().filter(|other| *other != site))
        .collect();
    // ① 各家当前生效的清单（远程优先，空则兜底）
    for candidate in &ordered {
        if let Some(found) = find_in(&catalog_for(&state, *candidate), &wanted) {
            return Some(found);
        }
    }
    // ② 远程目录非空、但**缺这个模型**的站点，再看一眼它的静态兜底：
    //    这一步覆盖「远程目录在一边有、另一边没有」的交错情形。
    for candidate in &ordered {
        if state.slot(*candidate).models.is_empty() {
            continue;
        }
        if let Some(found) = find_in(&fallback(*candidate), &wanted) {
            return Some(found);
        }
    }
    None
}

/// 在**不掌握站点**的场合解析模型名（例如拿上游回来的名字反查对外 id）。
///
/// [`resolve`] 的第一个参数只是「优先看哪家」，其余站点照样会查；这里给一个
/// 固定的起点，把「起点是谁」这件事从调用方手里收回来 —— 三家目录有交集时
/// 结果一致，没必要让调用方去猜一个站点。
pub fn resolve_any(model_id: &str) -> Option<Value> {
    resolve(model_id, Site::QoderCn)
}

/// 在一个清单里按 id → name → upstreamKey 的顺序找（都与源实现同序）
fn find_in(models: &[Value], wanted: &str) -> Option<Value> {
    let matches = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(|text| text.trim().to_lowercase() == wanted)
            .unwrap_or(false)
    };
    for key in ["id", "name", "upstreamKey"] {
        if let Some(found) = models.iter().find(|model| matches(model, key)) {
            return Some(found.clone());
        }
    }
    None
}

/// 是否已经成功采用过远程清单（`catalog_refresh_meta` 用）
pub fn remote_refreshed(site: Site) -> bool {
    !read_state().slot(site).models.is_empty()
}

/// 某站点最后一次成功刷新远程目录的时间（毫秒；从未成功过为 0）。
///
/// 拆家后「来源 / 更新日期」列按**各家**取值（`catalog::refresh_meta` 的
/// Qoder / QoderIntl / Qwenwork 三个分支各查一格），不再取所有站点的最大值
/// —— 那会让几家显示同一次拉取时刻。
pub fn last_refreshed_at(site: Site) -> i64 {
    read_state().slot(site).fetched_at
}

/// 最后一次成功刷新远程目录的时间（毫秒；从未成功过为 0）——各站点的最大值。
#[allow(dead_code)]
pub fn last_refreshed_at_any() -> i64 {
    let state = read_state();
    Site::ALL
        .iter()
        .map(|site| state.slot(*site).fetched_at)
        .max()
        .unwrap_or(0)
}

/// 拉取并落地某站点的远程目录。
///
/// 三档返回值与 `ModelRefreshOutcome` 的契约一致（成功 / 没刷 / 失败了）。
/// `force = false` 时命中 TTL 直接早退（自动路径）；用户手动点刷新时
/// 调用方传 `true` 绕过 —— 缓存该不该复用只由**谁发起**决定。
pub async fn refresh(
    credentials: &Credentials,
    proxy: Option<&crate::server::core::proxies::ResolvedProxy>,
    force: bool,
) -> ModelRefreshOutcome {
    let site = credentials.site;
    if !force {
        let state = read_state();
        let slot = state.slot(site);
        if !slot.models.is_empty() && logging::now_ms() - slot.fetched_at < CACHE_TTL_MS {
            return ModelRefreshOutcome::unchanged();
        }
    }

    let url = format!("{}algo/api/v2/model/list?Encode=1", site.gateway());
    let identity = CosyIdentity {
        user_id: &credentials.user_id,
        auth_token: &credentials.access_token,
        name: &credentials.name,
        email: &credentials.email,
        machine_id: &credentials.machine_id,
    };
    // 目录是 GET：请求体为空，签名覆盖空体（源实现传 null 同上）
    let headers = match cosy::build_auth_headers(None, &url, &identity, site) {
        Ok(headers) => headers,
        Err(error) => return ModelRefreshOutcome::failed(error.message),
    };
    let outcome = crate::server::core::auth_http::send_raw(
        "GET",
        &url,
        None,
        &headers,
        proxy,
        Some(REQUEST_TIMEOUT_MS),
    )
    .await;
    let payload = match outcome {
        Ok(response) => {
            if !response.ok {
                return ModelRefreshOutcome::failed(format!("上游返回 HTTP {}", response.status));
            }
            response.payload.unwrap_or(Value::Null)
        }
        Err(error) => {
            return ModelRefreshOutcome::failed(
                crate::server::core::egress::describe_error_detail(&error),
            );
        }
    };
    // 上游信封：业务错误也在 200 里（`statusCodeValue` / `code`）
    if let Some(message) = envelope_error(&payload) {
        return ModelRefreshOutcome::failed(message);
    }
    let models = parse_catalog(&payload, site);
    if models.is_empty() {
        return ModelRefreshOutcome::failed("上游返回的模型目录为空");
    }
    let count = models.len();
    let mut state = read_state();
    let now = logging::now_ms();
    // 先落持久化缓存（进程重启后由 `restored_state` 读回）：只存刷新成功的
    // 这一格，其余站点的缓存原样留着（它们有自己的刷新周期）。`state` 随后
    // 要被 `write` 消费，所以缓存写在前面；**不在目录锁内** —— 缓存写入要拿
    // 库连接锁，两把锁不能嵌套。
    {
        let slot = state.slot_mut(site);
        slot.models = models;
        slot.fetched_at = now;
        catalog_cache::save(cache_scope(site), &slot.models, now);
    }
    match catalog().write() {
        Ok(mut guard) => *guard = state,
        Err(poisoned) => *poisoned.into_inner() = state,
    }
    logging::log(
        "[Models]",
        &format!("✅ {} 模型目录已更新（{count} 个）", site.display_name()),
    );
    // 默认规则种子（只默认启用 QODER_DEFAULT_ENABLED 白名单里的模型，见
    // `model_rules::seed_qoder_defaults`）：对**并集**种一次而不是本次这一格
    // 的清单 —— 各站点的 id 一次全覆盖，别家不必等自己刷新过才轮到。只对
    // 首次出现的 id 生效，用户的手动调整不会被这里覆盖。
    seed_default_rules();
    ModelRefreshOutcome::refreshed(count)
}

/// 对当前清单补一次默认规则种子（**每个站点各按各的 provider 种**）。
///
/// 刷新成功后由 [`refresh`] 调用；编排入口（`providers::adapter` 的
/// `seed_current_qoder_defaults`）也调它 —— 覆盖「远程刷新失败、手里只有静态
/// 兜底清单」与**升级用户**首次打开管理页的情形。幂等：种过的 `(provider, id)`
/// 不再动（用户的手动调整不会被覆盖）。拆家后种子按 provider id 分开记
/// （`qoder` / `qoder-intl` / `qwenwork` 各一套 `(provider, id)` 命名空间）。
pub fn seed_default_rules() {
    for site in Site::ALL {
        let ids: Vec<String> = list_for(site)
            .iter()
            .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        if let Some(summary) = crate::server::core::model_rules::seed_qoder_defaults(
            site.provider_id(),
            &site.display_name(),
            &ids,
        ) {
            logging::log("[Models]", &summary);
        }
    }
}

/// 上游目录响应里的业务错误（HTTP 200 也可能带错误）
fn envelope_error(payload: &Value) -> Option<String> {
    let code = payload.get("statusCodeValue").and_then(Value::as_i64)
        .or_else(|| payload.get("code").and_then(Value::as_i64));
    match code {
        Some(200) | None => None,
        Some(code) => Some(format!(
            "上游返回业务错误 {code}: {}",
            payload
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| payload.get("body").and_then(Value::as_str))
                .unwrap_or("")
                .chars()
                .take(200)
                .collect::<String>()
        )),
    }
}

/// 解析目录响应：`{ chat: [ { key, display_name, enable, is_vl, ... } ] }`。
///
/// 字段口径照抄源实现：只收 `chat` 数组，缺 `key` 或缺 `display_name` 的条目丢弃。
/// 对外的模型 id 用 `display_name` **去掉空白**（源实现 `toModelId`）——
/// 于是客户端看到的名称可读，请求时再映射回 `key`。
fn parse_catalog(payload: &Value, site: Site) -> Vec<Value> {
    // Qoder：`{ chat: [...] }`；千问办公：`{ qwork: [...] }`（见 `Site::model_scene`）
    let scene = site.model_scene().unwrap_or("chat");
    let Some(chat) = payload.get(scene).and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut models: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for item in chat {
        let key = item.get("key").and_then(Value::as_str).unwrap_or("").trim();
        let display = item.get("display_name").and_then(Value::as_str).unwrap_or("").trim();
        if key.is_empty() || display.is_empty() {
            continue;
        }
        let id: String = display.chars().filter(|ch| !ch.is_whitespace()).collect();
        if id.is_empty() {
            continue;
        }
        let lowered = id.to_lowercase();
        if seen.iter().any(|known| known == &lowered) {
            continue;
        }
        seen.push(lowered);

        let vision = item.get("is_vl").map(js_truthy).unwrap_or(false);
        let reasoning = item.get("is_reasoning").map(js_truthy).unwrap_or(false)
            || item.get("thinking_config").map(js_truthy).unwrap_or(false);
        // 上游在该模型条目里声明支持的思考档位，作为请求时的白名单
        let efforts: Vec<Value> = item
            .pointer("/thinking_config/enabled/efforts")
            .and_then(Value::as_object)
            .map(|map| map.keys().map(|key| Value::String(key.clone())).collect())
            .unwrap_or_default();
        let context_window = context_window_of(item, site);
        let source = item.get("source").and_then(Value::as_str).unwrap_or("system");
        // 倍率（官方中文名「Credit 消耗倍率」，界面标签「消耗」）：
        // 上游在模型条目**顶层**给 `price_factor`，取值 0～3.2。
        // 折扣时段 `price_factor` 本身就是折后价（另有 `original_price_factor`
        // 记原价）—— 这里取实际生效的那个，与 Qoder 界面一致。
        let credits = credits_of(item);
        // 千问办公把整份目录放在一个场景里，`enable` 只是「本套餐能不能用」，
        // 而它的客户端列表里这些模型是**列出来**的（含未开通的），所以按站点
        // 声明整份保留。Qoder 保持原样（`enable` 是逐账号的权益位）。
        let enabled = if site.list_all_models() {
            true
        } else {
            item.get("enable").map(js_truthy).unwrap_or(false)
        };

        let mut model = entry(
            &id,
            key,
            reasoning,
            &[],
            vision,
            enabled,
            context_window,
            source,
            &credits,
        );
        if let Some(object) = model.as_object_mut() {
            object.insert(
                "name".to_string(),
                Value::String(display.to_string()),
            );
            object.insert("efforts".to_string(), Value::Array(efforts));
            if let Some(format) = item.get("format") {
                if !format.is_null() {
                    if let Some(config) = object.get_mut("config").and_then(Value::as_object_mut) {
                        config.insert("format".to_string(), format.clone());
                    }
                }
            }
            // 上下文档位（请求时按 prompt 估算升级，见 `context.rs`）：含上游**当前**
            // 选中的那档（`max_input_tokens`）—— 升档判据要与它比，不能与最大档比。
            if let Some(config) = object.get_mut("config").and_then(Value::as_object_mut) {
                let tiers = context::tiers_from_catalog(item);
                if !tiers.is_empty() {
                    config.insert("tiers".to_string(), Value::Array(tiers));
                }
                if let Some(current) = item.get("max_input_tokens").and_then(Value::as_i64) {
                    if current > 0 {
                        config.insert("max_input_tokens".to_string(), Value::from(current));
                    }
                }
            }
        }
        models.push(model);
    }
    models
}

/// 上下文窗口：取 `context_config` 里各档位的最大 `token_count`（源实现同款）。
///
/// 目录没声明档位时按站点取值：千问办公用客户端声明的 1.02M（它没有
/// `context_config` 那几档，退回一个更小的默认值会让本可以放下的长对话被判
/// 成超长）；Qoder 沿用原来的 200K 兜底。
fn context_window_of(item: &Value, site: Site) -> i64 {
    let max = context::max_tokens(&context::tiers_from_catalog(item));
    if max > 0 {
        max
    } else {
        let declared = site.max_input_tokens();
        if declared > 0 { declared } else { DEFAULT_CONTEXT_WINDOW }
    }
}

/// 条目里某个键的文本形态（沿用小浣熊那边同一份 JS 语义）
fn text_of(value: &Value, key: &str) -> String {
    value
        .get(key)
        .map(super::super::raccoon::jwt::js_text)
        .unwrap_or_default()
}

/// JS 真值判定（`Boolean(x)`）：null/false/0/"" 为假，其余为真
fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().map(|item| item != 0.0).unwrap_or(false),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// 供排障：清单条数
#[allow(dead_code)]
pub fn count() -> usize {
    list().len()
}
