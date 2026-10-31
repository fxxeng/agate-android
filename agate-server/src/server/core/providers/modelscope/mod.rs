//! ModelScope（魔搭）：国内站 `modelscope` / 国际站 `modelscope-intl`。
//!
//! ── 一句话 ──────────────────────────────────────────────────
//! 标准 OpenAI 兼容转发 + 模型目录 + 魔粒余额 + 每日签到；**两站账号体系分开**，
//! 所以是一家实现、两个实例（地区即 provider 身份）。
//!
//! ── 文件分工 ────────────────────────────────────────────────
//! ```text
//!   region.rs      两套域名 / 两个 provider id / 账号 id 前缀（纯常量）
//!   credentials.rs `ms-` 令牌的唯一取值链（记录与 payload 共用，避免分叉）
//!   client.rs      两个域的 GET 封装 + 信封大小写归一 + 401 文案
//!   models.rs      `GET {推理域}/models` + 两地区各一格缓存
//!   probe.rs       添加账号时的令牌探活（拿 HavanaId 做稳定身份）
//!   balance.rs     魔粒钱包 + 今日已用次数
//!   checkin.rs     读一次登录信息即完成签到
//!   errors.rs      两条账号级失败判据 + 北京零点冷却
//!   adapter.rs     接线：trait 实现 + 必注入的 include_usage
//! ```
//!
//! ── 三个实测事实（决定了整个形状，细节见各模块头）────────────────
//!   1. 一个 `ms-` 令牌两用（Bearer 调推理 / cookie 调网页站）⇒ 只有一个凭证字段；
//!   2. 两站账号体系互不相通（令牌打对站才行）⇒ 地区必须是身份；
//!   3. 流式响应**只在请求带 `stream_options.include_usage` 时**才给真值 usage
//!      ⇒ 不注入会让非流式请求的记账恒为 0（见 `adapter::request_body`）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本模块零 unwrap/expect/panic。

pub mod adapter;
pub mod balance;
pub mod checkin;
pub mod client;
pub mod credentials;
pub mod errors;
pub mod models;
pub mod probe;
pub mod region;

pub use adapter::{ModelscopeAdapter, MODELSCOPE_ADAPTER, MODELSCOPE_INTL_ADAPTER};
pub use region::Region;

// 账号记录的形状知识（`AccountStore::modelscope_account_record`）落在
// `account_store::modelscope_accounts`，与其余家同一分工（account_store 侧
// 扩展 impl），本模块不放。
