//! AtomCode（AtomGit）适配器：**无状态 OpenAI 兼容转发** + 目录 / 额度 /
//! 套餐领取 / 轮询式浏览器登录。
//!
//! ── 上游就是 OpenAI 协议，所以转发这一层最薄 ──────────────────────
//! `POST {gateway}/chat/completions`、bearer 鉴权、SSE 帧也是标准 chat 形态 ——
//! 只改 `model` 与 `stream_options` 两处（见 [`super::chat`] 的模块头）。
//! **无状态**（一次 HTTP 请求 = 一次回答），`is_stateful` 保持默认 false。
//!
//! ── `supports_web_login` 刻意保持 false（这是本文件最容易写错的一处）──
//! 本家的浏览器登录是**轮询型**：授权回调在代理域名（`acs.atomgit.com/callback`）
//! 终结，不回到本机。trait 的 `build_login_url` / `exchange_login_code` 那一对
//! 钩子是**回调型**的（授权页 302 回本机 → 拿 code 换令牌），本家一个都不成立。
//! 宣称 `supports_web_login = true` 会让 `LoginService::start_web_login` 去等一个
//! 永远不来的回调（症状：授权成功、界面一直挂着等跳转，且没有任何报错）。
//! 本家的发起入口是 `LoginService::start_atomcode_login`（`api::session::login_start`
//! 在通用分支之前分流），与 ZCode / OfficeAce 同一处置。
//!
//! ── 续期会轮换 refresh token ────────────────────────────────
//! 所以 `supports_refresh` 为真，且 [`ProviderAdapter::refresh_access_token`] 走
//! [`super::refresh::ensure_fresh`]（单飞 + 比较-再写）。[`ProviderAdapter::ensure_access_token`]
//! **不发网络请求**：它在转发路径上被调，临期只记一行提示（与 lobsterai 同口径）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic，不持锁跨 await。

use axum::http::HeaderMap;
use serde_json::Value;

use crate::server::core::account_store::AccountStore;
use crate::server::core::providers::adapter::{
    ChatRequestPlan, ModelRefreshOutcome, ProviderAdapter, UpstreamErrorClass,
};
use crate::server::core::providers::ProviderKind;
use crate::server::errors::GatewayError;

use super::{chat, credentials, errors, models, refresh, usage};

/// 一家 AtomCode 适配器（**单一站点**，没有地区 / 额度池之分）
pub struct AtomcodeAdapter;

/// 唯一实例
pub static ATOMCODE_ADAPTER: AtomcodeAdapter = AtomcodeAdapter;

impl ProviderAdapter for AtomcodeAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Atomcode
    }

    /// 清单来自 `models.rs` 的进程缓存（**没有内置兜底**：没刷到过就是空）
    fn list_models(&self) -> Vec<Value> {
        models::list()
    }

    /// 构造 `POST {gateway}/chat/completions`（OpenAI 协议，body 只改两处）
    fn build_chat_request(
        &self,
        account: &Value,
        body: &Value,
        client_headers: &HeaderMap,
    ) -> Result<ChatRequestPlan, GatewayError> {
        chat::build_chat_request(account, body, client_headers)
    }

    /// 上游错误分类。判定顺序本身是契约（见 [`errors::classify_gateway_error`]）：
    ///   1. `ATOMCODE_SIG_MISSING`（打到了需要签名的主机）→ `Fatal`，**先于** 401/403；
    ///   2. 401 / 403 → `TokenExpired`（文案含 `codingplan` 时是「没领套餐」→ `Fatal`）；
    ///   3. 429 → `QuotaLimited`（网关不给结构化恢复时间）；
    ///   4. 其余 → 共用内容拦截分类。
    fn classify_error(&self, status: u16, error_body: &Value) -> UpstreamErrorClass {
        errors::classify_gateway_error(status, error_body)
    }

    /// **流内静默失败**的分类（`HTTP 200` + 正文是「参数错误」/「三方请求失败…」）。
    ///
    /// 本家的这两种失败都在**成功状态**下下发（上游实测记录见 `errors` 的模块头），
    /// `finish_reason` 那条路认不出它们 —— 认不出的后果是把网关的报错页当模型
    /// 回答粘进对话，客户端完全看不出异常。所以必须有这一支。
    fn stream_error_classifier(&self, message: &str) -> Option<UpstreamErrorClass> {
        errors::classify_stream_error(message)
    }

    /// 声明「有分类能力」—— 预读门控靠它决定要不要缓冲上游的流。
    /// ⚠️ 覆写了分类器就必须覆写本方法，否则分类器永远收不到文案。
    fn has_stream_error_classifier(&self) -> bool {
        true
    }

    /// 取可用令牌。**这里不发网络请求**（本方法会在转发路径上被调）：临期只记一行
    /// 提示，真正的续期由维护链路与 401 后的强制刷新负责。
    fn ensure_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>>
    {
        Box::pin(async move {
            let record = store.atomcode_account_record(account_id);
            if !account_id.is_empty() && record.is_none() {
                return Err(GatewayError::with_status(
                    401,
                    format!("atomcode 账号 {account_id} 不存在（请重新添加）"),
                ));
            }
            let credentials = credentials::from_record(record.as_ref())?;
            if credentials.expiring() {
                crate::server::logging::verbose(
                    "[AtomCode]",
                    "账号令牌已过期或临期，将按需续期（续期会轮换 refresh token）",
                );
            }
            Ok(credentials.access_token)
        })
    }

    /// 401 之后的**强制**续期：刷新并**把轮换后的令牌对写回记录**（单飞）。
    fn refresh_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>>
    {
        Box::pin(async move { refresh::ensure_fresh(store, account_id, true).await })
    }

    /// 有 `POST {broker}/oauth/refresh` 续期接口（会轮换 refresh token）
    fn supports_refresh(&self) -> bool {
        true
    }

    /// 临期判定：`createdAt + expiresIn`（令牌不是 JWT，读不到 `exp`），
    /// 到期前 1 小时算临期；判不出（0）时 false（只在 401 后懒刷新）
    fn credentials_expiring(&self, store: &AccountStore, account_id: &str) -> bool {
        store
            .atomcode_account_record(account_id)
            .and_then(|record| credentials::from_record(Some(&record)).ok())
            .is_some_and(|credentials| credentials.expiring())
    }

    /// 有远程目录（`GET {codingplan}/coding-plan/models-v2`）
    fn supports_model_refresh(&self) -> bool {
        true
    }

    /// 有额度读数（`GET {codingplan}/coding-plan/status-v2` 的滚动窗口）
    fn supports_usage(&self) -> bool {
        true
    }

    /// 查额度 + 逐窗口调用次数
    fn query_usage<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, GatewayError>> + Send + 'a>>
    {
        Box::pin(async move { usage::query(store, account_id).await })
    }

    /// 刷模型目录：用指定账号（空串 = 库里优先级最靠前的可用账号）的令牌。
    ///
    /// 「一个账号都没有」不是失败 —— 那是「用户还没添加账号」的正常状态，返回
    /// `unchanged()`（脚本 / CI 不该看到一个红色失败）。
    fn refresh_models<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
        force: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ModelRefreshOutcome> + Send + 'a>> {
        Box::pin(async move {
            let record = store.atomcode_account_record(account_id);
            let Some(record) = record else {
                if account_id.is_empty() {
                    crate::server::logging::verbose(
                        "[Models]",
                        "AtomCode 模型目录刷新跳过：尚未添加账号",
                    );
                    return ModelRefreshOutcome::unchanged();
                }
                return ModelRefreshOutcome::failed("指定的账号不存在或不可用，请重新选择");
            };
            let credentials = match credentials::from_record(Some(&record)) {
                Ok(credentials) => credentials,
                Err(error) => return ModelRefreshOutcome::failed(error.message),
            };
            models::refresh(&credentials, force).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本家是**无状态**（一次 HTTP 请求 = 一次回答），且四处能力位如实为真
    #[test]
    fn adapter_is_stateless_and_supports_the_four_facts() {
        assert_eq!(ATOMCODE_ADAPTER.kind(), ProviderKind::Atomcode);
        assert!(!ATOMCODE_ADAPTER.is_stateful());
        assert!(ATOMCODE_ADAPTER.supports_refresh());
        assert!(ATOMCODE_ADAPTER.supports_model_refresh());
        assert!(ATOMCODE_ADAPTER.supports_usage());
        assert!(ATOMCODE_ADAPTER.supports_chat());
    }

    /// ⚠️ **轮询型登录**：`supports_web_login` 必须保持 false —— 回调型钩子
    /// （`build_login_url` / `exchange_login_code`）在本家从不成立，宣称 true
    /// 会让通用网页登录分支去等一个永远不来的本机回调（见模块头）。
    #[test]
    fn web_login_hooks_stay_callback_free() {
        assert!(!ATOMCODE_ADAPTER.supports_web_login());
        assert_eq!(ATOMCODE_ADAPTER.build_login_url(), None);
    }
}
