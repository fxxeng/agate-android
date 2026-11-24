//! AutoClaw（智谱 autoglm）适配器的**凭证层**（Agate 二期 T-c1；
//! 架构文档 §10.1）。
//!
//! ── 本目录当前交付了什么 ────────────────────────────────────
//! 本目录是 AutoClaw 适配器的**全部实现**（T-c1 凭证层 + T-c2 适配器接线）：
//!
//!   crypto.rs       Electron safeStorage 解密（DPAPI + AES-256-GCM）+ JWT 声明解码
//!                   移植来源：`D:\APP\AutoClaw\autoclaw-local-proxy\crypto-helper.mjs`
//!   credentials.rs  凭证来源（auth.json 解密 / openclaw.json 明文 /
//!                   环境变量 / 账号记录）+ mtime 缓存，
//!                   移植来源：`autoclaw-local-auth.mjs`
//!   refresh.rs      token 刷新（单飞 + 400002 降级重试；**只读不回写**），
//!                   移植来源：`autoclaw-upstream-client.mjs` 的刷新链
//!   models.rs       模型路由表（静态映射 + `zai_auto` 回退规则 + **远程目录**
//!                   的读取侧），移植来源：`autoclaw-models.mjs`
//!   catalog.rs      模型目录的**远程拉取**（`GET .../proxy/autoclaw-model-config`），
//!                   移植来源：AutoClaw 桌面端 `app.asar` 的 `/out/main/index.js`
//!   adapter.rs      **ProviderAdapter 实现**（T-c2）：请求构造（双模型标识头 +
//!                   X-Authorization）/ 错误分类 / 凭证 / SSE model 回写声明，
//!                   移植来源：`autoclaw-upstream-client.mjs` 的转发链
//!   prompt.rs       出站 **system 提示规范化**：前置 OpenClaw 身份前缀 + 改写
//!                   外来 harness 身份句（上游 2026-09-22 起的 system 白名单，
//!                   国内 / 国际版同闸，实测表在该文件头）
//!   balance.rs      积分钱包 + 订阅信息查询（移植来源 `account-balance.mjs`）
//!   checkin.rs      每日签到 + 活动福利的触发与到账确认（**逆向**：老项目
//!                   没有这条链路，接口从 AutoClaw 桌面端的 app.asar 里读出
//!                   并实测确认；活动福利的「登录」触发走 user-info，见该文件头）
//!   promotion.rs    活动福利到账确认（**逆向 + 实测**：Weekly Perks / 登录奖励
//!                   这类活动没有领取接口、由服务端在「登录活跃」时自动入账，
//!                   这里只读积分流水把今天的到账状态读回来，见该文件头）
//!   oauth.rs        **国际版**的 OAuth 网页登录（Zai / Google）：服务端那两跳
//!                   （取授权地址 / 用码换凭证）。浏览器那一半 —— 强制风控
//!                   验证码 —— 在 `ui/autoclaw-oauth.js`（主窗口里跑阿里云 SDK）
//!   callback_server.rs  那条链的回调**必须**落在 z.ai 登记过的那四个端口上
//!                   （白名单只认它们，见该文件头），这里临时占用其中一个
//!                   并把回调转回网关自己的回调路由
//!
//! ── 接线现状（T-c2 已落地）──────────────────────────────────
//! `adapter_for(AutoClaw)` 返回 [`adapter::AUTOCLAW_ADAPTER`]，`implemented_kinds()`
//! 也已列入本家（过渡期用过的占位适配器 `pending::AUTOCLAW_PENDING` 随 W6 的
//! `pending.rs` 删除而彻底退场）。
//! 账号 API 与旧数据导入在 `core::account_store` 的 `autoclaw_accounts.rs` /
//! `autoclaw_import.rs`（模式与另外三家一致）。
//!
//! ── 与另外三家的一处分歧（核对结论，见 `adapter.rs` 模块头）──
//! 认证头是 **`X-Authorization`**（源实现 `modelProxyUpstreamHeaders`），
//! 不是 §10 规格文字里写的 `Authorization` —— 以源实现为准。
//!
//! ── 公开 API 一览（转发与账号层直接用，不必再翻实现）────────────
//! ```text
//! credentials::DESKTOP_ACCOUNT_ID      "desktop-auth"（凭证对象的 id，见下）
//! credentials::DEFAULT_UPSTREAM_BASE_URL
//! credentials::upstream_base_url()     ← 含 AUTOCLAW_UPSTREAM_BASE_URL 覆盖
//! credentials::userapi_base_url()      ← 刷新接口域
//! credentials::desktop_auth_file()     Option<PathBuf>（非 Windows = None）
//! credentials::gateway_config_file()   PathBuf
//! credentials::snapshot_for(record: Option<&Value>) -> Result<AutoClawCredentials, GatewayError>
//! credentials::local_credentials()     -> Result<AutoClawCredentials, GatewayError>
//! credentials::env_credentials()       -> Option<AutoClawCredentials>
//! credentials::credentials_from_record(&Value) -> Result<AutoClawCredentials, GatewayError>
//! credentials::local_summary()         -> Result<Value, String>（无 token，账号导入/状态展示）
//! refresh::refresh(&AutoClawCredentials, force: bool) -> Result<AutoClawCredentials, GatewayError>
//! refresh::ensure_fresh(&AutoClawCredentials) -> Result<AutoClawCredentials, GatewayError>
//! credentials::AutoClawCredentials { id, token, refresh_token, device_id,
//!                                    user_id, expires_at, origin, cache_key }
//! credentials::AutoClawCredentials::is_expiring() / can_refresh()
//! credentials::CredentialOrigin::{DesktopAuthFile, GatewayConfig, AccountStore, Environment}
//! models::resolve_model_route(raw: &str) -> ModelRoute { route_model_id, body_model_id, requested_model }
//! models::list()                       -> Vec<Value>（聚合层认的上游原始形态）
//! models::default_route()              -> String（AUTOCLAW_DEFAULT_ROUTE 可覆盖）
//! models::strip_route_prefix(&str)     -> &str
//! crypto::os_crypt_aes_key(&Path) -> Result<Vec<u8>, String>
//! crypto::decrypt_enc_value(&str, Option<&[u8]>) -> Result<String, String>
//! crypto::decode_jwt_claims(&str) -> Option<Value>
//! crypto::strip_bearer(&str) -> String
//! adapter::AUTOCLAW_ADAPTER           静态适配器实例（`adapter_for` 返回它）
//! prompt::normalize(&mut Value)       出站前规范化 system 提示（就地改 body）
//! prompt::IDENTITY_LINE               上游要求的身份句（幂等判定用）
//! balance::query_usage(&AccountStore, &str) -> Result<Value, GatewayError>
//! checkin::claim_daily_signin(&AccountStore, &str) -> Result<Value, GatewayError>
//! promotion::today_reward(&AccountStore, &str) -> Result<Value, GatewayError>
//!                                      活动福利到账确认（Weekly Perks 等
//!                                      「活跃即发」活动；无领取接口，见模块头）
//! ```
//!
//! ── 两个 id 别搞混 ──────────────────────────────────────────
//! `credentials::DESKTOP_ACCOUNT_ID` 是**凭证对象**的 id（原项目的 `desktop-auth`）；
//! `account_store::autoclaw_accounts::DESKTOP_ACCOUNT_ID` 是**账号记录**的 id
//! （`autoclaw-desktop`，因为 `desktop-auth` 已被 CatPaw 的桌面端账号占用）。
//! `snapshot_for` 认的是记录里的 `desktop` 标记，不依赖记录 id，因此两者不冲突。
//!
//! ── 平台约束 ───────────────────────────────────────────────
//! auth.json 来源（safeStorage 解密）**仅 Windows 可用**：DPAPI 是 Windows 独有
//! API，非 Windows 上 `crypto::os_crypt_aes_key` 返回
//! 「DPAPI 解密仅支持 Windows 平台」。openclaw.json 明文来源在非 Windows 上
//! 照常可用。本目录**能**在非 Windows 编译通过（CI / 交叉编译安全）。

pub mod adapter;
pub mod balance;
pub mod callback_server;
pub mod catalog;
pub mod checkin;
pub mod credentials;
pub mod crypto;
pub mod login;
pub mod models;
pub mod oauth;
pub mod profile;
pub mod promotion;
pub mod prompt;
pub mod refresh;
pub mod region;

pub use adapter::{AUTOCLAW_ADAPTER, AUTOCLAW_INTL_ADAPTER};
pub use region::Region;

/// 对外申报的 AutoClaw 桌面端版本，供**两条链路共用**（模型目录的签名头
/// `refresh::signed_auth_headers` 与转发 chat 的品牌头 `adapter::brand_headers`）。
///
/// ── 为什么必须单点定义 ─────────────────────────────────────
/// 这两处此前各写一份（目录 1.18.5 / 转发 1.17.8），客户端升级后目录那份
/// 过期、服务端按 `X-Version` 门控只下发旧清单（GLM-5.4 等新模型拉不到），
/// 而转发那份还停在更早的版本 —— 同一「客户端版本」两个值，升级时漏改一处
/// 就是静默故障。现在统一到本常量：改一处两条链路同时生效。
///
/// ── 值怎么定 ───────────────────────────────────────────────
/// 跟随真实桌面端版本：官方 2026-11 活动链接里 `target_app_version=2.5.3`，
/// 该版本下 GLM-5.4 已在端内上架。上游按 `X-Version` 做目录门控，版本过低
/// 会回落到旧清单而不报错 —— 所以这个值的症状是「模型少」，不是请求失败。
/// 客户端再升级时改这里一处即可。
pub const CLIENT_VERSION: &str = "2.5.3";

#[cfg(test)]
mod client_version_tests {
    use super::*;

    /// 钉住「一个版本」：目录与转发都从 [CLIENT_VERSION] 取值，两处不得再各自
    /// 写字面量（历史上就是漏改目录那份，导致 GLM-5.4 拉不到）。这条断言把
    /// 「跟随客户端版本」的当前值固定下来，升级时明确改这一处。
    #[test]
    fn client_version_is_single_source() {
        assert_eq!(CLIENT_VERSION, "2.5.3");
        // 两条链路必须真的引用同一个常量（编译器保证没有第二个字面量常量）：
        // 目录头在 refresh::signed_auth_headers，转发头在 adapter::brand_headers。
        let catalog_header = refresh::signed_auth_headers("dummy-token")
            .into_iter()
            .find(|(key, _)| key == "X-Version")
            .map(|(_, value)| value);
        assert_eq!(catalog_header.as_deref(), Some(CLIENT_VERSION));
    }
}
