//! Tabbit 模型目录（远程优先 + 一条兜底）。
//!
//! 上游 `GET /proxy/v1/model_config/models?a=0&scene=chat` 返回
//! `{"models":[{display_name, model_access_type, supports_images, support_thinking,
//! display_multiplier, params…}]}`（部分站点多包一层 `data`）。
//!
//! ── 两条必须遵守的口径 ──────────────────────────────────────
//! 1. 上游 `selected_model` 只认 **display_name**：发我们对外用的 slug 会被上游
//!    **静默换成别的模型**（不报错，答非所问）。所以对外 id 是 slug、发上游时
//!    一律换回 `upstreamModel`（见 [`select`]）。
//! 2. 本适配器只做纯文本：`supportsImages` 与 `supportsToolCall` 恒 false。
//!    网页 completion 没有原生工具调用契约，按 Web 包口径明确拒绝
//!    （见 `prompt.rs`），不用提示词模拟。
//!
//! ── 为什么两个站点各有一份缓存 ──────────────────────────────
//! 两站目录不同（模型池、权益档都不同），缓存键与落盘 scope 都按站点分开
//! （`SCOPE_TABBIT` / `SCOPE_TABBIT_INTL`）—— 混用会让一张卡片显示另一张的模型。
//!
//! ── 兜底为什么只有一条 ──────────────────────────────────────
//! 目录拉不到时不编模型名：只给一条「让上游自己选默认模型」的入口
//! （`upstreamModel = "default"`）。编出来的名字发上去会被静默换成别的模型，
//! 那比「暂时没有清单」更糟。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic；持锁不跨 await。

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::core::providers::adapter::ModelRefreshOutcome;
use crate::server::core::providers::catalog_cache;
use crate::server::errors::GatewayError;

use super::credentials::Credentials;
use super::http;
use super::{endpoints, Realm, CATALOG_TTL_MS, DEFAULT_MODEL};

/// 一份远程目录
struct RemoteCatalog {
    models: Vec<Value>,
    fetched_at_ms: i64,
}

fn static_cell() -> &'static Mutex<BTreeMap<String, RemoteCatalog>> {
    static CELL: std::sync::OnceLock<Mutex<BTreeMap<String, RemoteCatalog>>> =
        std::sync::OnceLock::new();
    CELL.get_or_init(Mutex::default)
}

/// 落盘 scope（重启后读回上次拉到的清单，而不是退回兜底）
pub fn scope(realm: Realm) -> &'static str {
    match realm {
        Realm::Cn => catalog_cache::SCOPE_TABBIT,
        Realm::Intl => catalog_cache::SCOPE_TABBIT_INTL,
    }
}

/// 首次访问某站点时从落盘缓存还原（每个站点只查一次库）
fn ensure_restored(realm: Realm) {
    static DONE: Mutex<BTreeMap<String, bool>> = Mutex::new(BTreeMap::new());
    {
        let mut done = match DONE.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if done.contains_key(realm.id()) {
            return;
        }
        done.insert(realm.id().to_string(), true);
    }
    let Some(cached) = catalog_cache::load(scope(realm)) else {
        return;
    };
    if cached.models.is_empty() {
        return;
    }
    let mut guard = match static_cell().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.entry(realm.id().to_string()).or_insert(RemoteCatalog {
        models: cached.models,
        fetched_at_ms: cached.fetched_at,
    });
}

/// 兜底清单：一条「让上游选默认模型」的入口（不编模型名，见模块头）
pub fn fallback_for(realm: Realm) -> Vec<Value> {
    vec![json!({
        "id": format!("{}-default", realm.id()),
        "name": format!("{} 默认模型", realm.label()),
        "upstreamModel": DEFAULT_MODEL,
        "isDefault": true,
        "supportsImages": false,
        "supportsToolCall": false,
        "supportsReasoning": true,
        "inputModalities": ["text"],
        "synthetic": true,
    })]
}

/// 模型清单（远程优先，没拉到时给兜底）
pub fn list(realm: Realm) -> Vec<Value> {
    ensure_restored(realm);
    let guard = match static_cell().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    match guard.get(realm.id()) {
        Some(catalog) if !catalog.models.is_empty() => catalog.models.clone(),
        _ => fallback_for(realm),
    }
}

/// 远程目录是否拉到过（`catalog::refresh_meta` 的「来源」列判据）
pub fn remote_refreshed(realm: Realm) -> bool {
    ensure_restored(realm);
    let guard = match static_cell().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .get(realm.id())
        .map(|catalog| !catalog.models.is_empty())
        .unwrap_or(false)
}

/// 最后一次远程刷新的时刻（毫秒；没刷过给 0）
pub fn last_refreshed_at(realm: Realm) -> i64 {
    ensure_restored(realm);
    let guard = match static_cell().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .get(realm.id())
        .map(|catalog| catalog.fetched_at_ms)
        .unwrap_or(0)
}

// ─── 解析 ───────────────────────────────────────────────────

/// 取第一个非空字符串
fn text_of(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|text| !text.is_empty())
        .unwrap_or("")
        .to_string()
}

/// 任一布尔为真
fn flag(value: &Value, keys: &[&str]) -> bool {
    keys.iter()
        .any(|key| value.get(*key).and_then(Value::as_bool).unwrap_or(false))
}

/// 取第一个正整数（上游把上下文/输出上限放在顶层或 `params` 里两处都有）
fn pick_number(value: &Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        for container in [value, value.get("params").unwrap_or(&Value::Null)] {
            if let Some(number) = http::as_i64(container.get(*key).unwrap_or(&Value::Null)) {
                if number > 0 {
                    return Some(number);
                }
            }
        }
    }
    None
}

/// 展示名 → 对外 id：`GPT-5.5` → `gpt-5-5`，`Claude-Opus-4.7` → `claude-opus-4-7`
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if matches!(ch, ' ' | '.' | '_' | '-' | '/') && !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        DEFAULT_MODEL.to_string()
    } else {
        trimmed
    }
}

/// 名字归一（比较用：忽略大小写与分隔符）
fn normalize_key(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .filter(|ch| !matches!(ch, ' ' | '_' | '-' | '.'))
        .collect()
}

fn trim_number(value: f64) -> String {
    let text = format!("{value:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.').to_string();
    if text.is_empty() {
        "0".to_string()
    } else {
        text
    }
}

/// 解析一次目录响应（`models` 或 `data.models` 两种包络都认）
pub fn parse(payload: &Value) -> Result<Vec<Value>, GatewayError> {
    let mut raw = payload
        .get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if raw.is_empty() {
        raw = payload
            .get("data")
            .and_then(|data| data.get("models"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
    }
    let mut seen = BTreeMap::new();
    let mut models = Vec::new();
    for entry in &raw {
        let name = text_of(entry, &["display_name", "displayName", "model_name", "name"]);
        if name.is_empty() {
            continue;
        }
        let id = slug(&name);
        if seen.insert(id.clone(), true).is_some() {
            continue;
        }
        let access = text_of(entry, &["model_access_type", "access_type"]);
        let multiplier = entry
            .get("display_multiplier")
            .and_then(http::as_f64)
            .filter(|value| *value > 0.0)
            .map(|value| format!("x{}", trim_number(value)));
        let mut record = json!({
            "id": id,
            "name": name,
            "isDefault": false,
            "supportsImages": false,
            "supportsToolCall": false,
            "supportsReasoning": flag(entry, &["support_thinking", "supports_thinking", "support_reasoning"]),
            "inputModalities": ["text"],
            // 上游只认 display_name：发请求时用它（见 `select`）
            "upstreamModel": name,
            "synthetic": false,
        });
        if !access.is_empty() {
            record["accessType"] = Value::String(access);
        }
        if let Some(context) = pick_number(entry, &["context_window", "contextWindow", "max_context_tokens"]) {
            record["contextWindow"] = Value::from(context);
        }
        if let Some(max_output) = pick_number(entry, &["max_tokens", "maxTokens", "max_output_tokens"]) {
            record["maxOutput"] = Value::from(max_output);
        }
        if let Some(multiplier) = multiplier {
            record["creditMultiplier"] = Value::String(multiplier);
        }
        models.push(record);
    }
    if models.is_empty() {
        return Err(GatewayError::with_status(502, "Tabbit 模型目录为空"));
    }
    // 默认模型：第一个免费档；没有免费档就第一个
    let default_index = models
        .iter()
        .position(|model| {
            model
                .get("accessType")
                .and_then(Value::as_str)
                .is_some_and(|access| access.eq_ignore_ascii_case("free_unlimited"))
        })
        .unwrap_or(0);
    if let Some(default) = models.get_mut(default_index) {
        default["isDefault"] = Value::Bool(true);
    }
    Ok(models)
}

// ─── 客户端模型名 → 上游 display_name ──────────────────────

/// 在清单里按 id / name / upstreamModel 找（三者都归一后比较）
fn find<'a>(models: &'a [Value], requested: &str) -> Option<&'a Value> {
    let want = normalize_key(requested);
    if want.is_empty() {
        return None;
    }
    models.iter().find(|model| {
        ["id", "name", "upstreamModel"].iter().any(|key| {
            model
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(|value| normalize_key(value) == want)
        })
    })
}

/// 默认那条（`isDefault`）
fn default_entry(models: &[Value]) -> Option<&Value> {
    models
        .iter()
        .find(|model| model.get("isDefault").and_then(Value::as_bool) == Some(true))
}

/// 客户端请求的模型名 → 上游 `selected_model`。
///
/// 空串 / `default` / `*-default` 一律走默认那条；认不出来的名字**明确报错**
/// 而不是回退 —— 回退会让用户以为自己在用 A 模型、实际是 B（上游对未知
/// display_name 的处理正是静默换一个）。
pub fn select(models: &[Value], requested: &str) -> Result<String, GatewayError> {
    let requested = requested.trim();
    if requested.is_empty()
        || requested.eq_ignore_ascii_case(DEFAULT_MODEL)
        || requested.ends_with("-default")
    {
        if let Some(upstream) = default_entry(models)
            .and_then(|model| model.get("upstreamModel"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            return Ok(upstream.to_string());
        }
        return Ok(DEFAULT_MODEL.to_string());
    }
    match find(models, requested) {
        Some(model) => {
            let upstream = model.get("upstreamModel").and_then(Value::as_str).unwrap_or("");
            let name = model.get("name").and_then(Value::as_str).unwrap_or("");
            let chosen = if upstream.is_empty() { name } else { upstream };
            if chosen.is_empty() {
                Err(GatewayError::with_status(
                    400,
                    format!("Tabbit 模型「{requested}」缺少上游名称，请刷新模型目录"),
                ))
            } else {
                Ok(chosen.to_string())
            }
        }
        None => Err(GatewayError::with_status(
            400,
            format!("未知的 Tabbit 模型「{requested}」，请先刷新模型目录"),
        )),
    }
}

// ─── 刷新 ───────────────────────────────────────────────────

/// 刷新远程目录（`force = true` 跳过 TTL；失败不返回错误，保留现有清单）
pub async fn refresh(
    store: &AccountStore,
    realm: Realm,
    account_id: &str,
    force: bool,
) -> ModelRefreshOutcome {
    let Some(record) = store.tabbit_account_record(realm, account_id) else {
        crate::server::logging::verbose(
            "[Models]",
            &format!("{} 模型目录刷新跳过：还没有账号", realm.label()),
        );
        return ModelRefreshOutcome::unchanged();
    };
    let credentials = match Credentials::from_record(Some(&record)) {
        Ok(credentials) => credentials,
        Err(error) => {
            crate::server::logging::verbose(
                "[Models]",
                &format!("{} 模型目录刷新跳过：{}", realm.label(), error.message),
            );
            return ModelRefreshOutcome::unchanged();
        }
    };
    let proxy = match crate::server::core::proxies::resolve_account_proxy(record.get("proxy")) {
        Some(crate::server::core::proxies::ProxyResolution::Resolved(proxy)) => Some(proxy),
        Some(crate::server::core::proxies::ProxyResolution::Failed(reason)
            | crate::server::core::proxies::ProxyResolution::Broken(reason)) => {
            return ModelRefreshOutcome::failed(reason);
        }
        None => None,
    };
    if !force {
        let now = crate::server::logging::now_ms();
        let fresh = {
            let guard = match static_cell().lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard
                .get(realm.id())
                .map(|catalog| now - catalog.fetched_at_ms < CATALOG_TTL_MS)
                .unwrap_or(false)
        };
        if fresh {
            return ModelRefreshOutcome::unchanged();
        }
    }
    let client = http::client(proxy.as_ref());
    match http::signed_json(
        &credentials,
        realm,
        &client,
        "GET",
        endpoints::MODELS_PATH,
        None,
        "/newtab",
    )
    .await
    .map(|payload| parse(&payload))
    {
        Ok(Ok(models)) => {
            let now = crate::server::logging::now_ms();
            {
                let mut guard = match static_cell().lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                guard.insert(
                    realm.id().to_string(),
                    RemoteCatalog {
                        models: models.clone(),
                        fetched_at_ms: now,
                    },
                );
            }
            catalog_cache::save(scope(realm), &models, now);
            crate::server::logging::log(
                "[Models]",
                &format!("✅ {} 模型目录已更新（{} 个）", realm.label(), models.len()),
            );
            ModelRefreshOutcome::refreshed(models.len())
        }
        Ok(Err(error)) => {
            crate::server::logging::verbose(
                "[Models]",
                &format!("{} 模型目录为空：{}", realm.label(), error.message),
            );
            ModelRefreshOutcome::failed(error.message)
        }
        Err(error) => {
            crate::server::logging::verbose(
                "[Models]",
                &format!("{} 模型目录刷新失败：{}", realm.label(), error.message),
            );
            ModelRefreshOutcome::failed(error.message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Vec<Value> {
        parse(&json!({
            "models": [
                {
                    "display_name": "GPT-5.5",
                    "model_access_type": "premium",
                    "support_thinking": true,
                    "display_multiplier": 2.5,
                    "context_window": 200000
                },
                {
                    "display_name": "Tabbit-Lite",
                    "model_access_type": "free_unlimited"
                }
            ]
        }))
        .expect("目录应能解析")
    }

    /// slug：大小写与点/斜杠归一到横线
    #[test]
    fn slug_normalises_separators() {
        assert_eq!(slug("GPT-5.5"), "gpt-5-5");
        assert_eq!(slug("Claude-Opus-4.7"), "claude-opus-4-7");
        assert_eq!(slug("  Qwen / Max  "), "qwen-max");
        // 全是分隔符 → 回落到默认名（不能产出空 id）
        assert_eq!(slug("---"), DEFAULT_MODEL);
    }

    /// 解析后：id 是 slug、upstreamModel 是 display_name（发上游只认它）
    #[test]
    fn parse_keeps_display_name_for_upstream() {
        let models = catalog();
        let first = &models[0];
        assert_eq!(first["id"], "gpt-5-5");
        assert_eq!(first["name"], "GPT-5.5");
        assert_eq!(first["upstreamModel"], "GPT-5.5");
        assert_eq!(first["supportsReasoning"], true);
        assert_eq!(first["creditMultiplier"], "x2.5");
        assert_eq!(first["contextWindow"], 200000);
        // 本家只做纯文本
        assert_eq!(first["supportsImages"], false);
        assert_eq!(first["supportsToolCall"], false);
    }

    /// 默认档优先选免费档（`free_unlimited`）
    #[test]
    fn default_prefers_the_free_tier() {
        let models = catalog();
        let default = default_entry(&models).expect("应有一条默认");
        assert_eq!(default["name"], "Tabbit-Lite");
    }

    /// 客户端给 slug / 展示名 / 上游名，都要能反查到上游 display_name
    #[test]
    fn select_resolves_all_three_spellings() {
        let models = catalog();
        for asked in ["gpt-5-5", "GPT-5.5", "gpt 5 5"] {
            assert_eq!(
                select(&models, asked).expect("应能解析"),
                "GPT-5.5",
                "asked={asked}"
            );
        }
    }

    /// 空 / default / *-default → 默认那条的上游名
    #[test]
    fn select_falls_back_to_the_default_entry() {
        let models = catalog();
        for asked in ["", "default", "tabbit-default"] {
            assert_eq!(select(&models, asked).expect("应能解析"), "Tabbit-Lite");
        }
    }

    /// 未知模型**必须报错**（回退会让用户以为在用 A、实际是 B）
    #[test]
    fn unknown_model_is_a_readable_400() {
        let error = select(&catalog(), "no-such-model").expect_err("未知模型必须报错");
        assert_eq!(error.status_code, 400);
        assert!(error.message.contains("no-such-model"));
    }

    /// 两种包络（顶层 `models` / `data.models`）都要能解析
    #[test]
    fn both_envelopes_are_accepted() {
        let top = parse(&json!({ "models": [{ "display_name": "A" }] })).expect("顶层包络应能解析");
        let nested = parse(&json!({ "data": { "models": [{ "display_name": "A" }] } }))
            .expect("嵌套包络应能解析");
        assert_eq!(top.len(), 1);
        assert_eq!(nested.len(), 1);
        assert_eq!(top[0]["id"], nested[0]["id"]);
    }

    /// 空目录是错误（不是「零个模型」—— 那会让界面显示一片空白）
    #[test]
    fn empty_catalog_is_an_error() {
        assert!(parse(&json!({ "models": [] })).is_err());
        assert!(parse(&Value::Null).is_err());
    }

    /// 兜底只有一条、且标了 synthetic（不冒充真实模型）
    #[test]
    fn fallback_has_exactly_one_synthetic_entry() {
        for realm in Realm::ALL {
            let models = fallback_for(realm);
            assert_eq!(models.len(), 1);
            assert_eq!(models[0]["synthetic"], true);
            assert_eq!(models[0]["upstreamModel"], DEFAULT_MODEL);
            assert_eq!(models[0]["isDefault"], true);
            // id 按站点分（两家的兜底不能撞 id）
            assert_eq!(models[0]["id"], format!("{}-default", realm.id()));
        }
        assert_ne!(fallback_for(Realm::Cn)[0]["id"], fallback_for(Realm::Intl)[0]["id"]);
    }

    /// 两个站点的落盘 scope 必须分开
    #[test]
    fn scopes_are_per_site() {
        assert_ne!(scope(Realm::Cn), scope(Realm::Intl));
    }
}
