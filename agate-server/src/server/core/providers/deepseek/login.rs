//! DeepSeek 网页版「登录」：**粘贴归一化 + best-effort 探活**。
//!
//! ── 本家没有 OAuth / 设备码 / 窗口（与 Command Code 同一形态）────
//! 用户能拿到 userToken 的唯一途径就是**自己在浏览器里登录**，然后从
//! DevTools 复制出来。本机不跑任何登录流程（没有授权地址、没有回调端口）。
//!
//! ```text
//!   用户粘贴的东西（形态很杂）→ normalize_token → 形态检查（非空 / 无空白）
//!     → POST /api/v0/chat/create_pow_challenge 探活（401/403 才判无效）
//!     → {"accessToken": "<裸 token>"} 交给落账号入口
//! ```
//!
//! ── 归一化要处理哪些粘贴形态（用户从浏览器复制的东西很杂）────────
//! 认得：
//! ```text
//!   裸 token                            eyJhbGciOi…
//!   带成对引号（从 JSON 里复制）          "eyJhbGciOi…"
//!   整串 localStorage JSON               {"value":"eyJ…","__version":"0"}
//!   整行 `userToken: eyJ…`               userToken: eyJ…
//! ```
//! 这与 `credentials::normalize_token` 是同一件事，委托给它做（只有一份实现，
//! 两处逻辑分家迟早会出现「界面归一了、探活没归一」这种半截状态）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::json;
use serde_json::Value;

use crate::server::errors::GatewayError;
use crate::server::logging;

use super::credentials::{self, KeyCheck};

/// 输入长度上限（超过这个长度的输入一定是粘错了东西）
const MAX_INPUT_LENGTH: usize = 16_384;

/// 归一化 + 校验，返回可直接交给落账号入口的 payload。
///
/// 校验顺序（先本地、再线上）是刻意的：本地形态不对时**一次网络都不发**
/// （用户粘错了东西，探活没有意义）；线上探活失败**不阻断添加**
/// （见 `credentials::probe_token` 的三档语义）。
pub async fn verify_token(raw: &str) -> Result<Value, GatewayError> {
    if raw.trim().is_empty() {
        return Err(GatewayError::with_status(
            400,
            "请粘贴 DeepSeek 网页版的 userToken（浏览器 localStorage 里的那一串）",
        ));
    }
    if raw.chars().count() > MAX_INPUT_LENGTH {
        return Err(GatewayError::with_status(
            400,
            "粘贴的内容过长，请确认复制的是 userToken 本身而不是整页内容",
        ));
    }
    let token = credentials::normalize_token(raw);
    if token.is_empty() {
        return Err(GatewayError::with_status(400, "粘贴的内容里没有可识别的 userToken"));
    }
    if !credentials::shape_ok(&token) {
        return Err(GatewayError::with_status(
            400,
            "userToken 形态不对：应是不含空白的一整串（不要把 Cookie 或整行文本粘进来）",
        ));
    }

    // best-effort 探活（见 `credentials::probe_token` 的三档语义）
    match credentials::probe_token(&token).await {
        KeyCheck::Invalid(reason) => {
            return Err(GatewayError::with_status(
                401,
                format!("userToken 校验未通过：{reason}"),
            ))
        }
        KeyCheck::Unknown(reason) => {
            // 判不出来就放行（网络抖动不该让用户加不进账号）
            logging::verbose(
                "[DeepSeek]",
                &format!("userToken 探活没判出结果（按可用放行）：{reason}"),
            );
        }
        KeyCheck::Valid => {}
    }

    Ok(json!({ "accessToken": token }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 归一化委托给 credentials（只有一份实现）—— 这里验证「委托链是通的」
    #[test]
    fn normalize_delegates_to_credentials() {
        assert_eq!(
            credentials::normalize_token(r#"{"value":"eyJ.abc.def"}"#),
            "eyJ.abc.def"
        );
    }

    /// 空白输入在**本地**就被拒绝（不发网络）
    #[tokio::test]
    async fn blank_input_is_rejected_locally() {
        let error = verify_token("   ").await.expect_err("空白应被拒");
        assert_eq!(error.status_code, 400);
        assert!(error.message.contains("userToken"));
    }

    /// 形态不对（含空白 = 粘了整行）在本地被拒
    #[tokio::test]
    async fn malformed_input_is_rejected_locally() {
        let error = verify_token(r#"eyJ part1 part2"#)
            .await
            .expect_err("含空白应被拒");
        assert_eq!(error.status_code, 400);
        assert!(error.message.contains("形态"));
    }

    /// 过长输入被拒
    #[tokio::test]
    async fn oversized_input_is_rejected() {
        let long = "x".repeat(MAX_INPUT_LENGTH + 10);
        let error = verify_token(&long).await.expect_err("过长应被拒");
        assert_eq!(error.status_code, 400);
        assert!(error.message.contains("过长"));
    }
}