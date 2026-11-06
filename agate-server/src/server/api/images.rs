//! OpenAI Images 的两个入口：`POST /v1/images/generations`（生图）与
//! `POST /v1/images/edits`（带参考图的编辑）。
//!
//! ── 两个入口共用一个 `execute` ────────────────────────────────
//! 它们只在**入参形态**上不同：生图是纯 JSON，编辑可能是 multipart（参考图是
//! 文件字段）。所以编辑多一步 [`super::image_input::parse`]，把三种上传形态收成
//! 与生图同构的 JSON，之后从校验到记账走的是同一段代码 —— 差别的全部内容就是
//! [`ImageOperation`] 这一个枚举值，它在三个地方生效：默认模型从「能生图的里挑」
//! 变成「能编辑的里挑」、能力门禁的错误码与文案、以及最终调
//! `generate_images()` 还是 `edit_images()`。
//!
//! ── 与对话入口的分工 ──────────────────────────────────────────
//! 这个 handler 只做三件事，其余全交给既有链路：
//!   ① **校验与归一**（[`validate`]）：把客户端给的参数收窄成「所有生图上游都
//!      能理解的那一小撮」，超出范围的老实报 400，而不是转给上游让它猜；
//!   ② **模型解析**（[`resolve_model`]）：走 `pipeline::resolve_image_model`
//!      —— 与 `/v1/chat/completions` 是同一套目录 / 别名 / 白名单语义，只是
//!      允许图片模型（对话入口反过来会拒绝图片模型，见那里 `image_generation_required`
//!      那条门禁）；
//!   ③ **转发与记账**：`upstream().generate_images()` / `edit_images()` 走同一条
//!      转发链（多账号轮换、退避、冷却、手动终止都在那儿），响应再经
//!      `image_output::normalize` 归一成标准的 `{created, data:[…]}`。
//!
//! ── 为什么响应还要归一（上游不已经是 OpenAI 形态了吗）─────────────
//! 不完全是。各家有三种「不是标准 OpenAI」的形态（异步任务、信封、对话流里
//! 取图），而**最麻烦的是图片来源**：有的家给 `b64_json`，有的只给一个会过期
//! 的签名 URL。把 URL 直接转给客户端有两个后果 —— 链接几分钟后就失效，以及
//! 客户端（在国内网络里）根本连不上那个 CDN。所以网关**下载并转成 b64**（或转
//! 存到本机缓存换一个我们自己的短链接），见 `image_output` 的模块头。
//!
//! ── 编辑入口的参考图从哪来 ────────────────────────────────────
//! 不在这个文件里：multipart 解析、外链下载、data URL 内联与「这真是张图片」的
//! 校验全在 [`super::image_input`]。这里只负责在 `validate` 之后调它的 `prepare`
//! —— 顺序是「参数对不对 → 模型有没有通道 → 再去下载参考图」，前三者都是几微秒
//! 的判断，下载要几秒。

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

/// 「我们认识这个字段，但没人实现」的统一说法。
///
/// 两个入口的 `validate` 与 multipart 的 `parse` 共用这一份，免得同一件事在
/// 两条路径上有两种措辞（客户端从 multipart 上来时先过 `parse`，从 JSON 上来时
/// 先过 `validate`，措辞不一致会被当成两个不同的问题）。
pub(super) fn mask_unsupported() -> GatewayError {
    GatewayError::bad_request("当前图片接口不支持 mask 或 input_fidelity，请在 prompt 中描述修改范围")
}

/// 入口参数校验：把不认识的、越界的、还没实现的一律挡在网关里。
///
/// 顺序有意义：**先判「能不能干」再判「参数对不对」**。`stream:true` 与
/// `partial_images` 是「本网关不支持」，不是「你参数写错了」—— 两者都给 400，
/// 但文案不同，用户看到的下一步操作也不同。
///
/// `operation` 只影响三处判据：参考图是必需还是禁止、`images` 算不算已知字段、
/// 以及文案里那句「图片生成 / 图片编辑」。其余约束两个入口是同一条 —— 它们背后
/// 是同一批上游接口，能接受的参数本来就一样。
pub(super) fn validate(payload: &mut Value, operation: ImageOperation) -> Result<(), GatewayError> {
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
        return Err(GatewayError::bad_request(format!(
            "当前{}仅支持非流式响应，请使用 stream=false",
            operation.label()
        )));
    }
    if payload
        .get("partial_images")
        .is_some_and(|value| !value.is_null() && value.as_u64() != Some(0))
    {
        return Err(GatewayError::bad_request(format!(
            "当前{}不支持 partial_images",
            operation.label()
        )));
    }
    let editing = operation == ImageOperation::Edit;
    // 蒙版类参数两个入口都拒，且**先于**未知参数检查：让用户看到的是「不支持
    // mask」而不是「不支持参数 mask」—— 前者说明我们认识这个字段，只是不做。
    if ["mask", "input_fidelity"]
        .iter()
        .any(|key| payload.get(*key).is_some_and(|value| !value.is_null()))
    {
        return Err(mask_unsupported());
    }
    if payload.get("image").is_some() {
        return Err(GatewayError::bad_request(if editing {
            "JSON 请求体的参考图请放在 images[].image_url；multipart 表单可直接用 image 文件字段"
        } else {
            "参考图编辑走 POST /v1/images/edits，/generations 只接受纯文本生图"
        }));
    }
    if !editing && payload.get("images").is_some() {
        return Err(GatewayError::bad_request(
            "参考图编辑走 POST /v1/images/edits（参考图放在 images[].image_url 或 multipart 的 image 字段），/generations 只接受纯文本生图",
        ));
    }
    if editing
        && !payload
            .get("images")
            .and_then(Value::as_array)
            .is_some_and(|images| !images.is_empty() && images.len() <= 16)
    {
        return Err(GatewayError::bad_request("图片编辑需要 1 到 16 张参考图"));
    }
    // 默认给 b64_json：这是「客户端拿到就能显示」的那一种。要 url 也可以，
    // 但那时网关仍会把图片下载下来换成本机链接（见 `image_output`）。
    if payload.get("response_format").is_none_or(Value::is_null) {
        payload["response_format"] = json!("b64_json");
    }
    // `images` 只在编辑入口算已知字段：生图那边它已经被上面那条明确拒过了，
    // 走到这里还能撞上它说明是别的拼写（`image_urls` 之类），仍按未知参数报。
    if let Some(field) = payload.as_object().and_then(|object| {
        object
            .keys()
            .find(|key| !parameter_supported(key) && !(editing && key.as_str() == "images"))
    }) {
        return Err(GatewayError::bad_request(format!(
            "当前{}不支持参数 {field}",
            operation.label()
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

/// 解析要使用的模型（与对话入口同一套目录语义，只是放行图片模型）。
fn resolve_model(
    state: &ServerState,
    payload: &mut Value,
    scope: Option<&KeyScope>,
    operation: ImageOperation,
) -> Result<String, GatewayError> {
    if pipeline::model_field_text(payload).trim().is_empty() {
        // 没点名模型 → 从目录里挑**第一个能做这件操作的**（且这把 Key 允许的家）。
        // 挑不出来就给 503 并说清下一步：这不是「你参数错了」，是「网关里没有
        // 能生图 / 能编辑的东西」—— 让人去加账号 / 换插件，而不是反复改 prompt。
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
                    crate::server::core::providers::images::operation_providers(&model, operation)
                        .iter()
                        .any(|provider| key_scope::allows_provider(scope, provider))
                })
            })
            .and_then(|item| item.get("id"))
            .cloned();
        let Some(default) = default else {
            return Err(GatewayError::with_status(
                503,
                format!(
                    "没有可用的{}模型：请添加并启用支持该能力的账号，并更新模型目录",
                    operation.label()
                ),
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
    let candidates = crate::server::core::providers::images::operation_providers(&requested, operation);
    if !candidates
        .iter()
        .any(|provider| key_scope::allows_provider(scope, provider))
    {
        return Err(GatewayError::bad_request(
            crate::server::core::providers::images::unsupported_message(&requested, operation),
        )
        .with_code(operation.unsupported_code()));
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
    let ua = pipeline::user_agent_of(&headers);
    let client = crate::server::core::client::classify(&ua);
    let payload = match serde_json::from_slice::<Value>(&body) {
        Ok(payload) if payload.is_object() => payload,
        _ => {
            let error = GatewayError::bad_request("请求体必须是有效 JSON 对象");
            pipeline::record_early_failure(&state, started_at, "", "", &client, &error);
            return error.payload_response();
        }
    };
    // 调试落盘放在校验**之前**：校验失败的请求恰恰是最需要看原文的那一类。
    pipeline::write_debug_files(&body, "POST", ImageOperation::Generate.path(), &ua);
    handle(
        state,
        payload,
        ImageOperation::Generate,
        headers,
        scope,
        client,
        started_at,
    )
    .await
}

/// POST /v1/images/edits
///
/// 与 [`generations`] 的**全部**差异在 [`super::image_input::parse`] 这一行：
/// 它把 multipart / JSON 两种形态收成同构 JSON（参考图内联成 data URL）。之后
/// 走到的是同一个 [`handle`]。
pub async fn edits(
    State(state): State<ServerState>,
    headers: HeaderMap,
    key_scope: Option<Extension<KeyScope>>,
    request: axum::extract::Request,
) -> Response {
    let scope = key_scope.map(|Extension(scope)| scope);
    let started_at = logging::now_ms();
    let client = crate::server::core::client::classify(&pipeline::user_agent_of(&headers));
    let payload = match super::image_input::parse(request, &state).await {
        Ok(payload) => payload,
        Err(error) => {
            pipeline::record_early_failure(&state, started_at, "", "", &client, &error);
            return error.payload_response();
        }
    };
    handle(
        state,
        payload,
        ImageOperation::Edit,
        headers,
        scope,
        client,
        started_at,
    )
    .await
}

/// 两个入口共用的一段：校验 → 解析模型 → （编辑时）准备参考图 → 转发 → 记账。
async fn handle(
    state: ServerState,
    mut payload: Value,
    operation: ImageOperation,
    headers: HeaderMap,
    scope: Option<KeyScope>,
    client: String,
    started_at: i64,
) -> Response {
    let client_model = pipeline::model_field_text(&payload);
    // **先校验参数、再解析模型、最后才去碰参考图**：三者的耗时差三个数量级
    // （微秒 / 毫秒 / 秒级网络）。顺序反过来的症状是「prompt 写错了也要先等几秒
    // 下载」。错误文案的优先级也是这个方向 —— 「prompt 缺失」「stream 不支持」
    // 用户自己能改，而「模型没有生图通道」要去管理页折腾；先报前者，改完一次
    // 就通了，反过来会让人先以为模型配错了。
    let resolved = validate(&mut payload, operation)
        .and_then(|()| resolve_model(&state, &mut payload, scope.as_ref(), operation));
    let model = match resolved {
        Ok(model) => model,
        Err(error) => {
            pipeline::record_early_failure(&state, started_at, &client_model, "", &client, &error);
            return error.payload_response();
        }
    };
    if operation == ImageOperation::Edit {
        if let Err(error) = super::image_input::prepare(&mut payload).await {
            pipeline::record_early_failure(&state, started_at, &client_model, "", &client, &error);
            return error.payload_response();
        }
    }
    match execute(
        &state,
        payload,
        operation,
        headers,
        scope,
        client,
        started_at,
        model,
        client_model,
    )
    .await
    {
        Ok(body) => pipeline::json_response(body),
        Err(error) => error.payload_response(),
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute(
    state: &ServerState,
    payload: Value,
    operation: ImageOperation,
    headers: HeaderMap,
    scope: Option<KeyScope>,
    client: String,
    started_at: i64,
    model: String,
    client_model: String,
) -> Result<Value, GatewayError> {
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
    // 请求体在调试模式下抄一份（与对话入口同一份开关）。
    // **只在调试模式开着时序列化**：编辑请求的 body 里是内联的参考图，可达十几
    // MiB —— 为一个注定是 `None` 的结果先整份串行化一遍，是纯浪费。
    let raw_request = if crate::server::core::debug_traffic::enabled() {
        serde_json::to_vec(&payload)
            .ok()
            .and_then(|raw| pipeline::capture_raw_body(&raw))
    } else {
        None
    };
    let request = ForwardRequest {
        body: payload.clone(),
        // 生图 / 编辑恒为非流式：客户端请求里这个字段已被 `validate` 拒掉（只允许 false）
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
    let outcome = match operation {
        ImageOperation::Generate => state.upstream().generate_images(request).await,
        ImageOperation::Edit => state.upstream().edit_images(request).await,
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 生图入口必须明确拒绝参考图：两个入口各管一件事，混着来会让「为什么
    /// 我的参考图没生效」变成一个要靠猜的问题。
    #[test]
    fn generation_rejects_reference_images() {
        let mut body = json!({ "prompt": "画一只猫", "images": [{ "image_url": "data:image/png;base64,AA" }] });
        let error = validate(&mut body, ImageOperation::Generate).unwrap_err();
        assert!(error.message.contains("/v1/images/edits"), "{}", error.message);

        let mut body = json!({ "prompt": "画一只猫", "image": "data:image/png;base64,AA" });
        assert!(validate(&mut body, ImageOperation::Generate).is_err());
    }

    /// 编辑入口没有参考图就不是编辑 —— 让它过去只会收到一句上游的 400。
    #[test]
    fn editing_requires_reference_images() {
        let mut body = json!({ "prompt": "把背景换成蓝色" });
        let error = validate(&mut body, ImageOperation::Edit).unwrap_err();
        assert!(error.message.contains("参考图"), "{}", error.message);

        let mut body = json!({ "prompt": "把背景换成蓝色", "images": [] });
        assert!(validate(&mut body, ImageOperation::Edit).is_err());

        let mut body = json!({
            "prompt": "把背景换成蓝色",
            "images": [{ "image_url": "data:image/png;base64,AA" }],
        });
        assert!(validate(&mut body, ImageOperation::Edit).is_ok());
    }

    /// `images` 只在编辑入口算已知字段；生图那边是「走错门」而不是「不认识的
    /// 参数」。两条报错文案不同，用户看到的下一步也不同。
    #[test]
    fn unknown_field_message_is_operation_aware() {
        let mut body = json!({ "prompt": "画一只猫", "image_urls": ["x"] });
        let error = validate(&mut body, ImageOperation::Generate).unwrap_err();
        assert!(error.message.contains("图片生成"), "{}", error.message);

        let mut body = json!({
            "prompt": "把背景换成蓝色",
            "images": [{ "image_url": "data:image/png;base64,AA" }],
            "image_urls": ["x"],
        });
        let error = validate(&mut body, ImageOperation::Edit).unwrap_err();
        assert!(error.message.contains("图片编辑"), "{}", error.message);
    }

    /// 蒙版类参数在**两个**入口都拒，且报「不支持 mask」而不是「不支持参数
    /// mask」—— 前者说明我们认识它，只是不做。
    #[test]
    fn mask_is_rejected_with_an_honest_message() {
        for operation in [ImageOperation::Generate, ImageOperation::Edit] {
            let mut body = json!({ "prompt": "画一只猫", "mask": "data:image/png;base64,AA" });
            if operation == ImageOperation::Edit {
                body["images"] = json!([{ "image_url": "data:image/png;base64,AA" }]);
            }
            let error = validate(&mut body, operation).unwrap_err();
            assert!(error.message.contains("mask"), "{}", error.message);
            assert!(error.message.contains("prompt 中描述"), "{}", error.message);
        }
    }
}
