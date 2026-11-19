//! Tabbit 适配器：把「cookie 鉴权 + v3 房间协议」接进 provider 注册表。
//!
//! ── 一份实现、两个实例 ──────────────────────────────────────
//! 两站（`tabbit` / `tabbit-intl`）的协议完全相同，差异全在 [`Realm`] 里
//! （基址 / 签名兜底 key / 时区）。因此这里是一个 `TabbitAdapter { realm }`
//! 加两个静态实例 —— 与 Qoder / 千问办公「一份实现、按站点参数化」同款。
//!
//! ── 为什么 `is_stateful() == true` ──────────────────────────
//! 上游一次对话是「建房间 → 订阅事件流 → 投递消息 → 读流」四步，塞不进
//! 「一次 HTTP 请求 = 一次对话」的单请求契约（见 `mod.rs` 模块头）。因此
//! `build_chat_request` 只做**防御性报错**，真活在 `chat::run_chat` 里。
//!
//! ── 能力位逐条（都是「没有就如实说没有」）────────────────────
//! ```text
//!   chat                 ✓   四步时序
//!   refresh              ✗   上游没有 refresh 端点（JWT 7 天，过期重贴 Cookie）
//!   model_refresh        ✓   GET /proxy/v1/model_config/models
//!   usage                ✓   百分比制额度（见 usage.rs）
//!   checkin              ✓   daily_sign_in（见 checkin.rs）
//!   web_login            ✗   没有可复刻的授权回调；走粘贴 Cookie
//!   default_model        ✗   上游只认 display_name，不认「给我默认」
//!   anonymous session    ✗   必须有账号
//! ```
//!
//! ── 错误分类（四档语义见 `adapter` 模块头）──────────────────
//!   - 401 / 403 → `TokenExpired`：登录态失效。本家**没有**续期手段，编排层
//!     原地重试一次后换账号 —— 这是这套分类下唯一能让「过期 Cookie」被封禁
//!     而不是被反复重试的处置；
//!   - 429 / 492 → `QuotaLimited`：限流 / 权益不足（492 是「需要 Pro 或今日
//!     额度已用完」），冷却换号；
//!   - 409 → `QuotaLimited` + 30 秒冷却：房间被占用，**不罚账号额度**但也不该
//!     立刻再试同一个账号；
//!   - 其余 → `Fatal`：原样透传。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::sync::Arc;

use axum::http::HeaderMap;
use serde_json::Value;

use crate::server::core::account_store::AccountStore;
use crate::server::core::proxies::ResolvedProxy;
use crate::server::core::providers::adapter::{
    ChatRequestPlan, ModelRefreshOutcome, ProviderAdapter, UpstreamErrorClass,
};
use crate::server::core::providers::{kind_id, ProviderKind};
use crate::server::core::upstream::usage::RequestTelemetry;
use crate::server::core::upstream::ForwardOutcome;
use crate::server::errors::GatewayError;

use super::credentials::Credentials;
use super::{chat, http, models, usage, Realm};

/// 一家 Tabbit 站点实例
pub struct TabbitAdapter {
    realm: Realm,
}

impl TabbitAdapter {
    pub const fn new(realm: Realm) -> Self {
        Self { realm }
    }
}

/// 国内站实例（`adapter_for(ProviderKind::Tabbit)`）
pub static TABBIT_ADAPTER: TabbitAdapter = TabbitAdapter::new(Realm::Cn);
/// 国际站实例（`adapter_for(ProviderKind::TabbitIntl)`）
pub static TABBIT_INTL_ADAPTER: TabbitAdapter = TabbitAdapter::new(Realm::Intl);

impl ProviderAdapter for TabbitAdapter {
    fn kind(&self) -> ProviderKind {
        self.realm.kind()
    }

    /// 上游是四步会话协议（固有性质）—— 编排层据此分流到 `forward_conversation`
    fn is_stateful(&self) -> bool {
        true
    }

    /// 模型清单：远程目录优先，没拉到给一条兜底（`models.rs`）
    fn list_models(&self) -> Vec<Value> {
        models::list(self.realm)
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

    /// 上游错误分类（四档语义见模块头）
    fn classify_error(&self, status: u16, error_body: &Value) -> UpstreamErrorClass {
        let message = {
            let text = http::error_text(error_body);
            if text.is_empty() {
                match status {
                    401 | 403 => "Tabbit 登录态已失效，请重新导出 Cookie".to_string(),
                    409 => "Tabbit 会话正在处理中，请稍后重试".to_string(),
                    429 => "Tabbit 请求过于频繁，请稍后重试".to_string(),
                    492 => {
                        "Tabbit 当前账号没有该模型的权限（需要 Pro 或今日额度已用完）".to_string()
                    }
                    other => format!("Tabbit 上游返回 HTTP {other}"),
                }
            } else {
                text
            }
        };
        match status {
            401 | 403 => UpstreamErrorClass::TokenExpired { message },
            429 | 492 => UpstreamErrorClass::QuotaLimited {
                reset_at: None,
                message,
                upstream_code: Some(i64::from(status)),
                status,
            },
            // 房间占用：短冷却并换账号（不是永久失败，也不罚账号额度）
            409 => UpstreamErrorClass::QuotaLimited {
                reset_at: Some(crate::server::logging::now_ms() + 30_000),
                message,
                upstream_code: Some(409),
                status: 409,
            },
            other => UpstreamErrorClass::Fatal {
                status: other,
                message,
                upstream_code: None,
            },
        }
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
            chat::run_chat(
                store,
                self.realm,
                account_id,
                body,
                proxy,
                stream,
                telemetry,
            )
            .await
        })
    }

    /// 取可用凭证（本家没有续期，这一步只是「记录里有没有令牌」的检查）
    fn ensure_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let record = store.tabbit_account_record(self.realm, account_id).ok_or_else(|| {
                GatewayError::with_status(
                    404,
                    format!("还没有 {} 账号", self.realm.label()),
                )
            })?;
            let credentials = Credentials::from_record(Some(&record))?;
            Ok(credentials.token)
        })
    }

    /// **没有续期手段**：上游没有 refresh 端点（JWT 7 天，过期只能重新导出 Cookie）。
    /// 维护任务不问本家 —— 否则会每天对着一个不存在的接口重试。
    fn supports_refresh(&self) -> bool {
        false
    }

    /// 有远程模型目录（`/proxy/v1/model_config/models`）
    fn supports_model_refresh(&self) -> bool {
        true
    }

    /// 刷新目录（`force` 透传：`false` 走 TTL，`true` 真打上游）
    fn refresh_models<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
        force: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ModelRefreshOutcome> + Send + 'a>>
    {
        Box::pin(async move { models::refresh(store, self.realm, account_id, force).await })
    }

    /// 有额度概念（百分比制，见 `usage.rs`）
    fn supports_usage(&self) -> bool {
        true
    }

    /// 查额度（归一化形状见 trait 文档；本家 `unit` 是 `%`）
    fn query_usage<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, GatewayError>> + Send + 'a>>
    {
        Box::pin(async move { usage::query(store, self.realm, account_id).await })
    }

    /// 没有可复刻的网页授权回调：添加账号走粘贴 Cookie（见 `credentials.rs`）
    fn supports_web_login(&self) -> bool {
        false
    }

    /// 上游只认 display_name，没有「给我默认模型」的请求约定 —— 不声明默认模型
    fn supports_default_model(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 两个实例的 kind 必须与站点一一对应（接反了会把请求发到另一个站）
    #[test]
    fn each_static_instance_has_its_own_kind() {
        assert_eq!(TABBIT_ADAPTER.kind(), ProviderKind::Tabbit);
        assert_eq!(TABBIT_INTL_ADAPTER.kind(), ProviderKind::TabbitIntl);
        assert_ne!(TABBIT_ADAPTER.kind(), TABBIT_INTL_ADAPTER.kind());
    }

    /// 401 / 403 → TokenExpired（换账号），429 / 492 → 冷却，409 → 30 秒冷却
    #[test]
    fn error_classes_keep_the_rotation_semantics() {
        assert!(matches!(
            TABBIT_ADAPTER.classify_error(401, &Value::Null),
            UpstreamErrorClass::TokenExpired { .. }
        ));
        assert!(matches!(
            TABBIT_ADAPTER.classify_error(403, &Value::Null),
            UpstreamErrorClass::TokenExpired { .. }
        ));
        for status in [429u16, 492] {
            match TABBIT_ADAPTER.classify_error(status, &Value::Null) {
                UpstreamErrorClass::QuotaLimited { reset_at, .. } => {
                    assert!(reset_at.is_none(), "限流不带上游给的恢复时刻");
                }
                _ => panic!("{status} 应判成 QuotaLimited"),
            }
        }
        match TABBIT_ADAPTER.classify_error(409, &Value::Null) {
            UpstreamErrorClass::QuotaLimited { reset_at, .. } => {
                assert!(reset_at.is_some(), "409 需要短冷却，否则会立刻撞同一个房间");
            }
            _ => panic!("409 应判成带冷却的 QuotaLimited"),
        }
        assert!(matches!(
            TABBIT_ADAPTER.classify_error(500, &Value::Null),
            UpstreamErrorClass::Fatal { .. }
        ));
    }

    /// 上游原文优先（用户看到的应该是上游说的话）
    #[test]
    fn upstream_message_wins() {
        match TABBIT_ADAPTER.classify_error(492, &json!({ "message": "需要 Pro 订阅" })) {
            UpstreamErrorClass::QuotaLimited { message, .. } => {
                assert_eq!(message, "需要 Pro 订阅");
            }
            _ => panic!("492 应判成 QuotaLimited"),
        }
    }

    /// 本家是会话式转发；单请求路径必须明确拒绝（而不是发一半）
    #[test]
    fn single_request_path_is_refused() {
        assert!(TABBIT_ADAPTER.is_stateful());
        // 用 `let else` 而不是 `expect_err`：`ChatRequestPlan` 没有 `Debug`
        // （它是转发层的中间形态，只在本家内部流转），不为一条断言去给共享
        // 类型加派生 —— 那会让整个 plan 结构被拖进调试输出。
        let plan = TABBIT_ADAPTER.build_chat_request(&Value::Null, &Value::Null, &HeaderMap::new());
        let Err(error) = plan else {
            panic!("单请求路径必须拒绝（本家只走 forward_conversation）");
        };
        assert_eq!(error.status_code, 503);
    }

    /// 能力位：有目录 / 有额度 / 无续期 / 无网页登录 / 无默认模型
    #[test]
    fn capability_flags_are_honest() {
        assert!(TABBIT_ADAPTER.supports_model_refresh());
        assert!(TABBIT_ADAPTER.supports_usage());
        assert!(!TABBIT_ADAPTER.supports_refresh());
        assert!(!TABBIT_ADAPTER.supports_web_login());
        assert!(!TABBIT_ADAPTER.supports_default_model());
    }

    /// 两站的模型清单兜底 id 不同（混用会让一张卡片显示另一张的模型）
    #[test]
    fn model_lists_are_per_site() {
        let cn = TABBIT_ADAPTER.list_models();
        let intl = TABBIT_INTL_ADAPTER.list_models();
        assert_ne!(cn[0]["id"], intl[0]["id"]);
    }
}
