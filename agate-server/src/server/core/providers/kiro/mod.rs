//! Kiro（kiro.dev）适配器：**会话式转发** + 三种凭证来源。
//!
//! ── 上游是什么 ──────────────────────────────────────────────
//! Kiro 是 AWS 的 AI IDE 订阅（Free / Pro / Pro+ / Power）。它**没有**
//! OpenAI 兼容 API：模型语义是 Anthropic Messages，传输却是自家
//! `generateAssistantResponse` 的会话 JSON + **AWS event stream**（二进制帧）。
//! 上游 3.4.1 的插件包把它声明成 `stateful: true`，理由与我们这边的判断一致：
//! `UpstreamKind` 是封闭枚举，没有这个协议，由适配器自己读帧、翻成标准 chat SSE
//! 是唯一不扩协议的路。
//!
//! ── 不拆家（与 WorkBuddy / Qoder / Tabbit 的处置不同）─────────
//! Kiro 只有两个**订阅地区**（`us-east-1` / `eu-central-1`），两地共用同一套
//! 账号体系、同一份令牌，只是端点主机不同 —— 地区从 profile ARN 里读，读不到
//! 才按登录地区兜底。所以它是**一家 provider**，不是两家。
//!
//! ── 能力与「没有的东西」────────────────────────────────────
//! ```text
//!   有：对话（流式 / 工具调用 / 思考 / 图片）、凭证续期、在线模型目录、余额
//!   无：每日签到、领取类活动（上游确实没有这两类）
//! ```
//! 最后一条决定它**不进** `auto_checkin::CHECKIN_PROVIDERS`：登记一家没有签到
//! 链路的 provider，只会让签到中心多一行永远「不支持」的账号。
//!
//! ── 三种凭证来源 ────────────────────────────────────────────
//! ```text
//!   apikey      ksk_…（官方 API key，tokentype: API_KEY，不续期）
//!   oauth       登录换到的 access/refresh（social / idc / external-idp）
//!   本机登录态   kiro-cli 的 SQLite / Kiro IDE 的 kiro-auth-token.json
//!               —— 账号里只记来源，token 每次实时读（与 Raccoon 同一理由：
//!               客户端会自己续期，落快照会让网关一直拿着旧 token）
//! ```
//! Android 上没有 `kiro-cli` 也没有 Kiro IDE，所以第三个来源在本项目**不接**
//! （不移植 `local.rs`）：给了入口只会稳定报「本机没有登录态」。
//!
//! ── 子模块分工 ──────────────────────────────────────────────
//! ```text
//!   endpoints    地址、地区判定、ARN 解析、请求头口径（纯函数）
//!   credentials  凭证读/写/公开视图/身份（纯，无网络）
//!   errors       状态码与文案的四档映射（含「输入过长」的透传）
//!   eventstream  AWS event stream 二进制分帧（纯，可离线测）
//!   events       帧 → 中立增量事件（正文/思考/工具/用量/停止）
//!   request      OpenAI Chat → Kiro 会话 JSON
//!   models       在线模型目录 + 上下文窗口表
//!   usage        余额（`GetUsageLimits`）
//!   refresh      OAuth 续期（社交登录 / AWS OIDC / 外部 IdP）
//!   chat         会话式转发（读帧 → 写 SSE）
//!   adapter      ProviderAdapter 的实现
//! ```
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本目录零 unwrap/expect/panic。

pub mod credentials;
pub mod endpoints;
pub mod errors;
pub mod eventstream;

/// provider id（与内核 `PROVIDERS` 注册表、客户端 `PresetRegistry` 同一字面量）
pub const ID: &str = "kiro";

/// 默认地区（账号记录里没写地区时）
pub const DEFAULT_REGION: &str = "us-east-1";

/// 请求前「令牌快到期」的提前量：不足这么久就先刷一次
/// （上游 IDE 的口径是 2 分钟，见 `refresh.rs` 的模块头）
pub const EARLY_REFRESH_MS: i64 = 2 * 60 * 1000;

/// 后台续期的提前量（网关看 `token_expiring` 用它）
pub const LEAD_REFRESH_MS: i64 = 10 * 60 * 1000;

/// 单次对话允许的最长等待（含排队）：超过就按超时收尾
pub const STREAM_TIMEOUT_MS: u64 = 10 * 60 * 1000;

/// 控制面请求（目录 / 余额）的超时
pub const CONTROL_TIMEOUT_MS: u64 = 30 * 1000;

/// Kiro 门户（浏览器登录入口；本项目**不走**网页登录，保留用于文案）
pub const PORTAL: &str = "https://app.kiro.dev";

/// 社交登录换码服务：**与地区无关**（门户固定打 us-east-1 的 auth 服务）
pub const AUTH_SERVICE: &str = "https://prod.us-east-1.auth.desktop.kiro.dev";

/// Kiro IDE / kiro-cli 自报的桌面端 UA（社交登录刷新时用）
pub const DESKTOP_UA: &str = "Kiro-Desktop/0.2.13 (darwin; arm64)";

/// 官方 IDE 在登录页上申请的 scopes（AWS OIDC 换码时逐条带上）
pub const SCOPES: [&str; 5] = [
    "codewhisperer:completions",
    "codewhisperer:analysis",
    "codewhisperer:conversations",
    "codewhisperer:transformations",
    "codewhisperer:taskassist",
];

/// Builder ID 的 Start URL 与 profile（Builder ID 没有自己的 profile，
/// 官方 IDE 与 kiro-cli 都用这一个固定值）
pub const BUILDER_ID_START: &str = "https://view.awsapps.com/start";
pub const BUILDER_ID_PROFILE: &str =
    "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX";

/// 镜像式 `User-Agent`：`KiroIDE-<版本>-<hash>`，hash 由主机名派生。
///
/// ── 为什么要派生而不是写死 ──────────────────────────────────
/// 上游控制面按 UA 判「是不是官方 IDE 在调」，同一串 UA 从成百上千个出口
/// 发出去是明显的异常特征。派生只改 hash、不改版本段，因此不影响识别。
pub fn ide_user_agent(hostname: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"agate:");
    hasher.update(hostname.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("KiroIDE-1.1.70-{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UA 的形态：版本段固定、hash 段由主机名派生且稳定
    #[test]
    fn the_user_agent_is_stable_per_host() {
        let first = ide_user_agent("agate-host");
        assert!(first.starts_with("KiroIDE-1.1.70-"), "{first}");
        assert_eq!(first, ide_user_agent("agate-host"));
        assert_ne!(first, ide_user_agent("other-host"));
        // 64 位十六进制摘要，与官方 IDE 的形态一致
        let tail = first.trim_start_matches("KiroIDE-1.1.70-");
        assert_eq!(tail.len(), 64);
        assert!(tail.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    /// 没有签到链路 ⇒ 绝不能进 CHECKIN_PROVIDERS（这条是回归护栏）
    #[test]
    fn kiro_never_claims_a_checkin_chain() {
        assert!(
            !crate::server::core::auto_checkin::CHECKIN_PROVIDERS.contains(&ID),
            "Kiro 上游没有签到 / 领取类活动：登记进签到清单只会多出一行永远「不支持」的账号"
        );
    }

    /// 续期窗口：请求前 2 分钟、后台 10 分钟（后台必须更早，否则轮不到它）
    #[test]
    fn the_refresh_windows_are_ordered() {
        assert!(LEAD_REFRESH_MS > EARLY_REFRESH_MS);
    }
}
