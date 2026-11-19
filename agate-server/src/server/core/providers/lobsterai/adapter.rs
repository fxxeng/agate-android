//! LobsterAI（网易有道「有道龙虾」）适配器：**无状态 OpenAI 兼容转发** +
//! 目录 / 积分 / 每日签到 / 浏览器登录（loopback 回调）。
//!
//! ── 上游就是 OpenAI 兼容协议，所以转发这一层最薄 ──────────────────
//! `POST {api}/api/proxy/v1/chat/completions`、bearer 鉴权、SSE 帧也是标准 chat
//! 形态 —— 只改四处上游强制的 body 改写与五条功能性头（见 [`chat`] 的模块头）。
//! **无状态**（一次 HTTP 请求 = 一次回答），`is_stateful` 保持默认 false。
//!
//! ── 续期要一整组身份载荷件 ────────────────────────────────────
//! `uuid` + 两个 `keyfrom` 只在换码那一刻下发，续期必须原样回传（见
//! [`credentials`] 的模块头）。所以 `supports_refresh` 为真，且 [`ensure_access_token`]
//! 在临期 / 401 后走 [`super::oauth::refresh`]，**只回写** access / refresh / expiresAt
//! （三个载荷件**绝不覆盖**）。并发续期靠 `refresh_flight` 单飞。
//!
//! ── 登录是 loopback 回调那一套 ────────────────────────────────
//! `supports_web_login` 为真：授权页在门户完成、门户 302 回本机
//! `http://127.0.0.1:<网关端口>/auth/callback`，回调侧交 [`super::oauth::parse_callback`]
//! 取 code，再换码落账号（见 `core::login::lobsterai` 与 `api::session`）。
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

use super::{chat, credentials, errors, models, oauth, usage};

/// 一家 LobsterAI 适配器（**单一站点**，没有地区 / 额度池之分）
pub struct LobsterAiAdapter;

/// 唯一实例
pub static LOBSTERAI_ADAPTER: LobsterAiAdapter = LobsterAiAdapter;

impl ProviderAdapter for LobsterAiAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::LobsterAi
    }

    /// 清单来自 `models.rs` 的进程缓存（远程优先，有 19 条静态兜底）
    fn list_models(&self) -> Vec<Value> {
        models::list()
    }

    /// 构造 `POST {api}/api/proxy/v1/chat/completions`（OpenAI 协议，body 只改四处）
    fn build_chat_request(
        &self,
        account: &Value,
        body: &Value,
        client_headers: &HeaderMap,
    ) -> Result<ChatRequestPlan, GatewayError> {
        chat::build_chat_request(account, body, client_headers)
    }

    /// 上游错误分类。判定顺序本身是契约（见 `errors::classify` 的模块头）：
    ///   1. 402 / 正文含余额耗尽关键词 → `QuotaLimited`（换号 + 冷却）；
    ///   2. 40100 / 40101 → `TokenExpired`（只能重新登录）；
    ///   3. 429 → `QuotaLimited`；404 / 5xx → `Fatal`；
    ///   4. 其余 4xx → 共用内容拦截分类。
    fn classify_error(&self, status: u16, error_body: &Value) -> UpstreamErrorClass {
        errors::classify(status, error_body)
    }

    /// **流内错误帧**的分类（`HTTP 200` + `data: {"error":…}`）。
    ///
    /// 本家把「积分耗尽」既写成 HTTP 400 + 正文关键词，也（实测记录里）以
    /// `HTTP 200` 的流内错误帧下发 —— 后者走 `finish_reason` 那条路认不出
    /// （终帧是正常的 `stop`），所以必须有这一支，否则账号用完**不会换号也不会
    /// 冷却**，客户端把一个「余额用完了」当成正常回答（见 `errors` 的模块头）。
    fn stream_error_classifier(&self, message: &str) -> Option<UpstreamErrorClass> {
        errors::classify_stream_error(message)
    }

    /// 声明「有分类能力」—— 预读门控靠它决定要不要缓冲上游的流。
    /// ⚠️ 覆写了分类器就必须覆写本方法，否则分类器永远收不到文案。
    fn has_stream_error_classifier(&self) -> bool {
        true
    }

    /// 取可用令牌。**这里不发网络请求**（本方法会在转发路径上被调）：临期只记一行
    /// 提示，真正的续期由维护链路与 401 后的强制刷新负责（见 `credentials_expiring`
    /// 与 `refresh_access_token`）。
    fn ensure_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>>
    {
        Box::pin(async move {
            let record = store.lobsterai_account_record(account_id);
            if !account_id.is_empty() && record.is_none() {
                return Err(GatewayError::with_status(
                    401,
                    format!("lobsterai 账号 {account_id} 不存在（请重新添加）"),
                ));
            }
            let credentials = credentials::from_record(record.as_ref())?;
            if credentials.expiring() {
                crate::server::logging::verbose(
                    "[LobsterAI]",
                    "账号令牌已过期或临期，将按需续期（续期会轮换 refresh token）",
                );
            }
            Ok(credentials.access_token)
        })
    }

    /// 401 之后的**强制**续期：刷新并**把新令牌写回记录**（单飞）。
    fn refresh_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>>
    {
        Box::pin(async move { super::refresh::ensure_fresh(store, account_id, true).await })
    }

    /// 有 `/api/auth/refresh` 续期接口（载荷件齐全才谈得上）
    fn supports_refresh(&self) -> bool {
        true
    }

    /// 临期判定：`expiresAt`（落盘时算好）或 JWT 的 `exp`，到期前 1 小时算临期
    fn credentials_expiring(&self, store: &AccountStore, account_id: &str) -> bool {
        store
            .lobsterai_account_record(account_id)
            .and_then(|record| credentials::from_record(Some(&record)).ok())
            .is_some_and(|credentials| credentials.expiring())
    }

    /// 有远程目录（`GET /api/models/available`）
    fn supports_model_refresh(&self) -> bool {
        true
    }

    /// 有积分读数（`GET /api/user/profile-summary`）
    fn supports_usage(&self) -> bool {
        true
    }

    /// 查积分余额 + 逐份积分包
    fn query_usage<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, GatewayError>> + Send + 'a>>
    {
        Box::pin(async move { usage::query(store, account_id).await })
    }

    /// 刷模型目录：用指定账号（空串 = 库里第一条可用账号）的令牌。
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
            let record = store.lobsterai_account_record(account_id);
            let Some(record) = record else {
                if account_id.is_empty() {
                    crate::server::logging::verbose(
                        "[Models]",
                        "LobsterAI 模型目录刷新跳过：尚未添加账号",
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

    /// LobsterAI 支持网页登录（授权页 → 本机 loopback 回调 → 换码）
    fn supports_web_login(&self) -> bool {
        true
    }

    /// 授权地址：`{portal}/portal#/login?source=electron&redirect_uri=<本机回调>&state=…`
    ///
    /// 回调地址由本进程的监听端口拼出（bootstrap / JNI 桥写入，见
    /// `oauth::set_loopback_port`）。端口未知时返回 None —— 上层文案会把「未能生成
    /// 授权地址」透给用户，比拼一个必然 404 的地址要好。
    fn build_login_url(&self) -> Option<(String, String)> {
        let return_url = oauth::redirect_uri()?;
        Some(oauth::begin_login(&return_url))
    }

    /// 用回调里的一次性 `code` 换凭证并落账号。
    ///
    /// `state` 在这里再校验一次（深度防御：回调路由已比对过，但真正的凭据是
    /// pending 表里那份 —— 取不到就是「不是本进程发起的那一轮」）。
    ///
    /// `uuid` 与两个 keyfrom 由**本实现生成**（参考实现同款）：`uuid` 是这次登录
    /// 的安装标识、两个 keyfrom 取当前时刻 —— 三者随换码下发、续期时要原样回传，
    /// 所以必须一起落盘（见 `credentials.rs` 的模块头）。
    fn exchange_login_code<'a>(
        &'a self,
        store: &'a AccountStore,
        code: &'a str,
        state: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>>
    {
        Box::pin(async move {
            if !oauth::take_pending(state) {
                return Err(GatewayError::with_status(
                    404,
                    "这次登录已取消或已过期，请重新发起",
                ));
            }
            let client_version = chat::prime_client_version().await;
            let keyfrom = crate::server::logging::now_ms().max(0).to_string();
            let uuid = super::ids::uuid_v4();
            let bundle = oauth::exchange(code, &uuid, &keyfrom, &keyfrom, &client_version).await?;
            let account = store
                .add_lobsterai_account(&bundle, &uuid, &keyfrom, "web")
                .map_err(|error| GatewayError::with_status(error.status_code, error.message))?;
            let account_id = account
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            crate::server::logging::log(
                "[Login]",
                "✅ LobsterAI 网页登录完成，账号已加入列表",
            );
            Ok(account_id)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本家是**无状态**（一次 HTTP 请求 = 一次回答）
    #[test]
    fn adapter_is_stateless_and_supports_the_four_facts() {
        assert_eq!(LOBSTERAI_ADAPTER.kind(), ProviderKind::LobsterAi);
        assert!(!LOBSTERAI_ADAPTER.is_stateful());
        assert!(LOBSTERAI_ADAPTER.supports_refresh());
        assert!(LOBSTERAI_ADAPTER.supports_model_refresh());
        assert!(LOBSTERAI_ADAPTER.supports_usage());
        assert!(LOBSTERAI_ADAPTER.supports_web_login());
    }

    /// 兜底清单对外可见（一个账号都没加时也不该是空目录）
    #[test]
    fn list_models_has_fallback() {
        assert_eq!(LOBSTERAI_ADAPTER.list_models().len(), 19);
    }
}