//! 管理面的响应压缩（gzip）—— 对齐上游 3.4.1。
//!
//! ── 为什么需要这一层（本项目与上游的差别要说清）──────────────
//! 上游的收益主要来自面板静态资源（`islands/ui.js` 约 2.3 MB、`ui.css` 约
//! 130 KB、多语词典约 700 KB）。本项目 Android 端的界面是 Kotlin Compose，
//! **不走 HTTP 静态资源**；App 的管理请求走 JNI 进程内直调，既没有传输成本、
//! 也不会带 `Accept-Encoding` —— 这条路径上本中间件只会多写一个 `Vary` 头，
//! 不做压缩。真正受益的是**局域网 / headless 形态下远程打开面板**：
//! `/api/session`（账号多时上百 KB）、模型清单、日志导出等大块 JSON。
//!
//! ── 为什么只挂在管理面 ──────────────────────────────────────
//! `/v1/*` 的对话响应是 SSE 流。对流做压缩会引入缓冲、破坏逐帧下发，收益也远
//! 不如静态资源明显。`panel_router` 不包含 `/v1/*`，所以把这个中间件挂在它上面
//! 就天然避开了流式响应；`gateway_router` 保持原样。
//!
//! ── 压缩的判定 ──────────────────────────────────────────────
//! 只处理满足全部条件的响应：
//!   · 客户端明确接受 gzip（含 `*`；`gzip;q=0` 视为不接受）；
//!   · 状态码 200（304 / 302 / 4xx 没有可压的正文，压了反而添乱）；
//!   · 没有现成的 `Content-Encoding`（避免二次压缩）；
//!   · `Content-Type` 是文本类（JSON / JS / CSS / HTML / SVG / XML …）；
//!   · 正文长度已知且落在 1 KB – 8 MB 之间 —— 太小的压了不划算，太大的
//!     （备份导出、日志下载）要么不是文本，要么会把整份内容读进内存；
//!   · 响应头没有 `Cache-Control: no-transform`。
//!
//! 正文长度用 `Body::size_hint().exact()` 取，而不是只看 `Content-Length` 头：
//! axum 的 `Json` / `Body::from(Vec)` 在进入中间件时还没有写 `Content-Length`
//! （那是 hyper 序列化响应时才补的），只看头会漏掉几乎所有接口响应。

use axum::body::{to_bytes, Body, HttpBody};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::Write;

/// 小于这个长度不压：gzip 头尾与分块开销会让收益接近零。
const MIN_SIZE: usize = 1024;
/// 单份正文的压缩上限。超过它的响应（备份 / 日志下载）要么已经压过，要么不是文本。
const MAX_SIZE: usize = 8 * 1024 * 1024;

/// 管理面的 gzip 中间件。
pub(super) async fn gzip(request: Request<Body>, next: Next) -> Response {
    // HEAD 只回响应头、没有正文；压了反而会让 Content-Length 与实际不符。
    if request.method() == axum::http::Method::HEAD {
        return next.run(request).await;
    }
    let accepts_gzip = accepts_gzip(request.headers());

    let mut response = next.run(request).await;

    if !compressible(&response) {
        return response;
    }

    // 即使这次不压（比如正文太小），也要声明表示随 Accept-Encoding 变化 ——
    // 否则中间缓存可能把未压缩的那份回给接受 gzip 的客户端，反之亦然。
    append_vary(&mut response, "Accept-Encoding");

    if !accepts_gzip {
        return response;
    }

    let size = response
        .body()
        .size_hint()
        .exact()
        .or_else(|| content_length(&response).map(|value| value as u64));
    let Some(size) = size else {
        // 长度未知 = 流式正文（SSE / 下载），一律不碰。
        return response;
    };
    if !((MIN_SIZE as u64)..=(MAX_SIZE as u64)).contains(&size) {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let bytes = match to_bytes(body, MAX_SIZE).await {
        Ok(bytes) => bytes,
        // 理论上走不到（长度已知且在上面卡过上限）。真出错时宁可回空正文的 500，
        // 也不能把已经消费掉的 body 当成原响应发出去。
        Err(_) => return internal_error(),
    };

    // 压缩是 CPU 活，2 MB 级的脚本在单核 NAS 上也要几十毫秒；挪到阻塞线程池，
    // 避免压住 tokio 的 IO 线程。
    let original = bytes.clone();
    let compressed = tokio::task::spawn_blocking(move || gzip_bytes(&bytes)).await;
    let compressed = match compressed {
        Ok(Ok(value)) => value,
        // 压缩失败不是业务失败：原样发未压缩的正文，界面照常能用。
        _ => {
            parts.headers.remove(header::CONTENT_LENGTH);
            parts
                .headers
                .insert(header::CONTENT_LENGTH, length_value(original.len()));
            return Response::from_parts(parts, Body::from(original));
        }
    };

    parts
        .headers
        .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    parts.headers.remove(header::CONTENT_LENGTH);
    parts
        .headers
        .insert(header::CONTENT_LENGTH, length_value(compressed.len()));
    Response::from_parts(parts, Body::from(compressed))
}

/// 响应是否可以交给 gzip 分支（不含「客户端是否接受」这一步）。
fn compressible(response: &Response) -> bool {
    if response.status() != StatusCode::OK {
        return false;
    }
    if response.headers().contains_key(header::CONTENT_ENCODING) {
        return false;
    }
    let no_transform = response
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("no-transform"));
    if no_transform {
        return false;
    }
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    is_text_mime(content_type)
}

/// 文本类的 Content-Type 判定（只认面板实际会产出的那几档）。
fn is_text_mime(content_type: &str) -> bool {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    // SSE 单独挡一道：虽然也是 text/*，但它需要逐帧下发（见模块头）。
    if mime == "text/event-stream" {
        return false;
    }
    mime.starts_with("text/")
        || matches!(
            mime.as_str(),
            "application/json"
                | "application/manifest+json"
                | "application/javascript"
                | "application/x-javascript"
                | "application/xml"
                | "application/xhtml+xml"
                | "image/svg+xml"
        )
}

/// 解析 `Accept-Encoding`，判断客户端接不接受 gzip。
///
/// 显式的 `gzip` / `x-gzip` 优先于 `*`；`q=0` 表示明确拒绝。这里不处理
/// `identity` 或其它编码 —— 面板只产出 gzip 一种压缩形态。
fn accepts_gzip(headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let mut gzip_q: Option<f32> = None;
    let mut wildcard_q: Option<f32> = None;
    for item in value.split(',') {
        let mut parts = item.trim().split(';');
        let coding = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        if coding.is_empty() {
            continue;
        }
        let mut q = 1.0_f32;
        for parameter in parts {
            let parameter = parameter.trim();
            if let Some(raw) = parameter.strip_prefix("q=") {
                q = raw.trim().parse().unwrap_or(0.0);
            }
        }
        match coding.as_str() {
            "gzip" | "x-gzip" => gzip_q = Some(q),
            "*" => wildcard_q = Some(q),
            _ => {}
        }
    }
    gzip_q.or(wildcard_q).unwrap_or(0.0) > 0.0
}

/// 在 `Vary` 里补一项（已有则不重复）。
fn append_vary(response: &mut Response, value: &'static str) {
    let existing = response
        .headers()
        .get(header::VARY)
        .and_then(|header| header.to_str().ok())
        .unwrap_or("");
    if existing
        .split(',')
        .any(|item| item.trim().eq_ignore_ascii_case(value))
    {
        return;
    }
    let merged = if existing.trim().is_empty() {
        value.to_string()
    } else {
        format!("{existing}, {value}")
    };
    if let Ok(header) = HeaderValue::from_str(&merged) {
        response.headers_mut().insert(header::VARY, header);
    }
}

/// 从已有的 `Content-Length` 头读长度（`size_hint` 拿不到时的兜底）。
fn content_length(response: &Response) -> Option<usize> {
    response
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
}

fn length_value(length: usize) -> HeaderValue {
    HeaderValue::from_str(&length.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0"))
}

fn gzip_bytes(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    // 6 是 gzip 的默认档：压缩比与 CPU 的折中点。再高（9）只多几个百分点，
    // 却会让 NAS / 小主机上首次加载多等一截。
    let mut encoder = GzEncoder::new(Vec::new(), Compression::new(6));
    encoder.write_all(bytes)?;
    encoder.finish()
}

fn internal_error() -> Response {
    // 这一层只负责编码，不该产生业务错误；真的走到这里说明正文长度与声明不符。
    // 用最小化的 500 兜底，避免把「已消费的 body」当成正常响应发出去。
    let body = Body::from("response compression failed");
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(body)
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;

    fn header_map(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn accepts_gzip_honors_explicit_quality_values() {
        assert!(accepts_gzip(&header_map("gzip, deflate, br")));
        assert!(accepts_gzip(&header_map("x-gzip;q=0.8")));
        assert!(accepts_gzip(&header_map("*")));
        assert!(!accepts_gzip(&header_map("gzip;q=0")));
        assert!(!accepts_gzip(&header_map("gzip;q=0, *;q=0.9")));
        assert!(!accepts_gzip(&header_map("br")));
        assert!(!accepts_gzip(&HeaderMap::new()));
    }

    #[test]
    fn text_mime_covers_panel_responses_but_never_sse() {
        assert!(is_text_mime("application/json; charset=utf-8"));
        assert!(is_text_mime("text/html"));
        assert!(is_text_mime("application/javascript"));
        assert!(is_text_mime("image/svg+xml"));
        assert!(!is_text_mime("text/event-stream"));
        assert!(!is_text_mime("application/octet-stream"));
        assert!(!is_text_mime("image/png"));
    }

    /// 压缩结果能被标准 gzip 解回原文（编码格式本身没写错）
    #[test]
    fn gzip_round_trips() {
        let payload = b"{\"models\":[".to_vec()
            .into_iter()
            .chain(std::iter::repeat(b'x').take(4096))
            .chain(b"]}".iter().copied())
            .collect::<Vec<u8>>();
        let compressed = gzip_bytes(&payload).expect("压缩成功");
        assert!(compressed.len() < payload.len(), "文本应当压得动");
        let mut decoder = GzDecoder::new(compressed.as_slice());
        let mut restored = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut restored).expect("解压成功");
        assert_eq!(restored, payload);
    }

    /// 200 + 文本 + 无 no-transform 才进压缩分支；已编码与非 200 一律跳过
    #[test]
    fn compressible_checks_the_guard_rails() {
        let json = || {
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::empty())
                .expect("构造响应")
        };
        assert!(compressible(&json()));
        let mut already = json();
        already.headers_mut().insert(
            header::CONTENT_ENCODING,
            HeaderValue::from_static("br"),
        );
        assert!(!compressible(&already));
        let mut no_transform = json();
        no_transform.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, no-transform"),
        );
        assert!(!compressible(&no_transform));
        let not_found = Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::empty())
            .expect("构造响应");
        assert!(!compressible(&not_found));
    }

    /// 中间件真的会压缩一条大 JSON（端到端，而不只是测纯函数）
    #[tokio::test]
    async fn gzip_middleware_compresses_a_large_json_response() {
        use axum::body::to_bytes as read_body;
        use axum::http::Request as HttpRequest;
        use tower::ServiceExt;

        let app = axum::Router::new()
            .route(
                "/probe",
                axum::routing::get(|| async {
                    let body = "x".repeat(4096);
                    (
                        [(header::CONTENT_TYPE, "application/json")],
                        format!("{{\"data\":\"{body}\"}}"),
                    )
                }),
            )
            .layer(axum::middleware::from_fn(gzip));
        let request = HttpRequest::builder()
            .uri("/probe")
            .header(header::ACCEPT_ENCODING, "gzip")
            .body(Body::empty())
            .expect("构造请求");
        let response = app.clone().oneshot(request).await.expect("请求成功");
        assert_eq!(
            response.headers().get(header::CONTENT_ENCODING).and_then(|v| v.to_str().ok()),
            Some("gzip")
        );
        let bytes = read_body(response.into_body(), MAX_SIZE).await.expect("读正文");
        let mut decoder = GzDecoder::new(bytes.as_ref());
        let mut restored = String::new();
        std::io::Read::read_to_string(&mut decoder, &mut restored).expect("解压成功");
        assert!(restored.starts_with("{\"data\":\"xxxx"));

        // 不带 Accept-Encoding 的调用（JNI 进程内直调就是这种）不压，
        // 但仍要带 Vary，避免中间缓存串味
        let plain = HttpRequest::builder()
            .uri("/probe")
            .body(Body::empty())
            .expect("构造请求");
        let plain_response = app.oneshot(plain).await.expect("请求成功");
        assert!(!plain_response.headers().contains_key(header::CONTENT_ENCODING));
        assert!(plain_response
            .headers()
            .get(header::VARY)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("Accept-Encoding")));
    }
}
