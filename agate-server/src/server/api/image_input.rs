//! 图片编辑的入参归一：把 multipart 上传 / 外链 / data URL 三种形态收成一种。
//!
//! ── 为什么要归一 ──────────────────────────────────────────────
//! `/v1/images/edits` 的标准形态是 multipart（`image` 是文件字段）。但客户端
//! 实际会发三种：OpenAI 官方 SDK 发 multipart；一些前端把参考图读成 data URL
//! 塞进 JSON；还有的把上一次生图的结果链接（我们自己的
//! `/v1/images/files/<token>`，或上游的签名 URL）直接回传。**下游不需要知道这
//! 三种的区别** —— 全部收成 `images: [{"image_url": "data:image/png;base64,…"}]`，
//! 之后 `validate` → `resolve_model` → 转发 → 归一化整条链与生图完全共用，只有
//! 入口这一层分叉（见 `api::images` 模块头）。
//!
//! ── 为什么参考图必须内联成 data URL，而不是把链接转给上游 ──────────
//! 两条都成立才这么做：
//!   ① **上游吃不掉我们的链接**。`/v1/images/files/<token>` 是本机内存缓存里的
//!      令牌，上游访问不到（同机也不该让它访问）；把上游 A 给的签名 URL 转给上游
//!      B 更没意义 —— 签名的对象是「某一家 CDN」，换家即失效；
//!   ② **客户端给的链接是 SSRF 入口**。让网关去 GET 一个用户指定的 URL，等于把
//!      内网探测能力借出去。
//! 所以链接由网关自己下载（走 `image_output::download` —— 已经是「逐地址判公网
//! + DNS 钉死 + 逐跳跟转」的那一个），**并校验它真是一张图片**，再内联。下载器
//! 只保证「连的是公网」，不保证「拿回来的是图片」；后半句由这里的 `inspect_bytes`
//! 补上（解码一次，顺带把边长与解压炸弹挡掉）。
//!
//! ── mask / input_fidelity 为什么直接拒 ─────────────────────────
//! 已接的两家都不支持（ModelScope 的 `build` 会拒 `mask`；Loomy 的实现里没有蒙版
//! 概念）。收下再悄悄丢掉，用户会得到一张「蒙版没生效」的图 —— 比明说「不支持」
//! 糟得多：前者是错误的结果，后者是一句能照着改的话。
//!
//! ── 与上游实现的差异（刻意）──────────────────────────────────
//! 上游对 JSON 请求体里的 `image` 字段是「拒掉并指去 `images[].image_url`」，本
//! 文件保持一致：multipart 的 `image` 字段照收（那是 OpenAI SDK 的形态），JSON
//! 里只认 `images[].image_url`。少一种形态就少一处「两边都以为自己是对的」。

use std::io::Cursor;
use std::time::Duration;

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::{FromRequest, Multipart, Request};
use axum::http::request::Parts;
use base64::Engine;
use serde_json::{json, Map, Value};

use crate::server::core::providers::images::ImageOperation;
use crate::server::errors::GatewayError;

/// 整个编辑请求（表单 + 参考图）的上限。取 16 MiB：四张 3 MiB 的手机照片
/// 加上表单文本还留有余量，同时挡住「把整个相册发过来」这种误用。
const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
/// 单张参考图上限。与 `image_output` 的图片上限同一量级（那边是上游回图）。
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// 内联后（base64 会膨胀 4/3）整个请求体的上限。上游对请求体的容忍度普遍在
/// 20~25 MiB，14 MiB 内联后加表单字段仍在安全区内。
const MAX_INLINE_BYTES: usize = 14 * 1024 * 1024;
/// 参考图张数上限。各家真实上限不一（ModelScope 3 张，Loomy 更多），**这里只挡
/// 明显误用**，具体限制交给 `build_image_request` —— 它知道自己在跟哪一家说话，
/// 报错能指名道姓（「ModelScope 参考图编辑需要 1 至 3 张图片」）。
const MAX_IMAGES: usize = 16;

fn invalid(message: impl Into<String>) -> GatewayError {
    GatewayError::bad_request(message).with_code("invalid_image_input")
}

/// 这个表单字段名是不是一张参考图。
///
/// 认四种写法：`image`（OpenAI SDK）、`images`、带 `[]` 的数组写法、以及
/// `image[0]` 这种带下标的写法（PHP / Rails 风格的表单序列化）。不认的话它们会
/// 落到「未知参数」分支 —— 而客户端看到的会是一句完全对不上的抱怨。
fn image_field(name: &str) -> bool {
    matches!(name, "image" | "image[]" | "images" | "images[]")
        || name
            .strip_prefix("image[")
            .and_then(|name| name.strip_suffix(']'))
            .is_some_and(|index| {
                !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
            })
}

/// 入口：读完整个请求体（有上限），落一份调试快照，再交给 [`decode`]。
///
/// **不需要 `ServerState`**：`Multipart` 的提取器只读 body，签名里那个 `S` 一次
/// 也不碰。所以这里传 `&()` 而不是把手上的 `ServerState` 递进来 —— 参数列表上
/// 就没有「这里可能会查配置」的错觉。
pub(super) async fn parse(request: Request) -> Result<Value, GatewayError> {
    let (parts, body) = request.into_parts();
    // 先整包读进内存再判 content-type：两条路（JSON / multipart）都要有界，
    // 分别限流会变成两份「大小上限」需要同步维护。
    let bytes = to_bytes(body, MAX_REQUEST_BYTES)
        .await
        .map_err(|_| GatewayError::with_status(413, "图片编辑请求总大小不得超过 16 MiB"))?;
    // 与其它入口同一条约定（`{config_dir}/debug/last-request.json` 记最近一次请求
    // 原文，见 `pipeline::write_debug_files` 的调用点们）。这里刻意写**客户端发来
    // 的原始字节**而不是归一后的 JSON：debug 文件的用处就是回答「客户端到底发了
    // 什么形态」，而归一后的 body 会把参考图摊成 base64，反而看不出来。
    super::pipeline::write_debug_files(
        &bytes,
        "POST",
        ImageOperation::Edit.path(),
        &super::pipeline::user_agent_of(&parts.headers),
    );
    decode(parts, bytes).await
}

/// 纯解码：已读完的字节 + 原请求头 → 同构 JSON。
///
/// 与 [`parse`] 的唯一差别是那句调试落盘 —— 所以单测打这里，不碰磁盘也不读配置。
async fn decode(parts: Parts, bytes: Bytes) -> Result<Value, GatewayError> {
    let content_type = parts
        .headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if content_type.starts_with("application/json") {
        return serde_json::from_slice(&bytes)
            .map_err(|_| invalid("图片编辑请求必须是有效 JSON 对象"));
    }
    if !content_type.starts_with("multipart/form-data") {
        return Err(invalid(
            "图片编辑请使用 multipart/form-data（参考图字段名 image），或 JSON images[].image_url",
        ));
    }
    // 把已读出的字节重新包回 Body：`Multipart` 提取器要的是完整请求，而我们
    // 刚刚已经把它的 body 取空了。boundary 在 content-type 里，parts 原样带过去。
    let request = Request::from_parts(parts, Body::from(bytes));
    let mut multipart = Multipart::from_request(request, &())
        .await
        .map_err(|_| invalid("无法解析 multipart/form-data，请检查 boundary"))?;
    let mut payload = Map::new();
    let mut images = Vec::new();
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| invalid("图片上传表单不完整"))?
    {
        let name = field.name().unwrap_or("").to_string();
        if image_field(&name) {
            if images.len() >= MAX_IMAGES {
                return Err(invalid(format!(
                    "图片编辑最多支持 {MAX_IMAGES} 张参考图"
                )));
            }
            // 逐块累加而不是 `field.bytes()`：后者会把整个字段一次读进内存，
            // 上限只能事后检查 —— 一个 500 MiB 的字段就能把进程撑死。
            let mut data = Vec::new();
            while let Some(chunk) = field.chunk().await.map_err(|_| invalid("参考图上传中断"))? {
                if data.len().saturating_add(chunk.len()) > MAX_IMAGE_BYTES {
                    return Err(GatewayError::with_status(413, "单张参考图不得超过 10 MiB"));
                }
                data.extend_from_slice(&chunk);
            }
            let mime = mime_of(&data)?;
            images.push(json!({
                "image_url": format!(
                    "data:{mime};base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(data)
                )
            }));
            continue;
        }
        if !super::images::parameter_supported(&name) {
            // 蒙版类字段走 `validate` 那一份措辞：我们认识它们，只是没人实现。
            // 落进「不认识这个参数」会让人以为是拼写问题，去改字段名。
            if matches!(name.as_str(), "mask" | "input_fidelity") {
                return Err(super::images::mask_unsupported());
            }
            return Err(invalid(format!(
                "图片编辑不支持参数 {name}（参考图字段名请用 image）"
            )));
        }
        if payload.contains_key(&name) {
            return Err(invalid(format!("参数 {name} 不能重复提交")));
        }
        let text = field
            .text()
            .await
            .map_err(|_| invalid(format!("参数 {name} 必须是 UTF-8 文本")))?;
        // 表单里一切都是字符串，而这几个字段下游要用数字 / 布尔判断（`validate`
        // 里 `n.as_u64()`、`stream != false`）。在这里就地转成 JSON 类型，避免
        // 下游再为「multipart 形态」开一个分支。
        let value = if matches!(
            name.as_str(),
            "n" | "output_compression" | "partial_images" | "stream"
        ) {
            serde_json::from_str(&text).map_err(|_| invalid(format!("参数 {name} 格式无效")))?
        } else {
            Value::String(text)
        };
        payload.insert(name, value);
    }
    payload.insert("images".to_string(), Value::Array(images));
    Ok(Value::Object(payload))
}

/// 把 `images[].image_url` 统一成 data URL，并做「真的是图片」的校验。
///
/// 放在 `validate` / `resolve_model` **之后**调用：参数错误与「这个模型没有编辑
/// 通道」都不该先花几秒下载参考图（上游也是这个顺序）。
pub(super) async fn prepare(payload: &mut Value) -> Result<(), GatewayError> {
    let budget = Duration::from_millis(crate::server::config::timeout_settings().body_ms());
    tokio::time::timeout(budget, prepare_inner(payload))
        .await
        .map_err(|_| invalid("参考图读取超时，尚未发起图片编辑请求"))?
}

async fn prepare_inner(payload: &mut Value) -> Result<(), GatewayError> {
    let images = payload
        .get("images")
        .and_then(Value::as_array)
        .filter(|images| !images.is_empty() && images.len() <= MAX_IMAGES)
        .ok_or_else(|| invalid(format!("images 必须包含 1 到 {MAX_IMAGES} 张参考图")))?;
    let mut prepared = Vec::with_capacity(images.len());
    let mut total = 0usize;
    for image in images {
        let object = image
            .as_object()
            .ok_or_else(|| invalid("images 每项必须是 {\"image_url\": ...} 对象"))?;
        // 只认 image_url：(a) 上游没有统一的 file_id 概念，转过去也得先下载；
        // (b) 多一个字段就多一处「它到底该覆盖谁」的解释空间。
        if object.keys().any(|key| key != "image_url") {
            return Err(invalid("参考图仅支持 image_url，不支持 file_id 或其它字段"));
        }
        let link = object
            .get("image_url")
            .and_then(Value::as_str)
            .filter(|link| !link.trim().is_empty())
            .ok_or_else(|| invalid("参考图缺少 image_url"))?;
        let bytes = if let Some(rest) = link.strip_prefix("data:") {
            let (meta, encoded) = rest
                .split_once(',')
                .ok_or_else(|| invalid("参考图 data URL 无效"))?;
            if !meta.starts_with("image/") || !meta.ends_with(";base64") {
                return Err(invalid("参考图必须使用 data:image/...;base64 格式"));
            }
            // 先按编码后长度挡一道，避免为一个超限的输入分配解码缓冲。
            if encoded.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
                return Err(GatewayError::with_status(413, "单张参考图不得超过 10 MiB"));
            }
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| invalid("参考图 base64 无效"))?
        } else {
            let url = url::Url::parse(link).map_err(|_| invalid("参考图 URL 无效"))?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(invalid("参考图只接受无凭证的 HTTP(S) URL"));
            }
            // 是我们自己发出的链接就直接读本地缓存：省一次网络往返，也让「生成
            // 完接着编辑」在断网时仍然成立。令牌本身就是能力凭证，命中即可信。
            match url
                .path()
                .strip_prefix("/v1/images/files/")
                .and_then(super::image_assets::read)
            {
                Some((bytes, _)) => bytes.to_vec(),
                None => super::image_output::download(link, None, false)
                    .await
                    .map_err(|error| invalid(format!("参考图下载失败：{}", error.message)))?,
            }
        };
        let url = validate_bytes(bytes).await?;
        total = total.saturating_add(url.len());
        if total > MAX_INLINE_BYTES {
            return Err(GatewayError::with_status(
                413,
                "参考图内联数据总大小不得超过 14 MiB（请减少张数或降低分辨率）",
            ));
        }
        prepared.push(json!({ "image_url": url }));
    }
    payload["images"] = Value::Array(prepared);
    let size = serde_json::to_vec(payload)
        .map_err(|_| invalid("图片编辑请求无法序列化"))?
        .len();
    if size > MAX_INLINE_BYTES {
        return Err(GatewayError::with_status(
            413,
            "图片编辑内联请求总大小不得超过 14 MiB（请减少张数或降低分辨率）",
        ));
    }
    Ok(())
}

async fn validate_bytes(bytes: Vec<u8>) -> Result<String, GatewayError> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(GatewayError::with_status(413, "单张参考图不得超过 10 MiB"));
    }
    // 解码是纯 CPU 的重活（一张 8000×8000 的 PNG 解出来 256 MiB），必须离开
    // 异步线程。
    tokio::task::spawn_blocking(move || {
        inspect_bytes(&bytes)?;
        let mime = mime_of(&bytes)?;
        Ok(format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    })
    .await
    .map_err(|_| invalid("参考图解码任务失败"))?
}

/// 按文件头判格式。只放行这三家：它们是有损/无损的主流形态，也是已接的上游
/// 都认的三种。GIF / BMP / TIFF 之类能解但传上去大概率被上游拒，在这里明说
/// 比让上游拒更有用。
pub(super) fn mime_of(bytes: &[u8]) -> Result<&'static str, GatewayError> {
    match image::guess_format(bytes) {
        Ok(image::ImageFormat::Png) => Ok("image/png"),
        Ok(image::ImageFormat::Jpeg) => Ok("image/jpeg"),
        Ok(image::ImageFormat::WebP) => Ok("image/webp"),
        _ => Err(invalid("参考图仅支持 PNG、JPEG、WebP")),
    }
}

/// 真解一次：既确认「这是图片」而不是改了扩展名的别的东西，也把解压炸弹挡在
/// 外面（`image` 的 `Limits` 会在超过边长/分配上限时直接报错，不会真的分配
/// 256 MiB 之后再发现太大）。
pub(super) fn inspect_bytes(bytes: &[u8]) -> Result<(u32, u32), GatewayError> {
    if bytes.is_empty() || bytes.len() > 32 * 1024 * 1024 {
        return Err(invalid("图片为空或超过 32 MiB"));
    }
    mime_of(bytes)?;
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| invalid("图片格式无法识别"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| invalid("图片解码失败或边长超过 8192"))?;
    Ok((decoded.width(), decoded.height()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_png() -> Vec<u8> {
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            3,
            image::Rgba([200, 10, 10, 255]),
        ));
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// 表单里参考图的四种写法都要认出来：漏一种，那个客户端发上来的图会落到
    /// 「未知参数」分支，用户看到的是一句完全对不上的抱怨。
    #[test]
    fn reference_field_names_are_recognized() {
        for name in ["image", "image[]", "images", "images[]", "image[0]", "image[12]"] {
            assert!(image_field(name), "{name}");
        }
        for name in ["prompt", "images[", "image[a]", "image[]x", "", "mask"] {
            assert!(!image_field(name), "{name}");
        }
    }

    /// 只放行 PNG / JPEG / WebP，且判据是**文件头**而不是扩展名或客户端声明的
    /// content-type —— 后者两者都由客户端随手写。
    #[test]
    fn mime_is_sniffed_from_bytes() {
        assert_eq!(mime_of(&tiny_png()).unwrap(), "image/png");
        for bad in [&b"GIF89a"[..], b"not an image at all", b""] {
            assert!(mime_of(bad).is_err());
        }
    }

    /// 真解一次图：既确认「这是图片」（而不是改了扩展名的别的东西），也顺带
    /// 拿到尺寸。解不出来的一律拒，不把可能带病的字节转给上游。
    #[test]
    fn inspect_decodes_and_reports_size() {
        assert_eq!(inspect_bytes(&tiny_png()).unwrap(), (2, 3));
        assert!(inspect_bytes(b"not an image at all").is_err());
    }

    fn parts_with(content_type: &str) -> Parts {
        Request::builder()
            .method("POST")
            .uri("/v1/images/edits")
            .header("content-type", content_type)
            .body(())
            .unwrap()
            .into_parts()
            .0
    }

    fn multipart_body(boundary: &str, fields: &[(&str, &[u8])]) -> Vec<u8> {
        let mut body = Vec::new();
        for (name, data) in fields {
            body.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n")
                    .as_bytes(),
            );
            body.extend_from_slice(data);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        body
    }

    /// 表单收成与生图同构的 JSON：文本字段进顶层、文件字段进 `images[]`，且
    /// **数字类字段就地转成 JSON 数字** —— 留成字符串的话，下游 `n.as_u64()`
    /// 会拿到 `None` 而悄悄退回默认值（症状是「我明明要了 4 张，只出 1 张」）。
    #[tokio::test]
    async fn multipart_form_folds_into_one_shape() {
        let png = tiny_png();
        let body = multipart_body(
            "----agate",
            &[
                ("prompt", "把背景换成蓝色".as_bytes()),
                ("n", b"1"),
                ("size", b"1024x1024"),
                ("image", &png),
            ],
        );
        let payload = decode(
            parts_with("multipart/form-data; boundary=----agate"),
            Bytes::from(body),
        )
        .await
        .unwrap();
        assert_eq!(payload["prompt"], "把背景换成蓝色");
        assert_eq!(payload["n"], 1);
        assert_eq!(payload["size"], "1024x1024");
        let images = payload["images"].as_array().unwrap();
        assert_eq!(images.len(), 1);
        assert!(images[0]["image_url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
    }

    /// 参考图字节里出现 `\r\n`、甚至出现一段「长得像 boundary」的文本，都不能
    /// 把图切断 —— 这正是「不手搓 boundary」的理由（切错的症状是「上传成功但
    /// 图是花的」，比一个明确的 400 难查得多）。这里逐字节比对原值。
    #[tokio::test]
    async fn binary_field_survives_crlf_and_boundary_lookalikes() {
        // 以 PNG 头开始（`mime_of` 只看文件头），后面接 CRLF 与一段「长得像
        // boundary」的文本。切错的实现会在这里截断或错位。
        let raw: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x01\r\n--not-the-boundary\r\n\x02\x03";
        let body = multipart_body(
            "----agate",
            &[("prompt", b"x".as_slice()), ("image", raw)],
        );
        let payload = decode(
            parts_with("multipart/form-data; boundary=----agate"),
            Bytes::from(body),
        )
        .await
        .unwrap();
        let url = payload["images"][0]["image_url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"), "{url}");
        let encoded = url.split_once(',').unwrap().1;
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
            raw
        );
    }

    /// JSON 形态原样进（数字就是数字、不要转字符串），否则 `n` / `size` 这类
    /// 字段会在两个入口之间出现两套类型约定。
    #[tokio::test]
    async fn json_body_passes_through_untouched() {
        let body = br#"{"prompt":"x","n":4,"images":[{"image_url":"data:image/png;base64,AA"}]}"#;
        let payload = decode(
            parts_with("application/json"),
            Bytes::from_static(body),
        )
        .await
        .unwrap();
        assert_eq!(payload["n"], 4);
        assert_eq!(payload["images"][0]["image_url"], "data:image/png;base64,AA");
    }

    /// 既不是 JSON 也不是 multipart 时，文案要指出两条正确形态，而不是一句
    /// 「格式错误」（用户下一步不知道该改哪里）。
    #[tokio::test]
    async fn wrong_content_type_says_what_to_send() {
        let error = decode(parts_with("text/plain"), Bytes::from_static(b"hi"))
            .await
            .unwrap_err();
        assert!(error.message.contains("multipart/form-data"), "{}", error.message);
        assert!(error.message.contains("image_url"), "{}", error.message);
    }
}
