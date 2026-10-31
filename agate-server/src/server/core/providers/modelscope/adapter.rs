//! ModelScope（魔搭）适配器：**无状态 OpenAI 兼容转发** + 模型目录 + 魔粒余额 + 每日签到。
//!
//! ── 一家实现、两个实例 ────────────────────────────────────────
//! 地区不是账号字段而是 provider 身份（见 `region.rs` 的模块头）：两个实例各自
//! 申报自己的 id，于是「这条账号打哪个站」由身份保证，不需要在每一处转发 /
//! 查询里再读一次账号上的地区标签。
//!
//! ── 上游就是标准 OpenAI 协议，所以这一层最薄 ────────────────────
//! `POST {推理域}/chat/completions`、bearer 鉴权、SSE 帧也是标准 chat 形态 ——
//! 不需要翻译层、不需要伪装客户端头。唯一的改写是**必须注入**
//! `stream_options.include_usage`（见 [`request_body`]，一个会静默毁账的坑）。
//!
//! ── 没有续期 ────────────────────────────────────────────────
//! `ms-` 令牌是长期有效的 API key，上游没有 refresh 接口 ⇒ `supports_refresh`
//! 为假、`credentials_expiring` 保持默认假（判不出过期时间就说「需要刷新」会让
//! 维护任务每轮白打一次上游）。失效只能重新粘贴。
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

use super::{balance, credentials, errors, models, region::Region};

/// 一家 ModelScope 适配器（按地区参数化）
pub struct ModelscopeAdapter {
    region: Region,
}

impl ModelscopeAdapter {
    /// 国内站实例
    pub const CN: ModelscopeAdapter = ModelscopeAdapter { region: Region::Cn };
    /// 国际站实例
    pub const INTL: ModelscopeAdapter = ModelscopeAdapter { region: Region::Intl };
}

pub static MODELSCOPE_ADAPTER: ModelscopeAdapter = ModelscopeAdapter::CN;
pub static MODELSCOPE_INTL_ADAPTER: ModelscopeAdapter = ModelscopeAdapter::INTL;

/// 出站请求体：原样透传 + 把 `stream_options.include_usage` 置真。
///
/// ── 为什么必须注入（实测，一个会静默毁账的坑）───────────────────
/// 魔搭的流式响应是**非标准形态**：每个 chunk 都带一个 `usage` 对象，值全是 0；
/// 真值只在末尾那个 `choices: []` 的 usage-only 块里 —— **而那一块只在请求显式
/// 带上 `stream_options.include_usage: true` 时才发**。实测对照：
/// ```text
///   不带：最后一帧 {"choices":[{"finish_reason":"stop"}],"usage":{0,0,0}}   ← 真值没出现
///   带：  {"choices":[],"usage":{"prompt":62,"completion":32,"total":94}}   ← 真值在这
/// ```
/// 后果链：本网关**恒以流式请求上游**，客户端要非流式时再由聚合器把流折成完整
/// 响应。不注入这个开关时，聚合器读到的最后一个 usage 就是那串全零 —— 于是
/// **非流式请求的 usage 恒为 0**，请求日志、报表与前端 token 显示一起归零，
/// 而内容完全正常（这个故障只在统计上可见，排查时极难联想到「上游少发了一帧」）。
///
/// 客户端已经自己写了 `stream_options` 时**保留它的其余键**、只把
/// `include_usage` 置真：覆盖整个对象会把用户显式设过的东西悄悄丢掉。
///
/// 非对象 body（畸形请求）原样返回，不在这一层报错 —— 让上游去拒绝它。
pub fn request_body(body: &Value) -> Value {
    let mut payload = body.clone();
    let Some(object) = payload.as_object_mut() else {
        return payload;
    };
    let mut options = object
        .get("stream_options")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    options.insert("include_usage".to_string(), Value::Bool(true));
    object.insert("stream_options".to_string(), Value::Object(options));
    payload
}

/// 从会话形态里取令牌（`auth.accessToken`，兼容顶层的裸 `accessToken`）
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

impl ProviderAdapter for ModelscopeAdapter {
    fn kind(&self) -> ProviderKind {
        match self.region {
            Region::Cn => ProviderKind::Modelscope,
            Region::Intl => ProviderKind::ModelscopeIntl,
        }
    }

    /// 清单来自 `models.rs` 的进程缓存（本家没有内置兜底表，见那里的模块头）
    fn list_models(&self) -> Vec<Value> {
        models::list(self.region)
    }

    /// 构造 `POST {推理域}/chat/completions`（标准 OpenAI 协议）
    ///
    /// ── 为什么**不**改别的（模型与流式意向都原样透传）────────────────
    /// 魔搭的 `/models` 里混着非对话模型，但「哪些模型支持什么」本实现没有任何
    /// 可信依据去猜（按 id 猜会猜错），让上游自己判 —— 它给的 400 会被
    /// `classify_error` 如实透出。模型 id 也原样发（`组织/模型名`）。
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
                "ModelScope 账号缺少令牌，无法转发（请重新粘贴 ms- 令牌）",
            ));
        }
        let headers = vec![
            // 发送层会自动补 Content-Type；这里只加鉴权与 Accept
            ("Authorization".to_string(), format!("Bearer {token}")),
            ("Accept".to_string(), "text/event-stream".to_string()),
        ];
        Ok(ChatRequestPlan::chat(
            format!("{}/chat/completions", self.region.inference_base_url()),
            headers,
            request_body(body),
        ))
    }

    /// 上游错误分类。**顺序要紧**：两条「账号级」判据必须在裸 401/403 与裸 429
    /// 之前，否则它们会被归成「令牌失效」或「速率窗口」，各自的正确处置就都丢了。
    ///
    ///   1. **魔粒耗尽**（429 + `insufficient balance`）→ `QuotaLimited` + 冷却到
    ///      下一个北京零点。它不是速率窗口：充值之前这把令牌一直不可用，按分钟
    ///      冷却只会在当天反复白撞；
    ///   2. **未验证邮箱**（403 + 特征串）→ 同样冷却到次日。⚠️ 漏了这条最糟：
    ///      403 会被当成「令牌失效」，界面提示用户去重新粘贴一个本来就好好的
    ///      令牌，而真正要做的是去账号设置页完成邮箱验证；
    ///   3. **裸 401/403** → `TokenExpired`；
    ///   4. **其余 429** → `QuotaLimited`（不给结构化恢复时间，由限额标记给兜底）；
    ///   5. 其余 → `Fatal`（**不罚账号**：内容审核类也走这里，见 `errors.rs` 的口径）。
    fn classify_error(&self, status: u16, error_body: &Value) -> UpstreamErrorClass {
        let raw = error_body
            .get("message")
            .or_else(|| error_body.get("Message"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or("上游错误");
        let upstream_code = error_body
            .get("code")
            .or_else(|| error_body.get("Code"))
            .and_then(Value::as_i64);
        let message = format!("上游返回 {status}: {raw}");
        let label = self.region.label();

        if errors::is_insufficient_balance(status, raw) {
            return UpstreamErrorClass::QuotaLimited {
                reset_at: Some(errors::next_day_boundary_ms(crate::server::logging::now_ms())),
                message: format!(
                    "ModelScope {label}账号的魔粒已耗尽（上游：{raw}），将在下一个自然日\
                     （北京时间 0 点）自动恢复；给账号充值后立即恢复"
                ),
                upstream_code,
                status,
            };
        }
        if errors::is_unverified_account(status, raw) {
            return UpstreamErrorClass::QuotaLimited {
                reset_at: Some(errors::next_day_boundary_ms(crate::server::logging::now_ms())),
                message: format!(
                    "ModelScope {label}账号未完成邮箱验证，魔粒推理接口拒绝服务\
                     （上游：{raw}）——请到魔搭账号设置页完成邮箱验证，验证后次日自动恢复"
                ),
                upstream_code,
                status,
            };
        }
        if status == 401 || status == 403 {
            return UpstreamErrorClass::TokenExpired { message };
        }
        if status == 429 {
            return UpstreamErrorClass::QuotaLimited {
                reset_at: None,
                message,
                upstream_code,
                status,
            };
        }
        UpstreamErrorClass::Fatal { status, message, upstream_code }
    }

    /// 取可用令牌。本家**没有续期**：只做「记录里有没有令牌」的检查，服务端拒了
    /// 就是 401，编排层归成 `TokenExpired` 并如实告诉客户端「请重新粘贴」。
    fn ensure_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let id = self.region.provider_id();
            let record = store.modelscope_account_record(self.region, account_id);
            if !account_id.is_empty() && record.is_none() {
                return Err(GatewayError::with_status(
                    401,
                    format!("{id} 账号 {account_id} 不存在（请重新添加）"),
                ));
            }
            match credentials::token_of(&record.unwrap_or(Value::Null)) {
                Ok(token) => Ok(token),
                Err(reason) => Err(GatewayError::with_status(401, reason)),
            }
        })
    }

    /// 没有续期手段（长期 API key，上游无 refresh 接口）
    fn supports_refresh(&self) -> bool {
        false
    }

    /// 有远程目录（`GET {推理域}/models`）
    fn supports_model_refresh(&self) -> bool {
        true
    }

    /// 有魔粒余额（`GET /openapi/v1/magicubes/balance`）
    fn supports_usage(&self) -> bool {
        true
    }

    /// 查魔粒余额 + 今日已用调用数
    fn query_usage<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Value, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let record = store.modelscope_account_record(self.region, account_id);
            if !account_id.is_empty() && record.is_none() {
                return Err(GatewayError::with_status(
                    404,
                    format!("ModelScope 账号 {account_id} 不存在（请重新选择）"),
                ));
            }
            let token = credentials::token_of(&record.unwrap_or(Value::Null))
                .map_err(|reason| GatewayError::with_status(400, reason))?;
            match balance::query(self.region, &token).await {
                Ok(document) => Ok(document),
                Err(reason) => Err(GatewayError::with_status(502, reason)),
            }
        })
    }

    /// 刷模型目录：用指定账号（空串 = 本家第一个可用账号）的令牌。
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
            let record = store.modelscope_account_record(self.region, account_id);
            let Some(record) = record else {
                if account_id.is_empty() {
                    crate::server::logging::verbose(
                        "[Models]",
                        &format!("ModelScope {}模型目录刷新跳过：尚未添加账号", self.region.label()),
                    );
                    return ModelRefreshOutcome::unchanged();
                }
                return ModelRefreshOutcome::failed("指定的账号不存在或不可用，请重新选择");
            };
            let token = match credentials::token_of(&record) {
                Ok(token) => token,
                Err(reason) => return ModelRefreshOutcome::failed(reason),
            };
            models::refresh(self.region, &token, force).await
        })
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// **必须**注入 include_usage（否则非流式请求的 usage 恒为 0，见函数注释）
    #[test]
    fn request_body_injects_include_usage() {
        let out = request_body(&json!({"model": "q", "stream": true}));
        assert_eq!(
            out.get("stream_options").and_then(|o| o.get("include_usage")).and_then(Value::as_bool),
            Some(true)
        );
        // 其余键原样保留
        assert_eq!(out.get("model").and_then(Value::as_str), Some("q"));
    }

    /// 客户端自己写了 stream_options 时保留其余键，只覆盖 include_usage
    #[test]
    fn request_body_keeps_client_stream_options() {
        let out = request_body(&json!({"stream_options": {"foo": 1, "include_usage": false}}));
        let options = out.get("stream_options").cloned().unwrap_or(Value::Null);
        assert_eq!(options.get("foo").and_then(Value::as_i64), Some(1));
        assert_eq!(options.get("include_usage").and_then(Value::as_bool), Some(true));
    }

    /// 畸形 body 原样返回，不在这一层报错
    #[test]
    fn non_object_body_passes_through() {
        assert_eq!(request_body(&json!([1, 2, 3])), json!([1, 2, 3]));
    }

    /// 两个实例申报各自的 id（地区是身份，不是账号字段）
    #[test]
    fn two_instances_declare_distinct_kinds() {
        assert_eq!(MODELSCOPE_ADAPTER.kind(), ProviderKind::Modelscope);
        assert_eq!(MODELSCOPE_INTL_ADAPTER.kind(), ProviderKind::ModelscopeIntl);
    }
}
