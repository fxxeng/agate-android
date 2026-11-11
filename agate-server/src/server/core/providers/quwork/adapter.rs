//! QuWork 适配器（无状态、OpenAI 兼容转发 + 账号面能力）。
//!
//! 上游插件 SDK 的 `Provider` trait 在这里落成静态编译的 `ProviderAdapter`：
//! 只搬协议，不搬 `PluginCtx` / `StoreProxy` / `ChatPlan` / WASM 插件边界。
//!
//! ── 能力位 ─────────────────────────────────────────────────
//! chat / refresh / model_refresh / usage / checkin 全部声明为真（对齐上游
//! `meta().with_capabilities`）；目录是**公开无鉴权**的，故 `refresh_uses_account`
//! 为 false。上游 `supports_default_model` 亦为真。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic，不持锁跨 await。

use axum::http::HeaderMap;
use serde_json::Value;

use crate::server::core::account_store::AccountStore;
use crate::server::core::providers::adapter::{
    ChatRequestPlan, ModelRefreshOutcome, ProviderAdapter, ReasoningPatch, UpstreamErrorClass,
};
use crate::server::core::providers::ProviderKind;
use crate::server::errors::GatewayError;

use super::{chat, credentials, endpoints, models, refresh, usage};

/// `adapter_for(ProviderKind::QuWork)` 取它
pub static QUWORK_ADAPTER: QuWorkAdapter = QuWorkAdapter;

pub struct QuWorkAdapter;

/// 会话形态里的模型凭证（`session_from_record` 把落库的 `accessToken` 镜像放进
/// `auth.accessToken`；那个 `accessToken` 落库时就是 llmToken，见 `mod.rs`）。
fn token_in_session(account: &Value) -> String {
    account
        .get("auth")
        .and_then(|auth| auth.get("accessToken"))
        .or_else(|| account.get("accessToken"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

impl ProviderAdapter for QuWorkAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::QuWork
    }

    /// 当前目录（内置兜底 10 条 / 远程刷新后的清单）
    fn list_models(&self) -> Vec<Value> {
        models::current().list()
    }

    /// `POST {MODEL_ORIGIN}/chat/completions`
    fn build_chat_request(
        &self,
        account: &Value,
        body: &Value,
        _client_headers: &HeaderMap,
    ) -> Result<ChatRequestPlan, GatewayError> {
        let token = token_in_session(account);
        if token.is_empty() {
            return Err(GatewayError::with_status(
                401,
                "QuWork 账号缺少模型凭证 llmToken，无法转发（请重新粘贴凭证）",
            ));
        }
        Ok(ChatRequestPlan::chat(
            format!("{}/chat/completions", endpoints::MODEL_ORIGIN),
            chat::headers(&token),
            chat::request_body(body),
        ))
    }

    fn classify_error(&self, status: u16, error_body: &Value) -> UpstreamErrorClass {
        chat::classify(status, error_body)
    }

    /// 内容审核误报也可能以 HTTP 200 + `finish_reason=content_filter` 下发
    /// （上游 `refusal_markers` 声明了这一档）。
    fn stream_refusal_finishes(&self) -> &'static [&'static str] {
        &["content_filter"]
    }

    /// 思考等级 → 模型目录声明的 `reasoningEfforts`
    fn reasoning_patch(&self, level: &str, model: &str, body: &Value) -> ReasoningPatch {
        chat::reasoning_patch(level, model, body)
    }

    /// 取可用 llmToken（含临期主动刷新；账号服务不可用时仍返回原 llmToken）
    fn ensure_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>,
    > {
        Box::pin(refresh::model_token(store, account_id))
    }

    /// 401 之后的**强制**刷新：续期不成功就如实报错（不沿用临期窗口的早退）
    fn refresh_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let secret = refresh::refresh(store, account_id, true).await?;
            credentials::token_of(&secret)
                .map_err(|error| GatewayError::with_status(401, error.message))
        })
    }

    /// 有续期接口（且会轮换登录凭证）
    fn supports_refresh(&self) -> bool {
        true
    }

    /// 临期判据：loginToken 存在且 expiresAt 进入 1 小时窗口
    fn credentials_expiring(&self, store: &AccountStore, account_id: &str) -> bool {
        store
            .quwork_account_record(account_id)
            .is_some_and(|record| refresh::expiring(&record))
    }

    /// 公开远程目录
    fn supports_model_refresh(&self) -> bool {
        true
    }

    /// 目录接口无鉴权，不需要「模型来源」下拉
    fn refresh_uses_account(&self) -> bool {
        false
    }

    fn refresh_models<'a>(
        &'a self,
        _store: &'a AccountStore,
        _account_id: &'a str,
        force: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ModelRefreshOutcome> + Send + 'a>> {
        Box::pin(async move { models::refresh(force).await })
    }

    /// 上游 `meta` 声明 `supports_default_model: true`
    fn supports_default_model(&self) -> bool {
        true
    }

    /// 上游会回自己的内部模型名（对齐 `sse_model_rewrite: true`）
    fn sse_model_rewrite(&self) -> bool {
        true
    }

    fn supports_usage(&self) -> bool {
        true
    }

    fn query_usage<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Value, GatewayError>> + Send + 'a>,
    > {
        Box::pin(usage::query(store, account_id))
    }
}
