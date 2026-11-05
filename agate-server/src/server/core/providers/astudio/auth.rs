//! AStudio 的账号登录协议（手机号验证码 / 账号密码 + 极验）。
//!
//! ── 上游接口（从桌面端 `apps/server/dist/index.mjs` 逆向）────────
//! ```text
//!   账号服务  https://agent.xfyun.cn/xingchen-studio/
//!   GET  chat/gee-captcha                 极验配置 {gt, challenge, success, new_captcha}
//!   POST login/mobile/send-verify-code    发短信验证码（multipart，含极验三件）
//!   POST login/phone-quick-login          手机号 + 验证码登录（**不需要极验**）
//!   POST login/check-account              账号 + 密码登录（需要极验）
//!   POST tenant-app/v2/init-app           空 body → {banned}
//!   GET  userInfo                         → {userInfo:{uid, mobile, nickname}}
//!   GET  bot/models/configs               → [{api_key,is_current,is_default}]
//! ```
//!
//! ── 为什么这三个端点必须是 multipart ────────────────────────
//! 极验三件（`geetest_challenge` / `geetest_validate` / `geetest_seccode`）
//! 是**表单字段**，不是 JSON 字段；上游按 multipart 解析。手搓 boundary 一旦
//! 编码细节有偏差，症状是上游返回一句没有信息量的 400，所以走 reqwest 的
//! `multipart` 特性（见 `Cargo.toml` 里的说明）。
//!
//! ── 登录成功后拿到什么 ──────────────────────────────────────
//! `account_id` / `token`（或 ssoSessionId）/ `uid` / `mobile`，以及从模型配置里
//! 取出的 `modelBearerToken` —— 后者正是「粘贴桌面端文件」那条路用的同一个
//! 凭据，所以登录完成后对话 / 余额 / 签到都能直接用，不需要第二条实现。
//!
//! ── `modelBearerToken` 必须自己查一次 ───────────────────────
//! 登录响应里**没有**它。要再请求 `bot/models/configs`，按
//! `is_current` → `is_default` → 第一个非空 的顺序挑一个 —— 这个顺序是上游
//! 桌面端的原序，换了会让「当前正在用的那个 key」被一个旧 key 顶掉。
//!
//! ── 极验为什么要在浏览器里做 ────────────────────────────────
//! 滑块必须在真实页面里完成，所以本模块**只负责转发**页面上拿到的三件；
//! 页面本身由 `login_page` 起在 `127.0.0.1` 上。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::time::Duration;

use reqwest::multipart::Form;
use serde_json::{json, Value};

use crate::server::core::proxies::ResolvedProxy;

use super::client;
use super::credentials::Credentials;
use super::endpoints;

/// 账号服务根（生产环境）
pub const AUTH_BASE: &str = endpoints::STUDIO_API_BASE;

/// 读类请求超时
const GET_TIMEOUT: Duration = Duration::from_secs(20);
/// 写类请求超时（发码要打运营商网关，给宽一点）
const POST_TIMEOUT: Duration = Duration::from_secs(30);

/// 极验三件（页面完成滑块后拿到的）。
#[derive(Clone, Debug, Default)]
pub struct Captcha {
    pub challenge: String,
    pub validate: String,
    pub seccode: String,
}

impl Captcha {
    /// 三件齐了才算完成（缺一件上游必然判验证失败）
    pub fn is_complete(&self) -> bool {
        !self.challenge.is_empty() && !self.validate.is_empty() && !self.seccode.is_empty()
    }
}

/// 一次成功登录拿到的会话（登录中间态，**不是**落盘形态）。
#[derive(Clone, Debug, Default)]
pub struct Session {
    pub account_id: String,
    pub uid: String,
    pub token: String,
    pub sso_session_id: String,
    pub mobile: String,
    pub nickname: String,
    pub model_bearer_token: String,
    pub banned: bool,
    /// 这次登录走的是哪条（`sms` / `password`）—— 只用于账号上的 `source`，
    /// 让「同一个账号是验证码加的还密码加的」在记录里看得出来
    pub method: String,
}

impl Session {
    /// 转成落盘形态的凭据。
    ///
    /// **不在这里自己拼 Cookie**：`Credentials::cookie_header` 是唯一一份
    /// （`ssoSessionId` 发两遍、顺序 ssoSessionId → account_id → token）。
    /// 两处各拼一份，改一处就会让「登录加的账号」与「粘贴加的账号」在请求头
    /// 上出现细微差别 —— 那种差别只在上游偶发 401 时才暴露。
    pub fn to_credentials(&self) -> Credentials {
        Credentials {
            key: self.model_bearer_token.clone(),
            uid: self.uid.clone(),
            token: self.token.clone(),
            account_id: self.account_id.clone(),
            sso_session_id: self.sso_session_id.clone(),
            source: self.method.clone(),
        }
    }

    /// 备注名的候选顺序：手机号 → 昵称 → uid → 固定名
    pub fn display_name(&self) -> String {
        if !self.mobile.is_empty() {
            self.mobile.clone()
        } else if !self.nickname.is_empty() {
            self.nickname.clone()
        } else if !self.uid.is_empty() {
            self.uid.clone()
        } else {
            "AStudio".to_string()
        }
    }
}

/// 给请求加上桌面端身份头。
///
/// `clientType`（Windows 21 / macOS 22）与 `studioVersion` 少了，上游会按
/// 「未知客户端」处理。取 `endpoints` 的常量：登录链与余额 / 会员 / 签到链
/// **必须共用同一个版本号**，否则同一账号在不同请求里表现为两个客户端。
fn decorate(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    request
        .header("clientType", endpoints::CLIENT_TYPE)
        .header("studioVersion", endpoints::STUDIO_VERSION)
}

/// 从信封里找一句能读的错误文案。
fn message_of(value: &Value) -> String {
    for key in ["message", "msg", "desc", "error"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

/// 拆信封：`{code:0, data:…}`；`code` 非 0 时把上游文案带出来。
///
/// `code` 缺失不算失败（有的端点是裸对象）；`null` 同样不算。
fn unwrap_envelope(value: &Value, operation: &str) -> Result<Value, String> {
    if !value.is_object() {
        return Ok(value.clone());
    }
    if let Some(code) = value.get("code") {
        let zero = code.as_i64() == Some(0) || code.as_u64() == Some(0) || code.as_str() == Some("0");
        let missing = code.is_null();
        if !zero && !missing {
            let message = message_of(value);
            return Err(if message.is_empty() {
                format!("{operation}失败（上游返回 code {code}）")
            } else {
                format!("{operation}失败：{message}")
            });
        }
    }
    Ok(value.get("data").cloned().unwrap_or_else(|| value.clone()))
}

/// 把一次响应读成 `(状态, JSON)`。非 JSON 的响应把前 500 字塞进 `raw`。
async fn read_response(response: reqwest::Response) -> (u16, Value) {
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let value: Value = serde_json::from_str(&text).unwrap_or_else(|_| {
        json!({ "raw": text.chars().take(500).collect::<String>() })
    });
    (status, value)
}

/// 状态码 + 信封一起判（顺序刻意：**先按 HTTP 状态判**，再解信封 ——
/// 反了会把「凭据被拒」说成「上游返回 code 1」，把真正的原因盖掉）。
fn verdict(status: u16, value: &Value, operation: &str) -> Result<Value, String> {
    if !(200..300).contains(&status) {
        let message = message_of(value);
        // 状态码一并写出来：登录链的失败几乎都是「上游出问题」，用户报障时
        // 一句「发送验证码失败：unauthorized」没有可操作信息，带上 401/502
        // 才能区分「凭据被拒」与「上游挂了」。
        return Err(if message.is_empty() {
            format!("{operation}失败（HTTP {status}）")
        } else {
            format!("{operation}失败（HTTP {status}）：{message}")
        });
    }
    unwrap_envelope(value, operation)
}

/// 一次 GET（无 body）。
async fn get_value(
    proxy: Option<&ResolvedProxy>,
    path: &str,
    cookie: Option<&str>,
    operation: &str,
) -> Result<Value, String> {
    let url = format!("{AUTH_BASE}{path}");
    let mut request = client::client(proxy)
        .get(&url)
        .header("Accept", "application/json")
        .timeout(GET_TIMEOUT);
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    let response = decorate(request)
        .send()
        .await
        .map_err(|error| format!("{operation}网络失败：{}", client::describe(&error)))?;
    let (status, value) = read_response(response).await;
    verdict(status, &value, operation)
}

/// 一次 multipart POST。
async fn post_form(
    proxy: Option<&ResolvedProxy>,
    path: &str,
    form: Form,
    cookie: Option<&str>,
    operation: &str,
) -> Result<Value, String> {
    let url = format!("{AUTH_BASE}{path}");
    let mut request = client::client(proxy)
        .post(&url)
        .header("Accept", "application/json")
        .timeout(POST_TIMEOUT)
        .multipart(form);
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    let response = decorate(request)
        .send()
        .await
        .map_err(|error| format!("{operation}网络失败：{}", client::describe(&error)))?;
    let (status, value) = read_response(response).await;
    verdict(status, &value, operation)
}

/// 一次空 body 的 POST（上游 `init-app` 就是这种形态）。
async fn post_empty(
    proxy: Option<&ResolvedProxy>,
    path: &str,
    cookie: &str,
    operation: &str,
) -> Result<Value, String> {
    let url = format!("{AUTH_BASE}{path}");
    let request = client::client(proxy)
        .post(&url)
        .header("Accept", "application/json")
        .header("Cookie", cookie)
        .timeout(GET_TIMEOUT);
    let response = decorate(request)
        .send()
        .await
        .map_err(|error| format!("{operation}网络失败：{}", client::describe(&error)))?;
    let (status, value) = read_response(response).await;
    verdict(status, &value, operation)
}

/// 极验三件拼成表单（`seccode` 要 **base64** 一次 —— 上游原样这么收）。
fn captcha_form(captcha: &Captcha) -> Form {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    Form::new()
        .text("geetest_challenge", captcha.challenge.clone())
        .text("geetest_validate", captcha.validate.clone())
        .text("geetest_seccode", STANDARD.encode(captcha.seccode.as_bytes()))
}

/// 极验配置（页面初始化用）。
///
/// 每次发码 / 密码登录都要**新取一次**：`challenge` 是一次性的，复用会让
/// 滑块看起来正常、提交却判验证失败。
pub async fn geetest_config(proxy: Option<&ResolvedProxy>) -> Result<Value, String> {
    get_value(proxy, "chat/gee-captcha", None, "读取极验配置").await
}

/// 发短信验证码（要极验）。
pub async fn send_sms(
    proxy: Option<&ResolvedProxy>,
    mobile: &str,
    captcha: &Captcha,
) -> Result<(), String> {
    let form = captcha_form(captcha)
        .text("mobile", mobile.to_string())
        .text("countryCode", "86");
    post_form(
        proxy,
        "login/mobile/send-verify-code",
        form,
        None,
        "发送验证码",
    )
    .await
    .map(|_| ())
}

/// 手机号 + 验证码登录（**不需要极验**）。
pub async fn login_sms(
    proxy: Option<&ResolvedProxy>,
    mobile: &str,
    verify_code: &str,
) -> Result<Session, String> {
    let form = Form::new()
        .text("mobile", mobile.to_string())
        .text("verifyCode", verify_code.to_string())
        .text("countryCode", "86");
    let value = post_form(proxy, "login/phone-quick-login", form, None, "手机号登录").await?;
    complete(proxy, &value).await
}

/// 账号 + 密码登录（要极验）。
pub async fn login_password(
    proxy: Option<&ResolvedProxy>,
    account: &str,
    password: &str,
    captcha: &Captcha,
) -> Result<Session, String> {
    let form = captcha_form(captcha)
        .text("accountName", account.to_string())
        .text("accountPwd", password.to_string());
    let value = post_form(proxy, "login/check-account", form, None, "账号密码登录").await?;
    complete(proxy, &value).await
}

/// 登录之后的收尾：租户 App（顺带判封禁）→ 用户信息 → 模型凭据。
///
/// 三次请求里只有**最后一次**是必须的：`init-app` 与 `userInfo` 失败只损失
/// 「封禁标记」与「备注名更漂亮」，不该把一次已经成功的登录判死。模型凭据
/// 拿不到则真的没法用（没有 `modelBearerToken` 就调不了模型），必须报错。
async fn complete(proxy: Option<&ResolvedProxy>, login: &Value) -> Result<Session, String> {
    let account_id = login
        .get("account_id")
        .or_else(|| login.get("accountId"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let sso_session_id = login
        .get("ssoSessionId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    // 没有 token 时用 ssoSessionId 顶：上游这两个是同一枚会话的两种叫法
    let token = login
        .get("token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| sso_session_id.clone());
    if account_id.is_empty() || token.is_empty() {
        return Err("AStudio 登录返回不完整（缺少账号或令牌）".to_string());
    }
    let mut session = Session {
        account_id,
        uid: login
            .get("uid")
            .map(|value| value.to_string().trim_matches('"').to_string())
            .unwrap_or_default(),
        token,
        sso_session_id,
        mobile: login
            .get("mobile")
            .or_else(|| login.get("phoneNum"))
            .or_else(|| login.get("phone"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        ..Default::default()
    };
    let cookie = session.to_credentials().cookie_header();
    // 租户 App：顺便拿封禁标记（桌面端是一次不带 body 的 POST）
    if let Ok(value) = post_empty(proxy, "tenant-app/v2/init-app", &cookie, "初始化账号").await {
        session.banned = value.get("banned").and_then(Value::as_bool) == Some(true);
    }
    if session.banned {
        return Err("这个 AStudio 账号已被封禁".to_string());
    }
    // 用户信息：补 uid / mobile / 昵称
    if session.uid.is_empty() || session.mobile.is_empty() || session.nickname.is_empty() {
        if let Ok(value) = get_value(proxy, "userInfo", Some(&cookie), "读取用户信息").await {
            let info = value.get("userInfo").unwrap_or(&value);
            if session.uid.is_empty() {
                if let Some(uid) = info.get("uid") {
                    session.uid = uid.to_string().trim_matches('"').to_string();
                }
            }
            if session.mobile.is_empty() {
                if let Some(mobile) = info
                    .get("mobile")
                    .or_else(|| info.get("telephone"))
                    .and_then(Value::as_str)
                {
                    session.mobile = mobile.to_string();
                }
            }
            if session.nickname.is_empty() {
                if let Some(name) = info
                    .get("nickname")
                    .or_else(|| info.get("username"))
                    .and_then(Value::as_str)
                {
                    session.nickname = name.to_string();
                }
            }
        }
    }
    // 模型凭据：登录响应里没有，必须自己查
    let value = get_value(proxy, "bot/models/configs", Some(&cookie), "读取模型凭据").await?;
    let list = value.as_array().cloned().unwrap_or_default();
    let key_of = |predicate: fn(&Value) -> bool| -> Option<String> {
        list.iter()
            .find(|item| predicate(item))
            .and_then(|item| item.get("api_key").and_then(Value::as_str))
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
    };
    // 顺序是上游桌面端的原序：当前在用 → 默认 → 第一个非空
    let key = key_of(|item| item.get("is_current").and_then(Value::as_bool) == Some(true))
        .or_else(|| key_of(|item| item.get("is_default").and_then(Value::as_bool) == Some(true)))
        .or_else(|| {
            list.iter()
                .find_map(|item| item.get("api_key").and_then(Value::as_str))
                .map(|key| key.trim().to_string())
                .filter(|key| !key.is_empty())
        });
    let Some(key) = key else {
        return Err("这个 AStudio 账号没有可用的模型凭据".to_string());
    };
    session.model_bearer_token = key;
    Ok(session)
}

/// 手机号形态（与桌面端 `isValidAstronMobile` 同口径：中国大陆 11 位）
pub fn valid_mobile(value: &str) -> bool {
    let bytes = value.trim().as_bytes();
    bytes.len() == 11 && bytes[0] == b'1' && bytes.iter().all(|byte| byte.is_ascii_digit())
}

/// 验证码形态（6 位数字）
pub fn valid_code(value: &str) -> bool {
    let text = value.trim();
    text.len() == 6 && text.chars().all(|ch| ch.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `code` 为 0 / "0" / 缺失 / null 都算成功，其余带出上游文案
    #[test]
    fn the_envelope_is_unwrapped_or_explained() {
        assert_eq!(
            unwrap_envelope(&json!({"code": 0, "data": {"a": 1}}), "读取"),
            Ok(json!({"a": 1}))
        );
        assert_eq!(
            unwrap_envelope(&json!({"code": "0", "data": [1]}), "读取"),
            Ok(json!([1]))
        );
        // 没有 code：裸对象原样返回
        assert_eq!(
            unwrap_envelope(&json!({"a": 1}), "读取"),
            Ok(json!({"a": 1}))
        );
        // code 为 null 也不算失败
        assert_eq!(
            unwrap_envelope(&json!({"code": null, "data": 1}), "读取"),
            Ok(json!(1))
        );
        let failed = unwrap_envelope(&json!({"code": 1, "message": "账号不存在"}), "登录");
        assert_eq!(failed, Err("登录失败：账号不存在".to_string()));
        // 没有文案时把 code 带出来，别只说「失败」
        let bare = unwrap_envelope(&json!({"code": 500}), "登录");
        assert!(bare.err().unwrap_or_default().contains("500"));
    }

    /// 状态码的判断必须**在解信封之前**：否则「被拒」会被说成上游业务错
    #[test]
    fn the_http_status_wins_over_the_envelope() {
        let value = json!({"code": 0, "message": "unauthorized"});
        let error = verdict(401, &value, "读取余额").err().unwrap_or_default();
        assert!(error.contains("401"), "{error}");
        assert!(error.contains("unauthorized"), "{error}");
    }

    /// `captcha` 要三件齐备（缺一件上游必然判验证失败）
    #[test]
    fn a_partial_captcha_is_not_complete() {
        let mut captcha = Captcha::default();
        assert!(!captcha.is_complete());
        captcha.challenge = "c".to_string();
        captcha.validate = "v".to_string();
        assert!(!captcha.is_complete(), "缺 seccode 不算完成");
        captcha.seccode = "s".to_string();
        assert!(captcha.is_complete());
    }

    /// 手机号与验证码的形态检查按上游口径（11 位 / 6 位数字）
    #[test]
    fn the_mobile_and_code_shapes_are_checked() {
        assert!(valid_mobile("13800138000"));
        assert!(!valid_mobile("23800138000"));
        assert!(!valid_mobile("1380013800"));
        assert!(!valid_mobile("1380013800a"));
        assert!(valid_code("123456"));
        assert!(!valid_code("12345"));
        assert!(!valid_code("12345a"));
        // 前后空格是粘贴带进来的，不该因此被拒
        assert!(valid_mobile("  13800138000  "));
        assert!(valid_code(" 123456 "));
    }

    /// 备注名的兜底顺序：手机号 → 昵称 → uid → 固定名
    #[test]
    fn the_display_name_prefers_the_mobile() {
        let mut session = Session::default();
        assert_eq!(session.display_name(), "AStudio");
        session.uid = "u1".to_string();
        assert_eq!(session.display_name(), "u1");
        session.nickname = "小明".to_string();
        assert_eq!(session.display_name(), "小明");
        session.mobile = "13800138000".to_string();
        assert_eq!(session.display_name(), "13800138000");
    }

    /// 会话转凭据后，Cookie 必须与 `Credentials::cookie_header` 逐字一致
    /// （`ssoSessionId` 发两遍、顺序 ssoSessionId → account_id → token）
    #[test]
    fn the_session_cookie_follows_the_shared_rule() {
        let session = Session {
            account_id: "acc".to_string(),
            token: "tok".to_string(),
            sso_session_id: "sso".to_string(),
            model_bearer_token: "key".to_string(),
            ..Default::default()
        };
        let credentials = session.to_credentials();
        assert_eq!(
            credentials.cookie_header(),
            "ssoSessionId=sso; sso_sessionid=sso; account_id=acc; token=tok"
        );
        assert!(credentials.has_session());
        assert_eq!(credentials.key, "key");
    }
}
