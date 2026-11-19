//! LobsterAI（网易有道「有道龙虾」）：账号管理（浏览器登录）+ **无状态**推理
//! 转发 + 目录 / 积分 / 每日签到。
//!
//! ── 上游长什么样（细节见各子模块的模块头）──────────────────────
//! ```text
//!   登录门户   https://lobsterai.youdao.com        浏览器授权页（手机号 / 微信）
//!   API        https://lobsterai-server.youdao.com 换码 / 续期 / 目录 / 对话 / 积分 / 活动
//! ```
//! 对话端点是 **OpenAI 兼容的原生协议**且**只收 SSE**（`stream:false` 回 HTTP 500）：
//! 本网关的转发入口恒以 `stream:true` 请求上游，所以这条要求天然满足。本家
//! **无状态**（一次 HTTP 请求 = 一次回答），转发链只需 `build_chat_request`。
//!
//! ── 五处与别家不同的取舍（都写在各自模块头，这里给索引）─────────
//!   1. **登录是「回调带回一次性 code」那一套**（`supports_web_login = true`）：
//!      授权在门户完成、302 回本机 loopback（裸 `/auth/callback`），用 code 换令牌
//!      （`oauth.rs`）。
//!   2. **续期要一整组身份载荷件**（`uuid` + 两个 keyfrom），且 `latestKeyfrom`
//!      **永不更新** —— 只存两个 token 的账号续不了期（`credentials.rs` 的模块头
//!      有这条约束的一手来源）。
//!   3. **额度是积分**，读数在 `/api/user/profile-summary`（**不是**
//!      `/api/user/quota`，那个只报 300 免费额度）—— 见 `usage.rs`。
//!   4. **签到是三步协议且幂等靠客户端**（活动位 → 上下文两道预检 →
//!      check_in + uuid4 幂等键）—— 见 `checkin.rs`。
//!   5. **错误分类里「余额耗尽」是最主要的失败模式**，而且它可能以
//!      `HTTP 200` + 流内错误帧下发 —— 见 `errors.rs`（含一处**已知缺口**）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本模块零 unwrap/expect/panic。

pub mod adapter;
pub mod chat;
pub mod checkin;
pub mod credentials;
pub mod endpoints;
pub mod errors;
pub mod ids;
pub mod models;
pub mod oauth;
pub mod refresh;
pub mod usage;
pub mod version;

pub use adapter::{LobsterAiAdapter, LOBSTERAI_ADAPTER};

/// provider id（落盘契约，不能改）
pub const ID: &str = super::kind_id(super::ProviderKind::LobsterAi);

#[cfg(test)]
mod tests {
    use super::*;

    /// provider id 与注册表一致（落盘契约）
    #[test]
    fn id_matches_register() {
        assert_eq!(ID, "lobsterai");
        assert_eq!(super::super::kind_from_id(ID), Some(super::super::ProviderKind::LobsterAi));
    }
}