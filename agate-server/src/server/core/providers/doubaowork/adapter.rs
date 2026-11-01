//! 豆包工作适配器：**无状态转发** + 目录 / 额度 / 签到，**没有续期**。
//!
//! ── 为什么是「无状态」而不是有状态会话 ──────────────────────────
//! 上游是「一次 HTTP 请求 = 一次答复」（`POST /chat/completion` 发一次、
//! 一条 SSE 回来），本家**没有** CatPaw 那种 round → turn → 工具循环的多步
//! 会话协议。因此 `is_stateful` 保持默认 `false`，转发链只需
//! [`ProviderAdapter::build_chat_request`] 一条路 —— 账号轮换、限额冷却、
//! 退避重试、usage 记账、取消处理全部由编排层承担（见 `adapter.rs` 的论证）。
//!
//! ⚠️ **但响应协议不是标准 chat SSE**（见 [`super::chat`] 的模块头）：
//! 本适配器目前返回 `ChatRequestPlan::chat(...)`，那条路会把上游的自有事件流
//! **原样透传**给客户端 —— 客户端解析不了。需要接线方补
//! `UpstreamResponse::DoubaoworkSse` 变体 + `upstream::translate` 里的翻译器
//! （`super::chat::parse_event` 已经把解析做完了，翻译器主要做搬运与收尾）。
//! 这一点在 [`ProviderAdapter::build_chat_request`] 的文档里**再写一遍**——
//! 「适配器返回的是默认路径」这件事必须让接线方一眼看到。
//!
//! ── 五处能力位：逐条如实声明（别照抄别家）──────────────────────
//! ```text
//!   supports_chat            true   主推理入口 /chat/completion（SSE）
//!   supports_usage           true   subscription/overview + entry/config
//!   supports_model_refresh   true   /alice/slot/action_bar_v3/brief_list
//!   supports_refresh         false  **无续期接口**（Cookie 失效只能重新登录）
//!   supports_web_login       false  登录是「内嵌窗口取 Cookie」，不是回调换码
//! ```
//! `supports_web_login` 为什么是 false（**这一条最容易写错**）：trait 的
//! `build_login_url` / `exchange_login_code` 那一对钩子是**回调型**的
//! （拉起授权页 → 302 回本机 → 网关拿一次性 code 换凭证）。本家的登录是
//! 桌面端那套：`/api/session/login/start` 起一个内嵌 Webview 打开
//! `www.doubao.com`，壳侧从窗口里**直接读 Cookie** 再落账号
//! （beta `doubaowork_login.rs` 的 `run_cookie_login`）—— 没有 code、
//! 没有回调、也就没有 `exchange_login_code` 可写。把它标成 true 会让
//! `LoginService::start_web_login` 去等一个永远不来的回调
//! （症状：用户登录成功、界面一直挂着等跳转，且没有任何报错）。
//! 本家的发起入口与 AtomCode / ZCode / OfficeAce 同一形态：由
//! `api::session::login_start` 在通用网页登录分支**之前**分流。
//!
//! ── 工具调用：如实声明**不支持原生 function calling** ─────────────
//! `stream_refusal_finishes` / `stream_error_classifier` 都保持默认：
//! 本家的「工具调用」是插件自己的**文本桥接**（插件文档原话
//! 「Tool calls use a text bridge, not native function calling」），
//! 与原生 `finish_reason="tool_calls"` 不是一回事。硬把它映射成原生工具调用
//! 会让客户端收到一条它无法执行的指令。因此本家不做工具调用适配 ——
//! 模型的提问与答复按文本走（与 `/v1/models` 里 `supportsToolCall: false` 一致，
//! 见 [`super::models`]）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic，不持锁跨 await。

use axum::http::HeaderMap;
use serde_json::Value;

use crate::server::core::account_store::AccountStore;
use crate::server::core::providers::adapter::{
    ChatRequestPlan, ModelRefreshOutcome, ProviderAdapter, ReasoningPatch, UpstreamErrorClass,
};
use crate::server::core::providers::{content_block, ProviderKind};
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::{chat, credentials, models, plan, quota, rewards};

/// 一家豆包工作适配器（**单一站点**，没有地区之分）
pub struct DoubaoWorkAdapter;

/// 唯一实例（`adapter_for(ProviderKind::Doubaowork)` 返回这一个）
pub static DOUBAOWORK_ADAPTER: DoubaoWorkAdapter = DoubaoWorkAdapter;

impl ProviderAdapter for DoubaoWorkAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Doubaowork
    }

    /// 清单来自 [`models::list`]（远程目录 → 落盘缓存 → 内置兜底，**恒非空**）
    fn list_models(&self) -> Vec<Value> {
        models::list()
    }

    /// 构造 `POST /chat/completion`（自有消息块信封 + 风控 query）。
    ///
    /// ⚠️ **返回的是默认的 `ChatRequestPlan::chat(...)`** —— 本家的响应协议是
    /// 自有 SSE，不是标准 chat SSE。接线方要补
    /// `UpstreamResponse::DoubaoworkSse` 变体与 `upstream::translate` 里的
    /// 翻译器（解析已在 [`super::chat::parse_event`] 完成），并把本方法改成
    /// 返回那个变体的构造器。**在补充之前这条链路是不完整的**（上游帧会被
    /// 原样透传给客户端，客户端解析不了）—— 这一点如实写在这里，不假装可用。
    ///
    /// `account` 是会话形态；本家的凭证是一整套 Cookie + 一组指纹
    /// （见 [`super::chat::credentials_from_session`] 的说明）。
    fn build_chat_request(
        &self,
        account: &Value,
        body: &Value,
        client_headers: &HeaderMap,
    ) -> Result<ChatRequestPlan, GatewayError> {
        chat::build_chat_request(account, body, client_headers)
    }

    /// 上游错误分类（**判定顺序是契约**）：
    ///   1. `401` / `403` → [`UpstreamErrorClass::TokenExpired`]
    ///      （编排层会尝试刷新一次；本家 `supports_refresh = false`，
    ///      刷新必然拿回同一串 Cookie ⇒ 最终换账号，文案是「请重新登录」）；
    ///   2. `400` / `422` → 共用内容拦截判定（命中审核文案时**不罚账号**，
    ///      走「换中性提示词同账号重试一次」）；
    ///   3. `402` → `QuotaLimited`（**下游码按 429**：本家的 402 是
    ///      「额度 / 会员不足」，换账号比直接报错更有用）；
    ///   4. `429` → `QuotaLimited(429)`（本家会回 `710022004 rate limited`；
    ///      业务码随 `code` / `error_code` 一起进 `upstream_code`）；
    ///   5. 其余 → `Fatal`（`5xx` 与未列出的一切按原状态码透出，
    ///      不「顺手」改成 502 —— 那是别家对自有网关的处置，本家是官方站点）。
    fn classify_error(&self, status: u16, error_body: &Value) -> UpstreamErrorClass {
        let raw = error_body
            .get("message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or("上游错误");
        let message = format!("上游返回 {status}: {raw}");
        let code = error_body
            .get("code")
            .and_then(Value::as_i64)
            .or_else(|| error_body.get("error_code").and_then(Value::as_i64));
        match status {
            401 | 403 => UpstreamErrorClass::TokenExpired {
                message: format!("{message}（登录态失效：请在账号页重新登录）"),
            },
            400 | 422 => content_block::classify_or_fatal(400, error_body, message, code),
            402 => UpstreamErrorClass::QuotaLimited {
                reset_at: None,
                message: format!(
                    "{message}（额度 / 会员不足：本网关按限流处理，请换账号或升级会员）"
                ),
                upstream_code: code,
                status: 429,
            },
            429 => UpstreamErrorClass::QuotaLimited {
                reset_at: None,
                message,
                upstream_code: code,
                status: 429,
            },
            // 本家是官方站点，不是自有网关：5xx 原样透出（别家改成 502 的处置
            // 在这里没有依据）
            other => UpstreamErrorClass::Fatal {
                status: other,
                message,
                upstream_code: code,
            },
        }
    }

    /// 思考等级翻译：把映射上的档位写进**顶层整数通道**
    /// （[`super::plan::BODY_THINK_LEVEL_KEY`]），由 [`super::plan::build`]
    /// 在构造信封时同时落到 `option.need_deep_think`（整数）与
    /// `ext.use_deep_think`（字符串）两处。
    ///
    /// ── 为什么不直接写 `option`（这里踩过一次，别改回去）────────────
    /// `ReasoningPatch::Set` 的契约是「写**一个**顶层 `field`」。本家档位真正
    /// 落在 `option`（整数）与 `ext`（字符串）**两处**，而上游要求两处口径一致
    /// （见 [`super::plan`] 的模块头）。若在这里直接覆盖 `option`：
    ///   - `ext.use_deep_think` 会留在「客户端没指定」时那个推断值上 ——
    ///     两处不一致；
    ///   - 更糟的是 `option` 是一个**对象**，直接覆盖会把
    ///     `answer_with_suggest` / `sse_recv_event_options` 这些**必需子键**
    ///     一起抹掉（那会让上游不发思维链，甚至整轮失败）。
    /// 写「顶层整数通道」则没有这两个问题：一个值、两处形态、由构造期统一落。
    /// 该通道是网关内部的，最终信封里没有它（见 `plan::build`）。
    ///
    /// ── 客户端已指定时不覆盖 ────────────────────────────────────
    /// 判据用**全项目共用**的取值链 `model_rules::read_client_level`
    /// （它认 `reasoning_effort` / `reasoningEffort` / `effort` / `reasoning` /
    /// `thinking` 五个键）——**不要**另抄一份键名清单，也不要用本家自己的
    /// `think_level_from_body`（那个会**丢掉**「客户端给了一个本家认不出的值」
    /// 这种情况，于是绑定会去覆盖一个其实已经指定过的请求）。
    fn reasoning_patch(&self, level: &str, _model: &str, body: &Value) -> ReasoningPatch {
        if crate::server::core::model_rules::read_client_level(body).is_some() {
            return ReasoningPatch::Skip {
                reason: "客户端请求体里已指定思考档位",
            };
        }
        // 「关闭思考」两档由编排层在问适配器之前就拦掉了（见 `payload::apply_reasoning`），
        // 但本方法也可能被别处直接调用（单测 / 将来的新调用点），因此这里**再判一次**：
        // 本项目没有安全的「关闭思考」表达，`Skip` 就是正确答案。
        if crate::server::core::model_rules::reasoning_is_off(level) {
            return ReasoningPatch::Skip {
                reason: "该等级语义是关闭思考（本家不注入任何「关」的字段）",
            };
        }
        let mapped = match plan::think_level_of_level(level) {
            Some(mapped) => mapped,
            None => {
                return ReasoningPatch::Skip {
                    reason: "该等级不在豆包工作接受的档位内（本家只有 minimal/low → 关、\
                             medium/high → 思考、xhigh/max → 专家这三档）",
                }
            }
        };
        ReasoningPatch::Set {
            field: plan::BODY_THINK_LEVEL_KEY,
            // `Value::from(i64)` 而不是字符串：通道的读侧按整数优先解析
            // （`plan::resolve_think_level`），两处形态一致
            value: Value::from(mapped),
        }
    }

    /// 从发送体里读随行的思考档位（请求日志「上游等级」列的采集口）。
    ///
    /// 绑定注入时 `reasoning_patch` 返回的是 `Value::from(i64)`，而
    /// `payload::apply_reasoning` 只把**字符串形态**的值当成采集结果 ——
    /// 因此那里会给 `None` 并回落到这里（见它文档的最后一段）。本条读的是
    /// 构造期真正落进信封的 `option.need_deep_think`：**与字节同源**。
    fn outbound_reasoning(&self, body: &Value) -> Option<String> {
        plan::outbound_reasoning_of(body)
    }

    /// 取可用凭证。**这里不发网络请求**（本方法在转发路径上被调）。
    ///
    /// 本家没有续期手段：这里只做「记录里有没有可用 Cookie」的检查，
    /// 返回拼好的 **Cookie 头字符串**（与 KukuAI 同款 —— 那个字符串只是
    /// 「本次承载用的凭据标识」，真正的 Cookie 由 `build_chat_request` 从
    /// 记录里重新取整份）。上游拒了就是 401，编排层把它归成 `TokenExpired`
    /// （先原地试一次，再按队列换账号）。
    fn ensure_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>>
    {
        Box::pin(async move {
            let record = store.doubaowork_account_record(account_id);
            if !account_id.is_empty() && record.is_none() {
                return Err(GatewayError::with_status(
                    401,
                    format!("豆包工作账号 {account_id} 不存在（请重新添加）"),
                ));
            }
            let credentials = credentials::from_record(record.as_ref())?;
            if !credentials.has_session() {
                return Err(GatewayError::with_status(
                    401,
                    "豆包工作账号缺少登录态 Cookie（请重新登录或粘贴完整 Cookie）",
                ));
            }
            Ok(credentials.cookie_header())
        })
    }

    /// **没有续期手段**（Cookie 不续期、上游无 refresh 接口）——
    /// 维护任务不问本家，也就不会每次白跑一趟并往日志里灌失败记录。
    fn supports_refresh(&self) -> bool {
        false
    }

    /// `credentials_expiring` 保持默认 `false`（判不出来就返回 false）：
    /// 本家 Cookie 的过期时刻**读不到**（粘贴形态没有 `expires`，
    /// 导出形态的 `expirationDate` 只对 `sessionid_ss` 一族有效，且
    /// 上游并不按它判登录）。声称「临期」会让维护任务每轮都去打一次上游 ——
    /// 而本家**没有刷新接口**，那次请求必然什么都不改。
    /// 续期只能等 401 时那条懒链路（本家它会落到「请重新登录」）。
    ///
    /// （因此本方法刻意**不覆写**：默认 false 就是本家的正确答案。）

    /// 有远程目录（`/alice/slot/action_bar_v3/brief_list`，见 [`super::models`]）
    fn supports_model_refresh(&self) -> bool {
        true
    }

    /// 有额度读数（`subscription/overview` + `entry/config`）
    fn supports_usage(&self) -> bool {
        true
    }

    /// 查额度（订阅窗口 + 奖励额度，归一成本仓的统一形状）
    fn query_usage<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, GatewayError>> + Send + 'a>>
    {
        Box::pin(async move { quota::query(store, account_id).await })
    }

    /// 刷模型目录：用指定账号（空串 = 库里优先级最靠前的可用账号）的 Cookie。
    ///
    /// 「一个账号都没有」不是失败 —— 那是「用户还没添加账号」的正常状态，
    /// 返回 `unchanged()`（脚本 / CI 不该看到一个红色失败）。
    fn refresh_models<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
        force: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ModelRefreshOutcome> + Send + 'a>> {
        Box::pin(async move {
            let record = store.doubaowork_account_record(account_id);
            let Some(record) = record else {
                if account_id.is_empty() {
                    logging::verbose(
                        "[Models]",
                        "豆包工作模型目录刷新跳过：尚未添加账号",
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

    /// 目录接口是**账号级**的（不同账号可见的模式可能不同）——
    /// 「获取模型」弹窗每行的「模型来源」下拉因此要显示（保持默认 true）。
    fn refresh_uses_account(&self) -> bool {
        true
    }
}

/// 签到入口（供 `billing::checkin` 分派调用）。
///
/// ── 为什么暴露成一个**独立函数**而不是 trait 方法 ──────────────
/// 签到不属于 [`ProviderAdapter`] 的契约（见 trait 的文档：签到是
/// `billing::checkin` 按 provider 分派的一条链路，各家的实现散在自家目录里）。
/// 本家与 AtomCode 同一形态：模块级函数 + 在 `billing/checkin.rs` 的
/// `match provider` 里加一条 arm。
///
/// ⚠️ **需要接线方补**：`core/auto_checkin.rs` 的 `CHECKIN_PROVIDERS`
/// （当前 16 家，`PROVIDERS_VERSION = 5`）与 `billing/checkin.rs` 的分派表
/// 都要加上 `"doubaowork"` —— 否则本家的签到永远不会被调度
/// （症状：账号页有「签到」按钮但签到中心里没有这一家）。
pub async fn claim_for_account(
    store: &AccountStore,
    account_id: &str,
) -> Result<Value, GatewayError> {
    rewards::claim_for_account(store, account_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 五处能力位：四条 true + **续期与网页登录 false**（见模块头）
    #[test]
    fn capability_flags_are_honest() {
        assert_eq!(DOUBAOWORK_ADAPTER.kind(), ProviderKind::Doubaowork);
        assert!(DOUBAOWORK_ADAPTER.supports_chat());
        assert!(DOUBAOWORK_ADAPTER.supports_usage());
        assert!(DOUBAOWORK_ADAPTER.supports_model_refresh());
        assert!(DOUBAOWORK_ADAPTER.refresh_uses_account());
        // ⚠️ 本家**没有**续期接口（Cookie 失效只能重新登录）
        assert!(!DOUBAOWORK_ADAPTER.supports_refresh());
        // ⚠️ 本家登录是「内嵌窗口取 Cookie」，不是回调换码 —— 标成 true
        // 会让通用网页登录分支去等一个永远不来的本机回调
        assert!(!DOUBAOWORK_ADAPTER.supports_web_login());
        assert_eq!(DOUBAOWORK_ADAPTER.build_login_url(), None);
        // 无状态（一次 HTTP 请求 = 一次答复）
        assert!(!DOUBAOWORK_ADAPTER.is_stateful());
    }

    /// 工具调用保持默认（本家是文本桥接，不做原生 function calling 适配）
    #[test]
    fn tool_call_hooks_stay_default() {
        assert!(DOUBAOWORK_ADAPTER.stream_refusal_finishes().is_empty());
        assert!(!DOUBAOWORK_ADAPTER.has_stream_error_classifier());
        // `UpstreamErrorClass` 没有 `PartialEq`（它带 String 字段，比身份更
        // 合适的是判「是不是 None」）——因此用 `is_none()` 而不是 `assert_eq!`
        assert!(DOUBAOWORK_ADAPTER
            .stream_error_classifier("anything")
            .is_none());
    }

    /// 默认路径的几条（本家不覆写就该是默认值）
    #[test]
    fn defaulted_hooks_match_this_family() {
        // 不跟随系统代理（本家上游在国内可达）
        assert!(!DOUBAOWORK_ADAPTER.system_proxy_when_unset());
        // 没有环境变量凭证旁路
        assert!(!DOUBAOWORK_ADAPTER.allows_anonymous_default_session());
        assert!(!DOUBAOWORK_ADAPTER.env_credentials_present());
        // 没有「默认模型」概念注入（本家目录自己带默认模式）
        assert!(!DOUBAOWORK_ADAPTER.supports_default_model());
        // 上游原样回显模型名（不需要改写 SSE 帧里的 model）
        assert!(!DOUBAOWORK_ADAPTER.sse_model_rewrite());
        // 不做纯换行分片的丢弃（那是 WorkBuddy / AutoClaw 的保活节拍）
        assert!(!DOUBAOWORK_ADAPTER.sse_strip_newline_chunks());
    }

    /// 错误分类：401/403 → TokenExpired；402 → QuotaLimited(429)；
    /// 400 命中审核文案 → ContentBlocked；其余原样
    #[test]
    fn classify_error_mapping() {
        let expired = DOUBAOWORK_ADAPTER.classify_error(401, &json!({"message": "not logged in"}));
        assert!(matches!(expired, UpstreamErrorClass::TokenExpired { .. }));
        let quota = DOUBAOWORK_ADAPTER.classify_error(402, &json!({"message": "insufficient"}));
        // ⚠️ 402 是「额度 / 会员不足」，下游按 429 处理（换账号比直接报错有用）
        assert!(
            matches!(&quota, UpstreamErrorClass::QuotaLimited { status: 429, .. }),
            "402 应当按限流处理，实得 {quota:?}"
        );
        let rate = DOUBAOWORK_ADAPTER.classify_error(
            429,
            &json!({"message": "rate limited", "error_code": 710022004}),
        );
        assert!(
            matches!(
                &rate,
                UpstreamErrorClass::QuotaLimited {
                    upstream_code: Some(710022004),
                    status: 429,
                    ..
                }
            ),
            "429 应当带着上游业务码按限流处理，实得 {rate:?}"
        );
        let blocked = DOUBAOWORK_ADAPTER.classify_error(
            400,
            &json!({"message": "blocked by security policy"}),
        );
        assert!(matches!(blocked, UpstreamErrorClass::ContentBlocked { .. }));
        // 普通 400 不命中审核文案 → Fatal
        let plain = DOUBAOWORK_ADAPTER.classify_error(400, &json!({"message": "bad param"}));
        assert!(matches!(plain, UpstreamErrorClass::Fatal { .. }));
        // 5xx 原样（本家是官方站点，不改成 502）
        let server = DOUBAOWORK_ADAPTER.classify_error(503, &json!({"message": "unavailable"}));
        // 本家是**官方站点**，不是自有网关：5xx 原样透出（别家改成 502 的处置在这里没有依据）
        assert!(
            matches!(&server, UpstreamErrorClass::Fatal { status: 503, .. }),
            "503 应当原样透出，实得 {server:?}"
        );
    }

    /// 思考等级：客户端已指定 → Skip；关闭档 → Skip；认得的档 → 写内部通道
    #[test]
    fn reasoning_patch_respects_client_and_off() {
        let body_with_client = json!({"reasoning_effort": "high"});
        assert!(matches!(
            DOUBAOWORK_ADAPTER.reasoning_patch("low", "m", &body_with_client),
            ReasoningPatch::Skip { .. }
        ));
        assert!(matches!(
            DOUBAOWORK_ADAPTER.reasoning_patch("off", "m", &json!({})),
            ReasoningPatch::Skip { .. }
        ));
        // 表外的自定义等级一律 Skip（用户可能填了任何东西）
        assert!(matches!(
            DOUBAOWORK_ADAPTER.reasoning_patch("whatever", "m", &json!({})),
            ReasoningPatch::Skip { .. }
        ));
        // 认得的档位 → 写**顶层整数通道**（不是直接覆盖 `option` ——
        // 那样会抹掉 `answer_with_suggest` 等必需子键）
        let base = json!({"option": {"answer_with_suggest": true, "need_deep_think": 0}});
        let high = DOUBAOWORK_ADAPTER.reasoning_patch("high", "m", &base);
        assert!(
            matches!(&high, ReasoningPatch::Set { field, .. } if *field == plan::BODY_THINK_LEVEL_KEY),
            "认得的档位应当写进内部通道，实得 {high:?}"
        );
        if let ReasoningPatch::Set { value, .. } = &high {
            assert_eq!(value.as_i64(), Some(1));
        }
        let max = DOUBAOWORK_ADAPTER.reasoning_patch("max", "m", &json!({}));
        assert!(
            matches!(&max, ReasoningPatch::Set { .. }),
            "max 档应当注入，实得 {max:?}"
        );
        if let ReasoningPatch::Set { value, .. } = &max {
            assert_eq!(value.as_i64(), Some(3));
        }
    }

    /// 内部通道的**端到端**效果：通道有值 → 信封两处口径一致（整数 + 字符串），
    /// 且通道键**不出现在出网信封里**
    #[test]
    fn think_level_channel_lands_in_both_places_and_does_not_leak() {
        // `credentials` 已在文件头 `use super::{…, credentials, …}` 导入
        let credentials = credentials::account_from_cookies(
            "doubaowork-1",
            "测试",
            &[("sessionid".to_string(), "abcdef".to_string())],
            None,
        );
        let mut body = json!({
            "model": "doubao-fast",
            "messages": [{"role": "user", "content": "hi"}]
        });
        // 模拟绑定注入（`reasoning_patch` 的产物）
        if let Some(object) = body.as_object_mut() {
            object.insert(plan::BODY_THINK_LEVEL_KEY.to_string(), json!(3));
        }
        let built = plan::build(&credentials, &body, &plan::ReadyAttachments::default());
        assert!(
            built.is_ok(),
            "构造应当成功：{:?}",
            built.as_ref().err().map(|error| error.message.clone())
        );
        // 上一行已断言成功；`Err` 分支不可达，只为满足编译（release 是 panic=abort）
        let built = match built {
            Ok(built) => built,
            Err(_) => return,
        };
        // 两处口径一致：option 收整数、ext 收字符串
        assert_eq!(
            built.body.pointer("/option/need_deep_think").and_then(Value::as_i64),
            Some(3)
        );
        assert_eq!(
            built.body.pointer("/ext/use_deep_think").and_then(Value::as_str),
            Some("3")
        );
        // **内部通道绝不能漏给上游**（信封只有四个顶层键）
        let tops: Vec<&str> = built
            .body
            .as_object()
            .map(|object| object.keys().map(String::as_str).collect())
            .unwrap_or_default();
        assert!(!tops.contains(&plan::BODY_THINK_LEVEL_KEY));
        assert_eq!(tops.len(), 4);
        // 「上游等级」采集口读到的是**落进信封的那个值**
        assert_eq!(DOUBAOWORK_ADAPTER.outbound_reasoning(&built.body), Some("expert".to_string()));
    }

    /// 「上游等级」采集口：读 `option.need_deep_think`，关闭档返回 None
    #[test]
    fn outbound_reasoning_reads_option() {
        assert_eq!(
            DOUBAOWORK_ADAPTER.outbound_reasoning(&json!({"option": {"need_deep_think": 0}})),
            None
        );
        assert_eq!(
            DOUBAOWORK_ADAPTER.outbound_reasoning(&json!({"option": {"need_deep_think": 3}})),
            Some("expert".to_string())
        );
    }

    /// 模型清单恒非空（内置兜底），且**不含别家的模型名**
    #[test]
    fn list_models_is_never_empty_and_stays_in_family() {
        let items = DOUBAOWORK_ADAPTER.list_models();
        assert!(!items.is_empty());
        let ids: Vec<String> = items
            .iter()
            .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        assert!(ids.iter().all(|id| id.starts_with("doubao-")));
        assert!(!ids.iter().any(|id| id.contains("MiniMax") || id.contains("qwen")));
    }
}
