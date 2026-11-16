//! 标题生成路由配置：把客户端自动生成的标题请求改写到指定的低成本模型。
//!
//! ── 识别依据不在这一层 ──────────────────────────────────────────
//! 标题请求都是「固定 system 提示词 + 一段待总结内容」，正文里带客户端特有
//! 的措辞。判据归 `core::title_routing`，这里只管配置的形状与持久化。
//!
//! ── 与上游的差异（本文件是适配实现）────────────────────────────
//! 1. 上游把解析结果缓存进 `RuntimeConfig` 的 typed 字段
//!    （`config.title_routes`）；Agate 本批**不新增 typed 字段**，
//!    `current()` 每次从 `raw()` 解析。标题路由不是热路径（只在生成标题时
//!    读一次），这样能完全不碰 136 KB 的 `config/mod.rs` 结构。
//! 2. 上游保留了一套「旧形状（`clients` + `model`）读时折叠成一条规则」的
//!    兼容层；本项目已授权不留旧数据兼容，故删除，只认新形状。
//!
//! ── 持久化约定 ──────────────────────────────────────────────────
//! 整块保存、按修订号防覆盖（客户端提交它读到的那一版，期间被改过就 409）。

use std::collections::HashSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::server::errors::GatewayError;

/// `raw` 里的顶层键名。与上游同名，便于两边配置对照。
pub const KEY_TITLE_ROUTES: &str = "titleModelRoutes";

/// 单条规则最多认多少客户端。宽松上限：挡住异常载荷，不挡正常配置。
pub const MAX_CLIENTS: usize = 32;

/// 规则条数上限。标题生成是「小请求分流」，正常一两条就够。
pub const MAX_RULES: usize = 32;

/// 支持识别的客户端。新增一家要同时在 `core::title_routing` 加判据，
/// 否则界面能勾、请求永远不命中。
pub const CLIENT_ZCODE: &str = "zcode";
pub const CLIENT_CLAUDE_CODE: &str = "claude-code";
pub const CLIENT_CODEX: &str = "codex";

pub const CLIENTS: &[(&str, &str)] = &[
    (CLIENT_ZCODE, "ZCode"),
    (CLIENT_CLAUDE_CODE, "Claude Code"),
    (CLIENT_CODEX, "Codex"),
];

/// 标题模型不可用时怎么办。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fallback {
    /// 退回客户端原模型重试一次（默认）：标题请求失败会直接影响会话列表显示。
    #[default]
    Original,
    /// 明确报错，不改用原模型。
    Error,
}

impl Fallback {
    /// 用于日志与诊断；序列化本身走 serde 的 snake_case。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Error => "error",
        }
    }
}

/// 标题生成的一条规则：认哪些客户端、改成哪只模型、失败怎么办。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TitleRule {
    /// 单条是否生效。总开关关掉时整块都不生效。
    #[serde(default)]
    pub enabled: bool,
    /// 参与识别的客户端 id。空 = 这条规则不生效（等同关闭）。
    pub clients: Vec<String>,
    /// 标题模型。空串 = 未配置。
    pub model: String,
    /// 模型不可用时的行为。
    #[serde(default)]
    pub fallback: Fallback,
}

/// 整块配置。`#[serde(default)]` 让空库/空对象解析成「开 + 无规则」。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct TitleRoutes {
    pub enabled: bool,
    /// 按列表顺序匹配，第一条命中的生效。
    pub rules: Vec<TitleRule>,
}

impl Default for TitleRoutes {
    fn default() -> Self {
        Self { enabled: true, rules: Vec::new() }
    }
}

/// 运行期快照：配置本体 + 修订号。
///
/// `settings` 用 `Result` 而不是 `Option`：解析或校验失败时要能把**原因**
/// 带到界面上，不能只显示「空配置」——那会让用户以为自己没配过。
#[derive(Clone, Debug)]
pub struct TitleRoutesSnapshot {
    pub settings: Result<TitleRoutes, String>,
    pub revision: String,
}

impl Default for TitleRoutesSnapshot {
    fn default() -> Self {
        from_value(None)
    }
}

/// 模型 ID 的比较口径：去首尾空格 + 小写。各家大小写不一致是常态。
pub fn model_key(model: &str) -> String {
    model.trim().to_lowercase()
}

/// 客户端 id → 展示名。
pub fn client_label(id: &str) -> Option<&'static str> {
    CLIENTS.iter().find(|(key, _)| *key == id).map(|(_, label)| *label)
}

/// 校验配置。返回给界面直接展示的中文原因。
pub fn validate(settings: &TitleRoutes) -> Result<(), String> {
    if settings.rules.len() > MAX_RULES {
        return Err(format!("标题生成路由最多支持 {MAX_RULES} 条规则"));
    }
    for (index, rule) in settings.rules.iter().enumerate() {
        let at = index + 1;
        if rule.clients.len() > MAX_CLIENTS {
            return Err(format!("标题生成路由第 {at} 条规则最多支持 {MAX_CLIENTS} 个客户端"));
        }
        let mut seen = HashSet::new();
        for client in &rule.clients {
            if client_label(client).is_none() {
                return Err(format!("标题生成路由不支持客户端「{client}」"));
            }
            if !seen.insert(client.as_str()) {
                return Err(format!(
                    "第 {at} 条规则里「{}」重复出现",
                    client_label(client).unwrap_or(client)
                ));
            }
        }
        // 只有真的启用时才要求配全：关掉以后保留半截配置不该变成非法文件。
        let live = settings.enabled && rule.enabled;
        if live && rule.clients.is_empty() {
            return Err(format!("启用标题生成路由第 {at} 条规则前请先选择客户端"));
        }
        let model = &rule.model;
        if model.trim().is_empty() {
            if live {
                return Err(format!("启用标题生成路由第 {at} 条规则前请先选择标题模型"));
            }
            continue;
        }
        let trimmed = model.trim() != model;
        let too_long = model.chars().count() > 256;
        let has_control = model.chars().any(char::is_control);
        if trimmed || too_long || has_control {
            return Err(format!(
                "第 {at} 条规则的标题模型 ID 不能超过 256 个字符、不能含控制字符或首尾空格"
            ));
        }
    }
    Ok(())
}

/// 这条规则此刻是否真的会命中：自己开着、勾了客户端、配了模型。
fn effective(rule: &TitleRule) -> bool {
    rule.enabled && !rule.clients.is_empty() && !rule.model.trim().is_empty()
}

/// 修订号：对该键的 JSON 文本取 SHA-256。
fn revision(value: Option<&Value>) -> String {
    let raw = value.cloned().unwrap_or(Value::Null);
    format!("{:x}", Sha256::digest(raw.to_string().as_bytes()))
}

fn from_value(value: Option<&Value>) -> TitleRoutesSnapshot {
    let settings = match value {
        None => Ok(TitleRoutes::default()),
        Some(value) => serde_json::from_value::<TitleRoutes>(value.clone())
            .map_err(|error| format!("标题生成路由配置格式不正确：{error}"))
            .and_then(|settings| {
                validate(&settings)?;
                Ok(settings)
            }),
    };
    TitleRoutesSnapshot { settings, revision: revision(value) }
}

/// 从 `raw` 底稿构建快照。
pub(crate) fn from_raw(raw: &Map<String, Value>) -> Arc<TitleRoutesSnapshot> {
    Arc::new(from_value(raw.get(KEY_TITLE_ROUTES)))
}

/// 当前配置快照。每次从 `raw()` 解析（刻意不缓存，见文件头说明）。
pub fn current() -> Arc<TitleRoutesSnapshot> {
    from_raw(super::current().raw())
}

/// 当前生效的配置：总开关打开、且至少有一条真会命中的规则。
pub fn active() -> Option<TitleRoutes> {
    let snapshot = current();
    let settings = snapshot.settings.as_ref().ok()?;
    if !settings.enabled || !settings.rules.iter().any(effective) {
        return None;
    }
    Some(settings.clone())
}

/// 这次请求该走哪条规则：按列表顺序取第一条命中的 —— 顺序即优先级。
pub fn match_rule(settings: &TitleRoutes, client: &str) -> Option<TitleRule> {
    settings
        .rules
        .iter()
        .filter(|rule| effective(rule))
        .find(|rule| rule.clients.iter().any(|id| id == client))
        .cloned()
}

/// 整块保存。`expected` 是客户端读到的修订号，对不上返回 409 且不落盘。
pub fn save(settings: TitleRoutes, expected: &str) -> Result<(), GatewayError> {
    validate(&settings).map_err(GatewayError::bad_request)?;
    let value =
        serde_json::to_value(settings).map_err(|error| GatewayError::new(error.to_string()))?;
    super::try_update_raw_field(
        KEY_TITLE_ROUTES,
        |raw| {
            if revision(Some(raw)) != expected {
                return Err(GatewayError::with_status(
                    409,
                    "标题生成路由已在其他位置修改，请重新读取后再保存；当前草稿未丢失",
                ));
            }
            *raw = value;
            Ok(())
        },
        || GatewayError::new("标题生成路由保存失败，未应用本次修改"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(clients: &[&str], model: &str) -> TitleRule {
        TitleRule {
            enabled: true,
            clients: clients.iter().map(|id| id.to_string()).collect(),
            model: model.to_string(),
            fallback: Fallback::default(),
        }
    }

    #[test]
    fn default_is_enabled_without_rules() {
        let settings = TitleRoutes::default();
        assert!(settings.enabled);
        assert!(settings.rules.is_empty());
    }

    #[test]
    fn empty_raw_yields_default_snapshot() {
        let raw = Map::new();
        let snapshot = from_raw(&raw);
        assert!(snapshot.settings.is_ok());
        assert!(snapshot.settings.unwrap().rules.is_empty());
    }

    #[test]
    fn validate_rejects_unknown_client() {
        let settings = TitleRoutes { enabled: true, rules: vec![rule(&["nope"], "m")] };
        assert!(validate(&settings).is_err());
    }

    #[test]
    fn validate_rejects_duplicate_client() {
        let settings = TitleRoutes { enabled: true, rules: vec![rule(&["zcode", "zcode"], "m")] };
        let error = validate(&settings).unwrap_err();
        assert!(error.contains("重复出现"));
    }

    #[test]
    fn disabled_rule_may_keep_half_config() {
        // 总开关关掉时，规则里缺客户端/模型不该判非法。
        let mut half = rule(&[], "");
        half.enabled = false;
        let settings = TitleRoutes { enabled: false, rules: vec![half] };
        assert!(validate(&settings).is_ok());
    }

    #[test]
    fn match_rule_takes_first_effective() {
        let settings = TitleRoutes {
            enabled: true,
            rules: vec![rule(&["zcode"], "first"), rule(&["zcode"], "second")],
        };
        let hit = match_rule(&settings, "zcode").unwrap();
        assert_eq!(hit.model, "first");
    }

    #[test]
    fn revision_changes_with_content() {
        let a = revision(Some(&serde_json::json!({"enabled": true})));
        let b = revision(Some(&serde_json::json!({"enabled": false})));
        assert_ne!(a, b);
    }
}
