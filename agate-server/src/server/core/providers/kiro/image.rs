//! Kiro 的图片：data URL 解析 + 超限就地缩小。
//!
//! ── 上限是多少 ──────────────────────────────────────────────
//! 上游对**编码后**的 Base64 长度设 5 MiB 上限（报错原文
//! `image exceeds 5 MB maximum: … (IMAGE_SIZE_EXCEEDED)`）。原图不处理的话会
//! 留在历史里，之后每一轮都被同一个错误挡住 —— 所以必须就地缩小；缩不动就换成
//! 一句说明，让对话继续（比整次请求失败好）。
//!
//! ── 与 `catpaw::image_compress` 的区别（不是重复实现）──────
//! ```text
//!   catpaw   60KB 就触发，压到 120KB   —— 它的上游按「请求体总量」限，图必须小
//!   kiro     5MiB 才触发，压到 5MiB 内  —— 它的上游只限单张，别的一律不动
//! ```
//! 阈值差两个数量级，合成一份只会让一边无谓地掉画质。所以这里保留独立实现，
//! 但**格式选择策略同源**（PNG 优先、不行再 JPEG 降质）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic，坏图一律降级成说明。

use std::io::Cursor;

/// 上游的 Base64 上限（字符数）
pub const IMAGE_LIMIT: usize = 5 * 1024 * 1024;

/// 缩小后的长边
const IMAGE_EDGE: u32 = 2000;

/// 解码前先判尺寸：超过这个像素数直接放弃（防图片炸弹）
const MAX_PIXELS: u64 = 50_000_000;

/// 过大的输入（Base64 字符数）不再尝试解码
const MAX_INPUT: usize = 64 * 1024 * 1024;

/// 一次缩小的结果
pub enum Fitted {
    /// 缩小成功（新格式与新 Base64）
    Smaller { format: String, bytes: String },
    /// 缩不了（原因，用于替换成文字说明）
    Note(String),
}

/// 把一张超限的图片缩小到上限以内。
pub fn fit(b64: &str) -> Fitted {
    if b64.len() > MAX_INPUT {
        return Fitted::Note("it is too large to resize".to_string());
    }
    let Some(raw) = decode(b64) else {
        return Fitted::Note("it couldn't be read as an image".to_string());
    };
    let Ok(reader) = image::ImageReader::new(Cursor::new(&raw)).with_guessed_format() else {
        return Fitted::Note("it couldn't be read as an image".to_string());
    };
    let Ok(dimensions) = reader.into_dimensions() else {
        return Fitted::Note("it couldn't be read as an image".to_string());
    };
    if u64::from(dimensions.0) * u64::from(dimensions.1) > MAX_PIXELS {
        return Fitted::Note("it has too many pixels to resize".to_string());
    }
    let Ok(dynamic) = image::load_from_memory(&raw) else {
        return Fitted::Note("it couldn't be read as an image".to_string());
    };
    let long_edge = dimensions.0.max(dimensions.1);
    let target = long_edge.min(IMAGE_EDGE).max(1);
    let resized = if long_edge > target {
        dynamic.resize(target, target, image::imageops::FilterType::Lanczos3)
    } else {
        dynamic
    };
    // 原格式（PNG）优先：保住透明通道与无损画质
    if let Ok(bytes) = encode_png(&resized) {
        if bytes.len() <= IMAGE_LIMIT {
            return Fitted::Smaller {
                format: "png".to_string(),
                bytes,
            };
        }
    }
    for quality in [80u8, 70, 60, 50] {
        if let Ok(bytes) = encode_jpeg(&resized, quality) {
            if bytes.len() <= IMAGE_LIMIT {
                return Fitted::Smaller {
                    format: "jpeg".to_string(),
                    bytes,
                };
            }
        }
    }
    Fitted::Note("it couldn't be made small enough".to_string())
}

fn encode_png(image: &image::DynamicImage) -> Result<String, ()> {
    let mut buffer = Cursor::new(Vec::new());
    image
        .write_to(&mut buffer, image::ImageFormat::Png)
        .map_err(|_| ())?;
    Ok(encode(buffer.get_ref()))
}

fn encode_jpeg(image: &image::DynamicImage, quality: u8) -> Result<String, ()> {
    let mut buffer = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buffer, quality);
    encoder.encode_image(&image.to_rgb8()).map_err(|_| ())?;
    Ok(encode(&buffer))
}

fn decode(input: &str) -> Option<Vec<u8>> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let cleaned: String = input.chars().filter(|ch| !ch.is_whitespace()).collect();
    STANDARD.decode(cleaned).ok()
}

fn encode(bytes: &[u8]) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    STANDARD.encode(bytes)
}

/// 解码一张 data URL（`data:image/png;base64,…`）里的格式与正文。
///
/// 只认内联 base64：`https://` 的图片没有字节可发，返回 `None` 由调用方
/// 决定怎么提示（这里静默跳过 —— 提示语由 `request` 统一拼）。
pub fn split_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if !meta.contains(";base64") {
        return None;
    }
    let media = meta.split(';').next().unwrap_or("image/png");
    let format = media.strip_prefix("image/").unwrap_or("png");
    let format = match format.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "jpeg",
        "webp" => "webp",
        "gif" => "gif",
        "png" => "png",
        _ => "png",
    };
    Some((format.to_string(), data.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三种 data URL 形态：正常 PNG、jpeg 别名、非图片
    #[test]
    fn data_urls_are_parsed_by_media_type() {
        let (format, data) = split_data_url("data:image/jpeg;base64,AAA").expect("合法");
        assert_eq!(format, "jpeg");
        assert_eq!(data, "AAA");
        let (format, _) = split_data_url("data:image/jpg;base64,AAA").expect("合法");
        assert_eq!(format, "jpeg", "jpg 与 jpeg 归一");
        let (format, _) = split_data_url("data:image/webp;base64,AAA").expect("合法");
        assert_eq!(format, "webp");
        // 未知类型统一当 png（上游也只认这几种）
        let (format, _) = split_data_url("data:image/x-icon;base64,AAA").expect("合法");
        assert_eq!(format, "png");
    }

    /// 不是内联 base64 的一律不认（`https://` / 普通文本 / 缺 meta）
    #[test]
    fn only_inline_base64_data_urls_are_accepted() {
        assert!(split_data_url("https://example.com/a.png").is_none());
        assert!(split_data_url("data:image/png,notbase64").is_none());
        assert!(split_data_url("data:image/png;base64").is_none());
        assert!(split_data_url("").is_none());
    }

    /// 坏图不 panic（这是对话链路，panic=abort 会带走整个进程）
    #[test]
    fn broken_images_degrade_to_a_note() {
        for bad in ["", "不是base64", "AAAA", "data:image/png;base64,"] {
            match fit(bad) {
                Fitted::Note(note) => assert!(!note.is_empty()),
                Fitted::Smaller { .. } => panic!("{bad:.16} 不该被当成图片"),
            }
        }
    }
}
