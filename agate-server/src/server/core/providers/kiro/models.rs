//! Kiro 模型目录：`List-Available-Models`（两种凭证两条路）+ 上下文窗口表。
//!
//! ── 上游形状 ────────────────────────────────────────────────
//! ```json
//! {"models": [{"modelId":"claude-sonnet-4.5","modelName":"Claude Sonnet 4.5",
//!   "tokenLimits":{"maxInputTokens":200000,"maxOutputTokens":64000},
//!   "supportedInputTypes":["TEXT","IMAGE"],
//!   "additionalModelRequestFieldsSchema":{…}}],
//!  "defaultModel":{"modelId":"auto"}}
//! ```
//! API key 路线按 `nextToken` 分页（见 [`fetch`]），OAuth 路线一次给全。
//!
//! ── 为什么**不落盘**（与其它家不同）──────────────────────────
//! 本家的清单**随订阅等级变**：Free 只看得到两三个，Pro+ 能看到全部。落盘
//! 一份的话，换一个低等级账号登录会看到一份它无权使用的清单 —— 选了就报错，
//! 而且报错指向「模型不可用」，看不出真正原因。清单「少了」是可恢复的
//! （刷新一次就回来），「多了」不是（用户会反复撞同一面墙）。
//!
//! 代价只是冷启动时列表里只有 `auto`（Kiro 自己的路由模型，任何订阅都能用），
//! 点一次「拉取模型」就补齐。上游插件同样只做进程内缓存。
//!
//! ── 缓存按账号 ──────────────────────────────────────────────
//! 键是**账号 id**而不是 provider：同一台网关挂两个不同订阅的 Kiro 账号时，
//! 清单必须各算各的（这正是上面那条「不落盘」的同一个理由）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic；持锁不跨 await。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::server::core::proxies::ResolvedProxy;
use crate::server::core::providers::adapter::ModelRefreshOutcome;
use crate::server::errors::GatewayError;

use super::credentials::KiroCredentials;
use super::http;

/// 自动路径的缓存 TTL（手动刷新绕过）
pub const TTL_MS: i64 = 10 * 60 * 1000;

/// 分页上限：上游一页 50 个、最多两页，20 是「再多也肯定是上游出问题」的界
const MAX_PAGES: usize = 20;

/// 自带思考档位的模型（与 kiro-cli 同口径：Claude 与 Auto 路由）。
///
/// 其它模型（如 Nova）收 `<thinking_mode>` 前缀只会得到一段废话，所以
/// 只对这两类开。
pub fn thinks(model_id: &str) -> bool {
    let lower = model_id.to_ascii_lowercase();
    lower.contains("claude") || lower == "auto"
}

/// Kiro 侧把思考档位当**字段**收（自适应思考）的模型信息。
///
/// 目录里的 `additionalModelRequestFieldsSchema` 声明了这种模型：它的档位走
/// `output_config.effort` / `reasoning.effort`，而不是正文前缀。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeEffort {
    /// 字段所在的顶层键（`output_config` / `reasoning`）
    pub path: String,
    /// 可选档位（`low` / `medium` / `high` …，由上游给全）
    pub levels: Vec<String>,
}

/// 一个账号的目录快照
struct Catalog {
    models: Vec<Value>,
    fetched_at: i64,
    natives: HashMap<String, NativeEffort>,
    windows: HashMap<String, i64>,
}

fn cache() -> &'static Mutex<HashMap<String, Catalog>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Catalog>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 持锁执行（毒锁按内部值继续：一次 panic 不该让目录永久失效）
fn with_cache<T>(action: impl FnOnce(&mut HashMap<String, Catalog>) -> T) -> T {
    match cache().lock() {
        Ok(mut table) => action(&mut table),
        Err(poisoned) => action(&mut poisoned.into_inner()),
    }
}

fn now_ms() -> i64 {
    super::credentials::now_ms()
}

/// 内置兜底：`auto` 是 Kiro 自己的路由模型，任何订阅都能用。
///
/// 只有一条**是有意的**：编不出「这个账号还能用什么」，编出来的名字发上去会被
/// 上游拒掉（比清单短更难排查）。
pub fn fallback() -> Vec<Value> {
    vec![json!({
        "id": "auto",
        "name": "Auto",
        "supportsToolCall": true,
        "supportsReasoning": true,
        "isDefault": true,
    })]
}

/// 读一份未过期的缓存
fn cached(account: &str) -> Option<Catalog> {
    with_cache(|table| {
        let entry = table.get(account)?;
        (now_ms() - entry.fetched_at < TTL_MS).then(|| Catalog {
            models: entry.models.clone(),
            fetched_at: entry.fetched_at,
            natives: entry.natives.clone(),
            windows: entry.windows.clone(),
        })
    })
}

/// 当前清单（缓存优先；没有缓存时用兜底）。
pub fn list(account: &str) -> Vec<Value> {
    match cached(account) {
        Some(catalog) => catalog.models,
        None => fallback(),
    }
}

/// 上下文窗口（估算用量要用；0 = 不知道）
pub fn window_of(account: &str, model: &str) -> i64 {
    with_cache(|table| {
        table
            .get(account)
            .and_then(|catalog| catalog.windows.get(model).copied())
            .unwrap_or(0)
    })
}

/// 某个模型的原生思考档位（`None` = 走正文前缀那条路）
pub fn native_of(account: &str, model: &str) -> Option<NativeEffort> {
    with_cache(|table| {
        table
            .get(account)
            .and_then(|catalog| catalog.natives.get(model).cloned())
    })
}

/// 有没有远程清单（任一账号刷过）
pub fn remote_refreshed() -> bool {
    with_cache(|table| table.values().any(|catalog| !catalog.models.is_empty()))
}

/// 最近一次成功刷新的时刻（0 = 从没刷过）
pub fn last_refreshed_at() -> i64 {
    with_cache(|table| {
        table
            .values()
            .map(|catalog| catalog.fetched_at)
            .max()
            .unwrap_or(0)
    })
}

/// 解析后的目录（模型记录 + 两张附表）
pub struct Parsed {
    pub models: Vec<Value>,
    pub natives: HashMap<String, NativeEffort>,
    pub windows: HashMap<String, i64>,
}

/// 上游 payload → 目录记录（**纯函数**，可离线测）
pub fn parse(payload: &Value) -> Result<Parsed, String> {
    let rows = payload
        .get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let default_id = payload
        .pointer("/defaultModel/modelId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut models = Vec::new();
    let mut natives = HashMap::new();
    let mut windows = HashMap::new();
    for row in &rows {
        let Some(model_id) = row.get("modelId").and_then(Value::as_str) else {
            continue;
        };
        if model_id.is_empty() {
            continue;
        }
        // 显示名与 id 相同时用 id（`modelName` 常常就是机器名，两栏一模一样
        // 反而不像人写的名字）；`auto` 单独给一个可读名
        let name = row
            .get("modelName")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && *value != model_id)
            .unwrap_or(if model_id == "auto" { "Auto" } else { model_id });
        let context = row
            .pointer("/tokenLimits/maxInputTokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let output = row
            .pointer("/tokenLimits/maxOutputTokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let images = row
            .get("supportedInputTypes")
            .and_then(Value::as_array)
            .is_some_and(|types| {
                types.iter().any(|value| {
                    value
                        .as_str()
                        .is_some_and(|text| text.eq_ignore_ascii_case("IMAGE"))
                })
            });
        // 默认那条：目录说了听目录的；没说时 `auto` 就是默认
        // （上游的双重赋值逻辑折成这一条判据，行为等价）
        let is_default = if default_id.is_empty() {
            model_id == "auto"
        } else {
            model_id == default_id
        };
        let mut record = json!({
            "id": model_id,
            "name": name,
            "maxInputTokens": context,
            "maxOutputTokens": output,
            "supportsToolCall": true,
            "supportsReasoning": thinks(model_id),
            "isDefault": is_default,
        });
        if images {
            record["supportsImages"] = json!(true);
        }
        if context > 0 {
            windows.insert(model_id.to_string(), context);
        }
        if let Some(native) = native_effort(row.get("additionalModelRequestFieldsSchema")) {
            natives.insert(model_id.to_string(), native);
        }
        if is_default {
            models.insert(0, record);
        } else {
            models.push(record);
        }
    }
    if models.is_empty() {
        return Err("Kiro 没有返回任何模型".to_string());
    }
    Ok(Parsed {
        models,
        natives,
        windows,
    })
}

/// 从目录的 schema 里读出自适应思考的档位。
///
/// 判据是 `thinking.type.enum` 里有没有 `adaptive`：有才是「按字段调档」，
/// 否则那个模型只认正文前缀。两条候选路径优先 `output_config`
/// （Anthropic 的新写法），退到 `reasoning`。
fn native_effort(schema: Option<&Value>) -> Option<NativeEffort> {
    let schema = schema.filter(|value| value.is_object())?;
    let types = schema
        .pointer("/properties/thinking/properties/type/enum")
        .and_then(Value::as_array)?;
    if !types.iter().any(|value| value.as_str() == Some("adaptive")) {
        return None;
    }
    for path in ["output_config", "reasoning"] {
        let levels = schema
            .pointer(&format!("/properties/{path}/properties/effort/enum"))
            .and_then(Value::as_array);
        let Some(levels) = levels else { continue };
        let levels: Vec<String> = levels
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        if !levels.is_empty() {
            return Some(NativeEffort {
                path: path.to_string(),
                levels,
            });
        }
    }
    None
}

// ─── 拉取 ───────────────────────────────────────────────────

/// 刷新目录（`force = true` 跳过 TTL；失败不抛错，保留现有清单）。
///
/// 凭证由调用方准备好（`refresh::ensure` 负责续期）—— 目录只是「用一份能用
/// 的凭证去问一句」，不承担换 token 的职责。
pub async fn refresh(
    credentials: &KiroCredentials,
    account_id: &str,
    proxy: Option<&ResolvedProxy>,
    force: bool,
) -> ModelRefreshOutcome {
    if !force && cached(account_id).is_some() {
        return ModelRefreshOutcome::unchanged();
    }
    let payload = match fetch(credentials, proxy).await {
        Ok(payload) => payload,
        Err(error) => {
            crate::server::logging::verbose(
                "[Models]",
                &format!("Kiro 模型目录刷新失败（{}）：{}", credentials.masked(), error.message),
            );
            return ModelRefreshOutcome::failed(error.message);
        }
    };
    let parsed = match parse(&payload) {
        Ok(parsed) => parsed,
        Err(message) => {
            crate::server::logging::verbose(
                "[Models]",
                &format!("Kiro 模型目录为空：{message}"),
            );
            return ModelRefreshOutcome::failed(message);
        }
    };
    let count = parsed.models.len();
    let fetched_at = now_ms();
    with_cache(|table| {
        table.insert(
            account_id.to_string(),
            Catalog {
                models: parsed.models,
                fetched_at,
                natives: parsed.natives,
                windows: parsed.windows,
            },
        );
    });
    crate::server::logging::log("[Models]", &format!("✅ Kiro 模型目录已更新（{count} 个）"));
    ModelRefreshOutcome::refreshed(count)
}

/// 两种凭证两条路：API key 要按 `nextToken` 翻页，OAuth 一次给全。
async fn fetch(
    credentials: &KiroCredentials,
    proxy: Option<&ResolvedProxy>,
) -> Result<Value, GatewayError> {
    if credentials.is_api_key() {
        key_pages(credentials, proxy).await
    } else {
        http::control(credentials, http::OP_MODELS, &Value::Null, proxy).await
    }
}

/// API key 路线的分页。
///
/// 把每页的 `models` 拼起来、`defaultModel` 取第一个非空 —— 上游只在第一页
/// 给默认模型，后续页即使给了也是同一个值。
async fn key_pages(
    credentials: &KiroCredentials,
    proxy: Option<&ResolvedProxy>,
) -> Result<Value, GatewayError> {
    let mut models: Vec<Value> = Vec::new();
    let mut default_model = Value::Null;
    let mut next: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut extra = json!({ "origin": "KIRO_CLI" });
        if let Some(token) = &next {
            extra["nextToken"] = json!(token);
        }
        let page = http::control(credentials, http::OP_MODELS, &extra, proxy).await?;
        if let Some(list) = page.get("models").and_then(Value::as_array) {
            models.extend(list.iter().cloned());
        }
        if default_model.is_null() {
            if let Some(value) = page.get("defaultModel") {
                if !value.is_null() {
                    default_model = value.clone();
                }
            }
        }
        next = page
            .get("nextToken")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_string);
        if next.is_none() {
            break;
        }
    }
    Ok(json!({ "models": models, "defaultModel": default_model }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> Value {
        json!({
            "models": [
                {
                    "modelId": "claude-sonnet-4.5",
                    "modelName": "Claude Sonnet 4.5",
                    "tokenLimits": { "maxInputTokens": 200000, "maxOutputTokens": 64000 },
                    "supportedInputTypes": ["TEXT", "IMAGE"],
                    "additionalModelRequestFieldsSchema": {
                        "type": "object",
                        "properties": {
                            "thinking": { "type": "object", "properties": {
                                "type": { "enum": ["enabled", "adaptive"] }
                            }},
                            "output_config": { "type": "object", "properties": {
                                "effort": { "enum": ["low", "medium", "high"] }
                            }}
                        }
                    }
                },
                {
                    "modelId": "auto",
                    "tokenLimits": { "maxInputTokens": 100000 },
                    "supportedInputTypes": ["TEXT"]
                }
            ],
            "defaultModel": { "modelId": "auto" }
        })
    }

    /// 记录形态：默认那条排最前，名字/窗口/能力位都对
    #[test]
    fn parse_builds_records_and_puts_the_default_first() {
        let parsed = parse(&payload()).expect("payload 合法");
        assert_eq!(parsed.models.len(), 2);
        assert_eq!(parsed.models[0]["id"], "auto");
        assert_eq!(parsed.models[0]["name"], "Auto", "缺 modelName 时给可读名");
        assert_eq!(parsed.models[0]["isDefault"], true);
        assert_eq!(parsed.models[1]["id"], "claude-sonnet-4.5");
        assert_eq!(parsed.models[1]["name"], "Claude Sonnet 4.5");
        assert_eq!(parsed.models[1]["isDefault"], false);
        assert_eq!(parsed.models[1]["supportsImages"], true);
        assert!(parsed.models[0].get("supportsImages").is_none(), "不支持就不写这个键");
        assert_eq!(parsed.windows.get("claude-sonnet-4.5").copied(), Some(200_000));
    }

    /// 思考能力位按模型名判（Claude / Auto 才有）
    #[test]
    fn reasoning_support_follows_the_model_family() {
        assert!(thinks("claude-sonnet-4.5"));
        assert!(thinks("CLAUDE-OPUS-4"));
        assert!(thinks("auto"));
        assert!(!thinks("amazon-nova-pro"));
        let parsed = parse(&payload()).expect("payload 合法");
        let sonnet = parsed
            .models
            .iter()
            .find(|row| row["id"] == "claude-sonnet-4.5")
            .expect("有 sonnet");
        assert_eq!(sonnet["supportsReasoning"], true);
    }

    /// 目录声明自适应思考 ⇒ 档位走字段（不是正文前缀）
    #[test]
    fn adaptive_thinking_is_read_from_the_catalog_schema() {
        let parsed = parse(&payload()).expect("payload 合法");
        let native = parsed
            .natives
            .get("claude-sonnet-4.5")
            .expect("sonnet 是自适应");
        assert_eq!(native.path, "output_config");
        assert_eq!(native.levels, vec!["low", "medium", "high"]);
        assert!(
            !parsed.natives.contains_key("auto"),
            "没声明 schema 的模型走前缀那条路"
        );
    }

    /// 没 `adaptive` 的 schema 不认（有的模型只收 `enabled`）
    #[test]
    fn a_schema_without_adaptive_is_not_a_native_effort() {
        let schema = json!({
            "properties": {
                "thinking": { "properties": { "type": { "enum": ["enabled", "disabled"] } } },
                "output_config": { "properties": { "effort": { "enum": ["high"] } } }
            }
        });
        assert!(native_effort(Some(&schema)).is_none());
        assert!(native_effort(None).is_none());
        assert!(native_effort(Some(&json!("不是对象"))).is_none());
    }

    /// 只剩 `reasoning` 路径时也要认出来
    #[test]
    fn the_reasoning_path_is_the_fallback() {
        let schema = json!({
            "properties": {
                "thinking": { "properties": { "type": { "enum": ["adaptive"] } } },
                "reasoning": { "properties": { "effort": { "enum": ["low", "high"] } } }
            }
        });
        let native = native_effort(Some(&schema)).expect("有 reasoning 路径");
        assert_eq!(native.path, "reasoning");
        assert_eq!(native.levels, vec!["low", "high"]);
    }

    /// 目录说了默认就用它；没说时 `auto` 是默认
    #[test]
    fn the_default_model_rule_is_explicit() {
        let mut body = payload();
        body["defaultModel"] = json!({ "modelId": "claude-sonnet-4.5" });
        let parsed = parse(&body).expect("payload 合法");
        assert_eq!(parsed.models[0]["id"], "claude-sonnet-4.5");
        assert_eq!(parsed.models[0]["isDefault"], true);
        assert_eq!(parsed.models[1]["isDefault"], false);

        let mut no_default = payload();
        no_default.as_object_mut().map(|map| map.remove("defaultModel"));
        let parsed = parse(&no_default).expect("payload 合法");
        assert_eq!(parsed.models[0]["id"], "auto");
        assert_eq!(parsed.models[0]["isDefault"], true);
    }

    /// 空清单是错误而不是「成功但零条」：零条会让管理页显示成「已刷新」，
    /// 实际上下次请求只能靠兜底
    #[test]
    fn an_empty_catalog_is_an_error() {
        let empty = json!({ "models": [] });
        assert!(parse(&empty).is_err());
        // 没有 modelId 的行跳过，全跳过等于空清单
        let junk = json!({ "models": [{ "modelName": "x" }, { "modelId": "" }] });
        assert!(parse(&junk).is_err());
    }

    /// 兜底只有一条，且是 `auto`
    #[test]
    fn the_fallback_has_exactly_one_entry() {
        let fallback = fallback();
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0]["id"], "auto");
        assert_eq!(fallback[0]["isDefault"], true);
        assert_eq!(fallback[0]["supportsToolCall"], true);
    }

    /// 缓存按账号分（两个订阅看不到同一份清单）
    #[test]
    fn the_cache_is_keyed_by_account() {
        let parsed = parse(&payload()).expect("payload 合法");
        with_cache(|table| {
            table.insert(
                "kiro:test-a".to_string(),
                Catalog {
                    models: parsed.models,
                    fetched_at: now_ms(),
                    natives: parsed.natives,
                    windows: parsed.windows,
                },
            );
        });
        assert!(list("kiro:test-a").len() == 2);
        assert_eq!(list("kiro:test-b").len(), 1, "别的账号只看到兜底");
        assert_eq!(window_of("kiro:test-b", "claude-sonnet-4.5"), 0);
        assert!(native_of("kiro:test-b", "claude-sonnet-4.5").is_none());
        with_cache(|table| table.remove("kiro:test-a"));
    }
}
