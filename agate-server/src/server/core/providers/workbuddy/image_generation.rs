//! WorkBuddy 文生图 / 参考图编辑：官方客户端 `ImageService` 的同一对路由。
//!
//! ```text
//!   文生图   POST {endpoint}/v2/images/generations   （Bearer accessToken，纯 JSON）
//!   参考图   POST {endpoint}/v2/images/edits
//! ```
//!
//! `endpoint` 与对话共用同一个会话端点（账号记录里的 `endpoint` 覆盖优先，
//! 兜底本地区站点）。**请求体不是 multipart** —— 参考图是 `data:` URL 放进
//! `image` 数组，与 Raccoon 的 multipart 通道完全不同。
//!
//! ── 三档字段（照官方客户端的 `buildBaseRequestBody`）─────────────
//! | 模型前缀 | 认的字段 |
//! |---|---|
//! | `hunyuan-*` | `model` / `prompt` / `size` / `n` |
//! | `gemini-*` | `model` / `prompt` / `size` |
//! | 其余 | 上面三项 + `response_format` / `n` / `quality` / `style` / `background` |
//!
//! 多出来的 OpenAI 专属字段（`quality` / `style` / `background` / `moderation`）
//! 在原生通道上**明确拒绝**，不静默丢掉：静默会让用户以为风格 / 质量生效了，
//! 而这类「看起来成功了但没生效」比一句 400 难查得多。
//!
//! ── 模型是怎么被登记进来的 ────────────────────────────────────
//! 不靠 id 白名单，靠 `/v3/config` 的 `tags`：`text-to-image` 是文生图、
//! `image-to-image` 是参考图编辑。上游换 id 比换标签频繁得多（内置兜底是
//! `hunyuan-image-v3.0` 那一代，线上实际派发的是 `hunyuan-image-alpha*`）。
//! 摘取与打标在 `core::models` 的准入过滤里做（[`decorate`] / [`pick`]），
//! 于是目录是这些条目的**唯一**来源，不会出现「某处认、某处不认」。
//!
//! ── panic=abort ─────────────────────────────────────────────
//! 本文件在转发链路上，零 unwrap / expect / panic。

use axum::http::HeaderMap;
use serde_json::{json, Value};

use crate::server::core::providers::images::{ImageOperation, ImageRequestPlan, ImageResponseKind};
use crate::server::errors::GatewayError;

use super::region::Region;

/// 上游给图像条目的分类标签（`/v3/config` 的 `models[].tags`）。
const TEXT_TO_IMAGE_TAG: &str = "text-to-image";
const IMAGE_TO_IMAGE_TAG: &str = "image-to-image";

/// 客户端未指定 `size` 时上游的默认值（官方客户端硬编码同一个）。
const DEFAULT_SIZE: &str = "1024x1024";

/// 参考图张数上限（与 `api::image_input` 的 16 张口径一致）。
const MAX_IMAGE_REFERENCES: usize = 16;

/// 这条目录条目是不是图像模型；是的话给出它支持哪几类操作。
///
/// 判据只有标签，不看 id —— 理由见模块头。两条标签都没有返回 `None`（对话模型）。
pub fn capabilities(row: &Value) -> Option<(bool, bool)> {
    let tags = row.get("tags").and_then(Value::as_array)?;
    let has = |tag: &str| tags.iter().any(|value| value.as_str() == Some(tag));
    let generation = has(TEXT_TO_IMAGE_TAG);
    let editing = has(IMAGE_TO_IMAGE_TAG);
    (generation || editing).then_some((generation, editing))
}

/// 把一条上游图像条目补成下游认的形态。
///
/// `kind=image` 让它进图像通道，两个能力位决定它出现在 `/v1/images/generations`
/// 还是 `/edits` 的可用模型里。`supportsImages`（多模态**输入**）只在编辑模型上
/// 为真 —— 文生图模型不吃图，标成能吃会让客户端往上送参考图。
pub fn decorate(row: &Value) -> Option<Value> {
    let (generation, editing) = capabilities(row)?;
    let mut item = row.clone();
    item["kind"] = json!("image");
    item["output_modalities"] = json!(["image"]);
    item["supports_image_generation"] = json!(generation);
    item["supports_image_editing"] = json!(editing);
    item["supportsToolCall"] = json!(false);
    item["supportsImages"] = json!(editing);
    item["supportsReasoning"] = json!(false);
    // 图片模型不能当默认对话模型：`isDefault` 是默认回落链与账号页筛选用到的键，
    // 让它指到一条不能对话的模型上，用户点开就是一条生图入口的 400。
    item["isDefault"] = json!(false);
    Some(item)
}

/// 从一次 `/v3/config` 的条目里摘出全部图像模型（目录准入用）。
pub fn pick(rows: &[Value]) -> Vec<Value> {
    rows.iter().filter_map(decorate).collect()
}

/// 内置兜底：`/v3/config` 一次都没拉成功时给的两条图像条目。
///
/// id 取**实测**值（2026-11 的 `/v3/config` 派发 `hunyuan-image-alpha` /
/// `hunyuan-image-alpha-edit`）。它只在「还没有远程目录」的窗口里顶班，远程
/// 目录一到就整份替换掉它 —— 与对话模型的内置清单同一个道理。
pub fn builtin() -> Vec<Value> {
    [
        ("hunyuan-image-alpha", "Hunyuan Image Alpha", TEXT_TO_IMAGE_TAG),
        (
            "hunyuan-image-alpha-edit",
            "Hunyuan Image Alpha Edit",
            IMAGE_TO_IMAGE_TAG,
        ),
    ]
    .into_iter()
    .filter_map(|(id, name, tag)| decorate(&json!({ "id": id, "name": name, "tags": [tag] })))
    .collect()
}

/// 构造上游生图请求。
pub fn build(
    account: &Value,
    body: &Value,
    region: Region,
    _client_headers: &HeaderMap,
    operation: ImageOperation,
) -> Result<ImageRequestPlan, GatewayError> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| GatewayError::bad_request("WorkBuddy 生图请求缺少 model"))?;
    let prompt = body
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .ok_or_else(|| GatewayError::bad_request("图片请求缺少 prompt"))?;
    // `auto` 是网关侧的「让上游自己定」：官方客户端从不发这个值，所以按缺省
    // 处理（上游在缺 `size` 时自己回落到 1024x1024）。
    let size = body
        .get("size")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|size| !size.is_empty() && !size.eq_ignore_ascii_case("auto"))
        .unwrap_or(DEFAULT_SIZE);
    let mut payload = json!({ "model": model, "prompt": prompt, "size": size });
    let hunyuan = model.starts_with("hunyuan-");
    let gemini = model.starts_with("gemini-");
    // 原生图片通道（hunyuan / gemini）只认 model / prompt / size（hunyuan 另认 n），
    // 与 OpenAI 兼容那一档不是同一套字段。
    if hunyuan || gemini {
        if let Some(field) = ["quality", "style", "background", "moderation"]
            .into_iter()
            .find(|field| body.get(*field).is_some_and(|value| !value.is_null()))
        {
            return Err(GatewayError::bad_request(format!(
                "WorkBuddy 该图片模型不支持 {field}，请把要求写进 prompt"
            )));
        }
    }
    if hunyuan {
        payload["n"] = json!(body.get("n").and_then(Value::as_u64).unwrap_or(1));
    } else if !gemini {
        // OpenAI 兼容档：显式要 b64，别让上游给一个会过期的签名链接
        // （`image_output` 两种都能读，但 b64 少一次下载、少一次 SSRF 面）。
        payload["response_format"] = json!("b64_json");
        if let Some(n) = body.get("n").and_then(Value::as_u64) {
            payload["n"] = json!(n);
        }
        for key in ["quality", "style", "background"] {
            if let Some(value) = body.get(key).filter(|value| !value.is_null()) {
                payload[key] = value.clone();
            }
        }
    }
    if operation == ImageOperation::Edit {
        let images = body
            .get("images")
            .and_then(Value::as_array)
            .filter(|images| !images.is_empty() && images.len() <= MAX_IMAGE_REFERENCES)
            .ok_or_else(|| {
                GatewayError::bad_request(format!(
                    "WorkBuddy 参考图编辑需要 1 至 {MAX_IMAGE_REFERENCES} 张图片"
                ))
            })?;
        // 参考图在这里已经是 `data:` URL（`api::image_input` 的唯一产物形态），
        // 这家正好认这个形态，不必再转码 / 上传。
        payload["image"] = Value::Array(
            images
                .iter()
                .map(|image| image.get("image_url").cloned().unwrap_or(Value::Null))
                .collect(),
        );
    }
    let path = if operation == ImageOperation::Edit {
        "edits"
    } else {
        "generations"
    };
    Ok(ImageRequestPlan::with_response(
        format!("{}/v2/images/{path}", super::adapter::session_endpoint(account, region)),
        super::adapter::chat_headers(account, region, Some("application/json")),
        payload,
        ImageResponseKind::Workbuddy,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_decide_and_ids_do_not() {
        let row = json!({ "id": "whatever-2027", "tags": ["text-to-image"] });
        assert_eq!(capabilities(&row), Some((true, false)));
        let row = json!({ "id": "hunyuan-image-alpha", "tags": ["image-to-image"] });
        assert_eq!(capabilities(&row), Some((false, true)));
        // 名字像图像模型但没标签 ⇒ 不是（上游的 id 命名不作数）
        assert_eq!(capabilities(&json!({ "id": "hunyuan-image-x", "tags": ["chat"] })), None);
        assert_eq!(capabilities(&json!({ "id": "x" })), None);
    }

    #[test]
    fn decorate_closes_chat_capabilities() {
        let item = decorate(&json!({ "id": "a", "tags": ["text-to-image"], "supportsToolCall": true }))
            .expect("image entry");
        assert_eq!(item["kind"], json!("image"));
        assert_eq!(item["supportsToolCall"], json!(false));
        assert_eq!(item["supports_image_editing"], json!(false));
        // 文生图不吃参考图
        assert_eq!(item["supportsImages"], json!(false));
        // 不能当默认对话模型
        assert_eq!(item["isDefault"], json!(false));
    }

    #[test]
    fn builtin_covers_both_operations() {
        let rows = builtin();
        assert!(rows.iter().any(|row| row["supports_image_generation"] == json!(true)));
        assert!(rows.iter().any(|row| row["supports_image_editing"] == json!(true)));
        assert!(rows.iter().all(|row| row["kind"] == json!("image")));
    }

    #[test]
    fn native_channels_reject_openai_only_fields() {
        let account = json!({ "endpoint": "https://example.com" });
        let body = json!({
            "model": "hunyuan-image-alpha",
            "prompt": "画一只猫",
            "style": "vivid",
        });
        let error = build(&account, &body, Region::Cn, &HeaderMap::new(), ImageOperation::Generate)
            .expect_err("style 应被拒绝");
        assert!(error.message.contains("style"), "{}", error.message);
    }

    #[test]
    fn editing_carries_references_and_uses_edits_path() {
        let account = json!({ "endpoint": "https://example.com" });
        let body = json!({
            "model": "hunyuan-image-alpha-edit",
            "prompt": "把背景换成蓝色",
            "images": [{ "image_url": "data:image/png;base64,AA" }],
        });
        let plan = build(&account, &body, Region::Cn, &HeaderMap::new(), ImageOperation::Edit)
            .expect("编辑请求");
        assert!(plan.url.ends_with("/v2/images/edits"), "{}", plan.url);
        assert_eq!(plan.body["image"][0], json!("data:image/png;base64,AA"));
        assert_eq!(plan.response, ImageResponseKind::Workbuddy);
    }

    #[test]
    fn editing_without_references_is_rejected() {
        let account = json!({ "endpoint": "https://example.com" });
        let body = json!({ "model": "hunyuan-image-alpha-edit", "prompt": "改一下" });
        let error = build(&account, &body, Region::Cn, &HeaderMap::new(), ImageOperation::Edit)
            .expect_err("缺参考图应被拒绝");
        assert!(error.message.contains("参考图"), "{}", error.message);
    }

    #[test]
    fn openai_compatible_tier_asks_for_b64() {
        let account = json!({ "endpoint": "https://example.com" });
        let body = json!({ "model": "some-image-model", "prompt": "画一只猫" });
        let plan = build(&account, &body, Region::Cn, &HeaderMap::new(), ImageOperation::Generate)
            .expect("生图请求");
        assert_eq!(plan.body["response_format"], json!("b64_json"));
        // `auto` 不发给上游：官方客户端从不发这个值
        let body = json!({ "model": "some-image-model", "prompt": "画一只猫", "size": "auto" });
        let plan = build(&account, &body, Region::Cn, &HeaderMap::new(), ImageOperation::Generate)
            .expect("生图请求");
        assert_eq!(plan.body["size"], json!(DEFAULT_SIZE));
    }
}
