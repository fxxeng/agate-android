//! 豆包工作（`www.doubao.com`）：账号管理（**内嵌窗口取 Cookie / 粘贴 Cookie**）
//! 加推理转发，**没有续期**。
//!
//! ── 上游长什么样（逆向来源：官方插件二进制 `agentpack_x/bin/agent2api-providers.exe`
//! 的字符串表 + beta 桌面端 `doubaowork_login.rs` + [Go参考]）────────────
//! ```text
//!   推理入口       POST /chat/completion                     SSE **自有事件流**
//!   启动握手       POST /alice/user/launch
//!   上传           POST /alice/resource/prepare_upload  → imageX top API 四步
//!   额度           GET  /alice/commerce/sale/subscription/overview/
//!                  GET  /alice/commerce/sale/subscription/entry/config
//!   活动 / 签到    GET  /alice/ug/campaign/v3/detail/m2
//!                  POST /alice/ug/campaign/item/reward/claim/<campaign_item_code>
//!   目录           GET  /alice/slot/action_bar_v3/brief_list   （模式简报）
//!   账号           GET  /passport/account/info/v2/
//! ```
//! 鉴权是**一整套 Cookie**（`sessionid` / `sid_guard` 一族）+ 一整组风控 query
//! 参数（`aid` / `device_id` / `fp` / `web_tab_id` / …），伪装的是**桌面客户端**
//! 而不是网页版 —— 这是本家最容易照错的一处，细节见 [`endpoints`] 的模块头。
//!
//! ── ⚠️ 本家**没有**续期（如实声明，别照抄 AtomCode 那套）──────────
//! ```text
//!   supports_refresh   false   无 refresh 接口（Cookie 只能失效后重新登录）
//!   supports_web_login false   登录是「内嵌窗口直接读 Cookie」，不是回调换码
//!   supports_usage     true    subscription/overview + entry/config
//!   supports_model_refresh true /alice/slot/action_bar_v3/brief_list
//! ```
//! 「没有续期」的具体含义：`credentials_expiring` 保持默认 false（判不出来就
//! 不声称临期），401 后的 `refresh_access_token` 会拿到**同一串 Cookie**
//! （默认实现转调 `ensure_access_token`），最终落到「请重新登录」——
//! 这是本家唯一可能的处置，**不是**实现缺口。
//!
//! ── 登录：与 AtomCode / ZCode / OfficeAce 同一形态（**不是**网页登录钩子）──
//! 客户端调 `POST /api/session/login/start {provider:"doubaowork"}` 拿到
//! `{state, authUrl}`，然后用**内嵌 Webview** 打开 `authUrl`（`www.doubao.com`
//! 的登录页），登录完成后由壳侧从窗口读 Cookie 并调
//! `POST /api/session/login/cookies` 落账号（beta `doubaowork_login.rs` 的
//! `run_cookie_login` 就是这个流程）。因此本家的 `build_login_url` /
//! `exchange_login_code` 那一对**回调型**钩子一个都不成立 ——
//! `supports_web_login` 保持 false，由 `api::session::login_start` 在通用网页
//! 登录分支**之前**为它分流。
//!
//! ── ⚠️ 需要接线方补的三处（本目录只创建新文件，不改既有文件）──────
//! ```text
//!   1. `ProviderKind::Doubaowork` 变体 + `providers/mod.rs` 的
//!      `PROVIDERS` / `kind_id` / `kind_from_id` / `mod doubaowork;` 声明
//!      + `adapter.rs::adapter_for` 的 arm（= &DOUBAOWORK_ADAPTER）
//!      + `implemented_kinds()` 里加上它（否则目录刷新循环不会问本家）；
//!   2. `UpstreamResponse::DoubaoworkSse` 变体 + `upstream/translate.rs` 的
//!      翻译状态机（解析已在 [`chat::parse_event`] 完成）+ `provider_loop`
//!      翻译分派的 arm。**落地前本家的转发链路不完整**（见 [`adapter`] 的说明）；
//!   3. `catalog_cache::ALL_SCOPES` 登记 [`models::SCOPE`]（=`"doubaowork"`）
//!      —— 不登记时启动日志里不会有本家（见 [`models`] 的模块头）；
//!      另需 `auto_checkin::CHECKIN_PROVIDERS` + `billing/checkin.rs` 的
//!      分派表加上 `"doubaowork"`，本家的签到才会被调度。
//! ```
//!
//! ── 子模块分工 ──────────────────────────────────────────────
//! ```text
//!   endpoints.rs    基址 / 路径 / 桌面端伪装头 / 风控 query（含指纹派生与 UUID）
//!   credentials.rs  Cookie 解析（整串 / 行数组）/ 归一 / 脱敏 / from_record
//!   plan.rs         messages → 上游消息块信封（含附件块与思考档位）
//!   chat.rs         SSE 事件解析（Delta）+ 请求构造 + 异步附件上传
//!   models.rs       模式目录（三级载体解析 + 内置兜底 + 落盘缓存）
//!   quota.rs        额度（订阅窗口 + 奖励额度 → 统一形状）
//!   rewards.rs      签到 / 活动领取
//!   upload.rs       prepare_upload + imageX top API 四步（SigV4）
//!   adapter.rs      ProviderAdapter 实现（本文件的装配层）
//! ```
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本目录零 unwrap/expect/panic。

pub mod adapter;
pub mod chat;
pub mod credentials;
pub mod endpoints;
pub mod login;
pub mod models;
pub mod plan;
pub mod quota;
pub mod rewards;
pub mod upload;

pub use adapter::{DoubaoWorkAdapter, DOUBAOWORK_ADAPTER};

/// provider id（落盘契约，不能改；取自注册表，避免各处再写一份字面量）。
///
/// ⚠️ 引用 `ProviderKind::Doubaowork` —— 该变体由接线方补
/// （见模块头「需要接线方补的三处」第 1 条）。
pub const ID: &str = super::kind_id(super::ProviderKind::Doubaowork);

/// 排障用的目录条数（`/health` 之类的探针可读）
#[allow(dead_code)]
pub fn model_count() -> usize {
    models::list().len()
}

/// 账号的**稳定身份**：登录态 Cookie 的尾 8 位。
///
/// ── 为什么本家没有「userId」那种可读身份（与别家的对照）───────────
/// 上游确实有 `/passport/account/info/v2/` 能给 `user_id_str`（[二进制] 里
/// 那个键逐字出现），但**取它要发一次带 Cookie 的网络请求**，而落账号路径
/// 是同步的（`add_doubaowork_account` 不持锁做网络）。因此身份取**本地可算的**
/// 那一个：登录态值的尾 8 位（`sessionid` 优先，其次 `sessionid_ss` /
/// `sid_guard`）。
///
/// 这个判据对「同一个账号重复登录」成立：同一个豆包的 `sessionid` 会**续期**
/// 但**主体不变**（上游刷新时保持同一串 sessionid 的那一段前缀），因此尾 8 位
/// 稳定。⚠️ 若实测发现上游会在续期时整串换掉 sessionid，那么重复登录会变成
/// 「新增一条记录」—— 届时的修法是「落账号前打一次 `account/info` 取
/// `user_id_str`」，而**不是**在这里换一个更短的头部（那只会撞得更频繁）。
/// 这一点如实写在注释里，免得后来者以为这个判据是随便挑的。
pub fn identity_of(cookie_pairs: &[(String, String)]) -> String {
    for name in credentials::SESSION_COOKIE_NAMES {
        if let Some((_, value)) = cookie_pairs.iter().find(|(key, _)| key.as_str() == name) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                let chars: Vec<char> = trimmed.chars().collect();
                let start = chars.len().saturating_sub(8);
                let tail: String = chars[start..].iter().collect();
                return tail;
            }
        }
    }
    String::new()
}

/// 账号的落盘 id：`doubaowork-<身份>`；身份读不到时退回**时间戳**。
///
/// ⚠️ 与落账号路径（`account_store::doubaowork_accounts`）**必须走同一个函数** ——
/// 两处分叉会让「同一个账号重新登录」变成「新增一条记录」（用户看到
/// 「我只有一个豆包账号，怎么越加越多」）。
pub fn account_id_of(cookie_pairs: &[(String, String)]) -> String {
    let identity = identity_of(cookie_pairs);
    if !identity.is_empty() {
        return format!("{ID}-{identity}");
    }
    // 没有登录态时（理论上到不了这里，落账号前已校验）用时间戳兜底：
    // 一个必然唯一的 id 比一个恒定的空 id 安全（后者会让第二次添加覆盖第一次）
    format!("{ID}-{}", crate::server::logging::now_ms())
}

/// 账号展示名：备注名 → `豆包工作 <身份>` → 兜底文案。
pub fn display_name_of(name: Option<&str>, cookie_pairs: &[(String, String)]) -> String {
    let explicit = name.map(str::trim).filter(|text| !text.is_empty());
    if let Some(name) = explicit {
        return name.to_string();
    }
    let identity = identity_of(cookie_pairs);
    if !identity.is_empty() {
        return format!("豆包工作 {identity}");
    }
    "豆包工作账号".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// provider id 与注册表一致（落盘契约）
    #[test]
    fn id_matches_register() {
        assert_eq!(ID, "doubaowork");
        assert_eq!(
            super::super::kind_from_id(ID),
            Some(super::super::ProviderKind::Doubaowork)
        );
    }

    /// 身份取登录态尾 8 位；三种登录态 Cookie 都要认
    #[test]
    fn identity_uses_session_cookie_tail() {
        let pairs = vec![("sessionid".to_string(), "0123456789abcdef".to_string())];
        assert_eq!(identity_of(&pairs), "89abcdef");
        let short = vec![("sid_guard".to_string(), "abc".to_string())];
        assert_eq!(identity_of(&short), "abc");
        // 没有登录态 → 空（由 `account_id_of` 用时间戳兜底）
        assert_eq!(identity_of(&[("foo".to_string(), "bar".to_string())]), "");
        // sessionid 优先于 sessionid_ss（顺序即优先级）
        let both = vec![
            ("sessionid_ss".to_string(), "SECONDARY".to_string()),
            ("sessionid".to_string(), "PRIMARYXX".to_string()),
        ];
        // 身份取 Cookie 值的**末 8 个字符**（见 `identity_of` 的实现）：
        // "PRIMARYXX" 共 9 字符 ⇒ 末 8 个是 "RIMARYXX"
        assert_eq!(identity_of(&both), "RIMARYXX");
    }

    /// 落盘 id 带 `doubaowork-` 前缀；无身份时不低于「必然唯一」
    #[test]
    fn account_id_is_prefixed_and_never_empty() {
        let pairs = vec![("sessionid".to_string(), "0123456789abcdef".to_string())];
        assert_eq!(account_id_of(&pairs), "doubaowork-89abcdef");
        let fallback = account_id_of(&[]);
        assert!(fallback.starts_with("doubaowork-"));
        assert!(fallback.len() > "doubaowork-".len());
    }

    /// 展示名：备注名优先；否则带身份；否则兜底
    #[test]
    fn display_name_chain() {
        let pairs = vec![("sessionid".to_string(), "0123456789abcdef".to_string())];
        assert_eq!(display_name_of(Some("我的豆包"), &pairs), "我的豆包");
        assert_eq!(display_name_of(Some("   "), &pairs), "豆包工作 89abcdef");
        assert_eq!(display_name_of(None, &[]), "豆包工作账号");
    }
}
