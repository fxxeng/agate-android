//! 灵犀模型目录归一：`key` / `multiplier` → 本仓 `list_item` 认的形态。
//!
//! 两个公开来源给的目录形状不一样（见方案 §1）：网关项目用 `key` +
//! `multiplier` + `max_context_tokens`，DSH 插件用 `GET sessions/plans` 的
//! `data.models`（元素可能只是字符串）。这里两种都认，拿不到的字段**留 null**，
//! 不拿 0 / 空串冒充。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::server::logging;

/// 内置兜底目录（公开参考实现里反复出现的模型 key；倍率按 `multiplier`）。
/// 上游插件包没有这一家，清单必须能离线给出，否则刚加完账号 `/v1/models`
/// 是空的，客户端连可点的名字都没有。
fn builtin_catalog() -> Vec<Value> {
    parse_catalog(&json!({ "models": [
        { "key": "deepseek-v4.1-flash", "name": "DeepSeek V4.1 Flash", "multiplier": 0.12 },
        { "key": "qwen3.8_flash", "name": "Qwen3.8 Flash", "multiplier": 0.08 },
        { "key": "qwen3.8_max", "name": "Qwen3.8 Max", "multiplier": 0.8 },
        { "key": "glm-5.3-flash", "name": "GLM-5.3 Flash", "multiplier": 0.11 },
        { "key": "glm-5.3", "name": "GLM-5.3", "multiplier": 0.75 },
        { "key": "kimi-k3", "name": "Kimi K3", "multiplier": 1.0 },
        { "key": "mimo-v2.6-pro", "name": "MiMo V2.6 Pro", "multiplier": 0.1 }
    ] }))
}

fn table() -> &'static Mutex<Vec<Value>> {
    static TABLE: OnceLock<Mutex<Vec<Value>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(builtin_catalog()))
}

fn lock_table() -> std::sync::MutexGuard<'static, Vec<Value>> {
    table().lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 目录「来源 / 更新日期」列要用的两个读数（消费方是
/// `providers::catalog::refresh_meta`）。内置兜底清单不算远程拉取，
/// 编一个「刚刚刷新过」的时刻会让界面说谎。
fn refresh_marks() -> (&'static AtomicBool, &'static AtomicI64) {
    static REMOTE: OnceLock<AtomicBool> = OnceLock::new();
    static LAST: OnceLock<AtomicI64> = OnceLock::new();
    (
        REMOTE.get_or_init(|| AtomicBool::new(false)),
        LAST.get_or_init(|| AtomicI64::new(0)),
    )
}

pub fn remote_refreshed() -> bool {
    refresh_marks().0.load(Ordering::Relaxed)
}

/// 没拉过给 0（不是「现在」，也不是内置清单写入时刻）。
pub fn last_refreshed_at() -> i64 {
    refresh_marks().1.load(Ordering::Relaxed)
}

/// 当前目录（内置兜底 / 远程刷新后的清单）。
pub fn current() -> Vec<Value> {
    lock_table().clone()
}

/// 用一次远程拉取的结果替换目录；空清单**不覆盖**（保留兜底，别把页面清空）。
pub fn replace(rows: Vec<Value>) -> usize {
    if rows.is_empty() {
        return current().len();
    }
    let mut guard = lock_table();
    let count = rows.len();
    *guard = rows;
    let (remote, last) = refresh_marks();
    remote.store(true, Ordering::Relaxed);
    last.store(logging::now_ms(), Ordering::Relaxed);
    count
}

fn first_text<'a>(item: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| item.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn first_number(item: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| item.get(*key).and_then(Value::as_f64))
}

/// 倍率直接当 `credits`（与 `lobsterai/models.rs` 同一写法）；0 = 免费，不是缺失。
fn credits_label(multiplier: Option<f64>) -> Option<String> {
    multiplier.map(|value| {
        if value == 0.0 {
            "免费".to_string()
        } else {
            format!("x{value}")
        }
    })
}

/// 把上游目录解析成本仓清单条目。**按小写去重**（同一 id 出现两次会让
/// `/v1/models` 广告两遍，见 `catalog::advertised_names` 的既有判据）。
pub fn parse_catalog(root: &Value) -> Vec<Value> {
    let list: &[Value] = root
        .get("data")
        .and_then(|data| data.get("models"))
        .and_then(Value::as_array)
        .or_else(|| root.get("models").and_then(Value::as_array))
        .or_else(|| root.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let mut claimed: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for item in list {
        let id = match item {
            Value::String(name) => name.trim().to_string(),
            Value::Object(_) => first_text(item, &["key", "id", "model", "name"])
                .unwrap_or("")
                .to_string(),
            _ => String::new(),
        };
        if id.is_empty() || !claimed.insert(id.to_lowercase()) {
            continue;
        }
        let name = first_text(item, &["name"]).unwrap_or(id.as_str()).to_string();
        let multiplier = first_number(item, &["multiplier", "cost_multiplier", "costMultiplier"]);
        let context = first_number(item, &["max_context_tokens", "maxContextTokens", "context_window"]);
        out.push(json!({
            "id": id,
            "name": name,
            "credits": credits_label(multiplier),
            "maxInputTokens": context.map(|value| value as i64),
            "kind": "chat",
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_field_is_the_model_id_not_id() {
        let rows = parse_catalog(&json!({
            "data": { "models": [
                { "key": "deepseek-v4.1-flash", "name": "DeepSeek V4.1 Flash",
                  "multiplier": 0.12, "max_context_tokens": 128000 }
            ] }
        }));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], json!("deepseek-v4.1-flash"));
        assert_eq!(rows[0]["credits"], json!("x0.12"));
        assert_eq!(rows[0]["maxInputTokens"], json!(128000));
    }

    #[test]
    fn plans_shape_with_string_entries_is_accepted() {
        let rows = parse_catalog(&json!({ "data": { "models": ["qwen3.8_flash", "kimi-k3"] } }));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], json!("qwen3.8_flash"));
        assert!(rows[0]["credits"].is_null(), "没给倍率时留 null，不拿 0 冒充");
    }

    #[test]
    fn duplicate_ids_are_deduplicated_case_insensitively() {
        let rows = parse_catalog(&json!({
            "models": ["GLM-5.3", "glm-5.3", { "key": "Glm-5.3" }]
        }));
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn free_multiplier_is_not_reported_as_missing() {
        let rows = parse_catalog(&json!({ "models": [{ "key": "free-model", "multiplier": 0 }] }));
        assert_eq!(rows[0]["credits"], json!("免费"));
    }
}
