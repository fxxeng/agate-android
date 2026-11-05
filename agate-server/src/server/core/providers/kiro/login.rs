//! Kiro 浏览器登录：门户 OAuth 四条分支，回调落在本机 loopback 端口。
//!
//! ── 流程（上游 `login.rs` 的逐条对应）────────────────────────
//! ```text
//! ① 抢一条回调端口（官方 IDE 那组白名单）+ 造 state / PKCE
//! ② 打开 https://app.kiro.dev/signin?…
//! ③ 用户在门户里选登录方式：
//!      Google / GitHub  → 带回 code → POST prod.us-east-1.auth.desktop.kiro.dev/oauth/token
//!      Builder ID / IDC → 带回 AWS 登录地址与地区 → 注册 AWS OIDC 客户端 → 302 去 AWS
//!      AWS 回来         → POST oidc.{region}.amazonaws.com/token
//! ④ 拿到 token 后问一次 GetUsageLimits 取邮箱与套餐，落成账号
//! ```
//!
//! ── 为什么必须走系统浏览器 ──────────────────────────────────
//! Google 与 GitHub 都禁止在 WebView 内完成 OAuth（回
//! `disallowed_useragent`），所以通道只能是 `EXTERNAL`。这一条也决定了本模块
//! 的形态：回调要落在一台**真的 HTTP 服务**上，而不是等壳侧拦截 WebView 跳转。
//!
//! ── 与 Trae 那台监听器的差别 ────────────────────────────────
//! Trae 只**收**一个 query 就收尾（它没有 state，端口就是身份）。这里不行：
//!   * 端口不能随机分配，必须是门户白名单里的那几条；
//!   * AWS 那一段是**服务端跳转** —— 浏览器带着门户给的 `login_option` 回来，
//!     我们要注册客户端再把它 302 送去 AWS，而不是收下就了事。
//! 所以这里是一台**能按路径分派、能回 302 与 HTML** 的小服务（`core::loopback`）。
//!
//! ── 门户会重复跳回来 ────────────────────────────────────────
//! 从 AWS 那一步回来时，门户可能又带同一个 `login_option` 打一次
//! `/signin/callback`。这时**不能重新注册客户端**（注册第二次会让第一次那条
//! 授权地址作废，用户停在 AWS 页面上点下去必然失败），而要把他送回已经算好的
//! 那条 AWS 地址 —— 所以待办里记着 `portal_asked`（门户这次问了什么），
//! 原样再问一次就直接 `302` 回旧的 `authorize`。
//!
//! ── `external_idp` 为什么不支持 ────────────────────────────
//! 公司自建身份提供商要先把 issuer 拿去换一套客户端注册，上游自己也没做完
//! （它回一句"暂不支持，请用 kiro-cli 导入"）。这里照它的处置给同一句文案，
//! 而不是给一个走到一半才失败的按钮。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use crate::server::core::hostinfo::hostname;
use crate::server::core::loopback;
use crate::server::core::proxies::ResolvedProxy;

use super::credentials::{self, KiroCredentials};
use super::{endpoints, http, refresh, usage, AUTH_SERVICE, CALLBACK_PORTS, PORTAL, SCOPES};

/// 门户跳回来的路径。
const PORTAL_PATH: &str = "/signin/callback";
/// AWS 跳回来的路径（注册客户端时用的就是它）。
const AWS_PATH: &str = "/oauth/callback";

/// 换码请求的超时。
const POST_TIMEOUT: Duration = Duration::from_secs(30);

/// 一轮登录里「已经送去 AWS、等它回来」的那一半状态。
#[derive(Clone)]
struct Aws {
    region: String,
    client_id: String,
    client_secret: String,
    /// 规范值：`builderid` / `enterprise` / `internal`
    provider: String,
    state: String,
    verifier: String,
    /// 算好的 AWS 授权地址（门户重复跳回来时直接再 302 一次）
    on_to_aws: String,
    /// 门户上一次问的 `login_option|issuer_url|idc_region`
    portal_asked: String,
}

/// 路由状态（一次登录一份）。
#[derive(Clone)]
struct Ctx {
    state: String,
    verifier: String,
    /// `http://localhost:<port>`：门户换码时要把当初的 redirect_uri 原样带上
    redirect: String,
    /// `http://127.0.0.1:<port>/oauth/callback`：AWS 换码同理
    aws_redirect: String,
    proxy: Option<ResolvedProxy>,
    pending: Arc<Mutex<Option<Aws>>>,
    tx: mpsc::Sender<Result<KiroCredentials, String>>,
}

/// 进行中的一轮登录（`Drop` 不释放端口 —— 收尾必须显式 [`Self::close`]）。
pub struct Session {
    pub auth_url: String,
    pub port: u16,
    results: tokio::sync::Mutex<mpsc::Receiver<Result<KiroCredentials, String>>>,
    abort: tokio::task::AbortHandle,
}

impl Session {
    /// 起一轮登录：先抢端口，再造 state / PKCE，最后拼门户地址。
    ///
    /// 顺序不能反：`redirect_uri` 里就是端口，地址发出去之后端口不能再变
    /// （变了回调会落到没人听的地方，用户看到的是浏览器连不上）。
    pub async fn begin(proxy: Option<ResolvedProxy>) -> Result<Self, String> {
        let (listener, port) = loopback::bind_from(&CALLBACK_PORTS).await?;
        let state = random_token(24);
        let verifier = random_token(48);
        let (tx, rx) = mpsc::channel(1);
        let ctx = Ctx {
            state: state.clone(),
            verifier: verifier.clone(),
            redirect: format!("http://localhost:{port}"),
            aws_redirect: format!("http://127.0.0.1:{port}{AWS_PATH}"),
            proxy,
            pending: Arc::new(Mutex::new(None)),
            tx,
        };
        let abort = loopback::serve(listener, router(ctx)).await;
        let challenge = pkce_challenge(&verifier);
        let auth_url = format!(
            "{PORTAL}/signin?state={}&code_challenge={}&code_challenge_method=S256&redirect_uri={}&redirect_from=KiroIDE",
            endpoints::query_escape(&state),
            endpoints::query_escape(&challenge),
            endpoints::query_escape(&format!("http://localhost:{port}")),
        );
        Ok(Self {
            auth_url,
            port,
            results: tokio::sync::Mutex::new(rx),
            abort,
        })
    }

    /// 主动结束这一轮：释放端口，并让正在等的 [`Self::complete`] 立刻退出。
    ///
    /// 取消 / 超时 / 完成三条路都要走到这里：会话被 `Arc` 共享着，只靠 `Drop`
    /// 还端口会拖到超时，而取消后立刻重试就会抢不到白名单里的那几条。
    pub fn close(&self) {
        self.abort.abort();
    }

    /// 等结果（**不**落库，落库由调用方做）。
    pub async fn complete(&self) -> Result<KiroCredentials, String> {
        let mut receiver = self.results.lock().await;
        match receiver.recv().await {
            Some(result) => result,
            None => Err("登录窗口已关闭，请重新发起".to_string()),
        }
    }
}

fn router(ctx: Ctx) -> Router {
    Router::new()
        .route(PORTAL_PATH, get(portal_step))
        .route(AWS_PATH, get(aws_step))
        .with_state(ctx)
}

async fn portal_step(State(ctx): State<Ctx>, Query(query): Query<HashMap<String, String>>) -> Response {
    route(ctx, query, false).await
}

async fn aws_step(State(ctx): State<Ctx>, Query(query): Query<HashMap<String, String>>) -> Response {
    route(ctx, query, true).await
}

/// 一条回调的完整处置。
async fn route(
    ctx: Ctx,
    query: HashMap<String, String>,
    aws_path: bool,
) -> Response {
    let got = query.get("state").cloned().unwrap_or_default();
    let pending = ctx
        .pending
        .lock()
        .ok()
        .and_then(|guard| guard.clone());
    let from_portal = got == ctx.state;
    let from_aws =
        pending.as_ref().is_some_and(|aws| !aws.state.is_empty() && aws.state == got);
    // 认不出是谁的回调：本机端口谁都能打，不能拿一次性授权材料去换码
    if !from_portal && !from_aws {
        return loopback::page(
            false,
            "这个地址不属于本次登录",
            "请回到刚打开的登录页重新操作，或重新发起登录。",
        )
        .into_response();
    }
    if let Some(error) = query.get("error") {
        let detail = query
            .get("error_description")
            .cloned()
            .unwrap_or_else(|| error.clone());
        return fail(&ctx, &detail).await;
    }

    if from_aws {
        let Some(aws) = pending.clone() else {
            return fail(&ctx, "AWS 回调缺少登录状态").await;
        };
        if !aws_path {
            return loopback::page(false, "这个地址不属于本次登录", "").into_response();
        }
        let Some(code) = query.get("code").cloned().filter(|value| !value.is_empty()) else {
            return fail(&ctx, "AWS 没有带回授权码").await;
        };
        return exchange_aws(&ctx, &aws, &code).await;
    }

    // ── 门户那一步：按用户点的方式分派 ──────────────────────
    let option = query.get("login_option").cloned().unwrap_or_default();
    let asked = format!(
        "{}|{}|{}",
        option,
        query.get("issuer_url").cloned().unwrap_or_default(),
        query.get("idc_region").cloned().unwrap_or_default()
    );
    // 门户重复问同一件事 = 上一次已经算好了 AWS 地址，别再注册一次
    // （注册第二次会让第一条 authorize 地址作废，用户停在 AWS 页面上点下去
    // 必然失败）。到这里已经是门户那一步，AWS 回调在上面的分支里返回过了。
    if let Some(aws) = &pending {
        if !aws.on_to_aws.is_empty() && asked == aws.portal_asked {
            return axum::response::Redirect::to(&aws.on_to_aws).into_response();
        }
    }
    match option.as_str() {
        "google" | "github" => {
            let Some(code) = query.get("code").cloned().filter(|value| !value.is_empty()) else {
                return fail(&ctx, "Kiro 登录页没有带回授权码").await;
            };
            // 用户改选了社交登录：把上一轮 AWS 的待办清掉，免得那条残留状态
            // 在后续请求里仍被判成「这一轮还在等 AWS」
            if let Ok(mut guard) = ctx.pending.lock() {
                *guard = None;
            }
            exchange_social(&ctx, &option, &code).await
        }
        "builderid" | "awsidc" | "internal" => {
            let Some(issuer) = query.get("issuer_url").cloned() else {
                return fail(&ctx, "Kiro 登录页没有说明 AWS 登录地址").await;
            };
            let Some(region) = query.get("idc_region").cloned() else {
                return fail(&ctx, "Kiro 登录页没有说明 AWS 地区").await;
            };
            // 地区要拼进 AWS 的主机名、issuer 要被我们 302 过去：两条都先判形态
            if !endpoints::is_aws_region(&region) {
                return fail(&ctx, "Kiro 登录页给的 AWS 地区不合法").await;
            }
            if !endpoints::is_https_url(&issuer) {
                return fail(&ctx, "Kiro 登录页给的 AWS 登录地址不是 https").await;
            }
            let provider = match option.as_str() {
                "builderid" => "builderid",
                "awsidc" => "enterprise",
                _ => "internal",
            }
            .to_string();
            match start_aws(&ctx, &provider, &issuer, &region, &asked).await {
                Ok(aws) => {
                    let url = aws.on_to_aws.clone();
                    if let Ok(mut guard) = ctx.pending.lock() {
                        *guard = Some(aws);
                    }
                    axum::response::Redirect::to(&url).into_response()
                }
                Err(message) => fail(&ctx, &message).await,
            }
        }
        "external_idp" => {
            fail(
                &ctx,
                "公司自建身份提供商的浏览器登录暂不支持；请先在 kiro-cli 里登录，再用「粘贴凭证」导入",
            )
            .await
        }
        other => fail(&ctx, &format!("Kiro 登录页返回了不认识的登录方式（{other}）")).await,
    }
}

/// Google / GitHub：单次 PKCE 换码。
async fn exchange_social(ctx: &Ctx, option: &str, code: &str) -> Response {
    let body = json!({
        "code": code,
        "code_verifier": ctx.verifier,
        // 当初发出去的 redirect_uri 必须**逐字**带上，门户按它校验 PKCE
        "redirect_uri": format!("{}{}?login_option={}", ctx.redirect, PORTAL_PATH, option),
    });
    let url = format!("{AUTH_SERVICE}/oauth/token");
    let token = match post_json(&ctx.proxy, &url, &body).await {
        Ok(token) => token,
        Err(message) => return fail(ctx, &message).await,
    };
    let Some(access) = token.get("accessToken").and_then(Value::as_str).filter(|value| !value.is_empty()) else {
        return fail(ctx, "Kiro 没有返回 token").await;
    };
    let credentials = KiroCredentials {
        access: access.to_string(),
        refresh: text_of(&token, "refreshToken"),
        expires: expiry(&token, "expiresIn"),
        method: "social".to_string(),
        login_provider: option.to_string(),
        region: "us-east-1".to_string(),
        profile: text_of(&token, "profileArn"),
        ..Default::default()
    };
    finish(ctx, credentials).await
}

/// AWS 那一段：注册客户端 → 造 authorize 地址（浏览器随后被 302 过去）。
///
/// `asked` 是门户这次问的三元组，原样记下来 —— 门户重复跳回来时靠它判断
/// 「问的是不是同一件事」，从而复用这条地址而不是重新注册客户端。
async fn start_aws(
    ctx: &Ctx,
    provider: &str,
    issuer: &str,
    region: &str,
    asked: &str,
) -> Result<Aws, String> {
    let body = json!({
        "clientName": "Kiro IDE",
        "clientType": "public",
        "scopes": SCOPES,
        "grantTypes": ["authorization_code", "refresh_token"],
        // 不带端口：AWS 对 native client 的回环地址按 RFC 8252 放行任意端口，
        // 而我们的端口是从白名单里抢的（不是固定的）。换成带端口的写法反而
        // 会让「抢到第二条端口」的那次注册失败。
        "redirectUris": ["http://127.0.0.1/oauth/callback"],
        "issuerUrl": issuer,
    });
    let url = format!("{}/client/register", endpoints::oidc(region));
    let registration = post_json(&ctx.proxy, &url, &body).await?;
    let client_id = text_of(&registration, "clientId");
    if client_id.is_empty() {
        return Err("AWS 没有注册出客户端".to_string());
    }
    let client_secret = text_of(&registration, "clientSecret");
    let state = random_token(24);
    let verifier = random_token(48);
    let challenge = pkce_challenge(&verifier);
    let on_to_aws = format!(
        "{}/authorize?response_type=code&client_id={}&redirect_uri={}&scopes={}&state={}&code_challenge={}&code_challenge_method=S256",
        endpoints::oidc(region),
        endpoints::query_escape(&client_id),
        endpoints::query_escape(&ctx.aws_redirect),
        endpoints::query_escape(&SCOPES.join(",")),
        endpoints::query_escape(&state),
        endpoints::query_escape(&challenge),
    );
    Ok(Aws {
        region: region.to_string(),
        client_id,
        client_secret,
        provider: provider.to_string(),
        state,
        verifier,
        on_to_aws,
        portal_asked: asked.to_string(),
    })
}

/// AWS 换码（浏览器带着 AWS 的授权码回来）。
async fn exchange_aws(ctx: &Ctx, aws: &Aws, code: &str) -> Response {
    let body = json!({
        "clientId": aws.client_id,
        "clientSecret": aws.client_secret,
        "grantType": "authorization_code",
        "redirectUri": ctx.aws_redirect,
        "code": code,
        "codeVerifier": aws.verifier,
    });
    let url = format!("{}/token", endpoints::oidc(&aws.region));
    let token = match post_json(&ctx.proxy, &url, &body).await {
        Ok(token) => token,
        Err(message) => return fail(ctx, &message).await,
    };
    let Some(access) = token.get("accessToken").and_then(Value::as_str).filter(|value| !value.is_empty()) else {
        return fail(ctx, "AWS 没有返回 token").await;
    };
    let credentials = KiroCredentials {
        access: access.to_string(),
        refresh: text_of(&token, "refreshToken"),
        expires: expiry(&token, "expiresIn"),
        method: "idc".to_string(),
        login_provider: aws.provider.clone(),
        region: aws.region.clone(),
        client_id: aws.client_id.clone(),
        client_secret: aws.client_secret.clone(),
        ..Default::default()
    };
    finish(ctx, credentials).await
}

/// 拿到凭证后补齐身份：profile ARN → 邮箱 → 套餐，然后交给调用方。
///
/// 三次收尾请求只有**最后一次必须成功**：profile 缺了还能在首次转发时补
/// （`chat.rs` 有那条），邮箱缺了只影响账号名，而**没有 token 就真的没法用**。
/// 所以前两步失败一律当"没问到"，不把一次已经花掉用户点击的登录判死。
async fn finish(ctx: &Ctx, mut credentials: KiroCredentials) -> Response {
    if credentials.profile.is_empty() {
        if let Ok(profile) = refresh::lookup_profile(&credentials, ctx.proxy.as_ref()).await {
            if !profile.is_empty() {
                credentials.profile = profile;
            }
        }
    }
    if let Ok(payload) = http::control(&credentials, http::OP_USAGE, &Value::Null, ctx.proxy.as_ref()).await {
        if let Some(email) = payload
            .pointer("/userInfo/email")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            credentials.account_id = email.to_string();
        }
        if let Some(title) = payload
            .pointer("/subscriptionInfo/subscriptionTitle")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            credentials.plan = usage::plan_name(title);
        }
    }
    crate::server::logging::log(
        "[Login]",
        &format!("✅ Kiro 浏览器登录完成（{}）", display_name(&credentials)),
    );
    let _ = ctx.tx.send(Ok(credentials)).await;
    loopback::page(true, "登录完成", "Kiro 账号已添加，可以关闭这个标签页了。").into_response()
}

async fn fail(ctx: &Ctx, message: &str) -> Response {
    crate::server::logging::log("[Login]", &format!("❌ Kiro 登录未完成：{message}"));
    let _ = ctx.tx.send(Err(message.to_string())).await;
    loopback::page(false, "登录没有完成", message).into_response()
}

/// 账号名：`邮箱 (登录方式)`，缺什么就退一级。
///
/// 与上游 `finish` 同一个口径：`uid` 为空时**不**加括号（否则会出现
/// 「a@b.com ()」这种名字），完全查不到就叫 `Kiro 账号`。
pub fn display_name(credentials: &KiroCredentials) -> String {
    if credentials.account_id.is_empty() {
        return "Kiro 账号".to_string();
    }
    if credentials.login_provider.is_empty() {
        return credentials.account_id.clone();
    }
    format!(
        "{} ({})",
        credentials.account_id,
        display_way(&credentials.login_provider)
    )
}

/// 规范值 → 账号名里的括号说明。
fn display_way(way: &str) -> &str {
    match way {
        "google" => "Google",
        "github" => "GitHub",
        "builderid" => "Builder ID",
        "enterprise" => "IAM Identity Center",
        "internal" => "Internal",
        other => other,
    }
}

/// 落库用的字段表（与「粘贴凭证」那条路是**同一套键**，见 `credentials`）。
pub fn payload_of(credentials: &KiroCredentials) -> Value {
    let mut map = Map::new();
    credentials.write_into(&mut map);
    Value::Object(map)
}

async fn post_json(proxy: &Option<ResolvedProxy>, url: &str, body: &Value) -> Result<Value, String> {
    let response = http::client(proxy.as_ref())
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("User-Agent", super::ide_user_agent(&hostname()))
        .timeout(POST_TIMEOUT)
        .json(body)
        .send()
        .await
        .map_err(|error| format!("登录请求发不出去：{}", http::describe(&error)))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(failure_text(status, &text));
    }
    serde_json::from_str(&text).map_err(|_| "登录返回不是有效 JSON".to_string())
}

/// 换码失败的文案：**带上 HTTP 状态码**。
///
/// 「登录被拒绝」这一句没有任何可操作信息 —— 403 是门户不认这次授权、429 是
/// 打太快、5xx 是 AWS 自己挂了，用户该做的处置完全不同。上游的消息不在时
/// 退到响应体前 160 字（AWS 有时回一页 HTML，至少能看出是哪一关）。
fn failure_text(status: u16, text: &str) -> String {
    let mut detail = String::new();
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        for key in ["error_description", "message", "error"] {
            if let Some(found) = value.get(key).and_then(Value::as_str) {
                if !found.trim().is_empty() {
                    detail = found.trim().to_string();
                    break;
                }
            }
        }
    }
    if detail.is_empty() {
        detail = text.trim().chars().take(160).collect();
    }
    if detail.is_empty() {
        format!("登录被拒绝（HTTP {status}）")
    } else {
        format!("登录被拒绝（HTTP {status}）：{detail}")
    }
}

fn text_of(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 毫秒时间戳；响应里没给有效期时按一小时（上游同一条）。
fn expiry(token: &Value, field: &str) -> i64 {
    let seconds = token.get(field).and_then(Value::as_i64).unwrap_or(0);
    credentials::now_ms() + if seconds > 0 { seconds } else { 3600 } * 1000
}

/// 随机串（base64url，无填充）。
fn random_token(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    if getrandom::getrandom(&mut buffer).is_err() {
        // 取不到真随机时也不 panic：铺一个按时间派生的串。安全性由「本机端口
        // 只有自己能打 + 一次性」兜着，这里的随机串只用来防串台。
        let seed = credentials::now_ms().to_string();
        buffer = seed.into_bytes();
        buffer.resize(bytes, b'0');
    }
    URL_SAFE_NO_PAD.encode(buffer)
}

/// PKCE 的 `code_challenge`（S256）。
fn pkce_challenge(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 中间态串的形态：base64url 无填充，长度随字节数走
    #[test]
    fn the_login_tokens_have_the_pkce_shape() {
        let verifier = random_token(48);
        assert_eq!(verifier.len(), 64, "48 字节 → 64 个 base64 字符");
        assert!(!verifier.contains('='), "不能带填充");
        assert!(
            verifier.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'),
            "只能是 base64url 字符：{verifier}"
        );
        assert_ne!(verifier, random_token(48));
        // challenge 是 verifier 的 sha256（RFC 7636 附录 B 的向量）
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    /// 失败文案必须带状态码与上游原因（只给「登录被拒绝」没有可操作信息）
    #[test]
    fn the_failure_text_stays_actionable() {
        let text = failure_text(403, r#"{"error_description":"user cancelled"}"#);
        assert!(text.contains("403"), "{text}");
        assert!(text.contains("user cancelled"), "{text}");
        // AWS 有时回一页 HTML：正文要截断后带出来，而不是空着
        let html = failure_text(502, "<html><body>Bad gateway</body></html>");
        assert!(html.contains("502"), "{html}");
        assert!(html.contains("Bad gateway"), "{html}");
        // 什么都没有时也不能出现一个孤零零的冒号
        assert_eq!(failure_text(429, ""), "登录被拒绝（HTTP 429）");
    }

    /// 账号名：邮箱优先，登录方式退一级，全缺时叫 `Kiro 账号`
    #[test]
    fn the_display_name_degrades_gracefully() {
        let full = KiroCredentials {
            account_id: "a@b.com".to_string(),
            login_provider: "builderid".to_string(),
            ..Default::default()
        };
        assert_eq!(display_name(&full), "a@b.com (Builder ID)");
        let no_way = KiroCredentials {
            account_id: "a@b.com".to_string(),
            ..Default::default()
        };
        assert_eq!(display_name(&no_way), "a@b.com", "没有登录方式时不能出现空括号");
        assert_eq!(display_name(&KiroCredentials::default()), "Kiro 账号");
    }

    /// 落库字段与「粘贴凭证」同一套键（两处分叉 = 同一个账号加出两条）
    #[test]
    fn the_payload_uses_the_same_field_names_as_pasting() {
        let credentials = KiroCredentials {
            method: "social".to_string(),
            access: "at-1".to_string(),
            refresh: "rt-1".to_string(),
            expires: 1_900_000_000_000,
            region: "us-east-1".to_string(),
            profile: "arn:aws:codewhisperer:us-east-1:1:profile/X".to_string(),
            login_provider: "google".to_string(),
            account_id: "a@b.com".to_string(),
            plan: "Pro".to_string(),
            ..Default::default()
        };
        let payload = payload_of(&credentials);
        // 复用同一条读路径：写得出的必须读得回，且身份不变
        let back = KiroCredentials::from_fields(&payload).expect("写出去的字段要能读回来");
        assert_eq!(back.access, "at-1");
        assert_eq!(back.method, "social");
        assert_eq!(back.profile, credentials.profile);
        assert_eq!(back.account_id, "a@b.com");
        assert_eq!(back.plan, "Pro");
    }

    /// 默认有效期：响应没给就给一小时（不能落一个 0 = 永远过期）
    #[test]
    fn a_token_without_expiry_still_gets_one() {
        let now = credentials::now_ms();
        let expires = expiry(&json!({ "accessToken": "x" }), "expiresIn");
        assert!(expires > now, "没给有效期也要落一个未来时间");
        assert_eq!(expiry(&json!({ "expiresIn": 60 }), "expiresIn") - now, 60_000);
    }

    /// 白名单端口必须全是「非 root 可绑」的（Android 上低位端口会直接失败）
    #[test]
    fn the_callback_ports_are_bindable_without_root() {
        assert_eq!(CALLBACK_PORTS.len(), 10, "与官方 IDE 注册的那一组逐条对齐");
        assert!(CALLBACK_PORTS.iter().all(|port| *port > 1024));
    }
}
