//! WPS 灵犀适配器：三请求编排 + 私有 SSE 翻译通道。
//!
//! ⚠️ 端点与字段按公开参考实现移植，**尚未真机实测**（方案 §1 的 4 处冲突）。
//! 硬约束：release 是 `panic=abort`，零 unwrap/expect/panic，不持锁跨 await。

use axum::http::HeaderMap;
use serde_json::{json, Map, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::core::providers::adapter::{
    ChatRequestPlan, ModelRefreshOutcome, ProviderAdapter, UpstreamErrorClass, UpstreamResponse,
};
use crate::server::core::providers::ProviderKind;
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::{channel, chat, classify, client, credentials, endpoints, models, session, usage, ws};

pub static LINGXI_ADAPTER: LingxiAdapter = LingxiAdapter;
pub struct LingxiAdapter;

fn account_id(account: &Value) -> String {
    account
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn model_of(body: &Value) -> &str {
    body.get("model").and_then(Value::as_str).unwrap_or("").trim()
}

fn identity(sid: &str, uid: &str) -> String {
    if uid.is_empty() {
        sid.to_string()
    } else {
        uid.to_string()
    }
}

impl ProviderAdapter for LingxiAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Lingxi
    }

    fn list_models(&self) -> Vec<Value> {
        models::current()
    }

    /// 发送前异步准备：320KB 预检 → 建会话 → 领 run → 上报 running。
    /// 结果落在进程内 `session::global()`，`build_chat_request` 按同一键读。
    fn prepare_body<'a>(
        &'a self,
        account: &'a Value,
        body: &'a Value,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<Value>, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            if let Some(error) = client::precheck(body) {
                return Err(error);
            }
            let (sid, cookie, uid) = client::credentials_of(account);
            if sid.is_empty() {
                return Err(GatewayError::with_status(
                    401,
                    "WPS 灵犀账号缺少 wps_sid，请重新粘贴 Cookie",
                ));
            }
            let model = model_of(body);
            let id = account_id(account);
            let key = session::cache_key(&id, model);
            let cached = session::global().lookup(&key, logging::now_ms());
            let device = credentials::device_id(&identity(&sid, &uid));
            let prepared = client::prepare_conversation(
                model,
                &id,
                &sid,
                &device,
                cookie.as_deref(),
                body.get("messages").unwrap_or(&Value::Null),
                cached,
            )
            .await?;
            session::global().store(&key, prepared);
            Ok(None)
        })
    }

    fn build_chat_request(
        &self,
        account: &Value,
        body: &Value,
        _client_headers: &HeaderMap,
    ) -> Result<ChatRequestPlan, GatewayError> {
        let (sid, cookie, uid) = client::credentials_of(account);
        if sid.is_empty() {
            return Err(GatewayError::with_status(
                401,
                "WPS 灵犀账号缺少 wps_sid，请重新粘贴 Cookie",
            ));
        }
        let model = model_of(body);
        let key = session::cache_key(&account_id(account), model);
        let prepared = session::global()
            .lookup(&key, logging::now_ms())
            .ok_or_else(|| {
                GatewayError::with_status(502, "灵犀会话未就绪（prepare_body 未成功）")
            })?;
        let device = credentials::device_id(&identity(&sid, &uid));
        // ⚠️ 沿用「建 run 成功的那个域」：对话由 provider_loop 直接发，不走
        // client::request 的双域回落；只在 kdocs.cn 有效的账号用主域会 401。
        let host = if prepared.base.is_empty() {
            endpoints::API_HOST
        } else {
            prepared.base.as_str()
        };
        Ok(ChatRequestPlan {
            url: endpoints::url_on(host, endpoints::chat_path()),
            // session_id / run_id 只在头里，不进 body（两个参考实现源码一致）。
            headers: endpoints::chat_headers(
                host,
                &sid,
                &device,
                cookie.as_deref(),
                &endpoints::request_id(),
                &prepared.session_id,
                &prepared.run_id,
            ),
            body: chat::chat_body(body, model),
            response: UpstreamResponse::LingxiSse,
        })
    }

    fn classify_error(&self, status: u16, error_body: &Value) -> UpstreamErrorClass {
        let message = format!("上游返回 {status}: {}", client_message(error_body));
        match classify::classify(status, error_body) {
            classify::Failure::Quota | classify::Failure::ModelRateLimited
            | classify::Failure::ShortCooldown => UpstreamErrorClass::QuotaLimited {
                reset_at: None,
                message,
                upstream_code: None,
                status,
            },
            classify::Failure::ContentFiltered => UpstreamErrorClass::ContentBlocked {
                status,
                message,
                upstream_code: None,
            },
            // 401 = 凭证死号：灵犀没有 refresh 接口，不刷新、不换号，
            // 直接给可读错误（方案 L1）。
            _ => UpstreamErrorClass::Fatal {
                status,
                message,
                upstream_code: None,
            },
        }
    }

    fn stream_refusal_finishes(&self) -> &'static [&'static str] {
        &["content_filter", "refusal"]
    }

    fn ensure_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let record = store.lingxi_account_record(account_id).ok_or_else(|| {
                GatewayError::with_status(401, "没有可用的 WPS 灵犀账号，请先粘贴 wps_sid")
            })?;
            Ok(record
                .get("wpsSid")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string())
        })
    }

    fn refresh_models<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
        _force: bool,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = ModelRefreshOutcome> + Send + 'a>,
    > {
        Box::pin(async move {
            let Some(record) = store.lingxi_account_record(account_id) else {
                return ModelRefreshOutcome::failed("没有可用的 WPS 灵犀账号");
            };
            let (sid, cookie, uid) = client::credentials_of(&record);
            if sid.is_empty() {
                return ModelRefreshOutcome::failed("灵犀账号缺少 wps_sid");
            }
            let device = credentials::device_id(&identity(&sid, &uid));
            match client::catalog(&sid, &device, cookie.as_deref()).await {
                Ok(rows) => ModelRefreshOutcome::refreshed(models::replace(rows)),
                Err(error) => ModelRefreshOutcome::failed(error.message),
            }
        })
    }

    /// 额度＝智点。`GET /api/public/v1/credits/balance` 已由两个参考实现源码
    /// 核实；但仍**读不出就降级**：`available: null` + 空 wallets，由界面写
    /// 「暂不可用」，绝不拿 0 冒充已查到（真机 2xx 未复验）。
    fn query_usage<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Value, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let record = store.lingxi_account_record(account_id).ok_or_else(|| {
                GatewayError::with_status(401, "没有可用的 WPS 灵犀账号，请先粘贴 wps_sid")
            })?;
            let (sid, cookie, uid) = client::credentials_of(&record);
            if sid.is_empty() {
                return Err(GatewayError::with_status(
                    400,
                    "灵犀账号缺少 wps_sid，请重新粘贴 Cookie",
                ));
            }
            let device = credentials::device_id(&identity(&sid, &uid));
            let reply = client::request(
                "GET",
                endpoints::balance_path(),
                &sid,
                &device,
                cookie.as_deref(),
                None,
            )
            .await?;
            // 死号只认上游 401/403（方案 L1，不按时间猜过期）。
            if reply.status == 401 || reply.status == 403 {
                return Err(GatewayError::with_status(
                    401,
                    "WPS 灵犀登录态已失效，请重新粘贴 Cookie",
                ));
            }
            let parsed = if (200..300).contains(&reply.status) {
                usage::parse_balance(&reply.body, logging::now_ms())
            } else {
                None
            };
            Ok(balance_view(parsed, reply.status))
        })
    }

    fn supports_model_refresh(&self) -> bool {
        true
    }

    fn supports_usage(&self) -> bool {
        true
    }

    fn supports_refresh(&self) -> bool {
        false // 无续期接口（方案 L1）
    }

    /// **只有选 WS 通道时才是有状态的**（P3，2026-11-14）。
    ///
    /// 这个返回值决定编排层走哪条路：
    ///   · `false`（默认，`channel::current() == Sse`）→ 现有单请求路径：
    ///     `prepare_body` 三请求 + `build_chat_request` + `LingxiSse` 翻译器，
    ///     一行都不用改；
    ///   · `true` → [`Self::forward_conversation`]，里面只有 cowork WS 一条链。
    ///
    /// ⚠️ 为什么用「同一个 provider 按设置切形态」而不是开两家：WS 通道要的
    /// `ForwardOutcome` 与 SSE 的 `reqwest::Response` 不是一个类型，无法共存于
    /// 单请求路径；而另开一家 provider 会让模型清单/账号/Key 白名单各多一套，
    /// 收益换不来复杂度。编排层每请求现读这个值，**切换下一条请求即生效**。
    fn is_stateful(&self) -> bool {
        channel::current() == channel::Channel::Ws
    }

    /// WS 通道的会话式转发（只有 `is_stateful()` 为真时会走到）。
    ///
    /// 与 SSE 通道的三处差异（单段文本入参 / 无 token 用量 / 代理只作用于建
    /// 会话）在 `ws.rs` 模块头逐条写明 —— 界面与文档要按同一口径解释。
    fn forward_conversation<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
        body: &'a Value,
        _client_headers: &HeaderMap,
        _proxy: Option<crate::server::core::proxies::ResolvedProxy>,
        stream: bool,
        _telemetry: &'a std::sync::Arc<
            crate::server::core::upstream::usage::RequestTelemetry,
        >,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        crate::server::core::upstream::ForwardOutcome,
                        GatewayError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let record = store.lingxi_account_record(account_id).ok_or_else(|| {
                GatewayError::with_status(401, "没有可用的 WPS 灵犀账号，请先登录或粘贴 wps_sid")
            })?;
            let (sid, cookie, uid) = client::credentials_of(&record);
            if sid.is_empty() {
                return Err(GatewayError::with_status(
                    401,
                    "WPS 灵犀账号缺少 wps_sid，请重新登录或粘贴 Cookie",
                ));
            }
            if let Some(error) = client::precheck(body) {
                return Err(error);
            }
            let model = model_of(body);
            if model.is_empty() {
                return Err(GatewayError::with_status(400, "请求缺少 model"));
            }
            let device = credentials::device_id(&identity(&sid, &uid));
            ws::forward(&sid, &device, cookie.as_deref(), model, body, stream).await
        })
    }

    /// 灵犀支持网页登录（P1，2026-11-14）：登录页就是 `lingxi.wps.cn`，
    /// 登录成功后 `wps_sid` 落在 `.wps.cn` 域，壳侧 `CookieManager` 读得到
    /// （HttpOnly 不影响读）。**没有** OAuth 授权码可换，所以收尾走
    /// `exchange_login_code`（`callbackUrl` 字段里装的是 Cookie 载荷，
    /// 与豆包工作同一契约）。
    fn supports_web_login(&self) -> bool {
        true
    }

    /// 授权地址 = **WPS 通行证登录页**（`account.wps.cn`，服务端直出的手机号
    /// 登录表单）；state 只用于把回调配回本次任务（没有 CSRF 换码语义）。
    ///
    /// ⚠️ 为什么不做「带 redirect 的回调页」：灵犀的登录是纯 Cookie 站点，
    /// 官方页不会把任何码回推到本机端口；壳侧登录窗直接读 Cookie 交回即可
    /// （Android 上比桌面 CDP 更简单，见方案 L3）。
    ///
    /// ⚠️ 为什么不加载灵犀自己的网页版（`lingxi.kdocs.cn`）：那个域对移动 UA
    /// 返回的是 **WPS 客户端容器页**（依赖 `window.wps` JSBridge），普通 WebView
    /// 里就是一片空白；桌面 UA 下虽然出 SPA，但首屏要拉 CDN 上十几个带 SRI 的
    /// ES module，移动网络下很容易白屏。详见 `endpoints::LOGIN_HOST` 的注释。
    fn build_login_url(&self) -> Option<(String, String)> {
        Some((
            format!("{}/", endpoints::LOGIN_HOST),
            crate::server::core::upstream::request::new_request_id(),
        ))
    }

    /// 网页登录收尾：Cookie 载荷 → 归一 → **上游探活** → 落账号。
    ///
    /// 探活是这条链唯一的正确性判据：`wps_sid` 约半天失效（方案 L1），
    /// 而且同一浏览器后登录的账号会把前一个顶掉（L2）—— 不探活就落库，
    /// 用户会得到一个「加进去了但一调就 401」的账号。
    /// 探活只认「两个域都 401/403」为死号（`client::probe`），传输失败不算。
    fn exchange_login_code<'a>(
        &'a self,
        store: &'a AccountStore,
        code: &'a str,
        _state: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let credential = credentials::normalize_paste(code).map_err(|reason| {
                GatewayError::with_status(400, format!("登录窗口里没有拿到有效的 wps_sid：{reason}"))
            })?;
            let identity = credential.uid.clone().unwrap_or_else(|| credential.wps_sid.clone());
            let device = credentials::device_id(&identity);
            if !client::probe(&credential.wps_sid, &device, credential.cookie.as_deref()).await {
                return Err(GatewayError::with_status(
                    401,
                    "WPS 灵犀登录态无效（另一个账号刚登录过会把上一个顶掉），请重新登录",
                ));
            }
            let payload = json!({
                "wps_sid": credential.wps_sid,
                "cookie": credential.cookie,
                "uid": credential.uid.unwrap_or_default(),
            });
            let account = store
                .add_lingxi_account(&payload, None, "web")
                .map_err(|error| GatewayError::with_status(error.status_code, error.message))?;
            Ok(account
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string())
        })
    }

    fn supports_default_model(&self) -> bool {
        false
    }
}

/// 余额的归一化形状（与 `ProviderAdapter::query_usage` 契约一致）。
/// 判据：`None` ⇒ `available` 必须是 `null`、wallets 必须为空 —— 界面据此
/// 写「暂不可用」，而不是把「没读到」画成一个 0 积分的钱包。
fn balance_view(parsed: Option<usage::Balance>, status: u16) -> Value {
    let available = parsed
        .as_ref()
        .map(|balance| Value::from(balance.total))
        .unwrap_or(Value::Null);
    let wallets: Vec<Value> = parsed
        .as_ref()
        .map(|balance| {
            vec![json!({
                "type": "points",
                "displayName": "积分",
                "balance": balance.total,
            })]
        })
        .unwrap_or_default();
    let mut raw = Map::new();
    raw.insert("upstreamStatus".to_string(), Value::from(status));
    if let Some(ts) = parsed.as_ref().and_then(|balance| balance.earliest_expiry_ms) {
        raw.insert("earliestExpiryMs".to_string(), Value::from(ts));
    }
    json!({
        "available": available,
        "unit": "积分",
        "wallets": wallets,
        "subscription": Value::Null,
        "raw": Value::Object(raw),
    })
}

/// 上游文案（三个常见位置都认，截断 300 字；不把 Cookie 写进日志）。
fn client_message(body: &Value) -> String {
    body.pointer("/error/message")
        .or_else(|| body.get("message"))
        .or_else(|| body.get("msg"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .take(300)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::core::providers::adapter::ProviderAdapter;
    use serde_json::json;

    #[test]
    fn adapter_declares_lingxi_kind() {
        assert_eq!(LINGXI_ADAPTER.kind(), ProviderKind::Lingxi);
        assert_eq!(endpoints::ID, "lingxi");
    }

    #[test]
    fn builtin_catalog_is_not_empty() {
        assert!(!LINGXI_ADAPTER.list_models().is_empty());
    }

    #[test]
    fn dead_credential_is_fatal_not_a_refresh() {
        assert!(matches!(
            LINGXI_ADAPTER.classify_error(401, &json!({ "message": "用户无权限" })),
            UpstreamErrorClass::Fatal { status: 401, .. }
        ));
    }

    #[test]
    fn quota_and_rate_limit_feed_the_model_cooldown() {
        assert!(matches!(
            LINGXI_ADAPTER.classify_error(429, &json!({})),
            UpstreamErrorClass::QuotaLimited { .. }
        ));
        assert!(matches!(
            LINGXI_ADAPTER.classify_error(402, &json!({})),
            UpstreamErrorClass::QuotaLimited { .. }
        ));
    }

    #[test]
    fn content_filter_is_not_blamed_on_the_account() {
        assert!(matches!(
            LINGXI_ADAPTER.classify_error(200, &json!({ "message": "AuditFailed" })),
            UpstreamErrorClass::ContentBlocked { .. }
        ));
    }

    #[test]
    fn unreadable_balance_degrades_to_null_not_zero() {
        let view = balance_view(None, 502);
        assert!(view.get("available").is_some_and(Value::is_null));
        assert_eq!(view["wallets"].as_array().map(Vec::len), Some(0));
        assert_eq!(view["raw"]["upstreamStatus"], json!(502));
    }

    #[test]
    fn parsed_balance_is_reported_as_points() {
        let view = balance_view(
            Some(usage::Balance {
                total: 42.5,
                earliest_expiry_ms: Some(1_700_000_000_000),
            }),
            200,
        );
        assert_eq!(view["available"].as_f64(), Some(42.5));
        assert_eq!(view["wallets"].as_array().map(Vec::len), Some(1));
        assert_eq!(view["raw"]["earliestExpiryMs"], json!(1_700_000_000_000_i64));
    }
}
