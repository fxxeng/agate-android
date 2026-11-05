//! AStudio 的本机登录页：手机号验证码 / 账号密码（含极验滑块）。
//!
//! ── 为什么需要一个页面 ──────────────────────────────────────
//! 发短信验证码与账号密码登录都要极验的
//! `geetest_challenge / validate / seccode`，而极验必须在真实浏览器里完成。
//! 所以起一个只监听 `127.0.0.1` 的小服务，页面里加载极验脚本、完成滑块后把
//! 结果 POST 回来，服务端再转成上游的 multipart 请求（见 `auth.rs`）。
//!
//! ── 为什么是「我们自己起页面」而不是打开外站 ────────────────
//! 上游登录页是**自托管**的：`login_page.html` 是随包里发的一段静态页，
//! 它只依赖 `static.geetest.com` 的脚本。因此 Android 上不需要内嵌 WebView
//! 去访问某个第三方站点 —— 网关本来就跑在手机上，`127.0.0.1` 直接可达。
//! 客户端现有的 `AuthWebViewSheet` 打开这个地址即可（`http(s)` 一律放行，
//! 且 `network_security_config.xml` 本来就为内核回环放行了明文）。
//!
//! ── 与 `core::login_listener` 不是一回事 ────────────────────
//! 那个只负责「Core 态占住网关端口，让 OAuth 回调有地方落」，不是能挂路由的
//! HTTP 框架。本模块是**一次登录一个临时端口**（`bind(("127.0.0.1", 0))`），
//! 登录结束就随任务一起关掉 —— 端口数不随登录次数累积。
//!
//! ── 安全 ────────────────────────────────────────────────────
//! 只绑回环；每次登录一个随机 `token`（URL 的 `?t=`，API 也校验），防止本机
//! 其它进程误打；密码只在内存里转发，不落盘、不写日志。
//!
//! ── 收尾为什么走 channel ────────────────────────────────────
//! 本模块**不认识** `LoginService`（provider 反向依赖登录服务会把两层的边界
//! 搅混）。它只把 `auth::Session` 通过一个 oneshot 送出去，由调用方决定怎么
//! 落账号、怎么标任务完成。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::server::core::proxies::ResolvedProxy;

use super::auth;

/// 内嵌的登录页（HTML + 极验 v3 初始化 + 表单逻辑，与上游逐字相同）。
const PAGE: &str = include_str!("login_page.html");

/// 一次登录的最长等待（与客户端 `AuthWebViewSheet` 的 5 分钟一致 ——
/// 服务端比客户端等得久没有意义，那头早就报超时了）。
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// 请求体上限（登录页只发几 KB 的 JSON；64KB 足够且有明确上限）
const MAX_BODY_BYTES: usize = 64 * 1024;

/// 一次登录页的进程内状态。
struct PageState {
    token: String,
    proxy: Option<ResolvedProxy>,
}

/// 起一个本机登录页，返回 `(页面地址, 结果通道, 关闭句柄)`。
///
/// 通道只送**一次**东西：登录成功给 `Ok(Session)`，页面报错给 `Err(说明)`。
/// 发送端在成功送出后即可丢弃，接收端不必判断「还会不会再来」。
///
/// 关闭句柄交给调用方：一次登录一个端口，拿到结果（或放弃）就该立刻关掉，
/// 不该留到超时那一步 —— 否则「取消一次登录」会在本机多占 5 分钟端口。
pub async fn start(
    proxy: Option<ResolvedProxy>,
) -> Result<
    (
        String,
        mpsc::Receiver<Result<auth::Session, String>>,
        tokio::task::AbortHandle,
    ),
    String,
> {
    // 端口交给系统分配：固定端口会在「上一次登录还没收尾」时撞车
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|error| format!("本机登录页启动失败：{error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("本机登录页启动失败：{error}"))?
        .port();
    let token = random_token();
    let state = Arc::new(PageState {
        token: token.clone(),
        proxy,
    });
    let (tx, rx) = mpsc::channel(1);
    let handle = tokio::spawn(serve(listener, state, tx));
    let abort = handle.abort_handle();
    // 兜底：调用方彻底不管时也不能永远开着端口
    let timeout_abort = abort.clone();
    tokio::spawn(async move {
        tokio::time::sleep(LOGIN_TIMEOUT).await;
        timeout_abort.abort();
    });
    Ok((format!("http://127.0.0.1:{port}/?t={token}"), rx, abort))
}

async fn serve(
    listener: tokio::net::TcpListener,
    state: Arc<PageState>,
    tx: mpsc::Sender<Result<auth::Session, String>>,
) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            break;
        };
        let state = state.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let response = match read_request(&mut stream).await {
                Some(request) => route(&state, request, &tx).await,
                None => error_response(400, "请求格式不正确"),
            };
            let _ = write_response(&mut stream, &response).await;
        });
    }
}

struct Request {
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// 读一个完整的 HTTP/1.1 请求（登录页只发小请求，够用即可）。
async fn read_request(stream: &mut tokio::net::TcpStream) -> Option<Request> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(position) = find(&buffer, b"\r\n\r\n") {
            break position;
        }
        if buffer.len() > MAX_BODY_BYTES {
            return None;
        }
        match stream.read(&mut chunk).await {
            Ok(0) => return None,
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
            Err(_) => return None,
        }
    };
    let header_text = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = header_text.lines();
    let first = lines.next()?;
    let mut parts = first.split_whitespace();
    let _method = parts.next()?;
    let target = parts.next()?;
    let mut headers = HashMap::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
        .min(MAX_BODY_BYTES);
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < length {
        match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(count) => body.extend_from_slice(&chunk[..count]),
            Err(_) => break,
        }
    }
    body.truncate(length);
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), parse_query(query)),
        None => (target.to_string(), HashMap::new()),
    };
    Some(Request {
        path,
        query,
        headers,
        body,
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        out.insert(percent_decode(key), percent_decode(value));
    }
    out
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                out.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

struct Response {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

fn json_response(status: u16, value: &Value) -> Response {
    Response {
        status,
        content_type: "application/json; charset=utf-8",
        body: serde_json::to_vec(value).unwrap_or_default(),
    }
}

fn error_response(status: u16, message: &str) -> Response {
    json_response(status, &json!({ "error": message }))
}

async fn write_response(
    stream: &mut tokio::net::TcpStream,
    response: &Response,
) -> std::io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        502 => "Bad Gateway",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        reason,
        response.content_type,
        response.body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&response.body).await?;
    stream.flush().await
}

async fn route(
    state: &PageState,
    request: Request,
    tx: &mpsc::Sender<Result<auth::Session, String>>,
) -> Response {
    // 页面凭证：查询串 `t` 或请求头 `x-login-token`，缺一个都不放行
    let provided = request
        .query
        .get("t")
        .cloned()
        .or_else(|| request.headers.get("x-login-token").cloned())
        .unwrap_or_default();
    if provided != state.token {
        return error_response(403, "登录页凭证不正确，请重新发起登录");
    }
    match request.path.as_str() {
        "/" | "/index.html" => Response {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: PAGE.as_bytes().to_vec(),
        },
        "/api/geetest" => match auth::geetest_config(state.proxy.as_ref()).await {
            Ok(config) => json_response(200, &config),
            Err(message) => error_response(502, &message),
        },
        "/api/send-code" => {
            let Some(payload) = parse_body(&request.body) else {
                return error_response(400, "请求内容不是 JSON");
            };
            let mobile = text_of(&payload, "mobile");
            if !auth::valid_mobile(&mobile) {
                return error_response(400, "请输入 11 位手机号");
            }
            let Some(captcha) = captcha_of(&payload) else {
                return error_response(400, "请先完成滑块验证");
            };
            match auth::send_sms(state.proxy.as_ref(), &mobile, &captcha).await {
                Ok(()) => json_response(200, &json!({ "ok": true })),
                Err(message) => error_response(502, &message),
            }
        }
        "/api/login-sms" => {
            let Some(payload) = parse_body(&request.body) else {
                return error_response(400, "请求内容不是 JSON");
            };
            let mobile = text_of(&payload, "mobile");
            let code = text_of(&payload, "verifyCode");
            if !auth::valid_mobile(&mobile) {
                return error_response(400, "请输入 11 位手机号");
            }
            if !auth::valid_code(&code) {
                return error_response(400, "请输入 6 位验证码");
            }
            match auth::login_sms(state.proxy.as_ref(), &mobile, &code).await {
                Ok(session) => finish(tx, session, "sms").await,
                Err(message) => error_response(502, &message),
            }
        }
        "/api/login-password" => {
            let Some(payload) = parse_body(&request.body) else {
                return error_response(400, "请求内容不是 JSON");
            };
            let account = text_of(&payload, "accountName");
            // 密码按原样提交（前后空格可能是有意义的字符）
            let password = payload
                .get("accountPwd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if account.is_empty() {
                return error_response(400, "请输入账号");
            }
            if password.len() < 6 || password.len() > 20 {
                return error_response(400, "密码长度必须为 6–20 位");
            }
            let Some(captcha) = captcha_of(&payload) else {
                return error_response(400, "请先完成滑块验证");
            };
            match auth::login_password(state.proxy.as_ref(), &account, &password, &captcha).await {
                Ok(session) => finish(tx, session, "password").await,
                Err(message) => error_response(502, &message),
            }
        }
        _ => error_response(404, "没有这个接口"),
    }
}

/// 登录成功：把会话送出，页面显示「完成」。
///
/// 送出失败（接收端已经不在，比如任务被取消 / 超时）不算页面的错：页面照常
/// 显示完成，用户看到的是客户端那句「登录已取消」。
async fn finish(
    tx: &mpsc::Sender<Result<auth::Session, String>>,
    mut session: auth::Session,
    method: &str,
) -> Response {
    // 记下走的是哪条：账号记录的 `source` 要如实反映「验证码加的」还是
    // 「密码加的」，而不是笼统一句「登录」
    session.method = method.to_string();
    crate::server::logging::log(
        "[Login]",
        &format!(
            "✅ AStudio {}登录成功（{}）",
            if method == "sms" { "验证码" } else { "密码" },
            session.display_name()
        ),
    );
    let _ = tx.send(Ok(session)).await;
    json_response(200, &json!({ "ok": true }))
}

fn parse_body(body: &[u8]) -> Option<Value> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .filter(Value::is_object)
}

fn text_of(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn captcha_of(value: &Value) -> Option<auth::Captcha> {
    let object = value.get("captcha")?;
    let pick = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let captcha = auth::Captcha {
        challenge: pick("geetest_challenge"),
        validate: pick("geetest_validate"),
        seccode: pick("geetest_seccode"),
    };
    captcha.is_complete().then_some(captcha)
}

/// 48 位 hex 的页面凭证（24 字节随机）。
///
/// 强度按「本机同用户下的其它进程猜不到」定：不需要抗离线爆破，但必须不可
/// 预测 —— 否则同机的任何程序都能替用户完成一次登录并拿到账号。
fn random_token() -> String {
    let mut bytes = [0u8; 24];
    if getrandom::getrandom(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        bytes[..16].copy_from_slice(&nanos.to_le_bytes());
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 页面凭证是 48 位 hex 且不可重复
    #[test]
    fn the_page_token_is_random_hex() {
        let token = random_token();
        assert_eq!(token.len(), 48);
        assert!(token.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_ne!(token, random_token());
    }

    /// 查询串要能解出 `t`（页面就是靠它带凭证的）
    #[test]
    fn the_query_string_is_decoded() {
        let query = parse_query("t=abc123&x=%E4%B8%AD");
        assert_eq!(query.get("t").map(String::as_str), Some("abc123"));
        assert_eq!(query.get("x").map(String::as_str), Some("中"));
        // 残缺的百分号转义原样保留，不丢字符
        assert_eq!(percent_decode("a%2"), "a%2");
    }

    /// 极验三件缺一件就不算完成（页面只在滑块成功后才带全）
    #[test]
    fn a_partial_captcha_payload_is_rejected() {
        let full = json!({
            "captcha": {
                "geetest_challenge": "c",
                "geetest_validate": "v",
                "geetest_seccode": "s"
            }
        });
        assert!(captcha_of(&full).is_some());
        let partial = json!({ "captcha": { "geetest_challenge": "c", "geetest_validate": "v" } });
        assert!(captcha_of(&partial).is_none());
        assert!(captcha_of(&json!({})).is_none());
    }

    /// 请求体必须是 JSON 对象（数组 / 字符串不算）
    #[test]
    fn the_body_must_be_a_json_object() {
        assert!(parse_body(br#"{"a":1}"#).is_some());
        assert!(parse_body(br#"[1,2]"#).is_none());
        assert!(parse_body(b"not json").is_none());
    }
}
