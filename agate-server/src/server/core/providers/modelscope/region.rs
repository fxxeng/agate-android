//! ModelScope（魔搭）的**地区**：两套域名、两条 provider 身份。
//!
//! ── 上游形态（2026-10 实测，来自上游 3.3 插件实现 `modelscope/region.rs`）──
//! ```text
//!              推理（OpenAI 兼容）                        网页站（身份 / 魔粒 / 签到）
//!   国内  https://api-inference.modelscope.cn/v1   https://modelscope.cn
//!   国际  https://api-inference.modelscope.ai/v1   https://www.modelscope.ai
//! ```
//!
//! ── 两条实测事实（决定了本家的整个形状）───────────────────────
//!   1. **一个 `ms-` 令牌两用**：同一串既作 `Authorization: Bearer` 调推理，
//!      又作 `Cookie: m_session_id=<key>` 调网页站的身份 / 魔粒 / 签到 ——
//!      两条链路实测都 200。所以本家**只有一个凭证字段**。
//!   2. **两个站点的账号体系分开**：国内站的令牌打国际站的身份接口得到
//!      `401 {"Code":10019901001,"Message":"access forbidden"}`，反之同理
//!      （而推理端点两地都能用同一个令牌打通）。因此「地区」不是标签而是
//!      **身份** —— 签到与魔粒必须打对站点。
//!
//! ── 为什么拆成两家 provider 而不是账号上的一个字段 ──────────────
//! 地区做成账号字段会让两个站的模型清单互相覆盖（单槽缓存），账号也无法按站
//! 隔离。拆开后各家有独立的账号、清单、启停与缓存槽 —— 与本仓 AutoClaw /
//! Qoder / MonkeyCode 的拆家口径一致。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic，且是纯常量。

use super::super::ProviderKind;

/// ModelScope 的地区。
///
/// 声明顺序 = 注册表顺序（国内版在前）：`ALL` 的遍历顺序决定界面上两家的先后。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Region {
    /// 国内站（`modelscope.cn`；provider id `modelscope`）
    #[default]
    Cn,
    /// 国际站（`www.modelscope.ai`；provider id `modelscope-intl`）
    Intl,
}

impl Region {
    /// 两个地区（顺序即注册表顺序）
    pub const ALL: [Region; 2] = [Region::Cn, Region::Intl];

    /// 本地区的 provider id（落盘契约，不能改）
    pub fn provider_id(self) -> &'static str {
        match self {
            Region::Cn => "modelscope",
            Region::Intl => "modelscope-intl",
        }
    }

    /// 展示名里跟在 `ModelScope` 后面的那一段
    pub fn label(self) -> &'static str {
        match self {
            Region::Cn => "国内版",
            Region::Intl => "国际版",
        }
    }

    /// 推理基址（**含** `api-inference` 主机与尾部 `/v1`）。
    ///
    /// `chat/completions` 与 `models` 都拼在它后面 —— 别处不要再写域名。
    pub fn inference_base_url(self) -> &'static str {
        match self {
            Region::Cn => "https://api-inference.modelscope.cn/v1",
            Region::Intl => "https://api-inference.modelscope.ai/v1",
        }
    }

    /// 网页站基址（身份 / 魔粒 / 今日用量 / 签到都在这个域下，**不带尾部斜杠**）
    pub fn site_base_url(self) -> &'static str {
        match self {
            Region::Cn => "https://modelscope.cn",
            Region::Intl => "https://www.modelscope.ai",
        }
    }

    /// 本地区账号 id 的前缀。
    ///
    /// ── 为什么两地带前缀（而不是共用 `modelscope-`）─────────────
    /// 两站的身份 id 空间互不相通，但**同一个人在两地的 `HavanaId` 完全可能
    /// 相同**（各自注册，服务端 id 生成规则同形就会撞）。撞了的话账号层的
    /// 跨 provider 撞 id 保护会拒绝，「两地账号并存」就不可能 —— 而那正是
    /// 拆家要支持的事。
    pub fn account_id_prefix(self) -> &'static str {
        match self {
            Region::Cn => "modelscope-",
            Region::Intl => "modelscope-intl-",
        }
    }
}

/// `ProviderKind` → 地区（只对这两家返回 `Some`）
pub fn region_of_kind(kind: ProviderKind) -> Option<Region> {
    match kind {
        ProviderKind::Modelscope => Some(Region::Cn),
        ProviderKind::ModelscopeIntl => Some(Region::Intl),
        _ => None,
    }
}

/// provider id → 地区（账号层与签到分派用：分派键是 id 而不是 kind）
pub fn region_of_provider_id(id: &str) -> Option<Region> {
    match id {
        "modelscope" => Some(Region::Cn),
        "modelscope-intl" => Some(Region::Intl),
        _ => None,
    }
}
