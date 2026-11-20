//! 提示词规则（`promptRules`）：按 (家, 客户端, 模型) 条件选不同提示词。
//!
//! ── 与「逐家覆盖」的分工 ────────────────────────────────────
//! `promptProviders` 是「这一家固定用这份」，本模块是「满足这些条件时用这份」。
//! 规则先判（按配置顺序，第一条命中即止），未命中才回落到逐家覆盖 / 全局默认。
//! 两条路径共用同一套取值与文件读取（`resolve_choice` / `read_prompt_file`），
//! 所以「规则里写的文件读不到」与「全局写的文件读不到」表现一致。
//!
//! ── 为什么解析结果要编译 ────────────────────────────────────
//! 条件里的 `glob` / `regex` 每次请求都编译一遍是不可接受的（正则编译是微秒
//! 级，而转发链每请求要过几次）。所以保存时校验一次、读取时编译一次，
//! 之后请求走的是 [`CompiledRule`]。编译失败不 panic：整块规则退化成空表并
//! 留下错误文案 —— 「规则写坏了」不该让所有请求失去提示词。

use std::sync::{Arc, RwLock};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::server::core::prompt::rules::{
    self, CompiledRule, Condition, Content, Field, Logic, Operator, Rule,
};
use crate::server::core::prompt::PromptMode;
use crate::server::errors::GatewayError;

pub const KEY_PROMPT_RULES: &str = "promptRules";

/// 当前生效的规则集 + 修订号 + 解析错误。
#[derive(Clone, Debug, Default)]
pub struct PromptRulesSnapshot {
    /// 编译好的规则（顺序即匹配优先级）。
    pub rules: Vec<CompiledRule>,
    /// 配置里有没有这一项（区分「没配」与「配了但空」）。
    pub configured: bool,
    /// 解析/编译失败的人话。`None` = 没问题。
    pub error: Option<String>,
    /// 与规则相关的**所有**键的哈希：保存时回传，防并发覆盖。
    pub revision: String,
}

fn revision_of(raw: &Map<String, Value>) -> String {
    // 规则与回落项都算进修订号：只改回落项也是「用户改过这份配置」，
    // 不纳入哈希会让两个窗口互相覆盖而不报冲突。
    let mut values: Vec<Value> = vec![raw.get(KEY_PROMPT_RULES).cloned().unwrap_or(Value::Null)];
    for key in [
        crate::server::config::KEY_PROMPT_MODE,
        crate::server::config::KEY_PROMPT_FILE,
        crate::server::config::KEY_PROMPT_TEXT,
    ] {
        values.push(raw.get(key).cloned().unwrap_or(Value::Null));
    }
    format!(
        "{:x}",
        Sha256::digest(Value::Array(values).to_string().as_bytes())
    )
}

fn parse(raw: &Map<String, Value>) -> PromptRulesSnapshot {
    let revision = revision_of(raw);
    let Some(value) = raw.get(KEY_PROMPT_RULES) else {
        return PromptRulesSnapshot {
            rules: Vec::new(),
            configured: false,
            error: None,
            revision,
        };
    };
    let result = serde_json::from_value::<Vec<Rule>>(value.clone())
        .map_err(|error| format!("提示词规则格式不正确：{error}"))
        .and_then(|rules| {
            rules::validate_rules(&rules)?;
            compile(rules)
        });
    match result {
        Ok(rules) => PromptRulesSnapshot {
            rules,
            configured: true,
            error: None,
            revision,
        },
        Err(error) => PromptRulesSnapshot {
            rules: Vec::new(),
            configured: true,
            error: Some(error),
            revision,
        },
    }
}

/// 把规则编译成「每请求可直接判」的形态。
fn compile(rules: Vec<Rule>) -> Result<Vec<CompiledRule>, String> {
    rules
        .into_iter()
        .filter(|rule| rule.enabled)
        .map(|rule| {
            let matchers = rule
                .conditions
                .iter()
                .map(rules::Matcher::compile)
                .collect::<Result<Vec<_>, _>>()?;
            let prompt = resolve_content(&rule.content());
            Ok(CompiledRule {
                rule,
                prompt,
                matchers,
            })
        })
        .collect()
}

/// 规则内容 → 实际生效的那一份（文件 / 内联 / 内置三档，与全局同一条规则）。
///
/// 走的正是全局那一份 `resolve_choice`：规则与回落项不能各有一套「文件优先还是
/// 内联优先」的判定，否则同一份提示词在两条路径上会解析出不同文本。
pub fn resolve_content(content: &Content) -> crate::server::config::ProviderPrompt {
    let choice = super::resolve_choice(
        content.mode,
        Some(content.file.clone()).filter(|text| !text.trim().is_empty()),
        Some(content.text.clone()).filter(|text| !text.trim().is_empty()),
    );
    crate::server::config::ProviderPrompt {
        mode: choice.mode,
        file: choice.file,
        inline: choice.inline,
        text: choice.text,
        source: choice.source,
        file_error: choice.file_error,
    }
}

/// 进程级快照（懒加载 + 写时刷新，与 `client_identification` 同模式）。
static SNAPSHOT: RwLock<Option<Arc<PromptRulesSnapshot>>> = RwLock::new(None);

/// 当前生效的规则集。
pub fn current() -> Arc<PromptRulesSnapshot> {
    if let Ok(guard) = SNAPSHOT.read() {
        if let Some(snapshot) = guard.as_ref() {
            return snapshot.clone();
        }
    }
    let snapshot = Arc::new(parse(crate::server::config::current().raw()));
    if let Ok(mut guard) = SNAPSHOT.write() {
        *guard = Some(snapshot.clone());
    }
    snapshot
}

/// 保存（整份替换，带修订号校验）。
pub fn save(rules: Vec<Rule>, fallback: Content, expected: &str) -> Result<(), GatewayError> {
    rules::validate_rules(&rules).map_err(GatewayError::bad_request)?;
    validate_content(&fallback)?;
    for rule in rules.iter().filter(|rule| rule.enabled) {
        validate_content(&rule.content())?;
    }
    let encoded = serde_json::to_value(&rules)
        .map_err(|error| GatewayError::new(format!("提示词规则无法序列化：{error}")))?;
    // `config::update` 的闭包不返回结果，冲突只能先记在闭包外（与
    // `config::mutations::try_update_raw_field` 同一手法）。
    let conflict = std::sync::atomic::AtomicBool::new(false);
    let saved = crate::server::config::update(|config| {
        if revision_of(&config.raw) != expected {
            conflict.store(true, std::sync::atomic::Ordering::Release);
            return;
        }
        config.raw.insert(KEY_PROMPT_RULES.to_string(), encoded);
        config.raw.insert(
            crate::server::config::KEY_PROMPT_MODE.to_string(),
            Value::String(fallback.mode.as_str().to_string()),
        );
        for (key, value) in [
            (crate::server::config::KEY_PROMPT_FILE, fallback.file),
            (crate::server::config::KEY_PROMPT_TEXT, fallback.text),
        ] {
            if value.trim().is_empty() {
                config.raw.remove(key);
            } else {
                config.raw.insert(key.to_string(), Value::String(value));
            }
        }
    });
    if conflict.load(std::sync::atomic::Ordering::Acquire) {
        return Err(GatewayError::with_status(
            409,
            "系统提示词已在其他位置修改，请刷新后再保存；当前草稿未丢失",
        ));
    }
    if !saved {
        return Err(GatewayError::new("系统提示词保存失败，未应用本次修改"));
    }
    // 写盘后重算快照（落盘值与内存生效值必须来自同一处解析）。
    let next = Arc::new(parse(crate::server::config::current().raw()));
    if let Ok(mut guard) = SNAPSHOT.write() {
        *guard = Some(next);
    }
    Ok(())
}

/// 内容级校验：形状 + 文件可读。
fn validate_content(content: &Content) -> Result<(), GatewayError> {
    rules::validate_content(content).map_err(GatewayError::bad_request)?;
    if content.mode != PromptMode::Passthrough
        && content.text.trim().is_empty()
        && !content.file.trim().is_empty()
    {
        crate::server::config::read_prompt_file(&content.file)
            .map_err(GatewayError::bad_request)?;
    }
    Ok(())
}

/// 旧版「逐家覆盖」的等价投影：不在读取时写库，只用于界面展示。
pub fn legacy_rules(
    providers: &std::collections::BTreeMap<String, crate::server::config::ProviderPrompt>,
) -> Vec<Rule> {
    providers
        .iter()
        .enumerate()
        .map(|(index, (provider, prompt))| Rule {
            id: format!("legacy-provider-{index}"),
            name: crate::server::core::providers::label_of(provider),
            enabled: true,
            logic: Logic::All,
            conditions: vec![Condition {
                id: format!("legacy-condition-{index}"),
                field: Field::Provider,
                op: Operator::Is,
                values: vec![provider.clone()],
            }],
            mode: prompt.mode,
            file: prompt.file.clone().unwrap_or_default(),
            text: prompt.inline.clone().unwrap_or_default(),
        })
        .collect()
}
