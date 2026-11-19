//! Kiro 凭证续期：社交登录 / AWS OIDC / 外部 IdP，以及首次补 profile。
//!
//! ── 三条刷新链路，一个入口 ──────────────────────────────────
//! ```text
//!   social        POST https://prod.{region}.auth.desktop.kiro.dev/refreshToken
//!                 {"refreshToken":"…"}   → {accessToken, refreshToken, expiresIn}
//!   idc           POST https://oidc.{region}.amazonaws.com/token
//!                 {clientId, clientSecret, refreshToken, grantType}
//!   external-idp  POST {tokenURL}（**form** 编码）→ {access_token, refresh_token, expires_in}
//!   apikey        没有续期接口 —— 只能换 key
//! ```
//! 三条路的**响应字段名都不一样**（驼峰 vs 下划线），这是 AWS 与外部 IdP 各自的
//! 习惯，不是笔误。转换只在本模块做一次，外面拿到的永远是同一个
//! [`KiroCredentials`]。
//!
//! ── 为什么要单飞 ────────────────────────────────────────────
//! refresh_token 多为一次性：两个并发请求同时刷新，后到的那个拿着已作废的
//! refresh_token 去换，会把一份好凭证覆盖成坏的，账号直接废掉。所以按
//! 「账号 + 凭证指纹」串行，后来者复用那一轮的结果（见
//! `providers::refresh_flight` 的模块头，那里写了取消与陈旧结果两个坑）。
//!
//! ── 本模块**不写库** ────────────────────────────────────────
//! 只做「旧凭证 → 新凭证」。落库由适配器决定（它才知道记录快照是否仍是同一份，
//! 见 autoclaw 的「比较-再写」）。写在这里会让每个调用点都隐式改库，
//! 而刷新失败时改库的时机就很难说清。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic；持锁不跨 await。

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};

use crate::server::core::proxies::ResolvedProxy;
use crate::server::core::providers::refresh_flight;
use crate::server::errors::GatewayError;

use super::credentials::{now_ms, KiroCredentials};
use super::{endpoints, http};

/// 刷新 / 补 profile 请求的超时（刷新是登录链路上的同步等待，超过这个数
/// 一定不对劲）
const REQUEST_TIMEOUT_MS: u64 = 30_000;

/// 上游没给 `expiresIn` 时的默认有效期（一小时，与 refresh_token 的常见口径一致）
const DEFAULT_EXPIRES_SECONDS: i64 = 3600;

/// 一次刷新得到的 token 三件套
struct Refreshed {
    access: String,
    refresh: String,
    expires_in: i64,
}

fn inflight() -> &'static refresh_flight::Table<KiroCredentials> {
    static TABLE: OnceLock<refresh_flight::Table<KiroCredentials>> = OnceLock::new();
    TABLE.get_or_init(refresh_flight::Table::new)
}

/// 单飞的 key：账号 + 登录方式 + refreshToken 指纹。
///
/// 三项缺一不可：不同账号不能互相复用；同一账号换了登录方式（重新导入成 API
/// key）不能拿旧结果；refresh_token 轮换后不能误取上一轮的结果。指纹是截断
/// SHA-256，**完整 token 不进 key、不打印**。
fn refresh_key(account_id: &str, credentials: &KiroCredentials) -> String {
    format!(
        "{account_id}:{}:{}",
        credentials.method,
        refresh_flight::fingerprint(&credentials.refresh)
    )
}

/// 刷新凭证（`force = false` 只在临期时刷）。
///
/// `force = true` 用于 401/403 之后：被拒的 token 时间上可能还很新，
/// 只看临期窗口会拿回同一个坏 token。
pub async fn refresh(
    account_id: &str,
    credentials: &KiroCredentials,
    force: bool,
    proxy: Option<&ResolvedProxy>,
) -> Result<KiroCredentials, GatewayError> {
    if !force && credentials.fresh(super::EARLY_REFRESH_MS) {
        return Ok(credentials.clone());
    }
    if credentials.is_api_key() {
        return Err(GatewayError::with_status(
            401,
            "Kiro 拒绝了保存的 API key，请重新填写",
        ));
    }
    if !credentials.can_refresh() {
        return Err(GatewayError::with_status(
            401,
            "Kiro 登录态没有 refreshToken，无法续期，请重新登录 Kiro",
        ));
    }
    let key = refresh_key(account_id, credentials);
    let guard = match inflight().join(&key) {
        refresh_flight::Join::Leader(guard) => guard,
        refresh_flight::Join::Waiter(waiter) => return waiter.wait().await,
    };
    let result = refresh_once(credentials, proxy).await;
    guard.finish(result.clone());
    result
}

/// 401 之后的强制刷新（不看临期窗口）
pub async fn force(
    account_id: &str,
    credentials: &KiroCredentials,
    proxy: Option<&ResolvedProxy>,
) -> Result<KiroCredentials, GatewayError> {
    refresh(account_id, credentials, true, proxy).await
}

/// 临期就刷，不临期原样返回（`ensure_access_token` 的语义）
pub async fn ensure_fresh(
    account_id: &str,
    credentials: &KiroCredentials,
    proxy: Option<&ResolvedProxy>,
) -> Result<KiroCredentials, GatewayError> {
    if credentials.fresh(super::EARLY_REFRESH_MS) {
        return Ok(credentials.clone());
    }
    crate::server::logging::verbose(
        "[Kiro]",
        &format!("账号 {account_id} 的登录态临期，主动续期"),
    );
    refresh(account_id, credentials, false, proxy).await
}

/// 真正发一次刷新请求
async fn refresh_once(
    credentials: &KiroCredentials,
    proxy: Option<&ResolvedProxy>,
) -> Result<KiroCredentials, GatewayError> {
    let client = http::client(proxy);
    let region = credentials.region();
    let refreshed = match credentials.method.as_str() {
        "social" => {
            let value = post_json(
                &client,
                &endpoints::refresh_url(&region),
                &json!({ "refreshToken": credentials.refresh }),
                // 社交登录的刷新端点按 UA 认「是不是官方桌面端」
                &[("User-Agent", super::DESKTOP_UA)],
            )
            .await?;
            Refreshed {
                access: text(&value, "accessToken"),
                refresh: text(&value, "refreshToken"),
                expires_in: value.get("expiresIn").and_then(Value::as_i64).unwrap_or(0),
            }
        }
        "idc" => {
            let value = post_json(
                &client,
                &format!("{}/token", endpoints::oidc(&region)),
                &json!({
                    "clientId": credentials.client_id,
                    "clientSecret": credentials.client_secret,
                    "refreshToken": credentials.refresh,
                    "grantType": "refresh_token",
                }),
                &[],
            )
            .await?;
            Refreshed {
                access: text(&value, "accessToken"),
                refresh: text(&value, "refreshToken"),
                expires_in: value.get("expiresIn").and_then(Value::as_i64).unwrap_or(0),
            }
        }
        "external-idp" => {
            if !endpoints::is_https_url(&credentials.token_url) {
                return Err(GatewayError::with_status(
                    401,
                    "Kiro 的外部 IdP 登录态缺少 token 端点，请重新登录",
                ));
            }
            // 外部 IdP 认 OAuth 的标准 form 编码，不认 JSON
            let form = [
                ("grant_type", "refresh_token".to_string()),
                ("client_id", credentials.client_id.clone()),
                ("refresh_token", credentials.refresh.clone()),
            ];
            let response = client
                .post(&credentials.token_url)
                .form(&form)
                .timeout(Duration::from_millis(REQUEST_TIMEOUT_MS))
                .send()
                .await
                .map_err(|error| {
                    GatewayError::with_status(
                        502,
                        format!("续期 Kiro 登录态时网络失败：{}", http::describe(&error)),
                    )
                })?;
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            if !(200..300).contains(&status) {
                return Err(expired());
            }
            let value: Value = serde_json::from_str(&body).map_err(|_| {
                GatewayError::with_status(502, "续期 Kiro 登录态的返回不是 JSON")
            })?;
            Refreshed {
                access: text(&value, "access_token"),
                refresh: text(&value, "refresh_token"),
                expires_in: value.get("expires_in").and_then(Value::as_i64).unwrap_or(0),
            }
        }
        _ => return Err(expired()),
    };
    apply(credentials, refreshed)
}

/// 把一次刷新结果落到凭证上
fn apply(credentials: &KiroCredentials, refreshed: Refreshed) -> Result<KiroCredentials, GatewayError> {
    if refreshed.access.is_empty() {
        return Err(expired());
    }
    let mut next = credentials.clone();
    next.access = refreshed.access;
    // 上游不一定轮换 refresh_token：返回空就保留旧的（写成空等于把账号弄坏）
    if !refreshed.refresh.is_empty() {
        next.refresh = refreshed.refresh;
    }
    let seconds = if refreshed.expires_in > 0 {
        refreshed.expires_in
    } else {
        DEFAULT_EXPIRES_SECONDS
    };
    next.expires = now_ms() + seconds * 1000;
    Ok(next)
}

/// POST 一段 JSON 并解析回 JSON（刷新链路的两条 JSON 路共用）
async fn post_json(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
    headers: &[(&str, &str)],
) -> Result<Value, GatewayError> {
    let mut request = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .timeout(Duration::from_millis(REQUEST_TIMEOUT_MS))
        .json(body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await.map_err(|error| {
        GatewayError::with_status(
            502,
            format!("续期 Kiro 登录态时网络失败：{}", http::describe(&error)),
        )
    })?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        // 400/401/403 是「这份登录态没救了」；别的状态码是上游一时出问题，
        // 分开报才好判断该让用户重登还是稍后重试
        return Err(if matches!(status, 400 | 401 | 403) {
            expired()
        } else {
            GatewayError::with_status(502, format!("续期 Kiro 登录态失败（HTTP {status}）"))
        });
    }
    serde_json::from_str(&text)
        .map_err(|_| GatewayError::with_status(502, "续期 Kiro 登录态的返回不是 JSON"))
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// 找一个登录态用的 profile ARN。
///
/// 为什么需要它：OAuth 账号第一次用的时候 `profileArn` 是空的，而余额、目录与
/// 对话都要带上它（缺了会拿到空清单或 400）。所以「第一次」要现问一次并回写。
///
/// 三个地区依次试：profile 属于**签发地区**，而登录地区只是我们记的，可能因为
/// 转区、门户改版而与实际不符。Builder ID 没有自己的 profile，官方 IDE 与
/// kiro-cli 都用同一个固定值，所以直接返回。
pub async fn lookup_profile(
    credentials: &KiroCredentials,
    proxy: Option<&ResolvedProxy>,
) -> Result<String, GatewayError> {
    if credentials.login_provider == "builderid" {
        return Ok(super::BUILDER_ID_PROFILE.to_string());
    }
    let mut regions: Vec<String> = vec![credentials.region()];
    for fallback in ["us-east-1", "eu-central-1"] {
        if !regions.iter().any(|region| region == fallback) {
            regions.push(fallback.to_string());
        }
    }
    let client = http::client(proxy);
    let mut last: Option<String> = None;
    for region in regions {
        let url = format!(
            "{}/List-Available-Profiles?origin=KIRO_CLI",
            endpoints::management(&region)
        );
        let mut request = client
            .post(&url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", credentials.access))
            .timeout(Duration::from_millis(REQUEST_TIMEOUT_MS))
            .json(&json!({}));
        if let Some(kind) = credentials.token_type() {
            request = request.header("tokentype", kind);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                last = Some(http::describe(&error));
                continue;
            }
        };
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            // IdC 账号在非 Builder ID 的端点会被回一句「Builder ID」：这就是
            // 它该用的那个固定 profile，不是错误
            if credentials.method == "idc" && status == 403 && body.contains("Builder ID") {
                return Ok(super::BUILDER_ID_PROFILE.to_string());
            }
            last = Some(format!("HTTP {status}"));
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&body) else {
            continue;
        };
        if let Some(arn) = value
            .get("profiles")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find_map(|profile| profile.get("arn").and_then(Value::as_str))
        {
            return Ok(arn.to_string());
        }
    }
    Err(GatewayError::with_status(
        502,
        format!(
            "Kiro 没有为这个登录态提供 profile：{}",
            last.unwrap_or_else(|| "接口没有返回任何条目".to_string())
        ),
    ))
}

fn expired() -> GatewayError {
    GatewayError::with_status(401, "Kiro 的登录态已失效，请重新登录 Kiro")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials() -> KiroCredentials {
        KiroCredentials {
            method: "social".to_string(),
            access: "old-access".to_string(),
            refresh: "old-refresh".to_string(),
            expires: 1,
            region: "us-east-1".to_string(),
            ..Default::default()
        }
    }

    /// 刷新结果落地：expires 按 expiresIn 算，refresh 空值不覆盖
    #[test]
    fn apply_keeps_the_old_refresh_token_when_upstream_omits_it() {
        let next = apply(
            &credentials(),
            Refreshed {
                access: "new-access".to_string(),
                refresh: String::new(),
                expires_in: 3600,
            },
        )
        .expect("有 access 就能落地");
        assert_eq!(next.access, "new-access");
        assert_eq!(next.refresh, "old-refresh", "空 refresh 不能把账号弄坏");
        assert!(next.expires > now_ms() + 3_000_000, "一小时有效期");
    }

    /// 上游没给 expiresIn 时默认一小时
    #[test]
    fn a_missing_expiry_defaults_to_one_hour() {
        let before = now_ms();
        let next = apply(
            &credentials(),
            Refreshed {
                access: "a".to_string(),
                refresh: "r".to_string(),
                expires_in: 0,
            },
        )
        .expect("有 access 就能落地");
        let delta = next.expires - before;
        assert!(
            (DEFAULT_EXPIRES_SECONDS * 1000..=DEFAULT_EXPIRES_SECONDS * 1000 + 5_000)
                .contains(&delta),
            "delta={delta}"
        );
    }

    /// 没有 access 的刷新结果一律算失效（不能把空 token 存进去）
    #[test]
    fn an_empty_access_token_is_a_failure() {
        let error = apply(
            &credentials(),
            Refreshed {
                access: String::new(),
                refresh: "r".to_string(),
                expires_in: 3600,
            },
        )
        .expect_err("空 token 必须报错");
        assert_eq!(error.status_code, 401);
    }

    /// 单飞 key：账号 / 方式 / refresh 指纹三者缺一不可
    #[test]
    fn the_refresh_key_separates_accounts_methods_and_token_rotations() {
        let base = credentials();
        let first = refresh_key("kiro-1", &base);
        assert!(first.starts_with("kiro-1:social:"), "{first}");
        assert!(!first.contains("old-refresh"), "完整 token 不许进 key");
        assert_ne!(first, refresh_key("kiro-2", &base), "不同账号不能复用");
        let mut idc = base.clone();
        idc.method = "idc".to_string();
        assert_ne!(first, refresh_key("kiro-1", &idc), "不同方式不能复用");
        let mut rotated = base.clone();
        rotated.refresh = "new-refresh".to_string();
        assert_ne!(first, refresh_key("kiro-1", &rotated), "轮换后不能误取");
    }

    /// API key 没有续期接口 —— 不能因为「临期」就去打刷新
    #[test]
    fn api_keys_are_refused_by_the_refresh_path() {
        let mut key = credentials();
        key.method = "apikey".to_string();
        key.access = "ksk_abc".to_string();
        assert!(!key.can_refresh());
    }
}
