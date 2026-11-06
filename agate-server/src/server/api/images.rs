//! `POST /v1/images/generations`：OpenAI Images 形态的生图入口。
//!
//! ── 与对话入口的分工 ──────────────────────────────────────────
//! 这个 handler 只做三件事，其余全交给既有链路：
//!   ① **校验与归一**（[`validate`]）：把客户端给的参数收窄成「所有生图上游都
//!      能理解的那一小撮」，超出范围的老实报 400，而不是转给上游让它猜；
//!   ② **模型解析**（[`resolve_model`]）：走 `pipeline::resolve_image_model`
//!      —— 与 `/v1/chat/completions` 是同一套目录 / 别名 / 白名单语义，只是
//!      允许图片模型（对话入口反过来会拒绝图片模型，见那里 `image_generation_required`
//!      那条门禁）；
//!   ③ **转发与记账**：`upstream().generate_images()` 走同一条转发链（多账号
//!      轮换、退避、冷却、手动终止都在那儿），响应再经 `image_output::normalize`
//!      归一成标准的 `{created, data:[…]}`。
//!
//! ── 为什么响应还要归一（上游不已经是 OpenAI 形态了吗）─────────────
//! 不完全是。各家有三种「不是标准 OpenAI」的形态（异步任务、信封、对话流里
//! 取图），而**最麻烦的是图片来源**：有的家给 `b64_json`，有的只给一个会过期
//! 的签名 URL。把 URL 直接转给客户端有两个后果 —— 链接几分钟后就失效，以及
//! 客户端（在国内网络里）根本连不上那个 CDN。所以网关**下载并转成 b64**（或转
//! 存到本机缓存换一个我们自己的短链接），见 `image_output` 的模块头。
//!
//! ── 本文件目前只有「生成」这一半 ──────────────────────────────
//! `/v1/images/edits`（带参考图的编辑）需要 multipart 解析，是下一批的事。
//! 现在**不挂载**那条路由：它会是 404，而不是一个「接受请求但永远失败」的入口
//! —— 后者最招人骂（看起来支持、点了没反应）。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::HeaderMap;
use axum::response::Response;
use serde_json::{json, Value};

use crate::server::core::key_scope::{self, KeyScope};
use crate::server::core::providers::images::ImageOperation;
use crate::server::core::upstream::cancellation;
use crate::server::core::upstream::usage::RequestTelemetry;
use crate::server::core::upstream::{ForwardOutcome, ForwardRequest};
use crate::server::errors::GatewayError;
use crate::server::{logging, ServerState};

use super::disconnect_guard::DisconnectGuard;
use super::image_output;
use super::pipeline::{self, RecordContext};

/// 这个参数名是不是生图接口认识的。
///
/// 白名单而不是黑名单：客户端（尤其是各种第三方 UI）会顺手把对话用的参数
/// （`messages` / `max_tokens` / `tools`）一起发过来，而严格的上游见到未知字段
/// 会整单 400 —— 那种错误对用户毫无解释力。在这里明确拒绝，文案能说清「这个
/// 参数不属于生图接口」。
pub(super) fn parameter_supported(field: &str) -> bool {
    matches!(
        field,
        "model"
            | "prompt"
            | "n"
            | "size"
            | "response_format"
            | "output_format"
            | "output_compression"
            | "quality"
            | "style"
            | "background"
            | "moderation"
            | "user"
            | "stream"
            | "partial_images"
    )
}

/// 入口参数校验：把不认识的、越界的、还没实现的一律挡在网关里。
///
/// 顺序有意义：**先判「能不能干」再判「参数对不对」**。`stream:true` 与
/// `partial_images` 是「本网关不支持」，不是「你参数写错了」—— 两者都给 400，
/// 但文案不同，用户看到的下一步操作也不同。
pub(super) fn validate(payload: &mut Value) -> Result<(), GatewayError> {
    if !payload.is_object() {
        return Err(GatewayError::bad_request("请求体必须是 JSON 对象"));
    }
    let prompt = payload
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|prompt| !prompt.trim().is_empty())
        .ok_or_else(|| GatewayError::bad_request("prompt 必须是非空字符串"))?;
    // 32000 字符：各家 model card 的提示词上限在 1k~4k token，32000 字符足够
    // 覆盖最宽松的那一家，同时挡住「把整本书发过来」这种明显的误用。
    if prompt.chars().count() > 32_000 {
        return Err(GatewayError::bad_request("prompt 最多 32000 个字符"));
    }
    if payload
        .get("model")
        .is_some_and(|model| !model.is_string() && !model.is_null())
    {
        return Err(GatewayError::bad_request("model 必须是字符串"));
    }
    // `n` 归一成具体数字：上游实现里 `n` 缺省与 `n:1` 的处理路径常常不同，
    // 补一个能让下游只有一种形态要处理。
    let n = match payload.get("n").filter(|n| !n.is_null()) {
        Some(n) => n
            .as_u64()
            .filter(|n| (1..=10).contains(n))
            .ok_or_else(|| GatewayError::bad_request("n 必须是 1 到 10 之间的整数"))?,
        None => 1,
    };
    payload["n"] = json!(n);
    for (field, values) in [
        ("response_format", &["b64_json", "url"][..]),
        ("output_format", &["png", "jpeg", "webp"][..]),
        ("background", &["auto", "opaque", "transparent"][..]),
        (
            "quality",
            &[
                "auto", "standard", "hd", "low", "medium", "high", "xhigh", "max",
            ][..],
        ),
        ("style", &["natural", "vivid"][..]),
        ("moderation", &["auto", "low"][..]),
    ] {
        if let Some(value) = payload.get(field).filter(|value| !value.is_null()) {
            if !value.as_str().is_some_and(|value| values.contains(&value)) {
                return Err(GatewayError::bad_request(format!(
                    "{field} 必须是 {}",
                    values.join(" 或 ")
                )));
            }
        }
    }
    if let Some(size) = payload.get("size").filter(|size| !size.is_null()) {
        let valid = size.as_str().is_some_and(|size| {
            size == "auto"
                || size
                    .split_once('x')
                    .and_then(|(width, height)| {
                        Some((width.parse::<u32>().ok()?, height.parse::<u32>().ok()?))
                    })
                    .is_some_and(|(width, height)| {
                        (1..=8192).contains(&width) && (1..=8192).contains(&height)
                    })
        });
        if !valid {
            return Err(GatewayError::bad_request(
                "size 必须是 auto 或 WIDTHxHEIGHT，边长不得超过 8192",
            ));
        }
    }
    if let Some(compression) = payload
        .get("output_compression")
        .filter(|value| !value.is_null())
    {
        if !compression.as_u64().is_some_and(|value| value <= 100) {
            return Err(GatewayError::bad_request(
                "output_compression 必须是 0 到 100 的整数",
            ));
        }
        if payload.get("output_format").and_then(Value::as_str) != Some("jpeg") {
            return Err(GatewayError::bad_request(
                "output_compression 目前仅支持 output_format=jpeg",
            ));
        }
    }
    if payload
        .get("stream")
        .is_some_and(|value| !value.is_null() && value != &Value::Bool(false))
    {
        return Err(GatewayError::bad_request(
            "当前图片生成仅支持非流式响应，请使用 stream=false",
        ));
    }
    if payload
        .get("partial_images")
        .is_some_and(|value| !value.is_null() && value.as_u64() != Some(0))
    {
        return Err(GatewayError::bad_request(
            "当前图片生成不支持 partial_images",
        ));
    }
    if payload.get("image").is_some() || payload.get("images").is_some() {
        return Err(GatewayError::bad_request(
            "参考图编辑走 POST /v1/images/edits（当前版本尚未提供），/generations 只接受纯文本生图",
        ));
    }
    // 默认给 b64_json：这是「客户端拿到就能显示」的那一种。要 url 也可以，
    // 但那时网关仍会把图片下载下来换成本机链接（见 `image_output`）。
    if payload.get("response_format").is_none_or(Value::is_null) {
        payload["response_format"] = json!("b64_json");
    }
    if let Some(field) = payload
        .as_object()
        .and_then(|object| object.keys().find(|key| !parameter_supported(key)))
    {
        return Err(GatewayError::bad_request(format!(
            "当前图片生成不支持参数 {field}"
        )));
    }
    if payload
        .get("user")
        .is_some_and(|value| !value.is_string() && !value.is_null())
    {
        return Err(GatewayError::bad_request("user 必须是字符串"));
    }
    Ok(())
}

/// 解析要生成的模型（与对话入口同一套目录语义，只是放行图片模型）。
fn resolve_model(
    state: &ServerState,
    payload: &mut Value,
    scope: Option<&KeyScope>,
) -> Result<String, GatewayError> {
    if pipeline::model_field_text(payload).trim().is_empty() {
        // 没点名模型 → 从目录里挑**第一个能生图的**（且这把 Key 允许的家）。
        // 挑不出来就给 503 并说清下一步：这不是「你参数错了」，是「网关里没有
        // 能生图的东西」—— 让人去加账号 / 换插件，而不是反复改 prompt。
        let models = crate::server::core::providers::catalog::models_response(state.store(), scope);
        let default = models
            .get("data")
            .and_then(Value::as_array)
            .and_then(|items| {
                items.iter().find(|item| {
                    if !crate::server::core::providers::images::model_is_image(item) {
                        return false;
                    }
                    let model = crate::server::core::models::model_id(item);
                    crate::server::core::providers::images::operation_providers(
                        &model,
                        ImageOperation::Generate,
                    )
                    .iter()
                    .any(|provider| key_scope::allows_provider(scope, provider))
                })
            })
            .and_then(|item| item.get("id"))
            .cloned();
        let Some(default) = default else {
            return Err(GatewayError::with_status(
                503,
                "没有可用的图片生成模型：请添加并启用支持生图的账号，并更新模型目录",
            ));
        };
        payload["model"] = default;
    }
    if let Some(model) = payload.get_mut("model") {
        if let Some(name) = model.as_str() {
            *model = json!(name.trim());
        }
    }
    let requested = pipeline::resolve_image_model(state, payload, scope)?;
    if !key_scope::allows_model(scope, &requested) {
        return Err(GatewayError::with_status(
            404,
            format!("模型 '{requested}' 不在这把网关 Key 的可用模型列表里"),
        )
        .with_code("model_not_found"));
    }
    let candidates =
        crate::server::core::providers::images::operation_providers(&requested, ImageOperation::Generate);
    if !candidates
        .iter()
        .any(|provider| key_scope::allows_provider(scope, provider))
    {
        return Err(GatewayError::bad_request(
            crate::server::core::providers::images::unsupported_message(
                &requested,
                ImageOperation::Generate,
            ),
        )
        .with_code(ImageOperation::Generate.unsupported_code()));
    }
    Ok(requested)
}

/// POST /v1/images/generations
pub async fn generations(
    State(state): State<ServerState>,
    headers: HeaderMap,
    key_scope: Option<Extension<KeyScope>>,
    body: Bytes,
) -> Response {
    let scope = key_scope.map(|Extension(scope)| scope);
    let started_at = logging::now_ms();
    let client = crate::server::core::client::classify(&pipeline::user_agent_of(&headers));
    let mut payload = match serde_json::from_slice::<Value>(&body) {
        Ok(payload) if payload.is_object() => payload,
        _ => {
            let error = GatewayError::bad_request("请求体必须是有效 JSON 对象");
            pipeline::record_early_failure(&state, started_at, "", "", &client, &error);
            return error.payload_response();
        }
    };
    let client_model = pipeline::model_field_text(&payload);
    // **先校验参数、再解析模型**：顺序与上游一致，理由是错误文案的优先级 ——
    // 「prompt 缺失」「stream 不支持」这类问题用户自己能改，而「模型没有生图
    // 通道」要去管理页折腾。先报前者，用户改完一次就通了；反过来会让人先以为
    // 是模型配错了。
    let model = match validate(&mut payload)
        .and_then(|()| resolve_model(&state, &mut payload, scope.as_ref()))
    {
        Ok(model) => model,
        Err(error) => {
            pipeline::record_early_failure(&state, started_at, &client_model, "", &client, &error);
            return error.payload_response();
        }
    };
    pipeline::write_debug_files(&body, "POST", "/v1/images/generations", &pipeline::user_agent_of(&headers));
    match execute(&state, payload, headers, scope, client, started_at, model, client_model).await {
        Ok(body) => pipeline::json_response(body),
        Err(error) => error.payload_response(),
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute(
    state: &ServerState,
    payload: Value,
    headers: HeaderMap,
    scope: Option<KeyScope>,
    client: String,
    started_at: i64,
    model: String,
    client_model: String,
) -> Result<Value, GatewayError> {
    // 请求体在调试模式下抄一份（与对话入口同一份开关，见 `capture_raw_body`）
    let raw = serde_json::to_vec(&payload)
        .map_err(|_| GatewayError::bad_request("图片请求无法序列化"))?;
    let telemetry = Arc::new(RequestTelemetry::with_id());
    let id = telemetry.id();
    state.request_stats().record_started(
        &id,
        started_at,
        &model,
        &client_model,
        // 生图没有「思考等级」这回事，下游侧留空
        "",
        &client,
        false,
    );
    if let Some(token) = cancellation::register(&id) {
        telemetry.set_cancel_token(token);
    }
    let mut guard = DisconnectGuard::new(state.request_stats(), id.clone());
    telemetry.set_live_sink(pipeline::live_row_sink(
        state.request_stats(),
        id,
        started_at,
    ));
    let raw_request = pipeline::capture_raw_body(&raw);
    let request = ForwardRequest {
        body: payload.clone(),
        // 生图恒为非流式：客户端请求里这个字段已被 `validate` 拒掉（只允许 false）
        stream: false,
        // 不去重：生图的结果是**消耗性**的，两次相同 prompt 的请求本来就该
        // 各生成一张（用户点两次「再来一张」是正常操作）。去重会把第二次请求
        // 挂到第一次的结果上，用户拿到同一张图。
        dedupe_key: String::new(),
        client_headers: headers.clone(),
        telemetry: telemetry.clone(),
        allowed_providers: scope,
        pinned_account: None,
        ignore_model_gate: false,
        proxy_override: None,
        image: None,
    };
    let outcome = state.upstream().generate_images(request).await;
    let outcome = match outcome {
        Ok(ForwardOutcome::Completion { body }) => {
            // 归一化（下载 / 转码 / 换本机链接）是 CPU+网络密集型，可能超时，
            // 也可能被用户手动终止 —— 两者都在 `normalize` 里处理。
            image_output::normalize(state, &payload, body, &headers, &telemetry).await
        }
        Ok(ForwardOutcome::Stream { .. }) => Err(GatewayError::with_status(
            502,
            "上游把图片响应当成了对话流（网关内部不一致）",
        )),
        Err(error) => Err(error),
    };
    let (status, raw_response, error) = match &outcome {
        Ok(body) => (200, pipeline::capture_raw_json(body), None),
        Err(error) => (
            i64::from(error.status_code),
            None,
            Some(error.message.clone()),
        ),
    };
    pipeline::record_entry(
        &RecordContext {
            stats: state.request_stats(),
            telemetry,
            started_at,
            model,
            client_model,
            client_reasoning: String::new(),
            status,
            raw_request,
            raw_response,
            is_test: false,
            client,
        },
        error,
    );
    guard.complete();
    outcome
}
