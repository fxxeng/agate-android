//! 辅助识图的图片内联：把 http(s) 图片地址下载成 data URI。
//!
//! 为什么需要：视觉模型大多只认 data URI / 公网直链，而客户端传来的可能是
//! 内网图床或带鉴权的地址；统一转成 data URI 才能让识图那一次调用稳定读到图。
//!
//! SSRF 判据与 `api::image_output::download` 同源（本地已有设施）：逐地址判公网
//! （不含 private/loopback/link-local/保留段）、DNS 解析后**固定这些地址**
//! （`pinned_address_client`，防 DNS 重绑定）、不自动跟随跳转（每跳重新校验）、
//! 逐块累计体积上限。Android 上不落盘，全程在内存里。

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use crate::server::errors::GatewayError;
use base64::Engine;

use super::content::ImagePart;

const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 50 * 1024 * 1024;
const MAX_REDIRECTS: usize = 4;

fn invalid(message: impl Into<String>) -> GatewayError {
    GatewayError::bad_request(format!("{}；未删除图片、未切换处理方式", message.into()))
        .with_code("vision_image_invalid")
}

/// 公网地址判据（与 `api::image_output::public_address` 同一套，本地设施复用）。
fn public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_documentation()
                && ip.octets()[0] != 0
                && !(ip.octets()[0] == 192 && ip.octets()[1] == 0 && ip.octets()[2] == 0)
                && ip.octets()[0] & 0xf0 != 0xe0
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_address(IpAddr::V4(v4));
            }
            let segments = ip.segments();
            !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && segments[0] & 0xfe00 != 0xfc00
                && segments[0] & 0xffc0 != 0xfe80
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
                && segments[0] & 0xe000 == 0x2000
        }
    }
}

/// 校验 data URI 里的 base64 图片，返回解码后字节数。
fn validate_inline(reference: &str) -> Result<usize, GatewayError> {
    let (prefix, data) = reference
        .split_once(',')
        .ok_or_else(|| invalid("图片 data URI 格式不正确"))?;
    if !prefix.starts_with("data:image/") || !prefix.ends_with(";base64") {
        return Err(invalid("只支持 base64 编码的图片 data URI"));
    }
    if data.len() > MAX_IMAGE_BYTES * 4 / 3 + 4 {
        return Err(invalid("单张图片超过 20MB 限制"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| invalid("图片 base64 编码不正确"))?;
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return Err(invalid("图片为空或超过 20MB 限制"));
    }
    Ok(bytes.len())
}

/// 下载一个公网图片地址并转成 data URI（逐跳重新校验、固定解析地址）。
async fn download(reference: &str) -> Result<String, GatewayError> {
    let mut url = url::Url::parse(reference).map_err(|_| invalid("图片地址无效"))?;
    for _ in 0..=MAX_REDIRECTS {
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(invalid("图片地址只支持不含登录凭据的 HTTP 或 HTTPS"));
        }
        let host = url
            .host_str()
            .ok_or_else(|| invalid("图片地址缺少域名"))?
            .trim_matches(['[', ']'])
            .to_string();
        let port = url
            .port_or_known_default()
            .ok_or_else(|| invalid("图片地址端口无效"))?;
        let mut addresses: Vec<SocketAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
            vec![SocketAddr::new(ip, port)]
        } else {
            tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|_| invalid("图片域名解析失败"))?
                .collect()
        };
        addresses.sort();
        addresses.dedup();
        if addresses.is_empty() || addresses.iter().any(|a| !public_address(a.ip())) {
            return Err(invalid("图片地址不能指向本机、内网或保留地址"));
        }
        // 固定已校验地址、关闭自动跳转（每一跳都在循环里重新走上面的校验）。
        let client =
            crate::server::core::egress::pinned_address_client(None, false, &host, &addresses)
                .map_err(|_| invalid("无法创建图片下载连接"))?;
        let mut response = client
            .get(url.clone())
            .timeout(Duration::from_secs(30))
            .header("user-agent", "Agate-VisionCompat/1.0")
            .send()
            .await
            .map_err(|_| invalid("图片下载失败或超时"))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| invalid("图片跳转缺少目标地址"))?;
            url = url
                .join(location)
                .map_err(|_| invalid("图片跳转地址无效"))?;
            continue;
        }
        if !response.status().is_success() {
            return Err(invalid(format!(
                "图片下载返回 HTTP {}",
                response.status().as_u16()
            )));
        }
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(
            mime.as_str(),
            "image/png" | "image/jpeg" | "image/webp" | "image/gif"
        ) {
            return Err(invalid("下载内容不是支持的图片格式"));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_IMAGE_BYTES as u64)
        {
            return Err(invalid("单张图片超过 20MB 限制"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| invalid("图片下载中断"))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_IMAGE_BYTES {
                return Err(invalid("单张图片超过 20MB 限制"));
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {
            return Err(invalid("下载的图片为空"));
        }
        return Ok(format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ));
    }
    Err(invalid("图片地址跳转次数过多"))
}

/// 把所有图片引用归一成 data URI（data URI 原样校验，http(s) 下载转换）。
pub(super) async fn inline(images: &mut [ImagePart]) -> Result<(), GatewayError> {
    let mut total = 0usize;
    let mut downloaded = std::collections::HashMap::<String, String>::new();
    for image in images {
        if !image.reference.starts_with("data:") {
            let data = if let Some(data) = downloaded.get(&image.reference) {
                data.clone()
            } else {
                let data = download(&image.reference).await?;
                downloaded.insert(image.reference.clone(), data.clone());
                data
            };
            image.reference = data;
        }
        total = total.saturating_add(validate_inline(&image.reference)?);
        if total > MAX_TOTAL_BYTES {
            return Err(invalid("本次图片总大小超过 50MB 限制"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_private_and_reserved_addresses() {
        assert!(!public_address("127.0.0.1".parse().unwrap()));
        assert!(!public_address("10.0.0.1".parse().unwrap()));
        assert!(!public_address("192.168.1.1".parse().unwrap()));
        assert!(!public_address("169.254.1.1".parse().unwrap()));
        assert!(!public_address("192.0.0.1".parse().unwrap()));
        assert!(public_address("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn validate_inline_rejects_non_image_data_uri() {
        assert!(validate_inline("data:text/plain;base64,aGk=").is_err());
        assert!(validate_inline("data:image/png;base64,!!!").is_err());
        assert!(validate_inline(&format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3])
        ))
        .is_ok());
    }
}
