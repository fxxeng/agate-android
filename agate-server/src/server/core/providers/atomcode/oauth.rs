//! AtomCode 的登录与续期：**代理中介的浏览器流程，没有本地回调**。
//!
//! ── 与其余各家的最大不同：回调不回到本机 ────────────────────
//! AutoClaw / Accio / Qoder 的网页登录都在本机起一个回调口（或等设备授权码），
//! atomcode 不是：授权回调由代理自己在其域名上终结
//! （`acs.atomgit.com/callback`），**永远不回到**运行网关的这台机器。
//! 因此本家没有 loopback 监听器、没有固定回调端口 —— 与 ZCode 的 CLI 登录
//! 同一形状，靠**轮询**判完成（下面是官方 `oauth.rs` 的三步，本地 Go 参考实现
//! `cpa-jethub-plugins/plugins/atomcode/oauth.go` 逐字照做并实测过）：
//!
//! ```text
//!   1. GET {broker}/auth/login?provider=atomgit
//!      -> {login_url, state}
//!   2. 把 login_url 交给用户打开（浏览器手势必须发生在用户还认为它是
//!      「自己点的」那段时间里，所以这一步**绝不能阻塞等待**）
//!   3. GET {broker}/auth/check?state=<state>   -> {valid: bool}     （轮询）
//!      GET {broker}/auth/token?state=<state>   -> 令牌包              （取回）
//! ```
//!
//! 续期是**另一个端点**：`POST {broker}/oauth/refresh`，body 只有
//! `{"refresh_token": ...}`，**不带 Authorization**（官方 `oauth.rs:901-975`）。
//!
//! ── 续期会轮换 refresh token，而且可能不回新的（两条都要照顾）────
//! 实测：`/oauth/refresh` 同时下发新 access 与新 refresh，**旧 access 立即失效**。
//! 但官方注释（`oauth.go:350`）明确写了代理**可能省略** `refresh_token` /
//! `expires_in` / `user` 三个字段 —— 省略 `refresh_token` 时若把空串写回记录，
//! 账号就退化成必须重新走浏览器登录。所以这里的规则是：
//! **新值非空才覆盖，为空保留旧值**（见 [`TokenBundle::merge_into`]）。
//!
//! ── 本仓没有 loopback 端口常量 ────────────────────────────────
//! `lobsterai/oauth.rs` 里那组 `LOOPBACK_PORT` / `set_loopback_port` 是**回调型**
//! 登录才需要的（授权页要把浏览器 302 回本机端口）。本家回调在代理域名终结，
//! 没有任何代码会读一个本机端口 —— 照抄那几行只会造出「写了没人用」的死代码，
//! 让后来者以为本家有回调链。因此这里刻意没有它们（接线点也相应不含
//! `http.rs` 的回调路由与 `set_loopback_port` 两处）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::auth_http::send_raw;
use crate::server::errors::GatewayError;

use super::credentials::{expires_at_from, TokenBundle};
use super::endpoints::{
    broker_base_url, urlencode, BROKER_CHECK_PATH, BROKER_LOGIN_PATH, BROKER_PROVIDER,
    BROKER_REFRESH_PATH, BROKER_TOKEN_PATH, REQUEST_TIMEOUT_MS,
};

/// 取一个 JSON 字符串字段（缺失 / 非字符串 / 空串都归一成空串）
fn str_field(source: &Value, key: &str) -> String {
    source
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

/// 把一个代理响应体解成 [`TokenBundle`]（登录与续期共用同一信封）。
///
/// 代理的信封实测形态（官方 `AuthInfo` / Go 参考 `brokerTokenResponse`）：
///
/// ```text
///   { access_token, token_type, expires_in, refresh_token, user: {id, username, …} }
/// ```
///
/// 注意 `user` 可能整块缺失（官方注释：代理可省略），因此身份字段一律宽容读取。
pub fn parse_token_bundle(payload: &Value) -> Result<TokenBundle, GatewayError> {
    let access_token = str_field(payload, "access_token");
    if access_token.is_empty() {
        return Err(GatewayError::with_status(
            502,
            "AtomCode 登录代理未返回 access_token",
        ));
    }
    let user = payload.get("user").cloned().unwrap_or(Value::Null);
    let expires_in = payload
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .unwrap_or(0);
    Ok(TokenBundle {
        access_token,
        refresh_token: str_field(payload, "refresh_token"),
        token_type: str_field(payload, "token_type"),
        expires_in,
        user_id: {
            let id = str_field(&user, "id");
            if id.is_empty() {
                str_field(payload, "user_id")
            } else {
                id
            }
        },
        username: str_field(&user, "username"),
        name: str_field(&user, "name"),
        email: str_field(&user, "email"),
        avatar_url: str_field(&user, "avatar_url"),
    })
}

impl TokenBundle {
    /// 续期结果合并到旧令牌包上。
    ///
    /// ── 为什么不是整包替换 ──────────────────────────────────
    /// 代理**可能省略** `refresh_token` / `expires_in` / `user`（官方注释原文：
    /// > The broker may omit `refresh_token`/`expires_in`/`user`; each absent field …
    ///
    /// 整包替换的下场是：一次省略 `refresh_token` 的续期把记录里的 refresh 清成
    /// 空串，账号从此只能重新浏览器登录 —— 而这次续期「看起来是成功的」。
    /// 所以非空才覆盖、为空保留，`user_id` 同样（身份不该在一次续期后消失）。
    pub fn merge_into(self, previous: &TokenBundle) -> TokenBundle {
        TokenBundle {
            access_token: self.access_token,
            refresh_token: if self.refresh_token.is_empty() {
                previous.refresh_token.clone()
            } else {
                self.refresh_token
            },
            token_type: if self.token_type.is_empty() {
                previous.token_type.clone()
            } else {
                self.token_type
            },
            expires_in: if self.expires_in > 0 {
                self.expires_in
            } else {
                previous.expires_in
            },
            user_id: if self.user_id.is_empty() {
                previous.user_id.clone()
            } else {
                self.user_id
            },
            username: if self.username.is_empty() {
                previous.username.clone()
            } else {
                self.username
            },
            name: if self.name.is_empty() {
                previous.name.clone()
            } else {
                self.name
            },
            email: if self.email.is_empty() {
                previous.email.clone()
            } else {
                self.email
            },
            avatar_url: if self.avatar_url.is_empty() {
                previous.avatar_url.clone()
            } else {
                self.avatar_url
            },
        }
    }

    /// 这一次令牌包的过期时刻（毫秒）。`expires_in` 缺失时返回 0 = 判不出来。
    pub fn expires_at(&self, now_ms: i64) -> i64 {
        if self.expires_in <= 0 {
            return 0;
        }
        expires_at_from(now_ms / 1000, self.expires_in, 0)
    }
}

/// 一条已发起的浏览器登录（[`BrokerLogin::start`] 的返回值）。
///
/// 调用方持有它，把 [`Self::login_url`] 交给用户打开，之后按节奏调
/// [`Self::check`]，直到拿到令牌包或超时。
pub struct BrokerLogin {
    /// 代理给的关联 id，既是轮询凭据也是索引
    state: String,
    /// 交给用户打开的 AtomGit 授权地址
    login_url: String,
    /// 本地截止时刻（毫秒）—— 代理自己也有窗口，这个只用来阻止本地累积死会话
    #[allow(dead_code)]
    expires_at_ms: i64,
}

impl BrokerLogin {
    /// 发起登录：拿 `login_url` 与 `state`。
    ///
    /// **不阻塞**（不等待用户）—— 浏览器手势必须发生在用户还认为它是自己点开的
    /// 那段时间里，阻塞式登录会让界面变成一次导航跳转（壳侧拦不住）。
    pub async fn start(timeout_ms: u64) -> Result<Self, GatewayError> {
        let url = format!(
            "{}{}?provider={}",
            broker_base_url(),
            BROKER_LOGIN_PATH,
            BROKER_PROVIDER
        );
        let response = send_raw(
            "GET",
            &url,
            None,
            &[("accept".to_string(), "application/json".to_string())],
            None,
            Some(REQUEST_TIMEOUT_MS),
        )
        .await
        .map_err(|error| login_transport_error("登录发起", &error))?;

        let payload = response.payload.ok_or_else(|| {
            GatewayError::with_status(502, "AtomCode 登录发起：代理响应不是 JSON")
        })?;
        if !response.ok {
            return Err(GatewayError::with_status(
                502,
                format!(
                    "AtomCode 登录发起失败（HTTP {}）：{}",
                    response.status,
                    summarize(&payload)
                ),
            ));
        }
        let login_url = str_field(&payload, "login_url");
        let state = str_field(&payload, "state");
        if login_url.is_empty() || state.is_empty() {
            return Err(GatewayError::with_status(
                502,
                "AtomCode 登录代理未返回 login_url/state",
            ));
        }
        let expires = if timeout_ms > 0 { timeout_ms } else { 300_000 };
        Ok(Self {
            state,
            login_url,
            expires_at_ms: crate::server::logging::now_ms() + expires as i64,
        })
    }

    /// 从已知的 `state` 重建（**轮询侧用**）。
    ///
    /// ── 为什么不需要保存登录会话 ────────────────────────────────
    /// 代理那边以 `state` 为索引记着这一轮（授权进度、关联账号都在它那儿），
    /// 本机只要拿 state 去问 `auth/check` / `auth/token` 就够了。所以
    /// 「发起」与「轮询」之间**不需要**任何内存状态 —— 进程重启也不丢进度。
    pub fn resume(state: String) -> Self {
        Self {
            state,
            login_url: String::new(),
            expires_at_ms: 0,
        }
    }

    /// 给用户打开的授权地址
    pub fn login_url(&self) -> &str {
        &self.login_url
    }

    /// 关联 id（登录任务表用它当 key）
    pub fn state(&self) -> &str {
        &self.state
    }

    /// 本地是否已过截止时刻（过点就没必要再轮询）
    ///
    /// 本仓没有消费方：登录超时由**任务表**管（`LOGIN_TIMEOUT_MS`），
    /// [`Self::check`] 每拍只回答「完成没」。留着是因为 `start` 仍然会算这个
    /// 截止时刻（代理自己也有窗口），而判据放在类型上比散在调用方更不容易写错。
    #[allow(dead_code)]
    pub fn expired(&self) -> bool {
        self.expires_at_ms > 0 && self.expires_at_ms <= crate::server::logging::now_ms()
    }

    /// 问一次「用户授权完了没」。
    ///
    /// 返回 `Ok(true)` 表示可以取令牌了。**网络类失败当 pending 处理**（返回
    /// `Ok(false)`）：那是「这次没问成」而不是「问成了但被拒」，把它当致命错误
    /// 会让一次抖动就打断用户正在进行的授权。
    pub async fn check(&self) -> Result<bool, GatewayError> {
        let url = format!(
            "{}{}?state={}",
            broker_base_url(),
            BROKER_CHECK_PATH,
            urlencode(&self.state)
        );
        let response = send_raw(
            "GET",
            &url,
            None,
            &[("accept".to_string(), "application/json".to_string())],
            None,
            Some(REQUEST_TIMEOUT_MS),
        )
        .await
        .map_err(|error| login_transport_error("登录轮询", &error))?;
        // 5xx 与畸形体当 pending（与 ZCode 轮询同一口径）
        if response.status >= 500 {
            return Ok(false);
        }
        if !response.ok {
            return Ok(false);
        }
        let Some(payload) = response.payload else {
            return Ok(false);
        };
        Ok(payload.get("valid").and_then(Value::as_bool).unwrap_or(false))
    }

    /// 取回令牌包（`check` 为 true 之后再调）。
    pub async fn fetch_token(&self) -> Result<TokenBundle, GatewayError> {
        let url = format!(
            "{}{}?state={}",
            broker_base_url(),
            BROKER_TOKEN_PATH,
            urlencode(&self.state)
        );
        let response = send_raw(
            "GET",
            &url,
            None,
            &[("accept".to_string(), "application/json".to_string())],
            None,
            Some(REQUEST_TIMEOUT_MS),
        )
        .await
        .map_err(|error| login_transport_error("登录取令牌", &error))?;
        let payload = response.payload.ok_or_else(|| {
            GatewayError::with_status(502, "AtomCode 登录取令牌：代理响应不是 JSON")
        })?;
        if !response.ok {
            return Err(GatewayError::with_status(
                502,
                format!(
                    "AtomCode 登录取令牌失败（HTTP {}）：{}",
                    response.status,
                    summarize(&payload)
                ),
            ));
        }
        parse_token_bundle(&payload)
    }
}

/// 续期：`POST {broker}/oauth/refresh`，body 只有 `{"refresh_token": …}`。
///
/// ── 为什么失败要区分「refresh 失效」与「其它」────────────────
/// `refresh_token` 已失效（HTTP 401 / 403）是**用户必须重新登录**的信号，
/// 归类成 401 交给宿主会让界面明确提示重登；而一次网络抖动或 5xx 只是「这次没刷成」，
/// 归类成 502/504 由上层按可重试处理。
///
/// ⚠️ 调用者必须保证**单飞**（同一账号同一时刻只有一次续期在跑）：
/// 续期会轮换 refresh token，两个并发续期互相作废、后写的那次还会覆盖掉
/// 前一次拿回的新 refresh（见 [`super::refresh`]）。
pub async fn refresh(refresh_token: &str) -> Result<TokenBundle, GatewayError> {
    if refresh_token.trim().is_empty() {
        return Err(GatewayError::with_status(
            401,
            "AtomCode 账号没有 refresh_token，无法续期，请重新登录",
        ));
    }
    let url = format!("{}{}", broker_base_url(), BROKER_REFRESH_PATH);
    let body = json!({ "refresh_token": refresh_token });
    let response = send_raw(
        "POST",
        &url,
        Some(&body),
        &[
            ("accept".to_string(), "application/json".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ],
        None,
        Some(REQUEST_TIMEOUT_MS),
    )
    .await
    .map_err(|error| login_transport_error("续期", &error))?;

    let payload = response.payload.unwrap_or(Value::Null);
    if response.status == 401 || response.status == 403 {
        return Err(GatewayError::with_status(
            401,
            format!(
                "AtomGit refresh_token 已失效，请重新登录（{}）",
                summarize(&payload)
            ),
        ));
    }
    if !response.ok {
        return Err(GatewayError::with_status(
            502,
            format!(
                "AtomCode 续期失败（HTTP {}）：{}",
                response.status,
                summarize(&payload)
            ),
        ));
    }
    parse_token_bundle(&payload)
}

/// 把传输层错误统一成带中文前缀的网关错误。
fn login_transport_error(stage: &str, error: &reqwest::Error) -> GatewayError {
    if error.is_timeout() {
        GatewayError::with_status(504, format!("AtomCode {stage}超时"))
    } else {
        GatewayError::with_status(502, format!("AtomCode {stage}失败: {error}"))
    }
}

/// 把错误体压成一行可读文案（不把整块 HTML 塞进错误消息）
fn summarize(payload: &Value) -> String {
    payload
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("detail")
                .and_then(|detail| detail.get("message"))
                .and_then(Value::as_str)
        })
        .or_else(|| payload.get("error").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let raw = payload.to_string();
            raw.chars().take(180).collect()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 登录信封：`user` 整块缺失也要能解（官方明说代理可省略）
    #[test]
    fn parse_bundle_tolerates_missing_user() {
        let bundle = parse_token_bundle(&json!({
            "access_token": "at",
            "refresh_token": "rt",
            "expires_in": 604800
        }));
        assert!(bundle
            .as_ref()
            .is_some_and(|bundle| bundle.access_token == "at" && bundle.user_id.is_empty()));
        assert_eq!(
            bundle.ok().map(|bundle| bundle.expires_in),
            Some(604_800)
        );
        // 缺 access_token 是 502（不是 401：代理的问题，不是用户凭证的问题）
        assert_eq!(
            parse_token_bundle(&json!({"refresh_token": "x"}))
                .err()
                .map(|error| error.status_code),
            Some(502)
        );
    }

    /// `user.id` 优先；没有时退回顶层 `user_id`（两种信封形态都实测存在）
    #[test]
    fn identity_fields_fall_back_to_top_level() {
        let with_user = parse_token_bundle(&json!({
            "access_token": "a",
            "user": {"id": "42", "username": "qq_1", "name": "张三", "email": "a@b.c", "avatar_url": "u"}
        }))
        .ok();
        assert!(with_user.as_ref().is_some_and(|bundle| bundle.user_id == "42"
            && bundle.username == "qq_1"
            && bundle.name == "张三"
            && bundle.email == "a@b.c"
            && bundle.avatar_url == "u"));
        let flat = parse_token_bundle(&json!({"access_token": "a", "user_id": "7"})).ok();
        assert!(flat.is_some_and(|bundle| bundle.user_id == "7"));
    }

    /// ⚠️ 续期合并：**非空才覆盖**（省略 refresh_token 时整包替换会让账号失去续期能力）
    #[test]
    fn merge_keeps_previous_when_new_field_is_empty() {
        let previous = TokenBundle {
            access_token: "old-a".into(),
            refresh_token: "old-r".into(),
            expires_in: 604_800,
            user_id: "42".into(),
            username: "qq_1".into(),
            ..Default::default()
        };
        // 上游省略了 refresh_token / expires_in / user
        let merged = TokenBundle {
            access_token: "new-a".into(),
            ..Default::default()
        }
        .merge_into(&previous);
        assert_eq!(merged.access_token, "new-a");
        assert_eq!(merged.refresh_token, "old-r");
        assert_eq!(merged.expires_in, 604_800);
        assert_eq!(merged.user_id, "42");
        assert_eq!(merged.username, "qq_1");
        // 上游给了新值就覆盖
        let rotated = TokenBundle {
            access_token: "a2".into(),
            refresh_token: "r2".into(),
            expires_in: 60,
            ..Default::default()
        }
        .merge_into(&previous);
        assert_eq!(rotated.refresh_token, "r2");
        assert_eq!(rotated.expires_in, 60);
    }

    /// 过期时刻由 `expires_in` 推出；缺失时 0（不猜）
    #[test]
    fn bundle_expiry_is_derived_from_expires_in() {
        let bundle = TokenBundle {
            access_token: "a".into(),
            expires_in: 3_600,
            ..Default::default()
        };
        assert_eq!(bundle.expires_at(1_700_000_000_000), 1_700_003_600_000);
        let no_expiry = TokenBundle { expires_in: 0, ..Default::default() };
        assert_eq!(no_expiry.expires_at(1_700_000_000_000), 0);
    }

    /// 错误体压成一行：优先 message，其次 detail.message，最后截断原文
    #[test]
    fn summarize_prefers_readable_fields() {
        assert_eq!(summarize(&json!({"message": " 401 Unauthorized "})), "401 Unauthorized");
        assert_eq!(
            summarize(&json!({"detail": {"message": "bad state"}})),
            "bad state"
        );
        // HTML 报错页只留前 180 字
        let long = "x".repeat(400);
        assert_eq!(summarize(&json!({"body": long})).chars().count(), 180);
    }
}
