//! DeepSeek 网页版的**每次请求准备**：建会话 + 解 PoW（异步钩子 + 状态表）。
//!
//! ── 为什么需要这个模块（同步与异步的接缝）──────────────────────
//! 本家发一次 `chat/completion` 之前有两个**必须先完成的网络/计算步骤**
//! （见 `chat.rs` 的模块头）：
//! ```text
//!   1. 建会话（POST /api/v0/chat_session/create）—— 网络
//!   2. 领 PoW 题并求解（POST create_pow_challenge + 本地哈希）—— 网络 + 计算
//! ```
//! 而 `ProviderAdapter::build_chat_request` 是**同步**的（它只拼 URL / 头 / 体），
//! 不能在里面发请求。项目里对这类接缝的既定解法是 `commandcode` 那套：
//! **把异步工作挂在 `ensure_access_token`（它本来就是每次转发前的异步准备钩子，
//! `provider_loop` 在 `build_chat_request` 之前 await 它），
//! 结果放进一张按账号索引的内存状态表，同步的 `build_chat_request` 再取走。**
//! 本模块就是那张表与它的读写函数（范式与 `commandcode::fingerprint` 同源）。
//!
//! ── 为什么按 token 索引而不是按账号 id ──────────────────────
//! 两个钩子的入参不同：`ensure_access_token` 拿到的是 `account_id`，
//! `build_chat_request` 拿到的是**账号的会话形态**（`account: &Value`，
//! 里面未必带 id）。按 token 索引是两边都能拿到的那个量 —— 且 token 本身就是
//! 账号身份（同一个 token 重复添加会被落账号入口归并成同一条，见
//! `doubaowork_accounts.rs` 同款做法）。与 `fingerprint` 按 `key_id(api_key)`
//! 索引是同一思路。
//!
//! ── 为什么取走（`take`）而不是读取（`get`）────────────────────
//! **PoW 题一次性消费**（题面带随机 salt，用过就废，见 `pow.rs` 的模块头）。
//! 若允许同一份题解被第二次读取，第二次必然被上游拒；更糟的是它会**掩盖**
//! 「准备步骤没跑」这个真问题（拿到一份陈旧题解去发，报错看起来像上游问题）。
//! 因此 `take` 语义是「取走即失效」—— 与 `lobsterai::oauth` 的 pending 表同款。
//!
//! ── 会话为什么也每次重建（如实记录这个取舍）────────────────────
//! 本机网关是**无状态转发**：每次请求自带全部历史（`plan.rs` 把 messages
//! 拍平成一条 prompt）。上游的 `chat_session_id` + `parent_message_id` 是
//! 「服务端维护多轮」的机制，与无状态转发**不是同一套**。因此这里每次请求
//! 新建一个会话、不传 `parent_message_id`（每次都是一轮独立的对话，
//! 上下文由拍平的 prompt 承载）。
//!
//! ⚠️ **代价要写清楚**：这会在用户的 chat.deepseek.com 账号下**留下会话记录**
//! （一次请求一条）。这是本家当前实现的一个**已知副作用**，不是缺陷伪装成
//! 特性 —— 官方文档也点明了本家是「用你自己的网页登录态」，会话记录落在
//! 用户自己的账号里是这类转发的固有代价。
//!
//! ── 未联调的部分（诚实标注）────────────────────────────────
//! 本模块的**结构**是确定的（建会话 → 解题 → 拼请求），但整条链路没有真实
//! `userToken` 就无法跑通验证。可离线证明的只有 `pow.rs` 的算法（有官方自检
//! 向量）。凡是「上游是否接受这个请求体形状」的判断，都以二进制与公开逆向
//! 为依据，**未经实测** —— 这一点在 `chat.rs` 的模块头也写了。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::server::errors::GatewayError;
use crate::server::logging;

use super::{chat, pow};

/// 一次请求的准备结果（`build_chat_request` 要用的东西）
#[derive(Clone, Debug)]
pub struct Prepared {
    /// 会话 id（`chat/completion` 的 `chat_session_id`）
    pub session_id: String,
    /// 题解的请求头值（`x-ds-pow-response`，已 base64）
    pub pow_header: String,
    /// 准备完成时刻（毫秒；用于丢弃过期的准备结果）
    pub prepared_at: i64,
}

/// 准备结果的**有效期**。
///
/// ── 这个阈值是怎么定的 ─────────────────────────────────────
/// `ensure_access_token` 与 `build_chat_request` 是同一次转发里的**相邻两步**
/// （中间只隔着构建请求体），因此正常间隔是毫秒级。这里给 60 秒是**宽松上限**：
/// 超过它说明这两个钩子之间的流程出了大问题（重试链走岔了），
/// 此时宁可**当没有准备过**（让本次请求明确失败并重试），
/// 也不要拿一份陈旧题解去发（那会换来一条看不出原因的上游拒绝）。
const PREPARED_TTL_MS: i64 = 60_000;

/// 状态表（按 token 索引）
static PREPARED: OnceLock<Mutex<HashMap<String, Prepared>>> = OnceLock::new();

fn table() -> &'static Mutex<HashMap<String, Prepared>> {
    PREPARED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// token → 表键（不直接拿明文当键：表可能进日志 / 调试快照，摘要更稳）
fn key_of(user_token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"deepseek:prepared:v1\0");
    hasher.update(user_token.trim().as_bytes());
    format!("{:x}", hasher.finalize())
}

/// **异步准备钩子**（挂在 `ensure_access_token` 里）：
/// 建会话 + 领题求解，结果放进状态表。
///
/// 任一步失败都返回可读的 `GatewayError` —— 这里的失败是**真失败**
/// （不像 `fingerprint` 那种 best-effort 伪装），因为它准备的是本次请求
/// 必需的两样东西，缺一就发不出去。
pub async fn prepare(user_token: &str) -> Result<(), GatewayError> {
    // 1. 建会话（见模块头：每次新建，不传 parent_message_id）
    let session_id = chat::create_session(user_token).await?;
    // 2. 领题并求解（一次性，本地计算）
    let solved = chat::obtain_and_solve_pow(user_token).await?;
    let header = pow::response_header(&solved);

    let key = key_of(user_token);
    let prepared = Prepared {
        session_id,
        pow_header: header,
        prepared_at: logging::now_ms(),
    };
    {
        let mut guard = match table().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.insert(key, prepared);
    }
    logging::verbose(
        "[DeepSeek]",
        &format!(
            "已准备一次转发（会话 {}，PoW answer={}）",
            solved.answer, solved.answer
        ),
    );
    Ok(())
}

/// **同步取走**准备结果（`build_chat_request` 里调用）。
///
/// 取走即从表里移除（PoW 一次性，见模块头）。过期（超过
/// [`PREPARED_TTL_MS`]）也当作没有 —— 同样是为了不拿陈旧题解去发。
pub fn take(user_token: &str) -> Option<Prepared> {
    let key = key_of(user_token);
    let mut guard = match table().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let prepared = guard.remove(&key)?;
    let now = logging::now_ms();
    if now.saturating_sub(prepared.prepared_at) > PREPARED_TTL_MS {
        return None;
    }
    Some(prepared)
}

/// 丢弃某个 token 的准备结果（准备失败 / 出错时的清理；偶发失败不必清理，
/// 但显式丢弃能让「同一 token 的陈旧态」不残留）。
pub fn discard(user_token: &str) {
    let key = key_of(user_token);
    let mut guard = match table().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.remove(&key);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取走即失效（PoW 一次性 —— 第二次取必须取不到）
    #[test]
    fn take_consumes_the_preparation() {
        let token = "test-token-take-once";
        let key = key_of(token);
        {
            let mut guard = match table().lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.insert(
                key.clone(),
                Prepared {
                    session_id: "s1".to_string(),
                    pow_header: "h1".to_string(),
                    prepared_at: logging::now_ms(),
                },
            );
        }
        let first = take(token);
        assert!(first.is_some(), "第一次应取得到");
        assert_eq!(first.as_ref().map(|item| item.session_id.as_str()), Some("s1"));
        assert!(take(token).is_none(), "第二次必须取不到（一次性）");
    }

    /// 过期结果不返回（避免拿陈旧题解去发）
    #[test]
    fn expired_preparation_is_dropped() {
        let token = "test-token-expired";
        let key = key_of(token);
        {
            let mut guard = match table().lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.insert(
                key,
                Prepared {
                    session_id: "s2".to_string(),
                    pow_header: "h2".to_string(),
                    prepared_at: logging::now_ms() - PREPARED_TTL_MS - 1000,
                },
            );
        }
        assert!(take(token).is_none(), "过期结果不得返回");
    }

    /// discard 清掉未取走的结果
    #[test]
    fn discard_removes_pending() {
        let token = "test-token-discard";
        let key = key_of(token);
        {
            let mut guard = match table().lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.insert(
                key,
                Prepared {
                    session_id: "s3".to_string(),
                    pow_header: "h3".to_string(),
                    prepared_at: logging::now_ms(),
                },
            );
        }
        discard(token);
        assert!(take(token).is_none(), "discard 后应取不到");
    }

    /// 不同 token 互不干扰
    #[test]
    fn keys_are_isolated_per_token() {
        assert_ne!(key_of("token-a"), key_of("token-b"));
        // 前后空白归一：同一 token 的两种写法是同一格
        assert_eq!(key_of(" tok "), key_of("tok"));
    }

    /// 未准备过的 token 返回 None（不 panic）
    #[test]
    fn unknown_token_returns_none() {
        assert!(take("never-prepared-token").is_none());
    }
}