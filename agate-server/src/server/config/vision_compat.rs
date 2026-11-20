//! 图片兼容配置：发图给「看不了图」的模型时怎么降级。
//!
//! ── 解决什么问题 ──────────────────────────────────────────────
//! 用户给一个纯文本模型发图，上游会以「model does not support image」之类拒答。
//! 这里配置「哪些模型命中、怎么处理」两种降级方式：
//! · `assist`（辅助识图）：另调一只视觉模型把图片读成文字，替换掉请求里的图片
//!   块，再交回原模型回答 —— 用户看到的仍是原模型，只是它「看懂了」图片。
//! · `reroute`（整体接管）：把整个请求改交给一只视觉模型回答。
//!
//! ── 本地化重写（相对上游的裁剪）──────────────────────────────
//! 1. 上游这套配置有「旧形状 + 投影块 + 手机端兼容」三层历史包袱，本地不留旧数据
//!    兼容（与 `title_routes` 同政策），只认 `rules` 一种权威形状。
//! 2. 上游 `shared_target` 来自 `tool_services`（桌面工具服务），本地没有该子系统 ——
//!    视觉目标一律取规则里显式配置的 `target`，不搞「沿用工具服务共享模型」。
//! 3. raw-only：`current()` 每次从 `raw()` 解析（图片兼容不是热路径），不新增
//!    140KB `config/mod.rs` 的 typed 字段（见 `mutations.rs` 头部说明）。
//!
//! ── 持久化约定 ────────────────────────────────────────────────
//! 整块保存、按修订号防覆盖（客户端提交它读到的那一版，期间被改过就 409）。

use std::collections::HashSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::server::errors::GatewayError;

/// `raw` 里的顶层键名。与上游同名，便于两边配置对照。
pub const KEY_VISION_COMPAT: &str = "visionCompatibility";

/// 规则条数上限：图片兼容是「按模型定向降级」，正常一两条就够。
pub const MAX_RULES: usize = 32;
/// 单条规则里「看不了图的模型」条数上限。
pub const MAX_MODELS: usize = 500;
/// 接管目标（含备选）上限。
pub const MAX_TARGETS: usize = 8;
/// 识读超时上限（秒）。
pub const MAX_TIMEOUT_SECONDS: u64 = 300;

/// 处理方式。图片兼容只有这两种。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// 辅助识图：另调视觉模型把图读成文字，替换后交回原模型。
    #[default]
    Assist,
    /// 整体接管：把请求整体交给你指定的视觉模型。
    Reroute,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assist => "assist",
            Self::Reroute => "reroute",
        }
    }
}

/// 接管方式失败后怎么办。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fallback {
    /// 停止并报错（默认）：不擅自把图片删掉继续回答（那会静默丢信息）。
    #[default]
    Stop,
    /// 用指定的接管模型再试一次（见 `targets`）。
    Designated,
}

/// 不知道模型能不能看图时怎么办。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownCapability {
    /// 先按原样发（默认）：能力未知时不主动触发兼容，避免误伤本能看图的模型。
    #[default]
    TryOriginal,
    /// 按兼容处理：能力未知也走降级。
    Compat,
}

/// 一只接管/识图用的视觉模型。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisionTarget {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub reasoning: String,
}

/// 名单里的一项：哪个提供商的哪个模型。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

impl ModelRef {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }

    fn matches(&self, provider: &str, model: &str) -> bool {
        self.provider.eq_ignore_ascii_case(provider) && self.model.eq_ignore_ascii_case(model)
    }
}

/// 一条图片兼容规则。
///
/// `mode` 决定用哪一组字段；另一组照样存着（校验时按 mode 只查用到的那组），
/// 用户在界面上切换方式时之前填的那些还在。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct VisionRule {
    /// 稳定 id（界面列表的 key，规则可增删改、顺序是语义的一部分）。
    pub id: String,
    /// 展示名（界面友好标签）。
    pub name: String,
    pub enabled: bool,
    /// 这些模型看不了图时命中这条规则。空 = 规则不生效。
    pub models: Vec<ModelRef>,
    pub mode: Mode,
    /// 辅助识图用哪只模型读图（本地版：必须显式配置，见模块头）。
    pub target: Option<VisionTarget>,
    /// 辅助识图：复用相同图片的识读结果。
    pub reuse_results: bool,
    /// 整体接管：优先用当前路由范围内的视觉模型（本地版：见 targets 实现）。
    pub prefer_current_route: bool,
    pub fallback: Fallback,
    /// 整体接管：接管模型与备选，按顺序尝试。
    pub targets: Vec<VisionTarget>,
}

impl Default for VisionRule {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            enabled: true,
            models: Vec::new(),
            mode: Mode::Assist,
            target: None,
            reuse_results: true,
            prefer_current_route: true,
            fallback: Fallback::Stop,
            targets: Vec::new(),
        }
    }
}

/// 整块配置。`#[serde(default)]` 让空库/空对象解析成「开 + 无规则」。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct VisionSettings {
    #[serde(default)]
    pub enabled: bool,
    /// 权威形状：按列表顺序匹配，第一条命中的生效。
    pub rules: Vec<VisionRule>,
    pub unknown_capability: UnknownCapability,
    pub timeout_seconds: u64,
}

impl Default for VisionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            rules: Vec::new(),
            unknown_capability: UnknownCapability::TryOriginal,
            timeout_seconds: 30,
        }
    }
}

impl VisionSettings {
    /// 这个模型命中的那条规则（顺序即优先级）；`None` = 不做图片兼容。
    pub fn rule_for(&self, provider: &str, model: &str) -> Option<VisionRule> {
        if !self.enabled {
            return None;
        }
        self.rules
            .iter()
            .filter(|rule| rule.enabled && !rule.models.is_empty())
            .find(|rule| {
                rule.models
                    .iter()
                    .any(|entry| entry.matches(provider, model))
            })
            .cloned()
    }
}

/// 运行期快照：配置本体 + 修订号。`settings` 用 `Result` 把校验/解析原因带到界面。
#[derive(Clone, Debug)]
pub struct VisionSnapshot {
    pub settings: Result<VisionSettings, String>,
    pub revision: String,
}

/// 修订号：对该键的 JSON 文本取 SHA-256（与 title_routes 同一口径）。
fn revision(value: Option<&Value>) -> String {
    let raw = value.cloned().unwrap_or(Value::Null);
    format!("{:x}", Sha256::digest(raw.to_string().as_bytes()))
}

fn from_value(value: Option<&Value>) -> VisionSnapshot {
    let settings = match value {
        None => Ok(VisionSettings::default()),
        Some(value) => serde_json::from_value::<VisionSettings>(value.clone())
            .map_err(|error| format!("图片兼容配置格式不正确：{error}"))
            .and_then(|settings| {
                validate(&settings)?;
                Ok(settings)
            }),
    };
    VisionSnapshot {
        settings,
        revision: revision(value),
    }
}

/// 校验配置。返回给界面直接展示的中文原因。
pub fn validate(settings: &VisionSettings) -> Result<(), String> {
    if settings.rules.len() > MAX_RULES {
        return Err(format!("图片兼容最多支持 {MAX_RULES} 条规则"));
    }
    if settings.timeout_seconds == 0 || settings.timeout_seconds > MAX_TIMEOUT_SECONDS {
        return Err(format!("识读超时必须在 1–{MAX_TIMEOUT_SECONDS} 秒之间"));
    }
    let mut ids = HashSet::new();
    for (index, rule) in settings.rules.iter().enumerate() {
        let at = index + 1;
        if rule.id.trim().is_empty() {
            return Err(format!("图片兼容第 {at} 条规则缺少 id"));
        }
        if !ids.insert(rule.id.as_str()) {
            return Err(format!("图片兼容规则 id 重复：{}", rule.id));
        }
        if rule.models.len() > MAX_MODELS {
            return Err(format!(
                "图片兼容第 {at} 条规则最多支持 {MAX_MODELS} 个模型"
            ));
        }
        if rule.targets.len() > MAX_TARGETS {
            return Err(format!(
                "图片兼容第 {at} 条规则最多支持 {MAX_TARGETS} 个接管目标"
            ));
        }
        // 启用时才要求配全；关掉后保留半截配置不该变成非法文件。
        if settings.enabled && rule.enabled {
            if rule.models.is_empty() {
                return Err(format!("启用图片兼容第 {at} 条规则前请先选择要降级的模型"));
            }
            for entry in &rule.models {
                if entry.provider.trim().is_empty() || entry.model.trim().is_empty() {
                    return Err(format!("图片兼容第 {at} 条规则有模型的提供商或模型名为空"));
                }
            }
            match rule.mode {
                Mode::Assist => {
                    let target = rule
                        .target
                        .as_ref()
                        .ok_or_else(|| format!("启用辅助识图第 {at} 条规则前请先指定识图模型"))?;
                    if target.provider.trim().is_empty() || target.model.trim().is_empty() {
                        return Err(format!("图片兼容第 {at} 条规则的识图模型未填完整"));
                    }
                }
                Mode::Reroute => {
                    if rule.targets.is_empty() {
                        return Err(format!("启用整体接管第 {at} 条规则前请先指定接管模型"));
                    }
                    for target in &rule.targets {
                        if target.provider.trim().is_empty() || target.model.trim().is_empty() {
                            return Err(format!("图片兼容第 {at} 条规则有接管目标未填完整"));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// 从 `raw` 底稿构建快照。
pub(crate) fn from_raw(raw: &Map<String, Value>) -> Arc<VisionSnapshot> {
    Arc::new(from_value(raw.get(KEY_VISION_COMPAT)))
}

/// 当前配置快照。每次从 `raw()` 解析（非热路径，见模块头）。
pub fn current() -> Arc<VisionSnapshot> {
    from_raw(super::current().raw())
}

/// 整块保存。`expected` 是客户端读到的修订号，对不上返回 409 且不落盘。
pub fn save(settings: VisionSettings, expected: &str) -> Result<(), GatewayError> {
    validate(&settings).map_err(GatewayError::bad_request)?;
    let value =
        serde_json::to_value(settings).map_err(|error| GatewayError::new(error.to_string()))?;
    super::try_update_raw_field(
        KEY_VISION_COMPAT,
        |raw| {
            if revision(Some(raw)) != expected {
                return Err(GatewayError::with_status(
                    409,
                    "图片兼容配置已在其他位置修改，请重新读取后再保存；当前草稿未丢失",
                ));
            }
            *raw = value;
            Ok(())
        },
        || GatewayError::new("图片兼容配置保存失败，未应用本次修改"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assist_rule() -> VisionRule {
        VisionRule {
            id: "r1".into(),
            name: "测试规则".into(),
            models: vec![ModelRef::new("zhipu", "glm-4.7")],
            mode: Mode::Assist,
            target: Some(VisionTarget {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                reasoning: String::new(),
            }),
            ..VisionRule::default()
        }
    }

    #[test]
    fn default_is_enabled_without_rules() {
        let settings = VisionSettings::default();
        assert!(settings.enabled);
        assert!(settings.rules.is_empty());
        assert_eq!(settings.timeout_seconds, 30);
    }

    #[test]
    fn empty_raw_yields_default_snapshot() {
        let snapshot = from_raw(&Map::new());
        let settings = snapshot.settings.as_ref().unwrap();
        assert!(settings.rules.is_empty());
        assert!(settings.enabled);
    }

    #[test]
    fn rule_for_matches_first_case_insensitively() {
        let settings = VisionSettings {
            rules: vec![assist_rule()],
            ..VisionSettings::default()
        };
        let hit = settings.rule_for("ZhiPu", "GLM-4.7").unwrap();
        assert_eq!(hit.mode, Mode::Assist);
        assert!(settings.rule_for("zhipu", "glm-4").is_none());
    }

    #[test]
    fn disabled_settings_never_matches() {
        let settings = VisionSettings {
            enabled: false,
            rules: vec![assist_rule()],
            ..VisionSettings::default()
        };
        assert!(settings.rule_for("zhipu", "glm-4.7").is_none());
    }

    #[test]
    fn validate_requires_target_for_assist() {
        let mut settings = VisionSettings {
            rules: vec![VisionRule {
                id: "r1".into(),
                models: vec![ModelRef::new("p", "m")],
                target: None,
                ..VisionRule::default()
            }],
            ..VisionSettings::default()
        };
        assert!(validate(&settings).is_err());
        settings.rules[0] = assist_rule();
        assert!(validate(&settings).is_ok());
    }

    #[test]
    fn revision_changes_with_value() {
        let a = json!({"enabled": true});
        let b = json!({"enabled": false});
        assert_ne!(revision(Some(&a)), revision(Some(&b)));
        assert_ne!(revision(None), revision(Some(&a)));
    }
}
