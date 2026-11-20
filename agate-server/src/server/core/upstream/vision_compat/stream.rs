//! 拒答识别：判定一条上游错误/响应是不是「模型不支持图片」。
//!
//! 纯文本判据，对齐上游 `vision_compat/stream.rs` 的 `rejection_text` /
//! `explicit_rejection` —— 这是触发图片兼容的入口判据，判错会把正常答复杂化或
//! 该降级的没降级。判据逐字照抄上游（它们是多家上游拒答话术的实测归纳）。

/// 一条错误是否「明确是模型不支持图片」（含状态码门控）。
///
/// 状态码门控与上游一致：只在这几种「客户端错误/服务端间接拒绝」里认，避免把
/// 鉴权 401、限流 429 之类的错误误判成图片问题。
pub fn explicit_rejection(status: i32, message: &str) -> bool {
    if !matches!(status, 400 | 406 | 415 | 422 | 500 | 502) {
        return false;
    }
    rejection_text(message)
}

/// 拒答文本判据（不看状态码）。
///
/// 分两段：先排除「图片过大/格式错/下载失败」这类**不是能力缺失**的错误（这些
/// 换模型也救不了，不该触发兼容），再匹配各家「不支持图片输入」的话术。
pub fn rejection_text(text: &str) -> bool {
    let lower = text.to_lowercase();
    // 这些是图片本身的问题，换模型没用 —— 不触发兼容。
    if [
        "too large",
        "too many image",
        "image size",
        "image format",
        "invalid image",
        "download",
        "fetch image",
        "图片过大",
        "图片格式",
        "图片下载",
        "image dimensions",
        "image resolution",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return false;
    }
    // 各家「模型不支持图片」的实测话术。
    [
        "001001020",
        "model is not multimodal",
        "model does not support image",
        "model doesn't support image",
        "model does not support vision",
        "model does not support multimodal",
        "image input is not supported for this model",
        "image inputs are not supported",
        "image_url is only supported by certain models",
        "this model only supports text",
        "不支持图片输入",
        "不支持图像输入",
        "模型不支持图片",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_english_rejection() {
        assert!(rejection_text("model does not support image input"));
        assert!(rejection_text("This model only supports text"));
        assert!(rejection_text("image_url is only supported by certain models"));
    }

    #[test]
    fn matches_chinese_rejection() {
        assert!(rejection_text("该模型不支持图片输入"));
        assert!(rejection_text("模型不支持图片"));
    }

    #[test]
    fn excludes_image_problem_errors() {
        assert!(!rejection_text("image size too large"));
        assert!(!rejection_text("图片格式不支持"));
        assert!(!rejection_text("failed to download image"));
    }

    #[test]
    fn status_gate() {
        assert!(explicit_rejection(400, "model does not support image"));
        assert!(!explicit_rejection(401, "model does not support image"));
        assert!(!explicit_rejection(429, "model does not support image"));
        assert!(explicit_rejection(422, "不支持图片输入"));
    }

    #[test]
    fn unrelated_errors_not_rejection() {
        assert!(!rejection_text("invalid api key"));
        assert!(!rejection_text("rate limit exceeded"));
        assert!(!rejection_text(""));
    }
}
