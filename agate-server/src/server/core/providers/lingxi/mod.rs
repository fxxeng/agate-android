//! WPS 灵犀（lingxi）协议判据层 —— P0 第一批。
//!
//! ── 这一批**是什么** ──────────────────────────────────────────
//! 把三个公开参考实现（见 `docs/WPS灵犀-移植方案.md` §1）里可交叉验证的
//! **判据**做成纯函数 + 单测：凭证三形态、`key` 目录归一、私有 SSE 帧改写、
//! usage 重算、错误分类、320KB 预检、session/run 缓存状态机、额度两形状、
//! 签到幂等台账、三请求 body。
//!
//! ── 这一批**不是什么**（刻意的边界）──────────────────────────
//! · 没有注册 `ProviderKind` / `PROVIDERS` / `adapter_for`：把半成品挂进转发链
//!   只会得到「看得到、调不通」；
//! · 没有账号存储 / API 端点 / 客户端预设 / 网络编排：那些要一起接才自洽；
//! · **没有任何真机实测**：所有端点与字段名都来自公开 README，方案 §1 列的
//!   4 处来源冲突（host / 额度字段 / 目录来源 / 通道）在探针验证前不是事实。
//!   本批的价值是「判据先被单测钉住」，让下一批接线时改的只是拼装，不是猜。
//!
//! ── 已知限制对照（方案 §3）────────────────────────────────────
//! L1 无续期 → `credentials` 不做有效期推算，`classify(401)` 才判死号；
//! L5 320KB → `classify::oversized` 本地拦；
//! L6 审核 200 → `stream` 只认结构化 `finish_reason`，不编造中文话术白名单；
//! L7 计费 → `chat::report_lifecycle()` 默认开启，五步 body 里 `completed` 是结算点；
//! L8 缓存 → `session` 的 1h 不滑动 + 重建一次；
//! L9 usage → `stream::recompute_usage` 按 prompt+completion；
//! L10 工具 schema → `chat::flatten_tools`；
//! L11 签到 → `checkin::reconcile` 绝不看 HTTP 200；
//! L12 429 → `classify` 归为模型级。
//!
//! 硬约束：release 是 `panic=abort`，本模块零 unwrap/expect/panic。

pub mod chat;
pub mod checkin;
pub mod classify;
pub mod credentials;
pub mod endpoints;
pub mod models;
pub mod session;
pub mod stream;
pub mod usage;

pub use endpoints::ID;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 一串请求从上到下走一遍判据，钉住「各层用的是同一组事实」。
    #[test]
    fn one_request_walks_the_whole_judgement_chain() {
        let credential = credentials::normalize_paste("wps_sid=V02Sabcdefghij; uid=42")
            .unwrap_or_default();
        assert_eq!(credential.uid.as_deref(), Some("42"));

        let body = json!({ "model": "qwen3.8_flash", "messages": [], "stream": true });
        assert!(!classify::oversized(&body));

        let steps = chat::build_bodies(&body, "qwen3.8_flash", "s-1", "r-1");
        assert!(chat::report_lifecycle());
        assert_eq!(steps.completed["execution_status"], json!("completed"));

        let key = session::cache_key("42", "qwen3.8_flash");
        let cache = session::SessionCache::new();
        cache.store(
            &key,
            session::Session {
                session_id: "s-1".to_string(),
                run_id: "r-1".to_string(),
                created_ms: 1_000,
            },
        );
        assert!(cache.lookup(&key, 2_000).is_some());

        let rewritten = stream::rewrite_frame(
            &json!({ "choices": [{ "text": "你好" }], "usage": { "total_tokens": 99999 } }),
            10,
            2,
        );
        let rewritten = rewritten.unwrap_or(stream::Rewritten {
            chunk: serde_json::Value::Null,
            finished: false,
            refused: false,
        });
        assert_eq!(rewritten.chunk["usage"]["total_tokens"], json!(12));
        assert_eq!(rewritten.chunk["choices"][0]["delta"]["content"], json!("你好"));
    }

    #[test]
    fn provider_id_is_stable() {
        assert_eq!(ID, "lingxi");
        assert_eq!(checkin::task_name(), "daily_check_in");
    }
}
