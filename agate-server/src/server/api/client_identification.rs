//! 客户端识别规则：`GET/PUT /api/client-identification` + `POST …/preview`。
//!
//! ── 三条端点各自回答什么 ────────────────────────────────────
//! · `GET`：当前配置 + 内置目录 + 各项上限。界面要渲染「下拉里有哪些客户端」，
//!   那份清单必须由后端给 —— 前端抄一份就会与 `classify` 的固定表分叉。
//! · `PUT`：整份替换（带 `revision` 防并发覆盖）。**不合并**：规则是有序的，
//!   合并会让「谁先匹配」取决于提交顺序而不是用户排的顺序。
//! · `POST …/preview`：拿一份「假如客户端发来这些头」输入，回答「会识别成谁、
//!   命中哪条规则」。输入由调用方给（UA + 若干身份头），**不扫历史库** ——
//!   预览要回答的是「这条规则对不对」，而不是「过去 500 条请求会变成什么样」；
//!   后者要等采集侧把证据落库之后才有意义。
//!
//! ── 为什么标识为「显示识别」 ────────────────────────────────
//! 这套配置只影响统计列与提示词规则匹配。协议兼容（Codex 的推理折叠、按客户端
//! 改写请求）走 `core::client::classify` 的固定表，**不读这里的任何一项** ——
//! 让用户给客户端改个名字就改变请求形状，是最难查的一类 bug。

use axum::body::Bytes;
use axum::http::HeaderMap;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::config::client_identification;
use crate::server::core::client::{self, catalog, ClientEvidence, ClientSettings};
use crate::server::errors::{self, GatewayError};
use crate::server::http::{ok_json, parse_body};
use crate::server::logging;

fn view() -> Result<Value, GatewayError> {
    let snapshot = client_identification::current();
    let settings = snapshot
        .settings
        .as_ref()
        .map_err(|error| GatewayError::new(error.clone()))?;
    Ok(json!({
        "config": settings,
        "revision": snapshot.revision,
        "builtins": catalog::CLIENTS,
        "headers": std::iter::once("user-agent")
            .chain(catalog::IDENTITY_HEADERS.iter().copied())
            .collect::<Vec<_>>(),
        "declarationHeader": catalog::DECLARATION_HEADER,
        "maxRules": catalog::MAX_RULES,
        "maxClients": catalog::MAX_CLIENTS,
    }))
}

fn response(result: Result<Value, GatewayError>) -> Response {
    match result {
        Ok(value) => ok_json(value),
        Err(error) => errors::management_error(error.status_code, error.message),
    }
}

pub async fn get_settings() -> Response {
    response(view())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveSettings {
    config: ClientSettings,
    revision: String,
}

pub async fn put_settings(body: Bytes) -> Response {
    let payload = match parse_body(&body).and_then(|value| {
        serde_json::from_value::<SaveSettings>(value)
            .map_err(|error| GatewayError::bad_request(format!("客户端识别设置不完整：{error}")))
    }) {
        Ok(payload) => payload,
        Err(error) => return response(Err(error)),
    };
    let rules = payload.config.rules.len();
    let clients = payload.config.clients.len();
    match client_identification::save(payload.config, &payload.revision) {
        Ok(()) => {
            logging::log(
                "[Config]",
                &format!("客户端识别设置已保存：{rules} 条规则 / {clients} 个自定义客户端，仅对新请求生效"),
            );
            response(view())
        }
        Err(error) => response(Err(error)),
    }
}

/// 预览入参：整份候选配置 + 一份「假如收到这些头」的证据。
///
/// `headers` 用 `Vec<(name, value)>` 而不是 Map：同名头是有序的，而
/// `ClientEvidence::capture` 的判据正是「同名头只能有一个」—— 用 Map 表达
/// 会让「畸形请求」这种最常见的识别失败场景无法被预览覆盖。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewSettings {
    config: ClientSettings,
    #[serde(default)]
    user_agent: String,
    #[serde(default)]
    headers: Vec<(String, String)>,
}

fn preview(payload: PreviewSettings) -> Result<Value, GatewayError> {
    catalog::validate(&payload.config).map_err(GatewayError::bad_request)?;
    let mut headers = HeaderMap::new();
    for (name, value) in &payload.headers {
        let Ok(name) = name.parse::<axum::http::HeaderName>() else {
            return Err(GatewayError::bad_request(format!("请求头名不合法：{name}")));
        };
        let Ok(parsed) = value.parse::<axum::http::HeaderValue>() else {
            return Err(GatewayError::bad_request(format!("请求头值不合法：{name}")));
        };
        headers.append(name, parsed);
    }
    if !payload.user_agent.trim().is_empty() {
        if let Ok(value) = payload.user_agent.parse::<axum::http::HeaderValue>() {
            headers.insert(axum::http::header::USER_AGENT, value);
        }
    }
    let evidence = ClientEvidence::capture(&headers);
    // 两条并排给：`strict` 是协议兼容真正用的那个值，`value` 是显示识别。
    // 界面把两者都展示出来，「同一条 UA 两种结果」是这套设计的常态而不是 bug。
    let strict = client::classify(&payload.user_agent);
    let (value, detail) = client::identify(evidence, &payload.config);
    Ok(json!({
        "strict": strict,
        "value": value,
        "label": client::display_name(&value, ""),
        "evidence": detail,
    }))
}

pub async fn preview_settings(body: Bytes) -> Response {
    let payload = match parse_body(&body).and_then(|value| {
        serde_json::from_value::<PreviewSettings>(value)
            .map_err(|error| GatewayError::bad_request(format!("识别预览内容不完整：{error}")))
    }) {
        Ok(payload) => payload,
        Err(error) => return response(Err(error)),
    };
    response(preview(payload))
}
