//! DeepSeek 网页版适配器：**无状态转发 + 自定义 SSE + 每次请求现解 PoW**。
//!
//! ── 本家的特殊性（与 Command Code / ChatGLM 逐条对照）──────────
//! ```text
//!   Command Code：无状态 + 自定义 NDJSON；无 PoW
//!   ChatGLM     ：无状态 + 自定义 SSE（增量与快照混发）；无 PoW
//!   本家        ：无状态 + 自定义 SSE（path-value 结构）+ **每次请求的 PoW**
//! ```
//! PoW 是本家**唯一**需要「发正式请求前先做两件网络/计算事」的地方
//! （建会话 + 领题求解），落点在 `ensure_access_token`（见 `session.rs` 的
//! 模块头：为什么那里是唯一的异步钩子）。
//!
//! ── 响应协议：需要 `UpstreamResponse::DeepSeekWebSse` ────────────
//! 本家的 SSE 不是 OpenAI 形态（path-value patch 结构，见
//! `protocol::deepseek_stream` 的模块头），因此必须与 Anthropic / NDJSON /
//! Gemini / ChatGLM 那四台并列**另开一台状态机**，由编排层按
//! `UpstreamResponse` 分派。
//!
//! 走翻译层的理由与另外四家逐字相同：账号轮换、限额冷却、退避重试、usage
//! 记账与取消处理全部留在编排层，只有字节形态在翻译器里变。若改走
//! `forward_conversation`（会话式转发），那五样都要在本适配器里重写一遍，
//! 而本家并没有多步会话协议（它的「会话」是上游自己的概念，本机是无状态转发）
//! —— 没有理由付那份代价。
//!
//! ── 本家没有的东西（如实声明，别照抄别家）──────────────────────
//!   - **没有续期**：上游没有 refresh 接口（官方文档能力表里 refresh 是
//!     `undeclared`）→ `supports_refresh = false`；
//!   - **没有网页登录窗口**：粘贴式凭证（`supports_web_login` 保持默认 false）；
//!   - **不做签到**：官方文档里 checkin 是 `undeclared`（与 chatglm 那种有
//!     签到体系的家不同）；
//!   - **不做余额查询**：`usage` 是 `undeclared` → `supports_usage = false`；
//!   - **没有远程模型目录**：清单是静态两条（见 `models.rs`）→
//!     `supports_model_refresh = false`；
//!   - **上游不返回 token 计数**：usage 恒 0（如实，不编造）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use axum::http::HeaderMap;
use serde_json::Value;

use crate::server::core::account_store::AccountStore;
use crate::server::core::providers::adapter::{
    ChatRequestPlan, ModelRefreshOutcome, ProviderAdapter, UpstreamErrorClass, UpstreamResponse,
};
use crate::server::core::providers::ProviderKind;
use crate::server::errors::GatewayError;

use super::{credentials, endpoints, models, plan, session};

/// DeepSeek 网页版适配器（无状态单例）
pub struct DeepSeekWebAdapter;

/// 静态单例（`adapter_for(ProviderKind::DeepSeekWeb)` 返回这一个）
pub static DEEPSEEK_WEB_ADAPTER: DeepSeekWebAdapter = DeepSeekWebAdapter;

impl ProviderAdapter for DeepSeekWebAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::DeepSeekWeb
    }

    /// 静态清单两条（见 `models.rs`：本家没有远程目录接口）
    fn list_models(&self) -> Vec<Value> {
        models::list()
    }

    /// 构造 `POST /api/v0/chat/completion`（请求体是本家自有信封；
    /// 响应是 path-value SSE，见模块头）。
    ///
    /// ── 同步函数怎么做异步准备（见 `session.rs`）────────────────
    /// 建会话与解 PoW 都是异步的，已在 `ensure_access_token` 里做完并放进
    /// 状态表；这里**取走**（一次性）并拼进 URL / 头 / 体。取不到就明确报错
    /// （绝不发一条没有题解的请求 —— 上游必然拒绝，而且报错看起来会像
    /// 「用户 token 有问题」，掩盖真正的「准备步骤没跑」）。
    fn build_chat_request(
        &self,
        account: &Value,
        body: &Value,
        _client_headers: &HeaderMap,
    ) -> Result<ChatRequestPlan, GatewayError> {
        let credentials = credentials::from_record(Some(account))?;
        // 取走准备结果（建会话 + 题解）；取不到说明准备钩子没跑或结果已过期
        let prepared = session::take(&credentials.user_token).ok_or_else(|| {
            GatewayError::with_status(
                503,
                "DeepSeek 网页版本次请求尚未准备就绪（建会话 / 解 PoW 未完成），请重试",
            )
        })?;

        // 模型名由编排层按映射改写后塞进 body.model（见 `upstream::payload`）
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(endpoints::DEFAULT_MODEL)
            .to_string();

        // 拍平多轮（本家一次只收一条 prompt，见 `plan.rs`）
        let prompt = plan::build_prompt(body);
        if prompt.trim().is_empty() {
            return Err(GatewayError::with_status(
                400,
                "DeepSeek 网页版请求里没有可发送的内容（messages / prompt 都是空的）",
            ));
        }

        let mut headers = endpoints::base_headers();
        headers.extend(endpoints::auth_headers(&credentials.user_token));
        headers.extend(endpoints::sse_headers());
        headers.push(("x-ds-pow-response".to_string(), prepared.pow_header));

        let request_body = serde_json::json!({
            "chat_session_id": prepared.session_id,
            // 每次都是一轮独立对话（本机是无状态转发，上下文由拍平的 prompt 承载）
            "parent_message_id": Value::Null,
            "prompt": prompt,
            "model_type": super::chat::model_type_of(&model),
            "thinking_enabled": plan::thinking_enabled(body),
            "search_enabled": plan::search_enabled(body),
            "preempt": false,
            "ref_file_ids": Vec::<String>::new(),
            "stream": true,
        });

        Ok(ChatRequestPlan {
            url: endpoints::url(endpoints::COMPLETION_PATH),
            headers,
            body: request_body,
            response: UpstreamResponse::DeepSeekWebSse,
        })
    }

    /// 上游错误分类。
    ///
    /// 判据（形态来自二进制与公开逆向）：
    ///   - 401 / 403 → `TokenExpired`（登录态失效；编排层原地试一次后换账号）；
    ///   - 429 → `QuotaLimited`（上游限流）；
    ///   - 400 / 422 → 内容拦截判定优先，否则 `Fatal(400)`；
    ///   - 其余 → `Fatal(502)`。
    fn classify_error(&self, status: u16, error_body: &Value) -> UpstreamErrorClass {
        let raw = error_body
            .get("message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or("上游错误");
        let message = format!("上游返回 {status}: {raw}");
        let code = error_body.get("code").and_then(Value::as_i64);
        match status {
            400 | 422 => {
                crate::server::core::providers::content_block::classify_or_fatal(
                    400, error_body, message, code,
                )
            }
            401 | 403 => UpstreamErrorClass::TokenExpired { message },
            429 => UpstreamErrorClass::QuotaLimited {
                reset_at: None,
                message,
                upstream_code: code,
                status: 429,
            },
            _ => UpstreamErrorClass::Fatal {
                status: 502,
                message,
                upstream_code: code,
            },
        }
    }

    /// 取可用凭证，并在此之前完成本家必需的**两步异步准备**
    /// （建会话 + 领题求解，见 `session.rs` 与模块头）。
    ///
    /// 这里是**真失败**（不是 best-effort）：两样东西缺一就发不出请求。
    fn ensure_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let record = store.deepseek_web_account_record(account_id);
            let credentials = credentials::from_record(record.as_ref())?;
            // 建会话 + 解 PoW（失败即本次请求失败，见 `session::prepare`）
            session::prepare(&credentials.user_token).await?;
            Ok(credentials.user_token)
        })
    }

    /// 没有续期手段（上游无 refresh 接口）——维护任务不问本家
    fn supports_refresh(&self) -> bool {
        false
    }

    /// 没有远程目录（静态两条清单，见 `models.rs`）——不参与目录刷新调度。
    ///
    /// 保持 false 而不是「实现一个永远失败或永远返回静态值的 refresh」：
    /// 后者会让日志里出现一条误导性的「刷新」记录。
    fn supports_model_refresh(&self) -> bool {
        false
    }

    /// 刷新目录：本家**没有可刷的东西**（静态两条清单，见 `models.rs`）。
    ///
    /// 如实回 `unchanged()`（「这次没有新东西」）而不是 `failed(...)` ——
    /// 后者会在界面上报一条红色失败，而这个调用本就不该被发起
    /// （`supports_model_refresh()` 已是 false，刷新循环不会问本家；
    /// 手动「获取模型」若被点到，这里给出的是「无事发生」而不是「出错了」）。
    /// 同款先例见 `zcode/adapter.rs` 与 `commandcode/adapter.rs` 的对应分支。
    fn refresh_models<'a>(
        &'a self,
        _store: &'a AccountStore,
        account_id: &'a str,
        _force: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ModelRefreshOutcome> + Send + 'a>> {
        Box::pin(async move {
            crate::server::logging::verbose(
                "[Models]",
                &format!(
                    "DeepSeek 网页版模型目录刷新跳过：本家清单是静态两条（账号 {account_id}）"
                ),
            );
            ModelRefreshOutcome::unchanged()
        })
    }
}