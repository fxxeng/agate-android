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
//! ── 编辑：multipart 出站（2026-11-06）─────────────────────────
//! 上游的 `/images/edits` 是 **`multipart/form-data`**：重复的 `image` 字段带
//! 参考图，`prompt` / `size` 是普通文本字段。拼装在
//! [`crate::server::core::upstream::multipart`]（本仓唯一一处），这里只负责把
//! 客户端的 data URL 拆成「字节 + MIME」。
//!
//! 参考图到这一步**一定**已经是内联 data URL：`api::image_input::prepare`
//! 在我们自己的入口就把外链下载、校验并内联了。这里仍然逐张校验一遍，理由是
//! 这条函数不只有那一个调用方 —— 「上游拿到一个它读不懂的字段」会在上游那一侧
//! 变成一句和请求内容无关的 400。

use axum::http::HeaderMap;
use base64::Engine;
use serde_json::{json, Value};

use crate::server::core::providers::images::{ImageOperation, ImageRequestPlan};
use crate::server::core::upstream::multipart::MultipartForm;
use crate::server::errors::GatewayError;

/// 本站给这条合成条目用的 id（与上游插件的 `raccoon-image` 同名）。
pub const MODEL_ID: &str = "raccoon-image";

/// 上游 `images/gen` 认的模型名（它只认这一个，与我们的对外 id 无关）。
const WIRE_MODEL: &str = "gpt-image-1";

/// 默认尺寸（未指定时与 OpenAI 协议的默认值一致）。
const DEFAULT_SIZE: &str = "1024x1024";

/// 一次编辑最多带几张参考图（与上游一致）。
const MAX_REFERENCES: usize = 16;

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
        // 参考图编辑走 `/images/edits`（multipart 出站），见模块头
        "supports_image_editing": true,
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

/// 两个入口共用的前置检查（模型名、张数、只认 `auto` 那几档的字段）。
fn validate_request(body: &Value) -> Result<(), GatewayError> {
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
    Ok(())
}

/// 鉴权与身份头。
///
/// `Content-Type` 也在这里给：生成是 `application/json`，编辑由
/// [`ImageRequestPlan::multipart`] 负责替换成带 boundary 的那一版（它的
/// boundary 与报文字节必须逐字一致，所以只能由拼装处写）。两边都先写一份
/// JSON 反而更稳：**没有任何一条路会漏掉 Content-Type**。
fn identity_headers(account: &Value) -> Result<Vec<(String, String)>, GatewayError> {
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
    Ok(headers)
}

/// 一张参考图：上游要的字节 + 它声明的类型。
fn reference_bytes(image: &Value) -> Result<(&'static str, Vec<u8>), GatewayError> {
    let value = image
        .get("image_url")
        .and_then(Value::as_str)
        .ok_or_else(|| GatewayError::bad_request("参考图缺少 image_url"))?;
    let (mime, encoded) = value
        .strip_prefix("data:")
        .and_then(|value| value.split_once(";base64,"))
        .ok_or_else(|| {
            GatewayError::bad_request("小浣熊参考图需要内联 data URL（网关入口会先下载并内联外链）")
        })?;
    // 只认这三种：上游按 MIME 决定怎么解，丢一张它读不懂的进去，回来的是一句
    // 和请求内容无关的 400。
    let mime = match mime {
        "image/png" => "image/png",
        "image/jpeg" | "image/jpg" => "image/jpeg",
        "image/webp" => "image/webp",
        _ => {
            return Err(GatewayError::bad_request(
                "小浣熊参考图仅支持 PNG、JPEG 或 WebP",
            ))
        }
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| GatewayError::bad_request("小浣熊参考图不是有效的 base64"))?;
    Ok((mime, bytes))
}

/// 构造上游生图 / 图片编辑请求。
pub fn build(
    account: &Value,
    body: &Value,
    base_url: &str,
    _client_headers: &HeaderMap,
    operation: ImageOperation,
) -> Result<ImageRequestPlan, GatewayError> {
    validate_request(body)?;
    match operation {
        ImageOperation::Generate => {
            let headers = identity_headers(account)?;
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
        ImageOperation::Edit => build_edit(account, body, base_url),
    }
}

/// 构造 `/images/edits`（multipart）。
///
/// 字段名与上游一致：`prompt` / `size` 是文本字段，参考图一律叫 `image`
/// （重复出现，顺序即客户端给的顺序）。`model` **不发** —— 上游这条路由不认它，
/// 发了反而可能被它自己那套模型校验挡下。
fn build_edit(
    account: &Value,
    body: &Value,
    base_url: &str,
) -> Result<ImageRequestPlan, GatewayError> {
    let images = body
        .get("images")
        .and_then(Value::as_array)
        .filter(|images| !images.is_empty() && images.len() <= MAX_REFERENCES)
        .ok_or_else(|| {
            GatewayError::bad_request(format!("小浣熊参考图编辑需要 1 至 {MAX_REFERENCES} 张图片"))
        })?;
    let headers = identity_headers(account)?;
    let mut form = MultipartForm::new();
    form.text("prompt", &prompt_of(body)?);
    form.text("size", &normalize_size(body)?);
    for (index, image) in images.iter().enumerate() {
        let (mime, bytes) = reference_bytes(image)?;
        let extension = match mime {
            "image/png" => "png",
            "image/webp" => "webp",
            _ => "jpg",
        };
        form.file(
            "image",
            // 文件名由我们生成：客户端给的名字既不可信（头部注入），非 ASCII 时
            // 还要按 RFC 7578 另做编码（见 multipart 模块头）。
            &format!("reference-{}.{}", index + 1, extension),
            mime,
            bytes,
        );
    }
    Ok(ImageRequestPlan::multipart(
        format!("{base_url}/images/edits"),
        headers,
        form.finish()?,
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

    /// 「家有能力位」与「目录条目声明支持」必须**同时**为真，编辑入口才会把
    /// 请求派到这家来（`providers::images::operation_providers` 取交集）。
    ///
    /// 这两处任缺一个，症状都不是「报个清楚的错」，而是入口直接回一句「模型
    /// 没有可用的图片编辑通道」—— 用户会去换模型、重配账号，而真正的原因是
    /// 我们少写了一行。这条测试就是钉住这一对。
    #[test]
    fn editing_capability_and_directory_flag_agree() {
        use crate::server::core::providers::adapter::ProviderAdapter;
        assert!(
            super::super::RACCOON_ADAPTER.supports_image_editing(),
            "适配器要声明编修通道"
        );
        assert_eq!(
            model_row()["supports_image_editing"],
            json!(true),
            "目录条目要声明这条模型能改图"
        );
    }

    /// 编辑走 multipart：报文里必须有 prompt、参考图**字节原样**，且
    /// `Content-Type` 的 boundary 与报文分隔符逐字一致（两处各写一份就迟早
    /// 不一致，症状是上游说「拿不到字段」，而报文看起来完全正常）。
    #[test]
    fn edit_builds_multipart_with_matching_boundary() {
        use base64::Engine;
        let raw: &[u8] = b"\x89PNG\r\n\x1a\nreference-bytes";
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
        let body = json!({
            "model": MODEL_ID,
            "prompt": "把背景换成蓝色",
            "size": "1024x1024",
            "images": [{ "image_url": format!("data:image/png;base64,{encoded}") }],
        });
        let plan = build(
            &account(),
            &body,
            "https://api.example.com",
            &HeaderMap::new(),
            ImageOperation::Edit,
        )
        .expect("编辑应当能构造请求");

        assert!(plan.url.ends_with("/images/edits"), "{}", plan.url);
        let form = plan.multipart.expect("编辑必须带多段表单");
        // 头里的 boundary 与报文里的分隔符是同一个
        let header = plan
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.clone())
            .expect("必须有 Content-Type");
        assert_eq!(header, form.content_type());
        assert!(
            header.starts_with("multipart/form-data; boundary="),
            "{header}"
        );
        // 参考图字节逐字节在报文里
        assert!(form
            .bytes
            .windows(raw.len())
            .any(|window| window == raw));
        // prompt 是 UTF-8 文本字段
        assert!(String::from_utf8_lossy(&form.bytes).contains("把背景换成蓝色"));
        // 闭合分隔符
        assert!(form
            .bytes
            .ends_with(format!("--{}--\r\n", form.boundary).as_bytes()));
        // 可读近似里只有文件名与字节数，没有二进制
        assert_eq!(form.preview["image"][0]["filename"], json!("reference-1.png"));
    }

    /// 参考图的三种坏形态各自有话说：少了、多了、不是 data URL。
    /// 合成一句「参数错误」会让用户不知道去改哪。
    #[test]
    fn edit_validates_reference_images() {
        let build_with = |body: Value| {
            build(
                &account(),
                &body,
                "https://api.example.com",
                &HeaderMap::new(),
                ImageOperation::Edit,
            )
        };
        let error = build_with(json!({ "model": MODEL_ID, "prompt": "改一下" }))
            .expect_err("没有参考图不能编辑");
        assert!(error.message.contains("1 至 16"), "{}", error.message);

        let too_many: Vec<Value> = (0..17)
            .map(|_| json!({ "image_url": "data:image/png;base64,AA" }))
            .collect();
        assert!(build_with(json!({
            "model": MODEL_ID,
            "prompt": "改一下",
            "images": too_many,
        }))
        .is_err());

        let error = build_with(json!({
            "model": MODEL_ID,
            "prompt": "改一下",
            "images": [{ "image_url": "https://example.com/a.png" }],
        }))
        .expect_err("外链必须由入口先内联");
        assert!(error.message.contains("data URL"), "{}", error.message);

        let error = build_with(json!({
            "model": MODEL_ID,
            "prompt": "改一下",
            "images": [{ "image_url": "data:image/gif;base64,AA" }],
        }))
        .expect_err("只收 PNG / JPEG / WebP");
        assert!(error.message.contains("PNG"), "{}", error.message);
    }
}
