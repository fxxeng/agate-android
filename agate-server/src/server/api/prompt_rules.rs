//! 提示词规则：`GET/PUT /api/prompt-rules`。
//!
//! ── 为什么与 `/api/prompt` 分成两条 ──────────────────────────
//! `/api/prompt` 是「网关自带提示词」的全局开关与文本（一家的默认形态）；
//! 这条是「按条件换提示词」的规则表。两者共用同一份落点（`promptMode` /
//! `promptFile` / `promptText` 是规则的回落项），所以保存时会一起写 ——
//! 但**读**分开：界面上的「提示词」页与「提示词规则」页是两个心智模型，
//! 混在一个响应里会让两边的草稿互相覆盖。
//!
//! ── 修订号覆盖哪些键 ────────────────────────────────────────
//! 规则表 + 三个回落项。只改回落项也算一次改动：两个窗口一改规则、一改回落，
//! 不纳入哈希就会互相覆盖而不报冲突。

use axum::body::Bytes;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::config::{self, prompt_rules};
use crate::server::core::prompt::rules::{Content, Rule};
use crate::server::errors::{self, GatewayError};
use crate::server::http::{ok_json, parse_body};
use crate::server::logging;

fn view() -> Value {
    let snapshot = prompt_rules::current();
    let snapshot_cfg = config::current();
    let settings = snapshot_cfg.prompt_settings();
    // 回落项用**配置里的原始输入**（`file` / `inline`），不是 `text`：
    // `text` 是「实际生效的文本」（可能来自内置默认或读到的文件内容），
    // 把它当成用户输入回写给编辑器，会把内置提示词也塞进编辑框。
    let fallback = Content {
        mode: settings.mode,
        file: settings.file.clone().unwrap_or_default(),
        text: settings.inline.clone().unwrap_or_default(),
    };
    json!({
        "rules": snapshot.rules.iter().map(|compiled| {
            let prompt = &compiled.prompt;
            json!({
                "id": compiled.rule.id,
                "name": compiled.rule.name,
                "enabled": compiled.rule.enabled,
                "logic": compiled.rule.logic,
                "conditions": compiled.rule.conditions,
                "mode": compiled.rule.mode.as_str(),
                "file": compiled.rule.file,
                "text": compiled.rule.text,
                "effectiveText": prompt.text,
                "source": prompt.source.as_str(),
                "lines": prompt.text.lines().count(),
                "fileError": prompt.file_error,
            })
        }).collect::<Vec<_>>(),
        "fallback": {
            "mode": fallback.mode.as_str(),
            "file": fallback.file,
            "text": fallback.text,
        },
        // 旧版「逐家覆盖」的等价投影：只给界面看「这些家现在用的是哪份」，
        // 不在读取时写库（规则一旦保存，这份清单只是历史视图）。
        "legacy": prompt_rules::legacy_rules(&settings.providers).len(),
        "configured": snapshot.configured,
        "revision": snapshot.revision,
        "error": snapshot.error,
        "limits": {
            "rules": crate::server::core::prompt::rules::MAX_RULES,
            "conditions": crate::server::core::prompt::rules::MAX_CONDITIONS,
            "values": crate::server::core::prompt::rules::MAX_VALUES,
        },
    })
}

fn response(result: Result<Value, GatewayError>) -> Response {
    match result {
        Ok(value) => ok_json(value),
        Err(error) => errors::management_error(error.status_code, error.message),
    }
}

pub async fn get_rules() -> Response {
    response(Ok(view()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveRules {
    rules: Vec<Rule>,
    fallback: Content,
    revision: String,
}

pub async fn put_rules(body: Bytes) -> Response {
    let payload = match parse_body(&body).and_then(|value| {
        serde_json::from_value::<SaveRules>(value)
            .map_err(|error| GatewayError::bad_request(format!("提示词规则内容不完整：{error}")))
    }) {
        Ok(payload) => payload,
        Err(error) => return response(Err(error)),
    };
    let count = payload.rules.iter().filter(|rule| rule.enabled).count();
    match prompt_rules::save(payload.rules, payload.fallback, &payload.revision) {
        Ok(()) => {
            logging::log(
                "[Config]",
                &format!("提示词规则已保存：{count} 条启用，仅对新请求生效"),
            );
            response(Ok(view()))
        }
        Err(error) => response(Err(error)),
    }
}
