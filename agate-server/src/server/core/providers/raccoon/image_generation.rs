//! 小浣熊文生图：`POST {llm_base}/images/gen`（JSON，走 OpenAI 形态的响应）。
//!
//! ── 为什么是一个**合成**模型条目 ──────────────────────────────
//! 上游目录（`GET /models`）里没有出图模型：出图是独立路由，不是模型列表里的
//! 一项。所以本站自造一个 id（[`MODEL_ID`]）挂在目录末尾 —— 与上游插件的
//! `raccoon-image` 同名同义。**不能**拿对话模型的名字去打 `images/gen`：
//! 上游对 `model` 字段有自己的校验（只认 `gpt-image-1`），拿错名字回的是
//! 一句和生图无关的 400。
//!
//! ── 参数口径 ─────────────────────────────────────────────────
//! 上游只认 `prompt` / `size`；`style` / `negative_prompt` 是**我们**折进
//! prompt 的（`Style: …` / `Avoid: …`），因为它们在这家的接口里没有字段。
//! `quality` / `background` / `moderation` 只接受 `auto` 那一档的值，其余明确
//! 拒绝 —— 静默丢掉会让用户以为设置生效了。
//!
//! `size` 要**归一**：官方出图工具要求边长 16 对齐、至少 1024² 像素，且只认
//! 那八个常见宽高比。直接把客户端的 `1024x1792` 发过去会被拒。
//!
//! ── 编辑：暂不支持（能力位为 false）───────────────────────────
//! 上游的 `/images/edits` 是 **multipart**（重复的 `image` 字段带参考图），
//! 而本仓的 `ImageRequestPlan` 目前只有 JSON 体。所以 `supports_image_editing`
//! 声明为 false —— 声明成 true 会让 `/v1/images/edits` 接受请求、然后在构造
//! 请求时失败，用户看到的是「点了没反应」而不是「这条路还没开」。

use axum::http::HeaderMap;
use serde_json::{json, Value};

use crate::server::core::providers::images::{ImageOperation, ImageRequestPlan};
use crate::server::errors::GatewayError;

/// 本站给这条合成条目用的 id（与上游插件的 `raccoon-image` 同名）。
pub const MODEL_ID: &str = "raccoon-image";

/// 上游 `images/gen` 认的模型名（它只认这一个，与我们的对外 id 无关）。
const WIRE_MODEL: &str = "gpt-image-1";

/// 默认尺寸（未指定时与 OpenAI 协议的默认值一致）。
const DEFAULT_SIZE: &str = "1024x1024";

/// 目录里那条合成条目。
///
/// `kind=image` 让它进图像通道；`isDefault=false` 让它不参与默认模型回落
/// （否则「没指定模型」的对话请求会挑中一条不能对话的条目）。
pub fn model_row() -> Value {
    json!({
        "id": MODEL_ID,
        "name": "小浣熊生图",
        "kind": "image",
        "output_modalities": ["image"],
        "supports_image_generation": true,
        // 见模块头：multipart 出站还没做，这里如实为 false
        "supports_image_editing": false,
        "supportsToolCall": false,
        "supportsImages": false,
        "supportsReasoning": false,
        "isDefault": false,
    })
}

/// 把客户端要的尺寸折成官方出图工具认的尺寸。
///
/// 规则（与官方客户端一致）：先按最接近的常见宽高比对齐到 16 的倍数，再保证
/// 总像素不低于 1024²（`1_048_576`）。不在八个常见比例里的按原比例缩放
/// 后对齐 —— 只对齐比例，不改构图。
fn normalize_size(body: &Value) -> Result<String, GatewayError> {
    let size = body
        .get("size")
        .and_then(Value::as_str)
        .filter(|value| !value.eq_ignore_ascii_case("auto"))
        .unwrap_or(DEFAULT_SIZE);
    let parsed = size
        .trim()
        .split_once('x')
        .or_else(|| size.trim().split_once('X'))
        .and_then(|(width, height)| Some((width.trim().parse::<u32>().ok()?, height.trim().parse::<u32>().ok()?)))
        .filter(|(width, height)| (1..=16_384).contains(width) && (1..=16_384).contains(height));
    let Some((width, height)) = parsed else {
        return Err(GatewayError::bad_request(
            "小浣熊生图的 size 需要正整数 WIDTHxHEIGHT，或 auto",
        ));
    };
    let ratios: [(u32, u32); 8] = [(1, 1), (3, 4), (4, 3), (16, 9), (9, 16), (3, 2), (2, 3), (21, 9)];
    for (ratio_width, ratio_height) in ratios {
        if width * ratio_height != height * ratio_width {
            continue;
        }
        // 这一比例的最小 16 对齐单位
        let unit_w = 16 / gcd(ratio_width, 16);
        let unit_h = 16 / gcd(ratio_height, 16);
        let factor = unit_w / gcd(unit_w, unit_h) * unit_h;
        let step_w = ratio_width * factor;
        let step_h = ratio_height * factor;
        let pixels = (1_048_576.0 / f64::from(step_w * step_h)).sqrt().ceil() as u32;
        let units = width
            .div_ceil(step_w)
            .max(height.div_ceil(step_h))
            .max(pixels)
            .max(1);
        return Ok(format!("{}x{}", step_w * units, step_h * units));
    }
    let scale = (1_048_576.0 / (f64::from(width) * f64::from(height))).sqrt().max(1.0);
    let aligned = |value: u32| (f64::from(value) * scale / 16.0).ceil() as u32 * 16;
    Ok(format!("{}x{}", aligned(width), aligned(height)))
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// `prompt` + 折进正文的 `style` / `negative_prompt`。
fn prompt_of(body: &Value) -> Result<String, GatewayError> {
    let prompt = body
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| GatewayError::bad_request("图片请求缺少 prompt"))?;
    let mut parts = vec![prompt.to_string()];
    for (field, label) in [("style", "Style"), ("negative_prompt", "Avoid")] {
        if let Some(text) = body
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            parts.push(format!("{label}: {text}"));
        }
    }
    Ok(parts.join("\n"))
}

/// 构造上游生图请求。
pub fn build(
    account: &Value,
    body: &Value,
    base_url: &str,
    _client_headers: &HeaderMap,
    operation: ImageOperation,
) -> Result<ImageRequestPlan, GatewayError> {
    if operation == ImageOperation::Edit {
        // 能力位已声明 false，候选链本来就不会把编辑派过来；真到了说明
        // 目录与能力位不同步，说清原因而不是拼一个注定失败的请求。
        return Err(GatewayError::bad_request(
            "小浣熊参考图编辑尚未支持（上游是 multipart 上传，本网关还没实现）",
        )
        .with_code(ImageOperation::Edit.unsupported_code()));
    }
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if !model.eq_ignore_ascii_case(MODEL_ID) {
        return Err(GatewayError::bad_request(format!(
            "小浣熊图片接口请使用 {MODEL_ID}（当前是 {model}）"
        )));
    }
    if body.get("n").and_then(Value::as_u64).unwrap_or(1) != 1 {
        return Err(GatewayError::bad_request("小浣熊图片接口单次仅支持 n=1"));
    }
    for (field, allowed) in [
        ("quality", &["auto", "standard"][..]),
        ("background", &["auto", "opaque"][..]),
        ("moderation", &["auto"][..]),
    ] {
        if let Some(value) = body.get(field).filter(|value| !value.is_null()) {
            if !value
                .as_str()
                .is_some_and(|value| allowed.contains(&value))
            {
                return Err(GatewayError::bad_request(format!(
                    "小浣熊图片接口不支持 {field}={value}"
                )));
            }
        }
    }
    let token = account
        .get("auth")
        .and_then(|auth| auth.get("accessToken"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if token.is_empty() {
        return Err(GatewayError::with_status(
            401,
            "小浣熊账号缺少 accessToken，无法生图（请重新登录）",
        ));
    }
    let mut headers: Vec<(String, String)> = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
        ("Authorization".to_string(), format!("Bearer {token}")),
    ];
    // 桌面端身份头是上游的风控基线（与对话链路同一套，见 `headers::append_identity`）。
    super::headers::append_identity(&mut headers, account)?;
    Ok(ImageRequestPlan::openai(
        format!("{base_url}/images/gen"),
        headers,
        json!({
            "model": WIRE_MODEL,
            "prompt": prompt_of(body)?,
            "size": normalize_size(body)?,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> Value {
        json!({ "auth": { "accessToken": "t" } })
    }

    #[test]
    fn size_is_aligned_and_upscaled() {
        // 已经是 1024²：原样
        assert_eq!(normalize_size(&json!({ "size": "1024x1024" })).unwrap(), "1024x1024");
        // 16:9 的常见请求值落在合法档位上
        assert_eq!(normalize_size(&json!({ "size": "1792x1024" })).unwrap(), "1792x1024");
        // 太小：抬到 1024² 以上
        let small = normalize_size(&json!({ "size": "256x256" })).unwrap();
        let (w, h) = small.split_once('x').unwrap();
        assert!(w.parse::<u32>().unwrap() >= 1024 && h.parse::<u32>().unwrap() >= 1024, "{small}");
        // auto / 缺省
        assert_eq!(normalize_size(&json!({})).unwrap(), "1024x1024");
        assert_eq!(normalize_size(&json!({ "size": "auto" })).unwrap(), "1024x1024");
        // 非比例值按比例缩放后 16 对齐
        let odd = normalize_size(&json!({ "size": "1000x700" })).unwrap();
        let (w, h) = odd.split_once('x').unwrap();
        assert_eq!(w.parse::<u32>().unwrap() % 16, 0, "{odd}");
        assert_eq!(h.parse::<u32>().unwrap() % 16, 0, "{odd}");
    }

    #[test]
    fn style_and_negative_are_folded_into_the_prompt() {
        let body = json!({
            "model": MODEL_ID,
            "prompt": "一只猫",
            "style": "黑白线条",
            "negative_prompt": "文字",
        });
        let plan = build(&account(), &body, "https://api.example.com", &HeaderMap::new(), ImageOperation::Generate)
            .unwrap();
        let prompt = plan.body["prompt"].as_str().unwrap();
        assert!(prompt.starts_with("一只猫"), "{prompt}");
        assert!(prompt.contains("Style: 黑白线条"), "{prompt}");
        assert!(prompt.contains("Avoid: 文字"), "{prompt}");
        // 上游只认它自己的模型名
        assert_eq!(plan.body["model"], json!(WIRE_MODEL));
    }

    #[test]
    fn other_models_are_rejected_with_a_hint() {
        let body = json!({ "model": "raccoon-8c4485", "prompt": "一只猫" });
        let error = build(&account(), &body, "https://api.example.com", &HeaderMap::new(), ImageOperation::Generate)
            .expect_err("对话模型不能打生图路由");
        assert!(error.message.contains(MODEL_ID), "{}", error.message);
    }

    #[test]
    fn editing_is_refused_until_multipart_exists() {
        let body = json!({ "model": MODEL_ID, "prompt": "改一下" });
        let error = build(&account(), &body, "https://api.example.com", &HeaderMap::new(), ImageOperation::Edit)
            .expect_err("编辑尚未实现");
        assert_eq!(error.code.as_deref(), Some(ImageOperation::Edit.unsupported_code()));
    }
}
