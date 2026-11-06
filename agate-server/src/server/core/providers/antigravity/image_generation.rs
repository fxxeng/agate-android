//! Antigravity 生图 / 图片编辑：`POST {base}:streamGenerateContent?alt=sse`，
//! 响应是 **Gemini SSE 帧**（图在 `candidates[].content.parts[].inlineData`）。
//!
//! ── 与对话链路的共同点比差别多 ────────────────────────────────
//! 请求信封**就是** `protocol::antigravity_outbound::antigravity_request_from_chat`
//! 那一份 —— 它已经认 `image`（`is_image_model(model)` → 不带 system / tools、
//! 固定采样档、`requestType: "image_gen"`）。所以这一家要额外做的只有两件事：
//!   1. 往 `generationConfig` 补 `responseModalities: ["TEXT","IMAGE"]` 与
//!      `imageConfig`（宽高比 + 1K/2K/4K 档）；
//!   2. 把生图入口的 `prompt` / `images` 摆成**一条 user 消息**再交给那个转换器
//!      （它读的是 chat 形态）。
//!
//! ── 基址用 sandbox 而不是对话用的 daily ────────────────────────
//! 上游插件的生图固定打 sandbox（它的对话走 daily）。这不是笔误：两套参考里
//! 生图那条路径都指向 sandbox，而本仓的对话链路选 daily 是为了「两参考交集里
//! 被长期使用、且最不容易 429」那条。**生图跟插件走**，避免「按自己的偏好在
//! 一个没验证过的端点上试」。
//!
//! ── 尺寸：Gemini 不保证精确像素 ───────────────────────────────
//! 它按**原生宽高比** + 1K/2K/4K 档出图，所以 `size` 要折成
//! `{aspectRatio, imageSize}`，且比例不在十个常见档里时**明确拒绝**而不是
//! 悄悄吸附到最近的档 —— 后者会让用户拿到与预期构图不同的图，却看不出为什么。

use serde_json::{json, Value};

use crate::server::core::providers::images::{ImageOperation, ImageRequestPlan, ImageResponseKind};
use crate::server::core::protocol::antigravity_outbound;
use crate::server::errors::GatewayError;

/// 图片模型判据：`gemini-*` 且名字里带 `image`。
///
/// 与 `protocol::antigravity_outbound::is_image_model`（只认 `image` 子串）
/// 口径一致但不完全等价 —— 那个函数在信封内部决定「走生图桶」，它多认一条
/// 只会让一个奇怪的名字悄悄走生图；这里只用于**本家的**入口判定，宁可窄。
pub fn is_image_model(id: &str) -> bool {
    let lowered = id.trim().to_ascii_lowercase();
    lowered.starts_with("gemini-") && lowered.contains("image")
}

/// 把 `size` 折成 Gemini 的 `{aspectRatio, imageSize}`。
///
/// 十个常见比例（与官方客户端同一份表）。**不在表里就拒绝**：Gemini 只按比例
/// 与档位出图，吸附到最近的档意味着用户拿到的构图和他要的不一样。
fn image_config(body: &Value) -> Result<Value, GatewayError> {
    let size = body
        .get("size")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("auto");
    if size.is_empty() || size.eq_ignore_ascii_case("auto") {
        // 编辑（带参考图）不指定比例：让上游按原图走。生成给一个方图默认值。
        return Ok(if body.get("images").is_some() {
            json!({ "imageSize": "1K" })
        } else {
            json!({ "aspectRatio": "1:1", "imageSize": "1K" })
        });
    }
    let (width, height) = size
        .split_once(['x', 'X'])
        .and_then(|(width, height)| {
            Some((
                width.trim().parse::<u32>().ok()?,
                height.trim().parse::<u32>().ok()?,
            ))
        })
        .filter(|(width, height)| *width > 0 && *height > 0)
        .ok_or_else(|| GatewayError::bad_request("Antigravity size 必须是 auto 或 WIDTHxHEIGHT"))?;
    let ratio = f64::from(width) / f64::from(height);
    let table: [(&str, f64); 10] = [
        ("1:1", 1.0),
        ("2:3", 2.0 / 3.0),
        ("3:2", 1.5),
        ("3:4", 0.75),
        ("4:3", 4.0 / 3.0),
        ("4:5", 0.8),
        ("5:4", 1.25),
        ("9:16", 9.0 / 16.0),
        ("16:9", 16.0 / 9.0),
        ("21:9", 21.0 / 9.0),
    ];
    // 4% 容差：客户端常见的 1024x1792 / 1792x1024 与 9:16 / 16:9 有千分之一级
    // 的偏差，卡死精确相等会让「明明是最常见的尺寸」被判成不支持。
    let aspect = table
        .iter()
        .min_by(|left, right| (ratio - left.1).abs().total_cmp(&(ratio - right.1).abs()))
        .filter(|(_, candidate)| (ratio - candidate).abs() / candidate <= 0.04)
        .map(|(name, _)| *name)
        .ok_or_else(|| {
            GatewayError::bad_request(
                "Antigravity 不支持该宽高比，请使用 1:1、2:3、3:2、3:4、4:3、4:5、5:4、\
                 9:16、16:9 或 21:9",
            )
        })?;
    // 档位按长边：≤1792 → 1K，≤3072 → 2K，≤4096 → 4K。超过就拒绝 ——
    // Gemini 没有 8K 档，发过去只会得到一句与尺寸无关的上游错误。
    let edge = width.max(height);
    let resolution = match edge {
        1..=1792 => "1K",
        1793..=3072 => "2K",
        3073..=4096 => "4K",
        _ => {
            return Err(GatewayError::bad_request(
                "Antigravity 图片尺寸最大为 4K 档（长边 4096）",
            ))
        }
    };
    Ok(json!({ "aspectRatio": aspect, "imageSize": resolution }))
}

/// 单张参考图的上限：`gemini-2.5-*` 是三张，其余是十四张（上游插件的判据）。
fn reference_limit(wire_model: &str) -> usize {
    if wire_model.starts_with("gemini-2.5-") {
        3
    } else {
        14
    }
}

/// 把生图入口的字段摆成一条 user 消息（`antigravity_request_from_chat` 读的是
/// chat 形态：`messages[].content` 里可以有 `{type:"image_url"}` 的 part）。
fn as_chat(body: &Value, wire_model: &str) -> Value {
    let mut content = vec![json!({
        "type": "text",
        "text": body.get("prompt").cloned().unwrap_or(Value::Null),
    })];
    if let Some(images) = body.get("images").and_then(Value::as_array) {
        content.extend(images.iter().map(|image| {
            json!({
                "type": "image_url",
                "image_url": { "url": image.get("image_url") },
            })
        }));
    }
    json!({
        "model": wire_model,
        "messages": [{ "role": "user", "content": content }],
    })
}

/// 构造上游生图 / 编辑请求。
pub fn build(
    account: &Value,
    body: &Value,
    operation: ImageOperation,
    wire_model: &str,
) -> Result<ImageRequestPlan, GatewayError> {
    if !is_image_model(wire_model) {
        return Err(GatewayError::bad_request(format!(
            "Antigravity 生图请使用 gemini-*-image 模型（当前是 {wire_model}）"
        )));
    }
    if body.get("n").and_then(Value::as_u64).unwrap_or(1) != 1 {
        return Err(GatewayError::bad_request("Antigravity 单次生图仅支持 n=1"));
    }
    // 三档只认 `auto`（本家没有这些旋钮，分辨率统一由 size 表达）。收下再丢掉
    // 会让用户以为设置生效了。
    for (field, allowed) in [
        ("quality", &["auto", "standard"][..]),
        ("background", &["auto", "opaque"][..]),
        ("moderation", &["auto"][..]),
    ] {
        if let Some(value) = body.get(field).filter(|value| !value.is_null()) {
            if !value.as_str().is_some_and(|value| allowed.contains(&value)) {
                return Err(GatewayError::bad_request(format!(
                    "Antigravity 不支持 {field}={value}，分辨率请通过 size 指定"
                )));
            }
        }
    }
    if body.get("style").is_some_and(|value| !value.is_null()) {
        return Err(GatewayError::bad_request(
            "Antigravity 请在 prompt 中描述风格，不支持 style 参数",
        ));
    }
    if operation == ImageOperation::Edit {
        let images = body
            .get("images")
            .and_then(Value::as_array)
            .filter(|images| !images.is_empty())
            .ok_or_else(|| GatewayError::bad_request("图片编辑需要至少一张参考图"))?;
        let limit = reference_limit(wire_model);
        if images.len() > limit {
            return Err(GatewayError::bad_request(format!(
                "该 Antigravity 模型最多支持 {limit} 张参考图"
            )));
        }
        // 参考图必须已内联：`antigravity_request_from_chat` 把 `image_url`
        // 原样搬进 `inlineData`，外链进去就是一段上游读不懂的东西。
        if images.iter().any(|image| {
            !image
                .get("image_url")
                .and_then(Value::as_str)
                .is_some_and(|url| url.starts_with("data:image/"))
        }) {
            return Err(GatewayError::bad_request(
                "Antigravity 图片编辑需要内联的参考图（网关入口会先下载并内联外链）",
            ));
        }
    }
    let access_token = account
        .pointer("/auth/accessToken")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if access_token.is_empty() {
        return Err(GatewayError::with_status(
            401,
            "Antigravity 账号缺少 access token，无法生图（请重新登录或等令牌自动刷新）",
        ));
    }
    let project = account
        .get("projectId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let email = account
        .get("email")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let enterprise = !email.is_empty()
        && !email.ends_with("@gmail.com")
        && !email.ends_with("@googlemail.com");
    let config = image_config(body)?;
    let chat = as_chat(body, wire_model);
    let mut envelope =
        antigravity_outbound::antigravity_request_from_chat(&chat, wire_model, project, enterprise)
            .map_err(|message| {
                GatewayError::bad_request(format!("Antigravity 生图请求转换失败：{message}"))
            })?;
    // 两处生图专属字段。`responseModalities` 不带 IMAGE 的话上游只回文字；
    // `imageConfig` 决定比例与档位（Gemini 不保证精确像素，见模块头）。
    envelope["request"]["generationConfig"]["responseModalities"] = json!(["TEXT", "IMAGE"]);
    envelope["request"]["generationConfig"]["imageConfig"] = config;
    Ok(ImageRequestPlan::with_response(
        super::endpoints::stream_generate_url(super::endpoints::V1_BASE_URL_SANDBOX),
        super::endpoints::content_headers(access_token),
        envelope,
        ImageResponseKind::Gemini,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> Value {
        json!({
            "auth": { "accessToken": "t" },
            "projectId": "proj-1",
            "email": "user@gmail.com",
        })
    }

    fn build_generate(body: Value) -> Result<ImageRequestPlan, GatewayError> {
        build(&account(), &body, ImageOperation::Generate, "gemini-3-pro-image")
    }

    #[test]
    fn image_models_are_recognized_without_matching_text_models() {
        assert!(is_image_model("gemini-3-pro-image"));
        assert!(is_image_model("gemini-3.1-flash-image"));
        assert!(!is_image_model("gemini-3-flash"));
        // 「image」出现在别处也不算：这个判据只认 gemini-* 且带 image 的
        assert!(!is_image_model("imagen-3"));
    }

    /// 比例必须**精确落在**十个档位附近，且长边决定 1K/2K/4K。
    #[test]
    fn size_folds_into_aspect_and_resolution() {
        let config = |size: &str| image_config(&json!({ "size": size })).unwrap();
        assert_eq!(config("1024x1024")["aspectRatio"], json!("1:1"));
        assert_eq!(config("1024x1024")["imageSize"], json!("1K"));
        // 16:9 与 9:16 是客户端最常见的两档
        assert_eq!(config("1792x1024")["aspectRatio"], json!("16:9"));
        assert_eq!(config("1024x1792")["aspectRatio"], json!("9:16"));
        // 长边 2048 → 2K，4096 → 4K
        assert_eq!(config("2048x2048")["imageSize"], json!("2K"));
        assert_eq!(config("4096x4096")["imageSize"], json!("4K"));
        // auto：生成给方图，编辑不指定比例（让上游按原图走）
        assert_eq!(config("auto")["aspectRatio"], json!("1:1"));
        let editing = image_config(&json!({ "images": [{ "image_url": "data:image/png;base64,AA" }] }))
            .unwrap();
        assert!(editing.get("aspectRatio").is_none(), "{editing}");
        assert_eq!(editing["imageSize"], json!("1K"));
    }

    /// 不在十个档位里的比例要**明确拒绝**：吸附到最近的档会让用户拿到与预期
    /// 构图不同的图，却看不出为什么。
    ///
    /// 1000x999 是**故意留的对照**：它是 1:1 的千分之一偏差，客户端常见尺寸
    /// （1024x1792 对 9:16）就是这一量级，必须放行 —— 判据是「4% 以内算这个档」，
    /// 不是精确相等。
    #[test]
    fn unsupported_ratio_and_oversize_are_refused() {
        assert!(image_config(&json!({ "size": "1000x999" })).is_ok());
        // 5:3 不在表里，离 16:9 与 3:2 都超过 4%
        assert!(image_config(&json!({ "size": "1000x600" })).is_err());
        assert!(image_config(&json!({ "size": "5000x5000" })).is_err());
        assert!(image_config(&json!({ "size": "abc" })).is_err());
    }

    /// 信封的三件生图专属事：`responseModalities` 带 IMAGE、`imageConfig`
    /// 落地、`requestType=image_gen`（后者由共享转换器给，但它是「走生图桶」
    /// 的唯一标记，不能因为某次重构丢了）。
    #[test]
    fn envelope_carries_image_generation_markers() {
        let plan = build_generate(json!({ "model": "gemini-3-pro-image", "prompt": "画一只猫" }))
            .expect("应当能构造");
        assert_eq!(plan.response, ImageResponseKind::Gemini);
        let config = &plan.body["request"]["generationConfig"];
        assert_eq!(config["responseModalities"], json!(["TEXT", "IMAGE"]));
        assert_eq!(config["imageConfig"]["aspectRatio"], json!("1:1"));
        assert_eq!(plan.body["requestType"], json!("image_gen"));
        assert_eq!(plan.body["model"], json!("gemini-3-pro-image"));
        // 生图桶不带 system / tools（共享转换器已经这么处理，这里钉住）
        let request = plan.body["request"].as_object().unwrap();
        assert!(!request.contains_key("systemInstruction"));
        assert!(!request.contains_key("tools"));
        // prompt 要落在 user 消息里
        assert!(plan.body["request"]["contents"]
            .to_string()
            .contains("画一只猫"));
    }

    /// 参考图必须内联，且张数上限随型号不同（2.5 是三张）。
    #[test]
    fn editing_requires_inline_references_within_limit() {
        let one = json!([{ "image_url": "data:image/png;base64,AA" }]);
        let build_edit = |model: &str, images: Value| {
            build(
                &account(),
                &json!({ "model": model, "prompt": "改一下", "images": images }),
                ImageOperation::Edit,
                model,
            )
        };
        assert!(build_edit("gemini-3-pro-image", one.clone()).is_ok());

        let error = build_edit("gemini-3-pro-image", json!([])).expect_err("不能没有参考图");
        assert!(error.message.contains("至少一张"), "{}", error.message);

        let error = build_edit(
            "gemini-3-pro-image",
            json!([{ "image_url": "https://example.com/a.png" }]),
        )
        .expect_err("外链不在本家能力内");
        assert!(error.message.contains("内联"), "{}", error.message);

        // 2.5 系只有三张
        let four: Vec<Value> = (0..4).map(|_| json!({ "image_url": "data:image/png;base64,AA" })).collect();
        let error = build_edit("gemini-2.5-flash-image", json!(four)).expect_err("超过上限");
        assert!(error.message.contains("3 张"), "{}", error.message);
    }

    /// 对话模型不能打生图入口：gemini-3-flash 走这条路只会得到一句与生图无关
    /// 的上游错误。
    #[test]
    fn chat_models_are_rejected_with_a_hint() {
        let error = build(
            &account(),
            &json!({ "model": "gemini-3-flash", "prompt": "画一只猫" }),
            ImageOperation::Generate,
            "gemini-3-flash",
        )
        .expect_err("对话模型不能生图");
        assert!(error.message.contains("gemini-*-image"), "{}", error.message);
    }
}