//! WPS 灵犀（lingxi）协议判据层 —— P0 第一批。
//!
//! ── 这一批**是什么** ──────────────────────────────────────────
//! 把三个公开参考实现（见 `docs/WPS灵犀-移植方案.md` §1）里可交叉验证的
//! **判据**做成纯函数 + 单测：凭证三形态、`key` 目录归一、私有 SSE 帧改写、
//! usage 重算、错误分类、320KB 预检、session/run 缓存状态机、额度两形状、
//! 签到幂等台账、三请求 body。
//!
//! ── 接线状态 ────────────────────────────────────────────────
//! 第一批只做纯函数判据；第二批已把整条链接上：`ProviderKind` / `PROVIDERS` /
//! `adapter_for`、账号存储与粘贴端点、`prepare_body` 三请求编排、
//! `UpstreamResponse::LingxiSse` + `upstream::translate::LingxiToChatStream`
//! 私有 SSE 翻译器、流收尾的 `completed` 结算上报、客户端预设。
//!
//! ⚠️ **端点已由源码交叉验证，但真机 2xx 仍待复验**（2026-11-13 第二轮）：
//! 第一批按 README 语义猜的路径/body 已全部替换成
//! `ketion/lingxi2api-panel::internal/upstream/lingxi.go` 与
//! `RobbsLuo/Coding2API::src/provider/lingxi/client.py` 里逐字一致的常量与
//! 请求形状（主域/回落域、三请求 body、`execution_status`、任务领取、目录/余额
//! 字段、逐行 SSE）。**改了什么见各文件模块头**。方案 §1 的 4 处冲突收敛为
//! 「host 主次」「额度字段主口径」「目录两组」「SSE 逐行」四条已定事实。
//! 但代码能编译 ≠ 上游现在还认这套协议，真机探针仍是必须补的验收项。
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

pub mod adapter;
pub mod channel;
pub mod chat;
pub mod checkin;
pub mod client;
pub mod classify;
pub mod credentials;
pub mod endpoints;
pub mod ledger;
pub mod models;
pub mod session;
pub mod stream;
pub mod tasks;
pub mod usage;
pub mod ws;

pub use adapter::LINGXI_ADAPTER;
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

        let steps = chat::build_bodies(&body, "qwen3.8_flash", "s-1", "g-1", "dev-1");
        assert!(chat::report_lifecycle());
        assert_eq!(steps.completed["status"], json!("completed"));
        assert_eq!(steps.completed["terminal_reason"], json!("completed"));
        assert!(steps.chat.get("session_id").is_none());

        let key = session::cache_key("42", "qwen3.8_flash");
        let cache = session::SessionCache::new();
        cache.store(
            &key,
            session::Session {
                session_id: "s-1".to_string(),
                run_id: "r-1".to_string(),
                group_id: "g-1".to_string(),
                base: endpoints::API_HOST.to_string(),
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
