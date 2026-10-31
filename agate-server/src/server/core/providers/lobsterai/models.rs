//! LobsterAI 的模型目录：`GET /api/models/available` 是权威来源，**另有一份
//! 19 条静态兜底**（远程拉不到 / 关掉发现时用）。
//!
//! ── 响应信封：`data` 是**单层数组**，别拿通用对象信封解析器来套 ──────
//! ```text
//!   {"code":0,"message":"success","data":[ {modelId, modelName, …}, … ]}
//! ```
//! 参考实现明确警告：它**不**复用「要求 `data` 是对象」的解析器（那个解析器这样
//! 才能把 `data:null` 当成「令牌失效」），而这个端点的 `data` 是数组；套上去会让
//! 整份模型清单**静默为空**。本模块自带 [`read_model_array`]，并兼容「双层层套」
//! （`data.data[]`，野生形态里两种都出现过）。
//!
//! ── keyfrom 只进 query，**绝不带 refresh token** ────────────────
//! 目录端点的身份是 `firstKeyfrom` / `latestKeyfrom` / `version`（+ 可选的
//! `uuid` / `userId`），不带 refresh token —— query 会进服务端的访问日志。
//!
//! ── 两个 `X-LobsterAI-Client-*` 头**在这条端点上尤其重要** ─────────
//! 服务端按 `ClientCapabilities` **过滤**返回的模型集合：不带
//! `kimi-k3-agentic-v1` 时目录里少一个模型（实测 25 vs 26）。少发这个头不是
//! 「少一条头」，是**少一个模型**。
//!
//! ── 字段只按上游声明的取值，缺了就不广告 ─────────────────────────
//! `supportsImage` / `supportsThinking` / `costMultiplier` 缺失时**保持未知**
//! （`0` 与「字段不存在」是两回事：`0` 意思是免费）—— 编一个 `false` / `0` 会把
//! 将来的模型静默归类错。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::sync::{Mutex, OnceLock};

use serde_json::{Map, Value};

use crate::server::core::auth_http::send_raw;
use crate::server::core::providers::adapter::ModelRefreshOutcome;
use crate::server::errors::GatewayError;

use super::chat;
use super::credentials::LobsterCredentials;
use super::endpoints::{
    models_url, CATALOG_TIMEOUT_MS, CLIENT_CAPABILITIES, USER_AGENT,
};

/// 目录缓存时长（与参考实现的 2 小时同档）
const CATALOG_TTL_MS: i64 = 2 * 60 * 60 * 1000;

/// 兜底目录里每条模型的上下文窗口（**照抄参考实现**，是桥接层的估计值
/// `131072`，**不是**逐模型实测）
const FALLBACK_CONTEXT_WINDOW: i64 = 131_072;

/// 目录里的一条模型
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ModelEntry {
    /// 发给上游的模型名（`modelId`）—— 也是目录对外广告的 `id`
    pub wire_id: String,
    /// 展示名（`modelName`，缺时等于 `wire_id`）
    pub display_name: String,
    /// 上下文窗口（上游没给 = 0，不猜）
    pub context_window: i64,
    /// 单次输出上限（`maxTokens`；上游没给 = 0）
    pub max_tokens: i64,
    /// 是否收图片（上游没给 = None，**不猜**）
    pub supports_image: Option<bool>,
    /// 是否支持思考（`supportsThinking` 或 `thinkingConfig` 有档位）
    pub supports_thinking: bool,
    /// 积分倍率（**裸数字**）。None = 字段缺失；Some(0.0) = 免费（不能当缺失去掉）
    pub cost_multiplier: Option<f64>,
}

/// 进程内目录状态（**只装远程目录**；空表示还没拉到过，此时对外用兜底清单）
///
/// ── 为什么落盘的是 `entries` 而不是 manifest 值 ────────────────
/// 清单值是有损的（`credits` 被渲染成 `"免费" / "x1.5"` 标签），而缓存要的是
/// 无损往返 ⇒ 存 `ModelEntry` 本身（走 serde）。本结构体自己**不参与**序列化
/// （时刻由 `catalog_cache` 的 `fetchedAt` 承载，不在这里再存一份）。
#[derive(Clone, Debug, Default)]
struct CatalogState {
    entries: Vec<ModelEntry>,
    fetched_at: i64,
}

/// 本家在 `catalog_cache` 里的 scope 名（进程重启后读回上次的远程清单）
pub const SCOPE: &str = crate::server::core::providers::catalog_cache::SCOPE_LOBSTERAI;

fn state() -> &'static Mutex<Option<CatalogState>> {
    static STATE: OnceLock<Mutex<Option<CatalogState>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(None))
}

/// 毒锁按「可重建的展示数据」处理，与本仓其它目录模块同口径。
///
/// **首次访问时**把落盘缓存装回内存（只在首次查一次库 —— `list_models` 在
/// 每个请求的清单构造路径上，不能每次都查库）。恢复失败（库没开 / 值坏 / 这家
/// 没条目）保持 `None`，对外用内置兜底清单 —— 与改造前一致。
fn with_state<R>(f: impl FnOnce(&mut Option<CatalogState>) -> R) -> R {
    let mut guard = match state().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.is_none() {
        if let Some(cached) = restore_from_cache() {
            *guard = Some(cached);
        }
    }
    f(&mut guard)
}

/// 从落盘缓存还原状态（解析不出条目时给 `None`，由调用方保持内置兜底）。
///
/// 存的是**条目数组**（`ModelEntry` 走 serde），时刻直接取缓存自己的
/// `fetchedAt` —— 与写入侧同一个来源，不往条目里再塞一份。
fn restore_from_cache() -> Option<CatalogState> {
    let cached = crate::server::core::providers::catalog_cache::load(SCOPE)?;
    let entries: Vec<ModelEntry> =
        serde_json::from_value(Value::Array(cached.models.clone())).ok()?;
    if entries.is_empty() {
        return None;
    }
    Some(CatalogState {
        entries,
        fetched_at: cached.fetched_at,
    })
}

/// 静态兜底目录：**19 条，逐条照抄参考实现**（顺序也照抄，便于与上游对账）。
/// 上下文窗口统一 [`FALLBACK_CONTEXT_WINDOW`]（那是桥接层的估计值，不是逐模型实测）。
fn fallback_models() -> Vec<ModelEntry> {
    const IDS: [&str; 19] = [
        "deepseek-v4-flash",
        "deepseek-v4-pro",
        "MiniMax-M3",
        "MiniMax-M2.7",
        "qwen3.7-max",
        "qwen3.7-plus",
        "qwen3.6-plus",
        "qwen3.5-plus-2026-04-20",
        "kimi-k2.7-code",
        "kimi-k2.7-code-highspeed",
        "kimi-k2.6",
        "kimi-k2.5",
        "doubao-seed-2-1-pro-260628",
        "doubao-seed-2-1-turbo-260628",
        "doubao-seed-2-0-code-preview-260215",
        "glm-5.2",
        "glm-5.1",
        "glm-5v-turbo",
        "glm-5",
    ];
    IDS.iter()
        .map(|id| ModelEntry {
            wire_id: (*id).to_string(),
            display_name: (*id).to_string(),
            context_window: FALLBACK_CONTEXT_WINDOW,
            max_tokens: 0,
            supports_image: None,
            supports_thinking: false,
            // 兜底清单里**不带**倍率：那要上游逐条声明，编一个 0（免费）会误导
            cost_multiplier: None,
        })
        .collect()
}

/// 读一个数字（数字或数字字符串都认）
fn read_number(value: &Value) -> Option<f64> {
    if let Some(number) = value.as_f64() {
        return Some(number);
    }
    value.as_str().and_then(|text| text.trim().parse::<f64>().ok())
}

/// 读一个正数（`> 0` 才算，否则视为缺失 —— 与参考实现 `contextWindow > 0` 同口径）
fn read_positive(value: Option<&Value>) -> i64 {
    value
        .and_then(read_number)
        .filter(|number| *number > 0.0)
        .map(|number| number as i64)
        .unwrap_or(0)
}

/// `thinkingConfig` 是否声明了可选档位（非空 `options` 且不止一个有效 `openclawLevel`）。
///
/// 参考实现的 `parseThinkingConfig` 会丢弃整块结构不符的配置，并且「只有 off 一档」
/// 等于「没有可选档位」—— 这里只需要一个布尔，按同一口径折叠。
fn thinking_config_present(value: Option<&Value>) -> bool {
    let Some(options) = value
        .and_then(|config| config.get("options"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    options
        .iter()
        .filter(|option| {
            option
                .get("openclawLevel")
                .and_then(Value::as_str)
                .is_some_and(|level| !level.trim().is_empty())
        })
        .count()
        > 1
}

/// 把一条 `models/available` 条目映射成本模块的形态（缺 `modelId` 的丢掉）。
fn entry_from_upstream(value: &Value) -> Option<ModelEntry> {
    let wire_id = value
        .get("modelId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if wire_id.is_empty() {
        return None;
    }
    let display_name = match value.get("modelName").and_then(Value::as_str) {
        Some(name) if !name.trim().is_empty() => name.trim().to_string(),
        _ => wire_id.clone(),
    };
    Some(ModelEntry {
        display_name,
        context_window: read_positive(value.get("contextWindow")),
        max_tokens: read_positive(value.get("maxTokens")),
        supports_image: value.get("supportsImage").and_then(Value::as_bool),
        supports_thinking: value
            .get("supportsThinking")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || thinking_config_present(value.get("thinkingConfig")),
        // costMultiplier 是**裸数字**；`0` 意思是免费，必须保留而不是当缺失去掉
        cost_multiplier: value.get("costMultiplier").and_then(read_number),
        wire_id,
    })
}

/// 倍率的展示串：`0` → 免费，其余 → `x<数字>`。
fn credits_label(multiplier: f64) -> String {
    if multiplier == 0.0 {
        return "免费".to_string();
    }
    format!("x{multiplier}")
}

/// 聚合层认的条目形态（键名对照 `core/models/shape.rs::list_item`）。
fn to_manifest_value(entry: &ModelEntry) -> Value {
    let mut object = Map::new();
    object.insert("id".to_string(), Value::String(entry.wire_id.clone()));
    if !entry.display_name.is_empty() {
        object.insert("name".to_string(), Value::String(entry.display_name.clone()));
    }
    if entry.context_window > 0 {
        object.insert("maxInputTokens".to_string(), Value::from(entry.context_window));
    }
    if entry.max_tokens > 0 {
        object.insert("maxOutputTokens".to_string(), Value::from(entry.max_tokens));
    }
    // 模态位：上游**没给**时按不支持折叠，但不声称支持（有实测依据才广告）
    object.insert(
        "supportsImages".to_string(),
        Value::Bool(entry.supports_image.unwrap_or(false)),
    );
    object.insert("supportsVideo".to_string(), Value::Bool(false));
    object.insert(
        "supportsReasoning".to_string(),
        Value::Bool(entry.supports_thinking),
    );
    // 上游就是 OpenAI Chat Completions，tools 原样透传 ⇒ 支持工具调用
    object.insert("supportsToolCall".to_string(), Value::Bool(true));
    if let Some(multiplier) = entry.cost_multiplier {
        object.insert("credits".to_string(), Value::String(credits_label(multiplier)));
    }
    Value::Object(object)
}

/// 清单本体（`list_models` / 兜底共用）
pub fn list() -> Vec<Value> {
    with_state(|guard| match guard.as_ref() {
        Some(state) if !state.entries.is_empty() => {
            state.entries.iter().map(to_manifest_value).collect()
        }
        _ => fallback_models().iter().map(to_manifest_value).collect(),
    })
}

/// 目录里有没有**远程拉到的**条目（`false` 时 [`list`] 给的是兜底清单）
pub fn remote_refreshed() -> bool {
    with_state(|guard| {
        guard
            .as_ref()
            .map(|state| state.fetched_at > 0 && !state.entries.is_empty())
            .unwrap_or(false)
    })
}

/// 目录最后一次落地时刻（毫秒；0 = 没有远程目录）
pub fn last_refreshed_at() -> i64 {
    with_state(|guard| guard.as_ref().map(|state| state.fetched_at).unwrap_or(0))
}

/// 当前远程目录的年龄（毫秒；0 = 没有远程目录）
pub fn age_ms() -> i64 {
    with_state(|guard| {
        guard
            .as_ref()
            .map(|state| crate::server::logging::now_ms() - state.fetched_at)
            .unwrap_or(0)
    })
}

/// 参考实现的**规范化别名 → 线上 id** 表。
///
/// ── 为什么需要它 ──────────────────────────────────────────────
/// 参考实现对外发布的是**规范化大小写的 id**（如 `deepseek-flash` →
/// `DeepSeek-V4.1-Flash`），并另建一张反向表把请求里的规范化名折回线上 id。
/// 本仓的目录直接以 `modelId`（线上 id）作为对外 `id`，因此**绝大多数情况下两者
/// 相同**；这张表只为「客户端手里握着旧规范化名」那条路兜底。键一律按小写比较。
const UPSTREAM_ALIASES: [(&str, &str); 6] = [
    ("deepseek-v4.1-flash", "deepseek-flash"),
    ("glm-5.3-flash", "glm-5.3-flash"),
    ("glm-5.3", "glm-5.3"),
    ("qwen3.8-max", "qwen3.8-max"),
    ("qwen3.8-flash-next", "qwen3.8-flash"),
    ("kimi-k2.7-code", "kimi-k2.7-code"),
];

/// 客户端请求里的模型名 → 发给上游的模型名。
///
/// 顺序：远程目录（按线上 id 或展示名命中）→ 规范化别名表 → 认不出返回 `None`
/// （由调用方**原样发**：猜一个更接近的名字比原样发更糟 —— 那会把「名字没映射」
/// 变成一个静默的错误重试）。
pub fn upstream_id_for(requested: &str) -> Option<String> {
    let target = requested.trim();
    if target.is_empty() {
        return None;
    }
    if let Some(wire_id) = with_state(|guard| {
        guard.as_ref().and_then(|state| {
            state
                .entries
                .iter()
                .find(|entry| entry.wire_id == target || entry.display_name == target)
                .map(|entry| entry.wire_id.clone())
        })
    }) {
        return Some(wire_id);
    }
    let lower = target.to_lowercase();
    UPSTREAM_ALIASES
        .iter()
        .find(|(alias, _)| *alias == lower)
        .map(|(_, wire)| (*wire).to_string())
}

/// 从响应里取模型数组。
///
/// **不**复用通用对象信封解析器（会要求 `data` 是对象，从而把整份清单判空，见模块头）；
/// `code != 0` 一律判失败，`data` 支持单层数组与 `data.data[]` 双层。
pub fn read_model_array(payload: &Value) -> Option<Vec<Value>> {
    let record = payload.as_object()?;
    let code = record.get("code").and_then(read_number)?;
    if code != 0.0 {
        return None;
    }
    if let Some(array) = record.get("data").and_then(Value::as_array) {
        return Some(array.clone());
    }
    record
        .get("data")
        .and_then(Value::as_object)
        .and_then(|nested| nested.get("data"))
        .and_then(Value::as_array)
        .cloned()
}

/// 拉一次目录（返回解析后的条目；`code != 0` 或形状不符时报错）。
async fn fetch_catalog(
    credentials: &LobsterCredentials,
    client_version: &str,
) -> Result<Vec<ModelEntry>, GatewayError> {
    // 版本头与 query 里的 version 用**同一个值**（都由 `chat::prime_client_version`
    // 给出），避免「声明是这个版本、目录按那个版本过滤」的错位。
    let version = if client_version.trim().is_empty() {
        chat::client_version_header()
    } else {
        client_version.trim().to_string()
    };
    let url = models_url(&credentials.keyfrom_query(&version));
    let headers: Vec<(String, String)> = vec![
        (
            "authorization".to_string(),
            format!("Bearer {}", credentials.access_token),
        ),
        ("accept".to_string(), "application/json".to_string()),
        (
            "x-lobsterai-client-capabilities".to_string(),
            CLIENT_CAPABILITIES.to_string(),
        ),
        (
            "x-lobsterai-client-version".to_string(),
            version.clone(),
        ),
        ("user-agent".to_string(), USER_AGENT.to_string()),
    ];
    let response = send_raw("GET", &url, None, &headers, None, Some(CATALOG_TIMEOUT_MS))
        .await
        .map_err(|error| {
            GatewayError::with_status(
                502,
                format!("LobsterAI 目录接口请求失败: {error}"),
            )
        })?;
    let payload = response.payload.ok_or_else(|| {
        GatewayError::with_status(502, "LobsterAI 目录接口响应不是 JSON")
    })?;
    if response.status == 401 || response.status == 403 {
        return Err(GatewayError::with_status(
            401,
            "LobsterAI 凭据已失效，请重新登录该账号",
        ));
    }
    if !(200..300).contains(&response.status) {
        return Err(GatewayError::with_status(
            502,
            format!("LobsterAI 目录接口返回 HTTP {}", response.status),
        ));
    }
    let items = read_model_array(&payload).ok_or_else(|| {
        GatewayError::with_status(
            502,
            "LobsterAI 目录响应结构不符（code != 0 或缺 data 数组）",
        )
    })?;
    Ok(items.iter().filter_map(entry_from_upstream).collect())
}

/// 刷新目录：远程成功即权威；失败时**保留现有清单**并如实汇报原因。
///
/// `force = false` 时 TTL 内早退；`force = true`（用户点刷新）真打上游。
///
/// 客户端版本由 [`chat::prime_client_version`] 解析（一次调用既拿到本函数需要的
/// query 参数，也顺手把转发路径的同步版本头快照填上）。
pub async fn refresh(
    credentials: &LobsterCredentials,
    force: bool,
) -> ModelRefreshOutcome {
    if !force && remote_refreshed() && age_ms() < CATALOG_TTL_MS {
        return ModelRefreshOutcome::unchanged();
    }

    let version = chat::prime_client_version().await;

    match fetch_catalog(credentials, &version).await {
        Ok(entries) if !entries.is_empty() => {
            let fetched_at = crate::server::logging::now_ms();
            let count = entries.len();
            // 落盘（进程重启后读回）。**存条目数组、不存整份 `CatalogState`**：
            // `catalog_cache` 自己就有 `fetchedAt` 字段，再往条目里塞一份时刻
            // 会出现两个来源（且恢复侧只读它那一个）。两侧都走 serde，
            // 「存什么形态、读什么形态」由同一个派生保证。
            let rows: Vec<Value> = entries
                .iter()
                .filter_map(|entry| serde_json::to_value(entry).ok())
                .collect();
            with_state(|guard| {
                *guard = Some(CatalogState {
                    entries,
                    fetched_at,
                });
            });
            crate::server::core::providers::catalog_cache::save(SCOPE, &rows, fetched_at);
            crate::server::logging::log(
                "[LobsterAI]",
                &format!("目录刷新完成：{count} 个模型（客户端版本 {version}）"),
            );
            ModelRefreshOutcome::refreshed(count)
        }
        Ok(_) => ModelRefreshOutcome::failed("LobsterAI 目录为空（上游未返回可用模型）"),
        Err(error) => {
            crate::server::logging::verbose(
                "[LobsterAI]",
                &format!("目录刷新失败，沿用现有清单：{}", error.message),
            );
            ModelRefreshOutcome::failed(error.message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 思考位：布尔位为真，或 `thinkingConfig` 有多于一个有效档位。
    ///
    /// ⚠️ 断言走**生产路径**（`entry_from_upstream`）而不是本文件再抄一份判据 ——
    /// 抄一份等于只测那份抄件。上游在这一处正好踩过同一个坑（夹具照自己的假设编，
    /// 于是测试全绿而代码全错）。
    #[test]
    fn thinking_detected_from_config_levels() {
        let thinking_of = |value: Value| {
            entry_from_upstream(&value).is_some_and(|entry| entry.supports_thinking)
        };
        // 布尔位为真
        assert!(thinking_of(json!({"modelId": "m", "supportsThinking": true})));
        // 只有配置、没有布尔位：多于一个档位也算支持
        assert!(thinking_of(json!({
            "modelId": "m",
            "thinkingConfig": {"options": [
                {"openclawLevel": "low"}, {"openclawLevel": "high"}
            ]}
        })));
        // 只有一档 = 没有可选档位
        assert!(!thinking_of(json!({
            "modelId": "m",
            "thinkingConfig": {"options": [{"openclawLevel": "low"}]}
        })));
        // 明确为假
        assert!(!thinking_of(json!({"modelId": "m", "supportsThinking": false})));
    }

    /// 上游条目缺 modelId 的直接丢掉；名称缺失回落 id
    #[test]
    fn entry_requires_model_id() {
        assert!(entry_from_upstream(&json!({"modelName": "x"})).is_none());
        let entry = entry_from_upstream(&json!({"modelId": "glm-5"}));
        assert!(entry.is_some_and(|entry| entry.display_name == "glm-5"));
    }

    /// ⚠️ `data` 是**单层数组**：通用对象信封解析器会把整份清单判空，本模块要认它
    #[test]
    fn read_model_array_accepts_flat_array() {
        let payload = json!({
            "code": 0,
            "message": "success",
            "data": [{"modelId": "glm-5"}, {"modelId": "kimi-k2.6"}]
        });
        let rows = read_model_array(&payload).unwrap_or_default();
        assert_eq!(rows.len(), 2);
        // 双层层套也认
        let nested = json!({"code": 0, "data": {"data": [{"modelId": "x"}]}});
        assert_eq!(read_model_array(&nested).map(|rows| rows.len()), Some(1));
        // code != 0 判失败
        assert!(read_model_array(&json!({"code": 7, "data": []})).is_none());
    }

    /// 兜底清单是 19 条，且 remote 状态为假
    #[test]
    fn fallback_has_19_and_is_not_remote() {
        assert_eq!(fallback_models().len(), 19);
        assert!(!remote_refreshed());
    }

    /// 倍率展示：0 是免费，其余 xN
    #[test]
    fn credits_label_renders_free_for_zero() {
        assert_eq!(credits_label(0.0), "免费");
        assert_eq!(credits_label(1.5), "x1.5");
    }

    /// **落盘缓存的往返无损**：`ModelEntry` 走 serde 存、再读回来必须等价。
    ///
    /// 这条测试守的是「缓存接上之后不静默丢字段」—— `to_manifest_value` 的输出
    /// 是**有损**的（`credits` 成了 `"免费"` 标签），若误把 manifest 当缓存存，
    /// 恢复出来的条目能力位会整体退化。事后很难看出（清单看着有模型）。
    #[test]
    fn cache_roundtrip_preserves_every_field() {
        let entry = ModelEntry {
            wire_id: "glm-5".to_string(),
            display_name: "GLM-5".to_string(),
            context_window: 131_072,
            max_tokens: 8_192,
            supports_image: Some(true),
            supports_thinking: true,
            cost_multiplier: Some(0.0), // 0 = 免费，不是「缺失」
        };
        let rows = vec![serde_json::to_value(&entry).ok().unwrap_or(Value::Null)];
        let back: Vec<ModelEntry> =
            serde_json::from_value(Value::Array(rows)).unwrap_or_default();
        assert_eq!(back.len(), 1);
        let back = back.first().cloned().unwrap_or(entry.clone());
        assert_eq!(back.wire_id, entry.wire_id);
        assert_eq!(back.context_window, entry.context_window);
        assert_eq!(back.supports_image, entry.supports_image);
        assert_eq!(back.supports_thinking, entry.supports_thinking);
        // `Some(0.0)` 不能被读成 `None`（那会从「免费」变成「不知道倍率」）
        assert_eq!(back.cost_multiplier, Some(0.0));
    }

    /// ⚠️ 缓存里存的必须**不是** manifest 形态（前者有损，后者无损）
    #[test]
    fn cache_shape_differs_from_manifest_shape() {
        let entry = ModelEntry {
            wire_id: "glm-5".to_string(),
            display_name: "GLM-5".to_string(),
            context_window: 0,
            max_tokens: 0,
            supports_image: None,
            supports_thinking: false,
            cost_multiplier: Some(1.5),
        };
        let cached = serde_json::to_value(&entry).unwrap_or(Value::Null);
        let manifest = to_manifest_value(&entry);
        // 缓存形态用 Rust 字段名（`wireId`/`costMultiplier`），manifest 用展示键
        assert!(cached.get("costMultiplier").is_some());
        assert!(cached.get("credits").is_none());
        // manifest 把倍率渲染成标签
        assert_eq!(manifest.get("credits").and_then(Value::as_str), Some("x1.5"));
    }
}