//! Tabbit 网页版（`web.tabbit.com` / `web.tabbit.ai`）：**Cookie-JWT 登录态 +
//! v3 房间协议 + 签名三件套**。
//!
//! ── 上游是什么 ──────────────────────────────────────────────
//! Tabbit 的「客户端」是一个 Chromium 浏览器，登录态是网页 Cookie 里的 JWT；
//! 上游是**网页对话产品**，一次对话收一段纯文本、没有原生工具调用。因此它归
//! Web 一类（与智谱清言 / DeepSeek 网页同一形态）。
//!
//! 一次对话是**四步时序**，塞不进「一次 HTTP 请求 = 一次对话」的单请求契约：
//! ```text
//!   1. POST /panel/session                      → chat_session_id（房间，30 分钟复用）
//!   2. POST /api/v3/chat/rooms/{id}/join        → SSE 订阅（**唯一带签名的对话请求**）
//!   3. POST /api/v3/chat/rooms/{id}/runs        → 投递消息（不带签名）
//!   4. 读 2 的事件流，翻译成 chat.completion.chunk
//! ```
//! 所以本家是 `is_stateful() == true`，走 `forward_conversation`（与 KukuAI /
//! MonkeyCode 同一条路）。
//!
//! ── 两个站点是两家 provider（不是同一家的地区字段）──────────────
//! 两站**账号体系不通用、签名 key 不同、目录与额度各按站点计**，登录方式也不同
//! （国内站有手机号验证码，国际站只能导入已有 Cookie）。按仓库既有的
//! 「两套账号就拆家」规则：`tabbit`（国内）/ `tabbit-intl`（国际）。
//! 实现只有一份（本模块），按 [`Realm`] 参数化 —— 与 Qoder / 千问办公
//! 「一份实现、两个静态实例」同款。
//!
//! ── 没有续期接口（如实声明，不假装能自动维护）──────────────────
//! 登录态是 RS256 JWT，`expires_in: 604800`（7 天），上游**没有 refresh 端点**。
//! 因此 `supports_refresh() == false`，账号行如实显示有效期；过期只能重新导出
//! Cookie。`account_view` 也从 JWT 的 `exp` 算过期时刻，不报「长期有效」。
//!
//! ── 不做工具调用模拟（Web 包口径）────────────────────────────
//! 网页 completion 没有原生 `tool_calls` 契约，本适配器明确**拒绝** `tools` /
//! `functions` / 工具历史（见 `prompt.rs`），不用提示词模拟 —— 与 DeepSeek 网页
//! 同一口径。同理拒绝图片：v3 的图片要走上传 + references，当前未接，不能假装
//! 看得见。
//!
//! ── Pro 标记（上游的「默认浏览器」位）────────────────────────
//! `unique-uuid` 头第 5 位是「是否默认浏览器」，命中后解锁 Premium 模型
//! （Claude / GPT / Gemini）。本适配把账号上的 `unlockPro`（导入时默认开启）
//! 映射到这一位；关闭时如实填「非默认浏览器」（见 `sign::unique_uuid`）。
//!
//! ── 子模块分工 ──────────────────────────────────────────────
//! ```text
//!   sign.rs         签名三件套 / unique-uuid 指纹 / 摘要与编码（纯函数，可离线验）
//!   endpoints.rs    基址 / 路径 / 浏览器常规头 / 客户端常量
//!   credentials.rs  Cookie 与 JWT 的归一化（cURL / Cookie 串 / 裸 JWT 都认）
//!   http.rs         出站客户端 / sign-key 缓存 / 签名请求 / 房间缓存 / join + runs
//!   prompt.rs       messages 摊平成一段纯文本（字符预算裁剪）
//!   models.rs       远程目录（display_name → slug，发上游换回 display_name）
//!   chat.rs         会话式转发入口：建房间 → join → runs → 事件流 → OpenAI chunk
//!   usage.rs        额度（百分比制：只有「剩 X%」与重置时刻，没有积分包）
//!   checkin.rs      每日签到（`daily_sign_in`）
//!   adapter.rs      ProviderAdapter 实现（两个静态实例）
//! ```
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本目录零 unwrap/expect/panic，不持锁跨 await。

pub mod adapter;
pub mod chat;
pub mod checkin;
pub mod credentials;
pub mod endpoints;
pub mod http;
pub mod models;
pub mod prompt;
pub mod sign;
pub mod usage;

pub use adapter::{TabbitAdapter, TABBIT_ADAPTER, TABBIT_INTL_ADAPTER};

use crate::server::core::providers::ProviderKind;

/// 国内站 provider id（落盘契约，不能改）
pub const ID_CN: &str = "tabbit";
/// 国际站 provider id（落盘契约，不能改）
pub const ID_INTL: &str = "tabbit-intl";

/// 一次对话送上游的字符预算。
///
/// 实测：20000 字符成功、25000 必失败（上游对 `content` 有硬上限）。取 16000
/// 留出余量 —— 中文与 emoji 的字符数不等于上游的计数口径，余量比「卡线」安全。
pub const PROMPT_BUDGET: usize = 16_000;
/// 单条消息的字符上限
pub const PROMPT_SINGLE: usize = 9_000;
/// 系统指令合计的字符上限
pub const SYSTEM_BUDGET: usize = 5_000;
/// 单条消息裁剪时保留的头部字符数
pub const PROMPT_HEAD: usize = 6_000;
/// 单条消息裁剪时保留的尾部字符数
pub const PROMPT_TAIL: usize = 2_000;
/// 裁剪标记
pub const TRUNC_NOTE: &str = "\n…[内容过长，中间部分已省略]…\n";
/// 历史被丢弃时的说明
pub const HISTORY_NOTE: &str = "[注意：更早的对话历史因长度限制已省略]";

/// 桌面端版本号（`x-req-ctx` 的明文）。上游按 `{version}({build})` 解析，
/// 格式不对会判 493「版本过低」。可在账号上单独覆盖（`clientVersion`）。
pub const DEFAULT_CLIENT_VERSION: &str = "1.17.16(10117016)";

/// 上游前端固定的 Chrome 集成 client_id（`x-chrome-id-consistency-request`）
pub const CLIENT_ID: &str = "2dd8eb4c1ed9c344d173";

/// 默认模型名：请求没带 model、目录也拉不到时用它，让上游自己选。
pub const DEFAULT_MODEL: &str = "default";

/// 浏览器 UA（两站同一份 —— 上游按它判客户端形态）
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                             (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36";

/// `sec-ch-ua`（上游产品自己声明的品牌串）
pub const SEC_CH_UA: &str = r#""Tabbit Browser";v="149", "Chromium";v="149", "Not)A;Brand";v="24""#;

/// 房间（`chat_session_id`）缓存 TTL：上游房间 30 分钟量级
pub const SESSION_TTL_MS: i64 = 30 * 60 * 1000;
/// 签名 key 缓存 TTL（服务端会轮换；499 时强制刷新一次）
pub const SIGN_KEY_TTL_MS: i64 = 30 * 60 * 1000;
/// 模型目录缓存 TTL
pub const CATALOG_TTL_MS: i64 = 10 * 60 * 1000;

/// 上游站点。
///
/// 两站的 base / 签名兜底 key / 时区 / 登录方式各不相同，**账号也不通用** ——
/// 这是「拆成两家 provider」的判据（见模块头），不是同一个 provider 的地区字段。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Realm {
    /// 国内站 `web.tabbit.com`（有手机号验证码登录）
    Cn,
    /// 国际站 `web.tabbit.ai`（只能导入 Cookie）
    Intl,
}

impl Realm {
    /// 两个站点（遍历用；顺序 = 注册表顺序）
    pub const ALL: [Realm; 2] = [Realm::Cn, Realm::Intl];

    /// 落盘的 provider id
    pub fn id(self) -> &'static str {
        match self {
            Realm::Cn => ID_CN,
            Realm::Intl => ID_INTL,
        }
    }

    /// 界面上的一行字（与上游 `pack.json` 的 `label` 逐字一致）
    pub fn label(self) -> &'static str {
        match self {
            Realm::Cn => "Tabbit 国内版",
            Realm::Intl => "Tabbit 国际版",
        }
    }

    /// 站点基址（网页、账号接口、网关都是同一台机器）
    pub fn base(self) -> &'static str {
        match self {
            Realm::Cn => "https://web.tabbit.com",
            Realm::Intl => "https://web.tabbit.ai",
        }
    }

    /// `/chat/sign-key` 拉不到时的兜底 key。
    ///
    /// **两站不同，混用会被判签名错误**（不是「随便填一个也能过」）。
    pub fn fallback_sign_key(self) -> &'static str {
        match self {
            Realm::Cn => "3a9b1c8d5e7f4b209c2d1f4a6e7b8c9f",
            Realm::Intl => "f8d0e6a73f8d4b1a9c3d2e1f9a4b7c6d",
        }
    }

    /// 额度接口要的时区名。
    ///
    /// 只影响「什么时候重置」的展示，不影响百分比读数：国内站按上海、国际站
    /// 按 UTC（与上游对两站的默认时区一致）。
    pub fn timezone(self) -> &'static str {
        match self {
            Realm::Cn => "Asia/Shanghai",
            Realm::Intl => "UTC",
        }
    }

    /// 对应的 provider kind
    pub fn kind(self) -> ProviderKind {
        match self {
            Realm::Cn => ProviderKind::Tabbit,
            Realm::Intl => ProviderKind::TabbitIntl,
        }
    }

    /// provider kind → 站点
    pub fn from_kind(kind: ProviderKind) -> Option<Self> {
        match kind {
            ProviderKind::Tabbit => Some(Realm::Cn),
            ProviderKind::TabbitIntl => Some(Realm::Intl),
            _ => None,
        }
    }

    /// provider id → 站点
    pub fn from_provider_id(id: &str) -> Option<Self> {
        match id {
            ID_CN => Some(Realm::Cn),
            ID_INTL => Some(Realm::Intl),
            _ => None,
        }
    }

    /// 另一个站点（导入时提示「这可能是另一个地区的账号」用）
    pub fn other(self) -> Self {
        match self {
            Realm::Cn => Realm::Intl,
            Realm::Intl => Realm::Cn,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两个站点的身份、基址、签名 key 全不相同 —— 任一项相同就说明复制粘贴改漏了
    #[test]
    fn the_two_sites_share_nothing_that_must_differ() {
        assert_ne!(ID_CN, ID_INTL);
        assert_ne!(Realm::Cn.label(), Realm::Intl.label());
        assert_ne!(Realm::Cn.base(), Realm::Intl.base());
        assert_ne!(
            Realm::Cn.fallback_sign_key(),
            Realm::Intl.fallback_sign_key(),
            "兜底签名 key 必须按站取：混用会被上游判签名错误"
        );
        assert_ne!(Realm::Cn.timezone(), Realm::Intl.timezone());
    }

    /// id ↔ 站点 ↔ kind 三条互查必须闭环（漏一条就会出现「添加进去了但查不回来」）
    #[test]
    fn every_realm_round_trips_through_id_and_kind() {
        for realm in Realm::ALL {
            assert_eq!(Realm::from_provider_id(realm.id()), Some(realm));
            assert_eq!(Realm::from_kind(realm.kind()), Some(realm));
            assert_eq!(Realm::from_provider_id(realm.id()).map(Realm::kind), Some(realm.kind()));
        }
    }

    /// 基址是 https 且不带尾斜杠（拼接 `format!("{}{}", base, path)` 的前提）
    #[test]
    fn bases_are_https_without_trailing_slash() {
        for realm in Realm::ALL {
            let base = realm.base();
            assert!(base.starts_with("https://"), "{base}");
            assert!(!base.ends_with('/'), "{base}");
        }
    }
}
