//! Kiro 适配器：把「AWS 事件流 + 三种凭证」接进 provider 注册表。
//!
//! ── 为什么 `is_stateful() == true` ──────────────────────────
//! 上游响应是 **AWS event stream**（二进制帧），`UpstreamKind` 是封闭枚举、
//! 没有这个变体。走 `forward_conversation` 之后，本家自己读帧、自己推标准
//! chat 帧，网关的账号轮换 / 冷却 / 统计照旧工作。因此 `build_chat_request`
//! 只做**防御性报错**，真活在 `chat::run_chat`。
//!
//! ── 能力位逐条（「没有就如实说没有」）────────────────────────
//! ```text
//!   chat                 ✓   会话式转发（generateAssistantResponse）
//!   refresh              ✓   social / idc / external-idp 三条链路；
//!                            API key 没有续期接口，`ensure_fresh` 对它早退
//!   model_refresh        ✓   List-Available-Models（两种凭证两条路）
//!   usage                ✓   Get-Usage-Limits（三个窗口，见 usage.rs）
//!   checkin              ✗   上游没有签到 / 领取类活动
//!   web_login            ✗   登录流程是 UI 类改动，本轮不做（走粘贴）
//!   default_model        ✗   未指定时用本家自己的 `auto`，不接受别家注入的模型名
//! ```
//!
//! ── 错误分类 ────────────────────────────────────────────────
//! 全部交给 `errors::classify`（四档语义与「输入过长优先于一切」的判据都在
//! 那里）。这里不另写一套 —— 两套分类会让「同一个 403 在这里是换号、在那里
//! 是续期」这类分叉出现。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::sync::Arc;

use axum::http::HeaderMap;
use serde_json::Value;

use crate::server::core::account_store::AccountStore;
use crate::server::core::proxies::{self, ProxyResolution, ResolvedProxy};
use crate::server::core::providers::adapter::{
    ChatRequestPlan, ModelRefreshOutcome, ProviderAdapter, UpstreamErrorClass,
};
use crate::server::core::providers::{kind_id, ProviderKind};
use crate::server::core::upstream::usage::RequestTelemetry;
use crate::server::core::upstream::ForwardOutcome;
use crate::server::errors::GatewayError;

use super::credentials::KiroCredentials as Credentials;
use super::{chat, errors, models, refresh, usage};

/// Kiro 适配器（一家 provider，两个订阅地区共用，见 `mod.rs` 模块头）
pub struct KiroAdapter;

/// `adapter_for(ProviderKind::Kiro)` 返回的静态实例
pub static KIRO_ADAPTER: KiroAdapter = KiroAdapter;

/// 从账号记录里取凭证 + 解析代理，供目录 / 余额 / 探测三处共用。
///
/// 三处都要「记录 → 凭证 → 代理」这一段，分开写就会有一处忘了续期或忘了
/// 代理（代理漏了的表现是「配了代理但目录/余额走直连」，很难看出来）。
async fn prepare(
    store: &AccountStore,
    account_id: &str,
) -> Result<(String, Credentials, Option<ResolvedProxy>, Value), GatewayError> {
    let record = store.kiro_account_record(account_id).ok_or_else(|| {
        GatewayError::with_status(
            404,
            "还没有 Kiro 账号：请粘贴 ksk_ 开头的 API key，或粘贴登录态",
        )
    })?;
    let id = record
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(account_id)
        .to_string();
    let credentials = Credentials::from_fields(&record)?;
    let proxy = match proxies::resolve_account_proxy(record.get("proxy")) {
        Some(ProxyResolution::Resolved(proxy)) => Some(proxy),
        Some(ProxyResolution::Failed(reason) | ProxyResolution::Broken(reason)) => {
            return Err(GatewayError::with_status(502, reason))
        }
        None => None,
    };
    let credentials = refresh::ensure_fresh(&id, &credentials, proxy.as_ref()).await?;
    Ok((id, credentials, proxy, record))
}

impl ProviderAdapter for KiroAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Kiro
    }

    /// 上游是二进制事件流（固有性质）—— 编排层据此分流到 `forward_conversation`
    fn is_stateful(&self) -> bool {
        true
    }

    /// 模型清单：进程内按账号缓存；`list_models` 没有账号参数，所以这里给
    /// **本次会话里见过的并集**（见 `models::list_any`）
    fn list_models(&self) -> Vec<Value> {
        models::list_any()
    }

    /// **防御性报错**：本家走会话式转发，不走单请求路径
    fn build_chat_request(
        &self,
        _account: &Value,
        _body: &Value,
        _client_headers: &HeaderMap,
    ) -> Result<ChatRequestPlan, GatewayError> {
        Err(GatewayError::with_status(
            503,
            format!(
                "{} 走会话式转发，不走单请求路径（内部错误：编排层未按 is_stateful 分流）",
                kind_id(self.kind())
            ),
        ))
    }

    /// 上游错误分类（四档语义见 `adapter` 模块头与 `errors` 的判据）
    fn classify_error(&self, status: u16, error_body: &Value) -> UpstreamErrorClass {
        let text = error_body.to_string();
        errors::classify(status, &text)
    }

    /// 会话式转发入口
    fn forward_conversation<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
        body: &'a Value,
        _client_headers: &'a HeaderMap,
        proxy: Option<ResolvedProxy>,
        stream: bool,
        telemetry: &'a Arc<RequestTelemetry>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ForwardOutcome, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            chat::run_chat(store, account_id, body, proxy, stream, telemetry).await
        })
    }

    /// 取可用访问令牌（会续期并写回；API key 没有续期接口，原样返回）
    fn ensure_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let (_, credentials, _, original) = prepare(store, account_id).await?;
            // 续期结果按「比较-再写」回库：期间被重新导入就放弃
            let _ = store.update_kiro_credentials_if_current(&original, &credentials);
            Ok(credentials.access)
        })
    }

    /// 有续期链路（OAuth 三种登录方式）。API key 账号由 `ensure_fresh` 自己早退，
    /// 所以这里回答「这家有没有续期能力」而不是「这个账号能不能续期」。
    fn supports_refresh(&self) -> bool {
        true
    }

    /// 有远程模型目录
    fn supports_model_refresh(&self) -> bool {
        true
    }

    /// 刷新目录（`force = false` 走 TTL）
    fn refresh_models<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
        force: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ModelRefreshOutcome> + Send + 'a>>
    {
        Box::pin(async move {
            let (id, credentials, proxy, _) = match prepare(store, account_id).await {
                Ok(parts) => parts,
                Err(error) => return ModelRefreshOutcome::failed(error.message),
            };
            models::refresh(&credentials, &id, proxy.as_ref(), force).await
        })
    }

    /// 有额度概念（积分制，见 `usage.rs`）
    fn supports_usage(&self) -> bool {
        true
    }

    /// 查额度（归一化形状见 trait 文档）
    fn query_usage<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, GatewayError>> + Send + 'a>>
    {
        Box::pin(async move {
            let (_, credentials, proxy, _) = prepare(store, account_id).await?;
            usage::query(&credentials, proxy.as_ref()).await
        })
    }

    /// 有浏览器登录，但**不走**这条通用链。
    ///
    /// 通用链的契约是「`build_login_url` 给外站地址 + 等 `submit_login_callback`
    /// 带回 code」。Kiro 两样都不成立：回调落在**本进程自己抢的白名单端口**上
    /// （不是网关端口，门户只认那几条），而且中途要由我们 302 把浏览器送去
    /// AWS —— 一次登录会经过两个不同的回调路径。所以它有自己的入口
    /// （`LoginService::start_kiro_login`），与 Trae / AStudio 并列。
    fn supports_web_login(&self) -> bool {
        false
    }

    /// 未指定模型时用本家自己的 `auto`，不接受别家注入的模型名
    fn supports_default_model(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 本家是会话式转发；单请求路径必须明确拒绝（而不是发一半）
    #[test]
    fn single_request_path_is_refused() {
        assert!(KIRO_ADAPTER.is_stateful());
        let plan = KIRO_ADAPTER.build_chat_request(&Value::Null, &Value::Null, &HeaderMap::new());
        let Err(error) = plan else {
            panic!("单请求路径必须拒绝（本家只走 forward_conversation）");
        };
        assert_eq!(error.status_code, 503);
    }

    /// 能力位：有续期 / 有目录 / 有额度 / 无网页登录 / 无默认模型
    #[test]
    fn capability_flags_are_honest() {
        assert!(KIRO_ADAPTER.supports_refresh());
        assert!(KIRO_ADAPTER.supports_model_refresh());
        assert!(KIRO_ADAPTER.supports_usage());
        assert!(!KIRO_ADAPTER.supports_web_login());
        assert!(!KIRO_ADAPTER.supports_default_model());
    }

    /// 错误分类透传上游原文，且「输入过长」不被当成额度
    #[test]
    fn error_classes_follow_the_shared_judgement() {
        match KIRO_ADAPTER.classify_error(401, &json!({ "message": "token expired" })) {
            UpstreamErrorClass::TokenExpired { message } => assert_eq!(message, "token expired"),
            other => panic!("401 应判成 TokenExpired：{other:?}"),
        }
        match KIRO_ADAPTER.classify_error(
            400,
            &json!({ "message": "input is too long for the model's context: 200000" }),
        ) {
            UpstreamErrorClass::Fatal { status, message, .. } => {
                assert_eq!(status, 400, "输入过长要原样 400，不换号");
                assert!(message.starts_with(errors::TOO_LONG), "{message}");
            }
            other => panic!("输入过长应判成 Fatal 400：{other:?}"),
        }
        assert!(matches!(
            KIRO_ADAPTER.classify_error(429, &Value::Null),
            UpstreamErrorClass::QuotaLimited { .. }
        ));
        assert!(matches!(
            KIRO_ADAPTER.classify_error(500, &Value::Null),
            UpstreamErrorClass::Fatal { .. }
        ));
    }
}
