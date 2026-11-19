//! AStudio 适配器：把「Responses 协议 + 两套凭据」接进 provider 注册表。
//!
//! ── 为什么 `is_stateful() == true` ──────────────────────────
//! 上游响应是 OpenAI **Responses** 的事件流，`UpstreamKind` 是封闭枚举、没有
//! 这个变体。走 `forward_conversation` 之后本家自己读事件、自己推标准 chat 帧，
//! 网关的账号轮换 / 冷却 / 统计照旧工作。因此 `build_chat_request` 只做
//! **防御性报错**，真活在 `chat::run_chat`。
//!
//! ── 能力位逐条（「没有就如实说没有」）────────────────────────
//! ```text
//!   chat             ✓   会话式转发（/model-gateway/v1/responses）
//!   refresh          ✗   上游**没有续期接口**：凭据是登录时发的死票，
//!                        失效只能重贴。所以 `supports_refresh` 为 false ——
//!                        后台续期任务根本不会问它（界面也不该出现刷新按钮）
//!   model_refresh    ✓   /api/v1/model-manager/models（Bearer 模型凭据）
//!   usage            ✓   points/balance + membership/me（网页会话 Cookie）
//!   checkin          ✓   每日登录积分（靠余额差观测，见 `checkin.rs`）
//!   web_login        ✗   登录页（手机号 / 密码 + 极验滑块）属客户端交互，
//!                        按项目约束单独立项；本轮只保证粘贴路径
//!   default_model    ✗   未指定时用本家自己的 `astronclaw-auto`
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

use super::credentials::Credentials;
use super::{chat, errors, models, usage};

/// AStudio 适配器（一家 provider）
pub struct AStudioAdapter;

/// `adapter_for(ProviderKind::Astudio)` 返回的静态实例
pub static ASTUDIO_ADAPTER: AStudioAdapter = AStudioAdapter;

/// 从账号记录里取凭证 + 解析代理，供目录 / 余额 / 签到 / 探测几处共用。
///
/// 几处都要「记录 → 凭证 → 代理」这一段，分开写就会有一处漏了代理（症状是
/// 「配了代理，但目录/余额走直连」，很难看出来）。
///
/// **没有续期这一步**：本家没有续期接口（见模块头），`Credentials` 读出来
/// 就是终值。
pub(super) async fn prepare(
    store: &AccountStore,
    account_id: &str,
) -> Result<(String, Credentials, Option<ResolvedProxy>, Value), GatewayError> {
    let record = store.astudio_account_record(account_id).ok_or_else(|| {
        GatewayError::with_status(
            404,
            "还没有 AStudio 账号：请粘贴 modelBearerToken（或整段 astron-session.json）",
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
    Ok((id, credentials, proxy, record))
}

impl ProviderAdapter for AStudioAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Astudio
    }

    /// 上游是 Responses 事件流（固有性质）—— 编排层据此分流到 `forward_conversation`
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
            // 凭证只从存储里读；代理用**编排层给的这一份**（与 kiro 同一处置）。
            // `prepare` 里也会解析代理，但那条服务于目录 / 余额 / 签到这些
            // 「没有编排层」的路径 —— 转发这条再解析一次会覆盖掉编排层的选择。
            let (id, credentials, _, _) = prepare(store, account_id).await?;
            chat::run_chat(&credentials, &id, body, proxy, stream, telemetry).await
        })
    }

    /// 取可用访问令牌。
    ///
    /// 本家没有续期：这条只是「凭证存在性校验」，把模型凭据原样返回。
    /// 编排层拿它做发送前的前置检查（失败不致命，会继续尝试转发）。
    fn ensure_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let (_, credentials, _, _) = prepare(store, account_id).await?;
            Ok(credentials.key)
        })
    }

    /// 上游没有续期接口（见模块头）：后台续期任务不该问本家
    fn supports_refresh(&self) -> bool {
        false
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

    /// 有积分概念（见 `usage.rs`）
    fn supports_usage(&self) -> bool {
        true
    }

    /// 查余额（归一化形状见 trait 文档；只有模型凭据的账号返回 `noSession`）
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

    /// **有**登录页，但**不走这条通用链**，所以如实答 false。
    ///
    /// 这个能力位问的是「能不能按通用网页登录链跑」：`build_login_url` 给一个
    /// 外站授权地址，用户在浏览器里登录，回调带 code 回来
    /// （`submit_login_callback` → `exchange_login_code`）。AStudio 两样都不成立：
    /// 登录页是**我们自己起在 127.0.0.1 上**的（极验滑块只能在地页面里做），
    /// 页面完成后交回的是**已经换好的完整会话**（含 modelBearerToken），
    /// 没有 code 可换。所以它走专用入口
    /// `LoginService::start_astudio_login`（见 `core::login::astudio`）。
    ///
    /// 答 false 的后果只有一个：`login_start` 的通用分支不会把它接走 —— 正是
    /// 我们要的（那个分支会调 `build_login_url` 拿到 None 然后报 400）。
    fn supports_web_login(&self) -> bool {
        false
    }

    /// 未指定模型时用本家自己的 `astronclaw-auto`，不接受别家注入的模型名
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
        assert!(ASTUDIO_ADAPTER.is_stateful());
        let plan =
            ASTUDIO_ADAPTER.build_chat_request(&Value::Null, &Value::Null, &HeaderMap::new());
        let Err(error) = plan else {
            panic!("单请求路径必须拒绝（本家只走 forward_conversation）");
        };
        assert_eq!(error.status_code, 503);
    }

    /// 能力位：无续期 / 有目录 / 有余额 / 无网页登录 / 无默认模型
    #[test]
    fn capability_flags_are_honest() {
        // 「没有续期接口」是本家最容易被误加的地方：加一个 `supports_refresh`
        // 就会让后台每 2 分钟去打一次不存在的刷新端点
        assert!(!ASTUDIO_ADAPTER.supports_refresh());
        assert!(ASTUDIO_ADAPTER.supports_model_refresh());
        assert!(ASTUDIO_ADAPTER.supports_usage());
        assert!(!ASTUDIO_ADAPTER.supports_web_login());
        assert!(!ASTUDIO_ADAPTER.supports_default_model());
    }

    /// 错误分类透传上游原文，且「输入过长」不被当成额度
    #[test]
    fn error_classes_follow_the_shared_judgement() {
        match ASTUDIO_ADAPTER.classify_error(401, &json!({ "message": "invalid token" })) {
            UpstreamErrorClass::TokenExpired { message } => assert_eq!(message, "invalid token"),
            other => panic!("401 应判成 TokenExpired：{other:?}"),
        }
        match ASTUDIO_ADAPTER.classify_error(
            400,
            &json!({ "error": { "message": "This model's maximum context length is 262144 tokens" } }),
        ) {
            UpstreamErrorClass::Fatal { status, message, .. } => {
                assert_eq!(status, 400, "输入过长要原样 400，不换号");
                assert!(message.contains("maximum context length"), "{message}");
            }
            other => panic!("输入过长应判成 Fatal 400：{other:?}"),
        }
        assert!(matches!(
            ASTUDIO_ADAPTER.classify_error(429, &Value::Null),
            UpstreamErrorClass::QuotaLimited { .. }
        ));
        assert!(matches!(
            ASTUDIO_ADAPTER.classify_error(500, &Value::Null),
            UpstreamErrorClass::Fatal { .. }
        ));
    }
}
