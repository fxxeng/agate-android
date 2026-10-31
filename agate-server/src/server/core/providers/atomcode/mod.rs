//! AtomCode（AtomGit）：账号管理（**轮询式网页登录 + 粘贴令牌**）加推理转发。
//!
//! ── 上游长什么样 ────────────────────────────────────────────
//! 三条平面（细节见各子模块的模块头）：
//!
//! ```text
//!   登录代理（OAuth）  {broker}/auth/{login,check,token} + /oauth/refresh
//!   CodingPlan REST    {codingplan}/coding-plan/{status,models,claim,usage}-v2
//!   聊天网关           {gateway}/chat/completions             OpenAI Chat Completions
//! ```
//!
//! 本家**无状态**（一次 HTTP 请求 = 一次回答，上游就是 OpenAI 协议），
//! `is_stateful` 恒 false，转发链只需 `build_chat_request`。
//!
//! ── 四处与别家不同的取舍（都写在各自模块头，这里给索引）─────────
//!   1. **有续期，而且会轮换 refresh token**（`supports_refresh = true`）——
//!      续期必须单飞、必须写回新的一对，见 [`oauth`] 与 [`refresh`]；
//!   2. **登录没有本地回调**：授权在代理域名上终结，靠轮询判完成 ——
//!      与 ZCode / OfficeAce 同形，见 [`oauth`] 与 `core::login::atomcode`；
//!   3. **额度是「每窗口调用次数」**，不是积分也不是 token —— 见 [`usage`]；
//!   4. **网关会把失败包装成成功**：`HTTP 200` + 正文 `参数错误`
//!      （或一页上游 HTML 报错），见 [`errors`]。
//!
//! ── 登录形态：**轮询型**（`core::login::atomcode` 起后台任务按 3 秒问一次）──
//! 本家的授权回调由代理自己在 `acs.atomgit.com/callback` 终结，**永远不回到**
//! 本机，因此本模块**没有** `build_login_url` / `exchange_login_code` 这一对
//! 回调钩子（`supports_web_login` 保持默认 false，与 ZCode / OfficeAce 同一处置）：
//! 发起入口是 `LoginService::start_atomcode_login`，`api::session::login_start`
//! 在通用网页登录分支**之前**为它分流。把它标成「网页登录（回调型）」会让
//! 通用分支去等一个永远不来的回调 —— 症状是「授权成功、界面一直挂着」。
//!
//! ── 从插件形态搬来时改了什么（逐条对照，便于 parity 核对）────────
//! ```text
//!   不变：三处 body 改写（model 映射 / stream_options / 其余逐字）、
//!         四件功能性请求头（含功能性 UA）、判定顺序即契约的错误分类、
//!         档位推断与级联领取、续期的「非空才覆盖」合并规则、额度窗口归一
//!   改：  ① 凭证从 `store::record(ctx, ID, account)` 改成
//!          `store.atomcode_account_record(id)`（账号库由内核持有）
//!         ② 目录缓存只在本进程（内核的「来源 / 更新日期」列读
//!          [`models::remote_refreshed`] / [`models::last_refreshed_at`]）
//!         ③ 登录从「插件报 poll_interval_ms + 宿主轮询」改成
//!          「内核起后台任务轮询」，见 `core/login/atomcode.rs`
//!         ④ 轮询拿到令牌后直接落账号（插件返回 NormalizedAccount 交宿主落库）
//!         ⑤ 签到结果多带一个 `alreadyCompleted`（内核台账的完成判据，
//!          见 [`checkin`] 的模块头）
//! ```
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

pub use adapter::{AtomcodeAdapter, ATOMCODE_ADAPTER};

use serde_json::{json, Value};

use crate::server::errors::GatewayError;

use self::credentials::{pick, TokenBundle};

/// provider id（落盘契约，不能改）
pub const ID: &str = super::kind_id(super::ProviderKind::Atomcode);

/// 排障用的目录条数（`/health` 之类的探针会读）
#[allow(dead_code)]
pub fn model_count() -> usize {
    models::list().len()
}

/// 账号的稳定身份：`userId` → `username` → 空串（让调用方按令牌尾号兜底）。
///
/// AtomGit 的用户 id 是稳定不可变的（登录名可能改）。同一账号重复登录会被
/// 识别成更新而不是新增 —— 与内置版 `add_atomcode_account` 的去重口径一致。
pub fn identity_of(secret: &Value) -> String {
    let user_id = pick(secret, &["userId", "user_id", "uid"]);
    if !user_id.is_empty() {
        return user_id;
    }
    pick(secret, &["username", "userName"])
}

/// 账号的落盘 id：`atomcode-<身份>`；身份读不到时退回令牌尾号。
///
/// ⚠️ 与落账号路径（`account_store::atomcode_accounts`）**必须走同一个函数** ——
/// 两处分叉会让「同一个账号重新登录」变成「新增一条记录」（用户看到
/// 「我只有一个 AtomGit 账号，怎么越加越多」）。
pub fn account_id_of(secret: &Value) -> String {
    let identity = identity_of(secret);
    if !identity.is_empty() {
        return format!("{}-{identity}", ID);
    }
    let token = pick(secret, &["accessToken", "access_token", "token"]);
    format!(
        "{}-{}",
        ID,
        crate::server::core::account_store::store_util::token_tail_of(&token)
    )
}

/// 账号展示名：备注名（`name`）→ 登录名 → `AtomGit 账号 <userId>` → 兜底文案。
///
/// 「没有名字就报一串 id」对用户没有意义，但不给名字又会让多账号无法区分，
/// 所以末两档用的是「AtomGit 账号 + 能拿到的那个 id」。
pub fn display_name_of(secret: &Value) -> String {
    let name = pick(secret, &["name"]);
    if !name.is_empty() {
        return name;
    }
    let username = pick(secret, &["username", "userName"]);
    if !username.is_empty() {
        return username;
    }
    let user_id = pick(secret, &["userId", "user_id", "uid"]);
    if !user_id.is_empty() {
        return format!("AtomGit 账号 {user_id}");
    }
    "AtomCode 账号".to_string()
}

/// 令牌包 → 可落库的 secret（登录轮询成功后走这里）。
///
/// 字段名是**本项目落盘契约**（驼峰），与官方 `auth.toml` 的下划线名不同；
/// `credentials::from_record` 两种都认，但落盘只写驼峰一种，避免同一件事两种形态。
pub fn secret_from_bundle(bundle: TokenBundle) -> Value {
    let created_at = crate::server::logging::now_ms() / 1000;
    // 展示名要先算：`bundle` 的字段下面会被 move 进 JSON
    let name = credentials::display_name(&bundle);
    let mut secret = json!({
        "accessToken": bundle.access_token,
        "createdAt": created_at,
        "expiresIn": bundle.expires_in,
    });
    if !bundle.refresh_token.trim().is_empty() {
        secret["refreshToken"] = Value::String(bundle.refresh_token.clone());
    }
    if !bundle.user_id.trim().is_empty() {
        secret["userId"] = Value::String(bundle.user_id.clone());
    }
    if !bundle.username.trim().is_empty() {
        secret["username"] = Value::String(bundle.username.clone());
    }
    if !bundle.name.trim().is_empty() {
        secret["nickname"] = Value::String(bundle.name.clone());
    }
    if !bundle.email.trim().is_empty() {
        secret["email"] = Value::String(bundle.email.clone());
    }
    if !bundle.avatar_url.trim().is_empty() {
        secret["avatarUrl"] = Value::String(bundle.avatar_url.clone());
    }
    secret["name"] = Value::String(name);
    secret
}

/// 把粘贴的令牌包归一成可落库的 secret（`api::accounts` 的 AtomCode 分支调它）。
///
/// ── 为什么只认「粘贴令牌」而拒绝文件 / 桌面端 ──────────────────
/// AtomCode 的凭证是代理下发的一对令牌，既没有「客户端导出的凭证文件」这个
/// 约定（官方 `auth.toml` 是它自己 CLI 的落盘形态，字段与我们的记录不同），
/// 也没有可实时读取的本机登录态。硬凑出来的实现只会在用户点下去之后失败，
/// 现在就说清「请用浏览器登录」反倒省事 —— 与上游插件 `normalize_account`
/// 对 `FileBase64` / `Desktop` 的两种回绝逐字同口径。
///
/// 接受三种形态（都是同一件事的不同叫法，见 `from_record` 的取值链）：
///   - `{accessToken, refreshToken?}`：面板粘贴的标准形态；
///   - `{access_token, refresh_token}`：官方 `auth.toml` / 登录输出的形态；
///   - `{token}`：手工从别处抄来的一枚访问令牌（没有 refresh 则能转发但不能续期）。
pub fn normalize_paste(payload: &Value) -> Result<Value, GatewayError> {
    let access_token = pick(payload, &["accessToken", "access_token", "token"]);
    if access_token.is_empty() {
        return Err(GatewayError::with_status(
            400,
            "缺少 accessToken（请粘贴浏览器登录后的登录输出，或直接粘贴访问令牌）",
        ));
    }
    let mut secret = payload.clone();
    if !secret.is_object() {
        return Err(GatewayError::with_status(400, "请求体不是 JSON 对象"));
    }
    if let Some(object) = secret.as_object_mut() {
        object.insert(
            "accessToken".to_string(),
            Value::String(access_token.clone()),
        );
        // token_type 恒 Bearer，不落（没有消费方）
        object.remove("token_type");
        object.remove("tokenType");
        // `provider` / `importDesktop` 这些是**添加请求**的字段，不是账号字段：
        // 留在 secret 里会被原样写进账号库（公开形态也会把它带出去）
        object.remove("provider");
        object.remove("importDesktop");
    }
    // 备注名统一放 `name`（`display_name_of` 的取值链首项）。
    // 先算再插：`display_name_of` 读的是归一后的 secret（`accessToken` 已改名），
    // 而这一步在可变借用结束之后 —— 不能边借边读。
    let explicit_name = pick(payload, &["name"]);
    let name = if explicit_name.is_empty() {
        display_name_of(&secret)
    } else {
        explicit_name
    };
    if let Some(object) = secret.as_object_mut() {
        object.insert("name".to_string(), Value::String(name));
    }
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// provider id 与注册表一致（落盘契约）
    #[test]
    fn id_matches_register() {
        assert_eq!(ID, "atomcode");
        assert_eq!(
            super::super::kind_from_id(ID),
            Some(super::super::ProviderKind::Atomcode)
        );
    }

    /// 身份优先级：userId → username；落盘 id 带 `atomcode-` 前缀
    #[test]
    fn identity_prefers_user_id_and_ids_are_prefixed() {
        assert_eq!(identity_of(&json!({"userId": "42", "username": "qq_1"})), "42");
        assert_eq!(identity_of(&json!({"username": "qq_1"})), "qq_1");
        assert_eq!(identity_of(&json!({})), "");
        assert_eq!(account_id_of(&json!({"userId": "42"})), "atomcode-42");
        // 身份读不到时按令牌尾号兜底（不会退化成一条空 id）
        let tailed = account_id_of(&json!({"accessToken": "0123456789abcdef"}));
        assert!(tailed.starts_with("atomcode-"));
        assert!(tailed.ends_with("cdef"));
    }

    /// 展示名取值链：备注名 → 登录名 → `AtomGit 账号 <id>` → 兜底
    #[test]
    fn display_name_chain() {
        assert_eq!(display_name_of(&json!({"name": "我的号"})), "我的号");
        assert_eq!(display_name_of(&json!({"username": "qq_1"})), "qq_1");
        assert_eq!(
            display_name_of(&json!({"userId": "42"})),
            "AtomGit 账号 42"
        );
        assert_eq!(display_name_of(&json!({})), "AtomCode 账号");
    }

    /// 粘贴归一：三种令牌名都认；缺令牌 400；请求字段不落库
    #[test]
    fn paste_normalization_accepts_alias_chain() {
        let normalized = normalize_paste(&json!({
            "provider": "atomcode",
            "accessToken": " at ",
            "refreshToken": "rt",
            "userId": "42",
            "username": "qq_1"
        }));
        let secret = normalized.ok().unwrap_or(Value::Null);
        assert_eq!(
            secret.get("accessToken").and_then(Value::as_str),
            Some("at")
        );
        assert_eq!(
            secret.get("refreshToken").and_then(Value::as_str),
            Some("rt")
        );
        // 添加请求的字段不能进账号库
        assert!(secret.get("provider").is_none());
        assert_eq!(
            secret.get("name").and_then(Value::as_str),
            Some("qq_1")
        );
        // 官方形态（下划线）也认，并统一成驼峰落盘
        let snake = normalize_paste(&json!({"access_token": "b"})).ok().unwrap_or(Value::Null);
        assert_eq!(snake.get("accessToken").and_then(Value::as_str), Some("b"));
        assert!(snake.get("access_token").is_none());
        assert_eq!(
            normalize_paste(&json!({"provider": "atomcode"}))
                .err()
                .map(|error| error.status_code),
            Some(400)
        );
    }

    /// 令牌包 → secret：空字段不落（避免把空串写进账号库骗过 canRefresh 类判定）
    #[test]
    fn bundle_to_secret_skips_empty_fields() {
        let secret = secret_from_bundle(TokenBundle {
            access_token: "at".to_string(),
            expires_in: 604_800,
            user_id: "42".to_string(),
            ..Default::default()
        });
        assert_eq!(secret.get("accessToken").and_then(Value::as_str), Some("at"));
        assert_eq!(secret.get("expiresIn").and_then(Value::as_i64), Some(604_800));
        assert_eq!(secret.get("userId").and_then(Value::as_str), Some("42"));
        assert!(secret.get("refreshToken").is_none());
        assert!(secret.get("email").is_none());
        // 没有 username / name 时展示名退回「AtomGit 账号 42」
        assert_eq!(
            secret.get("name").and_then(Value::as_str),
            Some("AtomGit 账号 42")
        );
    }
}
