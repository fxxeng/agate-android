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
//! ── 服务这一层不是本模块写的 ────────────────────────────────
//! 读请求 / 解析 query / 写响应都交给 `core::loopback`（那里定死了"本机回环
//! 服务器怎么跑"这一件事）。本模块只负责**路由与业务**：哪条路径对应哪个上游
//! 接口、请求体怎么解、结果怎么送出去。曾在这里手搓过一份 HTTP，它与 Kiro
//! 那一份逐字重名却又各自缺一点东西（`+` 解空格、chunked、keep-alive）——
//! 现在只有一种做法。
//!
//! ── 安全 ────────────────────────────────────────────────────
//! 只绑回环；每次登录一个随机 `token`（URL 的 `?t=`，也是请求头
//! `x-login-token`），由一层**统一的闸**校验，防止本机其它进程误打；
//! 密码只在内存里转发，不落盘、不写日志。
//!
//! ── 收尾为什么走 channel ────────────────────────────────────
//! 本模块**不认识** `LoginService`（provider 反向依赖登录服务会把两层的边界
//! 搅混）。它只把 `auth::Session` 通过一个 channel 送出去，由调用方决定怎么
//! 落账号、怎么标任务完成。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::collections::HashMap;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::server::core::loopback;
use crate::server::core::proxies::ResolvedProxy;

use super::auth;

/// 内嵌的登录页（HTML + 极验 v3 初始化 + 表单逻辑，与上游逐字相同）。
const PAGE: &str = include_str!("login_page.html");

/// 一次登录的最长等待（与客户端 `AuthWebViewSheet` 的 5 分钟一致 ——
/// 服务端比客户端等得久没有意义，那头早就报超时了）。
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// 一次登录页的进程内状态（路由状态、凭证闸与结果通道共用这一份）。
///
/// 三样东西同生共死：页面凭证、代理设置、结果通道 —— 拆成几个参数传会让每个
/// handler 的签名都长一截，而它们本来只在同一次登录里有效。
#[derive(Clone)]
struct Ctx {
    token: String,
    proxy: Option<ResolvedProxy>,
    tx: mpsc::Sender<Result<auth::Session, String>>,
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
    let (listener, port) = loopback::bind_any().await?;
    let token = random_token();
    let (tx, rx) = mpsc::channel(1);
    let state = Ctx {
        token: token.clone(),
        proxy,
        tx,
    };
    let handle = loopback::serve(listener, router(state)).await;
    // 兜底：调用方彻底不管时也不能永远开着端口
    let timeout_abort = handle.clone();
    crate::spawn_task(async move {
        tokio::time::sleep(LOGIN_TIMEOUT).await;
        timeout_abort.abort();
    });
    Ok((format!("http://127.0.0.1:{port}/?t={token}"), rx, handle))
}

fn router(state: Ctx) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/api/geetest", get(geetest))
        .route("/api/send-code", post(send_code))
        .route("/api/login-sms", post(login_sms))
        .route("/api/login-password", post(login_password))
        // 一次登录一个端口，页面只发几 KB：上限跟着共享常量走，别各自定
        .layer(DefaultBodyLimit::max(loopback::MAX_BODY_BYTES))
        // 凭证闸做成一层，而不是每个 handler 开头复制一遍 —— 少一处就多一条
        // 「某个接口忘了校验」的后门
        .route_layer(middleware::from_fn_with_state(state.clone(), gate))
        .with_state(state)
}

/// 页面凭证闸：查询串 `t` 或请求头 `x-login-token`，缺一个都不放行。
async fn gate(
    State(state): State<Ctx>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if authorized(&state, &query, &headers) {
        next.run(request).await
    } else {
        failed(StatusCode::FORBIDDEN, "登录页凭证不正确，请重新发起登录")
    }
}

/// 闸的判据（抽出来是为了能离线测：这一条以前没有任何用例兜着）。
fn authorized(state: &Ctx, query: &HashMap<String, String>, headers: &HeaderMap) -> bool {
    let provided = query
        .get("t")
        .map(String::as_str)
        .or_else(|| headers.get("x-login-token").and_then(|value| value.to_str().ok()))
        .unwrap_or("");
    provided == state.token
}

async fn index() -> Html<&'static str> {
    Html(PAGE)
}

async fn geetest(State(state): State<Ctx>) -> Response {
    match auth::geetest_config(state.proxy.as_ref()).await {
        Ok(config) => axum::Json(config).into_response(),
        Err(message) => failed(StatusCode::BAD_GATEWAY, &message),
    }
}

async fn send_code(State(state): State<Ctx>, body: Bytes) -> Response {
    let Some(payload) = parse_body(&body) else {
        return failed(StatusCode::BAD_REQUEST, "请求内容不是 JSON");
    };
    let mobile = text_of(&payload, "mobile");
    if !auth::valid_mobile(&mobile) {
        return failed(StatusCode::BAD_REQUEST, "请输入 11 位手机号");
    }
    let Some(captcha) = captcha_of(&payload) else {
        return failed(StatusCode::BAD_REQUEST, "请先完成滑块验证");
    };
    match auth::send_sms(state.proxy.as_ref(), &mobile, &captcha).await {
        Ok(()) => axum::Json(json!({ "ok": true })).into_response(),
        Err(message) => failed(StatusCode::BAD_GATEWAY, &message),
    }
}

async fn login_sms(State(state): State<Ctx>, body: Bytes) -> Response {
    let Some(payload) = parse_body(&body) else {
        return failed(StatusCode::BAD_REQUEST, "请求内容不是 JSON");
    };
    let mobile = text_of(&payload, "mobile");
    let code = text_of(&payload, "verifyCode");
    if !auth::valid_mobile(&mobile) {
        return failed(StatusCode::BAD_REQUEST, "请输入 11 位手机号");
    }
    if !auth::valid_code(&code) {
        return failed(StatusCode::BAD_REQUEST, "请输入 6 位验证码");
    }
    match auth::login_sms(state.proxy.as_ref(), &mobile, &code).await {
        Ok(session) => finish(&state.tx, session, "sms").await,
        Err(message) => failed(StatusCode::BAD_GATEWAY, &message),
    }
}

async fn login_password(State(state): State<Ctx>, body: Bytes) -> Response {
    let Some(payload) = parse_body(&body) else {
        return failed(StatusCode::BAD_REQUEST, "请求内容不是 JSON");
    };
    let account = text_of(&payload, "accountName");
    // 密码按原样提交（前后空格可能是有意义的字符）
    let password = payload
        .get("accountPwd")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if account.is_empty() {
        return failed(StatusCode::BAD_REQUEST, "请输入账号");
    }
    if password.len() < 6 || password.len() > 20 {
        return failed(StatusCode::BAD_REQUEST, "密码长度必须为 6–20 位");
    }
    let Some(captcha) = captcha_of(&payload) else {
        return failed(StatusCode::BAD_REQUEST, "请先完成滑块验证");
    };
    match auth::login_password(state.proxy.as_ref(), &account, &password, &captcha).await {
        Ok(session) => finish(&state.tx, session, "password").await,
        Err(message) => failed(StatusCode::BAD_GATEWAY, &message),
    }
}

fn failed(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({ "error": message }))).into_response()
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
    axum::Json(json!({ "ok": true })).into_response()
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

    fn state_with(token: &str) -> Ctx {
        // 通道的接收端没人取：这一条只验闸的判据，不走到 `finish`
        let (tx, _rx) = mpsc::channel(1);
        Ctx {
            token: token.to_string(),
            proxy: None,
            tx,
        }
    }

    /// 页面凭证是 48 位 hex 且不可重复
    #[test]
    fn the_page_token_is_random_hex() {
        let token = random_token();
        assert_eq!(token.len(), 48);
        assert!(token.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_ne!(token, random_token());
    }

    /// 凭证闸：查询串与请求头两条都要认，错的一个都不能放
    ///
    /// 这一条以前不存在 —— 手搓那版把判据写在 handler 里，测不到；而它保护的
    /// 是「同机其它进程能不能替用户完成一次登录」。
    #[test]
    fn only_the_right_token_gets_through() {
        let state = state_with("abc123");
        let mut query = HashMap::new();
        query.insert("t".to_string(), "abc123".to_string());
        assert!(authorized(&state, &query, &HeaderMap::new()));

        let wrong = HashMap::from([("t".to_string(), "abc124".to_string())]);
        assert!(!authorized(&state, &wrong, &HeaderMap::new()));
        assert!(!authorized(&state, &HashMap::new(), &HeaderMap::new()));

        // 页面里 `fetch('/api/x?t=…')` 走查询串；请求头这条是给脚本调试留的
        let mut headers = HeaderMap::new();
        headers.insert("x-login-token", axum::http::HeaderValue::from_static("abc123"));
        assert!(authorized(&state, &HashMap::new(), &headers));
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
