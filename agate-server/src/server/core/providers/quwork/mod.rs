//! QuWork 1.6.0：OpenAI Chat 对话 + 独立账号续期 + 公开模型目录 + 积分 + 每日奖励。
//!
//! ── 从上游 v3.5.0-beta.1 去桌面化移植 ────────────────────────
//! 上游是**插件 SDK 形态**（`PluginCtx` / `StoreProxy` / `ChatPlan` / `Provider`
//! trait，WASM 边界）。本仓静态编译，只搬协议逻辑：
//! ```text
//!   accounts.rs    → credentials.rs   凭证解析 / 身份 / 公开视图
//!   chat.rs        → chat.rs         请求体 compat 改写 + 错误分类
//!   models.rs      → models.rs       models.json 兜底 + validate + 缓存
//!   refresh.rs     → refresh.rs      续期 / model_token 容错 / 比较-再写
//!   usage.rs       → usage.rs        积分（amount×100 / total_points 不乘）
//!   checkin.rs     → checkin.rs      每日奖励（结果不明不重复提交）
//!   client/endpoints → client/endpoints.rs  auth_http::send_raw 封装 + 常量
//! ```
//! **没搬**（桌面 / SDK / WASM）：
//!   - `desktop.rs`：读 Windows APPDATA / macOS 的 QuWork SQLite 登录库与
//!     `.credentials.yaml`（Android 上没有那个客户端，给入口只会稳定失败）；
//!   - `login.rs`：本机导入 / 设备码登录流程（本家只支持粘贴）；
//!   - `benefits.rs`：月度赠送、兑换码、读本机 `device-identity.json`（设备标识
//!     与远程兑换是桌面端专属，去桌面化后不做）；
//!   - SDK 的 `PluginCtx.settings_get/set` 落盘缓存 → 本仓用进程内
//!     `OnceLock<Mutex<…>>`；`ctx.ledger_get/put` → 进程内领取台账。
//!
//! ── 两套凭据 ────────────────────────────────────────────────
//! `llmToken` 调模型（必需）、`loginToken`（上游叫 accessToken）管账号面（可选）。
//! 落库时由 `account_store::quwork_accounts` 把 `llmToken` 镜像到本仓的
//! `accessToken` 键，供 `session_from_record` / 转发层使用。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本模块零 unwrap/expect/panic，不持锁跨 await。

pub mod adapter;
pub mod chat;
pub mod checkin;
pub mod client;
pub mod credentials;
pub mod endpoints;
pub mod models;
pub mod refresh;
pub mod usage;

pub use adapter::{QuWorkAdapter, QUWORK_ADAPTER};
pub use endpoints::ID;

#[cfg(test)]
mod tests {
    use super::*;
    // `kind()` 是 trait 方法：不导入 trait 就只有「找不到方法」这一句报错
    use crate::server::core::providers::adapter::ProviderAdapter;

    #[test]
    fn adapter_declares_quwork_kind() {
        assert_eq!(
            QUWORK_ADAPTER.kind(),
            crate::server::core::providers::ProviderKind::QuWork
        );
        assert_eq!(ID, "quwork");
    }

    #[test]
    fn default_catalog_is_not_empty() {
        assert_eq!(models::default_models().len(), 10);
    }
}
