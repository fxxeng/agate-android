//! 上下文压缩路由配置：整块保存、按修订号防覆盖，请求读同一份运行期快照。
//!
//! ── 与上游的差异（本文件是适配实现）────────────────────────────
//! 与 `title_routes` 同一处理：上游把解析结果缓存进 `RuntimeConfig` 的 typed
//! 字段（`config.compact_routes`），Agate 本批不新增 typed 字段，`current()`
//! 每次从 `raw()` 解析。压缩路由不是热路径（只在识别到压缩请求时读一次）。

use std::collections::HashSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::server::errors::GatewayError;

/// `raw` 里的顶层键名。与上游同名，便于两边配置对照。
pub const KEY_COMPACT_ROUTES: &str = "compactModelRoutes";

/// 规则条数上限。
pub const MAX_COMPACT_ROUTES: usize = 200;

/// 目前只支持 ZCode 的压缩请求识别。
pub const CLIENT_ZCODE: &str = "zcode";

/// 一条压缩映射：某客户端的某个来源模型 → 换成哪个目标模型。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompactRoute {
    pub client: String,
    pub source: String,
    pub target: String,
    pub enabled: bool,
}

/// 整块配置。`#[serde(default)]` 让空库/空对象解析成「开 + 无规则」。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct CompactRoutes {
    pub enabled: bool,
    pub rules: Vec<CompactRoute>,
}

impl Default for CompactRoutes {
    fn default() -> Self {
        Self { enabled: true, rules: Vec::new() }
    }
}

/// 运行期快照：配置本体 + 修订号。
#[derive(Clone, Debug)]
pub struct CompactRoutesSnapshot {
    pub settings: Result<CompactRoutes, String>,
    pub revision: String,
}

impl Default for CompactRoutesSnapshot {
    fn default() -> Self {
        from_value(None)
    }
}

/// 模型 ID 的比较口径：去首尾空格 + 小写。
pub fn model_key(model: &str) -> String {
    model.trim().to_lowercase()
}

/// 校验配置。返回给界面直接展示的中文原因。
pub fn validate(settings: &CompactRoutes) -> Result<(), String> {
    if settings.rules.len() > MAX_COMPACT_ROUTES {
        return Err(format!("压缩路由最多允许 {MAX_COMPACT_ROUTES} 条规则"));
    }
    let mut sources = HashSet::new();
    for rule in &settings.rules {
        if rule.client != CLIENT_ZCODE {
            return Err("压缩路由目前只支持 ZCode 客户端".into());
        }
        for model in [&rule.source, &rule.target] {
            let trimmed = model.trim() != model;
            let too_long = model.chars().count() > 256;
            let has_control = model.chars().any(char::is_control);
            if model.trim().is_empty() || trimmed || too_long || has_control {
                return Err(
                    "来源模型和压缩模型必须完整填写，模型 ID 不能超过 256 个字符或含控制字符"
                        .into(),
                );
            }
        }
        let source = model_key(&rule.source);
        if source == model_key(&rule.target) {
            return Err("来源模型和压缩模型不能相同".into());
        }
        if !sources.insert((rule.client.clone(), source)) {
            return Err(format!("同一客户端的来源模型不能重复：{}", rule.source));
        }
    }
    Ok(())
}

/// 修订号：对该键的 JSON 文本取 SHA-256。
fn revision(value: Option<&Value>) -> String {
    let raw = value.cloned().unwrap_or(Value::Null);
    format!("{:x}", Sha256::digest(raw.to_string().as_bytes()))
}

fn from_value(value: Option<&Value>) -> CompactRoutesSnapshot {
    let settings = match value {
        None => Ok(CompactRoutes::default()),
        Some(value) => serde_json::from_value::<CompactRoutes>(value.clone())
            .map_err(|error| format!("压缩路由配置格式不正确：{error}"))
            .and_then(|settings| {
                validate(&settings)?;
                Ok(settings)
            }),
    };
    CompactRoutesSnapshot { settings, revision: revision(value) }
}

/// 从 `raw` 底稿构建快照。
pub(crate) fn from_raw(raw: &Map<String, Value>) -> Arc<CompactRoutesSnapshot> {
    Arc::new(from_value(raw.get(KEY_COMPACT_ROUTES)))
}

/// 当前配置快照。每次从 `raw()` 解析（刻意不缓存，见文件头说明）。
pub fn current() -> Arc<CompactRoutesSnapshot> {
    from_raw(super::current().raw())
}

/// 当前生效的配置：总开关打开、且有至少一条启用的规则。
pub fn active() -> Option<CompactRoutes> {
    let snapshot = current();
    let settings = snapshot.settings.as_ref().ok()?;
    if !settings.enabled || !settings.rules.iter().any(|rule| rule.enabled) {
        return None;
    }
    Some(settings.clone())
}

/// 整块保存。`expected` 是客户端读到的修订号，对不上返回 409 且不落盘。
pub fn save(settings: CompactRoutes, expected: &str) -> Result<(), GatewayError> {
    validate(&settings).map_err(GatewayError::bad_request)?;
    let value =
        serde_json::to_value(settings).map_err(|error| GatewayError::new(error.to_string()))?;
    super::try_update_raw_field(
        KEY_COMPACT_ROUTES,
        |raw| {
            // 缺键时更新入口给 Null；它与读取端的初始修订号一致。
            if revision(Some(raw)) != expected {
                return Err(GatewayError::with_status(
                    409,
                    "压缩路由已在其他位置修改，请重新读取后再保存；当前草稿未丢失",
                ));
            }
            *raw = value;
            Ok(())
        },
        || GatewayError::new("压缩路由保存失败，未应用本次修改"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(source: &str, target: &str) -> CompactRoute {
        CompactRoute {
            client: CLIENT_ZCODE.to_string(),
            source: source.to_string(),
            target: target.to_string(),
            enabled: true,
        }
    }

    #[test]
    fn default_is_enabled_without_rules() {
        let settings = CompactRoutes::default();
        assert!(settings.enabled);
        assert!(settings.rules.is_empty());
    }

    #[test]
    fn empty_raw_yields_default_snapshot() {
        let snapshot = from_raw(&Map::new());
        assert!(snapshot.settings.is_ok());
    }

    #[test]
    fn validate_rejects_unsupported_client() {
        let mut rule = route("a", "b");
        rule.client = "other".into();
        let settings = CompactRoutes { enabled: true, rules: vec![rule] };
        assert!(validate(&settings).is_err());
    }

    #[test]
    fn validate_rejects_same_source_and_target() {
        let settings = CompactRoutes { enabled: true, rules: vec![route("m1", "M1")] };
        let error = validate(&settings).unwrap_err();
        assert!(error.contains("不能相同"));
    }

    #[test]
    fn validate_rejects_duplicate_source() {
        let settings =
            CompactRoutes { enabled: true, rules: vec![route("m1", "m2"), route("m1", "m3")] };
        let error = validate(&settings).unwrap_err();
        assert!(error.contains("不能重复"));
    }

    #[test]
    fn validate_accepts_well_formed_rules() {
        let settings = CompactRoutes { enabled: true, rules: vec![route("long-ctx", "short-ctx")] };
        assert!(validate(&settings).is_ok());
    }

    #[test]
    fn model_key_is_case_insensitive() {
        assert_eq!(model_key("  GPT-4O "), "gpt-4o");
    }
}
