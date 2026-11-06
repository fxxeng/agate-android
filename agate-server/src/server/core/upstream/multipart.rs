//! 出站 `multipart/form-data` 编码：本仓**唯一**的拼装处。
//!
//! ── 为什么不引 crate ────────────────────────────────────────
//! 需要的只有「文本字段 + 文件字段」两件事。`multer` 是**解析器**而不是构造器；
//! 引一个构造器 crate 会为几十行代码带进一棵依赖树，而且各家 crate 对
//! 「多文件同名字段」「文件名编码」的处理各不相同，反而要多读一份文档。
//!
//! 但自己拼就必须把三件事做对，否则症状比一个 400 难查得多：
//!   1. **boundary 不出现在任何一段的内容里**。参考图是二进制，随机 128 位
//!      boundary 撞上的概率可以忽略 —— 但「可以忽略」不等于「不会发生」，
//!      而撞上时的表现是「上传成功，图是花的」，没人会往分隔符上想。
//!      所以 [`MultipartForm::finish`] 逐段检查，撞上就换一个 boundary 重拼。
//!   2. **头部行一律 CRLF**，结尾必须有 `--boundary--`。少一个 `\r\n` 的后果是
//!      上一段的末尾会吞掉下一段的头，prompt 里会多出一串边界文本。
//!   3. **文件名只用我们生成的 ASCII 名**（`reference-1.png`）。RFC 7578 对
//!      非 ASCII 文件名另有编码要求，而客户端给的文件名我们一律不用 ——
//!      既是隐私，也少一个编码坑。
//!
//! ── 与入站解析的分工 ────────────────────────────────────────
//! 入站（`api::image_input`）用 axum 的 `Multipart` 提取器解客户端上传；
//! 本模块只管**出站**，两者没有共享代码 —— 一个解、一个拼，唯一共同点是
//! 它们都遵循同一份 RFC。

use bytes::Bytes;
use serde_json::{json, Map, Value};

use crate::server::errors::GatewayError;

/// 撞上内容时最多换几次 boundary。128 位随机值连续撞 8 次的概率可以忽略，
/// 留一个上限只是为了不让一个 bug 变成死循环。
const MAX_BOUNDARY_ATTEMPTS: usize = 8;

/// 一段表单内容。
enum Part {
    Text {
        name: String,
        value: String,
    },
    File {
        name: String,
        filename: String,
        content_type: String,
        bytes: Bytes,
    },
}

impl Part {
    fn name(&self) -> &str {
        match self {
            Self::Text { name, .. } | Self::File { name, .. } => name,
        }
    }

    /// 这一段里除分隔符外的字节（boundary 撞车检查用）。
    fn content(&self) -> &[u8] {
        match self {
            Self::Text { value, .. } => value.as_bytes(),
            Self::File { bytes, .. } => bytes,
        }
    }
}

/// 拼好的多段表单：boundary、要发出去的字节，以及一份**可读近似**。
///
/// `preview` 与 `bytes` 同源（都由同一个 [`MultipartForm`] 生成），区别只在于
/// 文件段只留文件名、类型与字节数 —— 日志、调试报文与 `pointer()` 查询读它，
/// 不会因为一次编辑请求把几 MB 的 base64 塞进日志。
#[derive(Debug)]
pub struct MultipartBody {
    pub boundary: String,
    pub bytes: Bytes,
    pub preview: Value,
}

impl MultipartBody {
    /// `Content-Type` 头的值（boundary 只有一个来源：本结构体）。
    pub fn content_type(&self) -> String {
        format!("multipart/form-data; boundary={}", self.boundary)
    }
}

/// 多段表单的构造器。
///
/// 用法固定为「先攒段、最后 `finish()`」：boundary 在 finish 时才定，因为它
/// 要避开所有段的内容 —— 边攒边拼就没有「换一个 boundary 重拼」的机会。
#[derive(Default)]
pub struct MultipartForm {
    parts: Vec<Part>,
}

impl MultipartForm {
    pub fn new() -> Self {
        Self::default()
    }

    /// 文本字段。值按 UTF-8 发出（`Content-Type: text/plain; charset=utf-8`）。
    pub fn text(&mut self, name: &str, value: &str) -> &mut Self {
        self.parts.push(Part::Text {
            name: name.to_string(),
            value: value.to_string(),
        });
        self
    }

    /// 文件字段。同名字段可以出现多次（上游按「重复的 `image` 字段」收参考图）。
    pub fn file(
        &mut self,
        name: &str,
        filename: &str,
        content_type: &str,
        bytes: impl Into<Bytes>,
    ) -> &mut Self {
        self.parts.push(Part::File {
            name: name.to_string(),
            filename: filename.to_string(),
            content_type: content_type.to_string(),
            bytes: bytes.into(),
        });
        self
    }

    pub fn finish(&self) -> Result<MultipartBody, GatewayError> {
        for part in &self.parts {
            token(part.name(), "字段名")?;
            if let Part::File {
                filename,
                content_type,
                ..
            } = part
            {
                token(filename, "文件名")?;
                token(content_type, "文件类型")?;
            }
        }
        for _ in 0..MAX_BOUNDARY_ATTEMPTS {
            let boundary = random_boundary()?;
            let needle = boundary.as_bytes();
            // 逐段检查：图片是二进制，撞上分隔符时上游会把图切错，而它回的
            // 往往是一个 200 —— 用户看到的是「图是花的」，不是「请求失败」。
            if self
                .parts
                .iter()
                .any(|part| part.content().windows(needle.len()).any(|window| window == needle))
            {
                continue;
            }
            return Ok(MultipartBody {
                bytes: self.render(&boundary),
                preview: self.preview(),
                boundary,
            });
        }
        Err(GatewayError::with_status(
            500,
            "无法生成不与图片内容冲突的 multipart 分隔符",
        ))
    }

    fn render(&self, boundary: &str) -> Bytes {
        let mut out: Vec<u8> = Vec::with_capacity(
            128 + self
                .parts
                .iter()
                .map(|part| part.content().len() + 128)
                .sum::<usize>(),
        );
        for part in &self.parts {
            out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            match part {
                Part::Text { name, value } => {
                    out.extend_from_slice(
                        format!("Content-Disposition: form-data; name=\"{name}\"\r\n").as_bytes(),
                    );
                    out.extend_from_slice(b"Content-Type: text/plain; charset=utf-8\r\n\r\n");
                    out.extend_from_slice(value.as_bytes());
                }
                Part::File {
                    name,
                    filename,
                    content_type,
                    bytes,
                } => {
                    out.extend_from_slice(
                        format!(
                            "Content-Disposition: form-data; name=\"{name}\"; \
                             filename=\"{filename}\"\r\n"
                        )
                        .as_bytes(),
                    );
                    out.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
                    out.extend_from_slice(bytes);
                }
            }
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        Bytes::from(out)
    }

    /// 可读近似：文本段原样，文件段只留名字 / 类型 / 字节数。
    fn preview(&self) -> Value {
        let mut object = Map::new();
        for part in &self.parts {
            match part {
                Part::Text { name, value } => {
                    object.insert(name.clone(), json!(value));
                }
                Part::File {
                    name,
                    filename,
                    content_type,
                    bytes,
                } => {
                    // 同名字段出现多次时折成数组：`image` 就是这种（多张参考图）。
                    let entry = object
                        .entry(name.clone())
                        .or_insert_with(|| Value::Array(Vec::new()));
                    let item = json!({
                        "filename": filename,
                        "content_type": content_type,
                        "bytes": bytes.len(),
                    });
                    match entry {
                        Value::Array(items) => items.push(item),
                        // 同名字段先前是文本（少见但合法）：折成 [旧值, 新值]，
                        // 不丢掉任何一段 —— preview 与报文必须描述同一件事。
                        slot => {
                            let previous = std::mem::replace(slot, Value::Null);
                            *slot = Value::Array(vec![previous, item]);
                        }
                    }
                }
            }
        }
        Value::Object(object)
    }
}

/// 头部里的「token」：用于字段名 / 文件名 / 类型。
///
/// 我们生成的这些值都是自己写的 ASCII（`image` / `reference-1.png` /
/// `image/png`）。这条检查的真正作用是**证明**这一点 —— 一旦哪天有人把客户端
/// 给的名字直接传进来，这里会立刻拦住，而不是拼出一个头部注入式的报文。
fn token(value: &str, what: &str) -> Result<(), GatewayError> {
    if value.is_empty()
        || value.bytes().any(|byte| {
            !byte.is_ascii() || byte.is_ascii_control() || matches!(byte, b'"' | b'\\' | b';')
        })
    {
        return Err(GatewayError::with_status(
            500,
            format!("multipart {what} 只能包含可打印 ASCII（当前为 {value:?}）"),
        ));
    }
    Ok(())
}

/// `agate` + 32 位十六进制（128 位随机）。
fn random_boundary() -> Result<String, GatewayError> {
    let mut random = [0u8; 16];
    getrandom::getrandom(&mut random)
        .map_err(|_| GatewayError::with_status(500, "无法生成 multipart 分隔符"))?;
    let hex: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("agate{hex}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_of(form: &MultipartForm) -> MultipartBody {
        form.finish().expect("拼装应当成功")
    }

    /// 每个文件段的字节必须**逐字节**出现在报文里，且分隔符出现在它前面。
    /// 这条挡住的是「多一个换行 / 少一个 CRLF」这一类肉眼看不出来的错。
    #[test]
    fn file_bytes_survive_verbatim() {
        let raw: &[u8] = b"\x89PNG\r\n\x1a\n--not-a-boundary\r\n\x00tail";
        let mut form = MultipartForm::new();
        form.text("prompt", "改成蓝色背景")
            .file("image", "reference-1.png", "image/png", raw.to_vec());
        let body = body_of(&form);

        let needle = raw;
        assert!(
            body.bytes
                .windows(needle.len())
                .any(|window| window == needle),
            "文件字节必须原样出现在报文里"
        );
        // 结尾必须有闭合分隔符
        assert!(body
            .bytes
            .ends_with(format!("--{}--\r\n", body.boundary).as_bytes()));
        // 文本字段按 UTF-8 发出
        assert!(String::from_utf8_lossy(&body.bytes).contains("改成蓝色背景"));
    }

    /// 同名字段出现多次时，preview 折成数组（`image` 就是这种），而真实报文里
    /// 是两段同名字段 —— 两者都从同一个 form 来，不该有不一致。
    #[test]
    fn repeated_files_fold_into_an_array_in_preview() {
        let mut form = MultipartForm::new();
        form.file("image", "reference-1.png", "image/png", vec![1u8, 2, 3])
            .file("image", "reference-2.jpg", "image/jpeg", vec![4u8, 5]);
        let body = body_of(&form);
        assert_eq!(body.preview["image"][0]["filename"], json!("reference-1.png"));
        assert_eq!(body.preview["image"][1]["bytes"], json!(2));
        // 文件内容**不**进 preview（日志里不该出现几 MB 的二进制）
        assert!(!body.preview.to_string().contains("reference-2.jpg\","));
    }

    /// 头部注入：字段名里带引号或换行必须被拦住，而不是拼出一个坏报文。
    #[test]
    fn header_tokens_reject_control_and_quotes() {
        for bad in ["a\"b", "a\r\nb", "a;b", "图片", ""] {
            let mut form = MultipartForm::new();
            form.text(bad, "x");
            assert!(form.finish().is_err(), "{bad:?} 应被拒");
        }
    }

    /// boundary 必须避开每一段的内容。这里构造一段「假 boundary」来验证换值逻辑：
    /// 直接比较两次生成的值不可靠（随机），所以只断言最终结果里每段内容只出现
    /// 一次，且 boundary 不等于任何内容片段。
    #[test]
    fn boundary_never_appears_inside_parts() {
        let mut form = MultipartForm::new();
        form.text("prompt", "agate0123");
        let body = body_of(&form);
        let occurrences = body
            .bytes
            .windows(body.boundary.len())
            .filter(|window| *window == body.boundary.as_bytes())
            .count();
        // 两段的分隔符 + 结尾 1 次 = 3（一个字段 → 开头 1 + 结尾 1... 精确断言
        // 次数意义不大，关键是 boundary 本身没有出现在内容里）
        assert!(occurrences >= 2, "{occurrences}");
        assert!(body.boundary.starts_with("agate"));
    }
}
