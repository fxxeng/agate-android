//! QuWork 1.6.0 的端点与协议常量。
//!
//! 上游把 QuWork 拆成两套平面（照搬 v3.5.0-beta.1 `quwork/endpoints.rs`）：
//! ```text
//!   模型网关  https://qurwork-llm.rrzu.com/v1   Bearer llmToken
//!   账号服务  https://llm.rrzu.com               Bearer accessToken
//! ```
//! **登录会话的 accessToken 不能代替模型调用的 llmToken** —— 这是本家最容易
//! 踩错的一处：只给了 accessToken 的账号能查积分 / 签到 / 续期，却一定发不出
//! 对话。反之只给 llmToken 也能对话，只是没有账号面能力。
//!
//! 去桌面化：上游 `desktop.rs` 读本机 SQLite / `.credentials.yaml`、
//! `login.rs` 的设备码流程、`benefits.rs` 的设备标识一律不搬（见 `mod.rs`）。

/// provider id（与注册表 `PROVIDERS` 一致，账号落盘契约）
pub const ID: &str = "quwork";
/// 客户端版本头 `X-QurWork-Version`
pub const CLIENT_VERSION: &str = "1.6.0";

/// 账号服务域（积分 / 续期 / 每日奖励）
pub const ACCOUNT_ORIGIN: &str = "https://llm.rrzu.com";
/// 模型网关域（OpenAI 兼容 chat/completions，末尾不带斜杠）
pub const MODEL_ORIGIN: &str = "https://qurwork-llm.rrzu.com/v1";
/// 公开模型目录配置（无鉴权）
pub const CATALOG_URL: &str =
    "https://img1.rrzuji.cn/qurvis-package/latest/qurvis-desktop-model-config.json";

/// `GET` 账号自身信息（积分）
pub const SELF_PATH: &str = "/api/user/self";
/// `GET` 登录续期
pub const REFRESH_PATH: &str = "/api/user/refresh";
/// `GET` 每日奖励状态
pub const DAILY_STATUS_PATH: &str = "/api/user/daily-reward/status";
/// `POST` 每日奖励领取
pub const DAILY_CLAIM_PATH: &str = "/api/user/daily-reward/claim";
/// `GET` 月度赠送列表（**只读**：上游没有「领取月度赠送」的写端点）
pub const MONTHLY_GIFTS_PATH: &str = "/api/user/benefit/monthly-gifts";
/// `POST` 订单号兑换（body `{ order_id: <int64> }`）
pub const REDEEM_ORDER_PATH: &str = "/api/user/benefit/redeem";
/// `POST` 邀请码激活（body `{ invite_code: "..." }`）
pub const ACTIVATE_INVITE_PATH: &str = "/api/user/activate";

/// 单条响应体上限（上游 `read_reply` 的安全边界）
pub const MAX_RESPONSE_BYTES: usize = 1_048_576;
/// 续期窗口：expiresAt 进入 1 小时内才刷
pub const REFRESH_WINDOW_MS: i64 = 60 * 60_000;
/// 模型目录缓存 TTL（上游同为 10 分钟）
pub const CATALOG_TTL_MS: i64 = 10 * 60 * 1000;

/// 远程模型配置校验用的三个契约值（上游 `models.rs::validate` 的安全边界）
pub const MODEL_CONFIG_KEY: &str = "qurvis-desktop-model-config";
pub const MODEL_SCHEMA_VERSION: u64 = 1;
pub const MODEL_PROVIDER_ID: &str = "qurwork";
pub const MODEL_API: &str = "openai-completions";

/// 两家域名都要带的客户端身份头
pub fn headers() -> Vec<(String, String)> {
    vec![
        ("X-QurWork-Env".to_string(), "prod".to_string()),
        ("X-QurWork-Version".to_string(), CLIENT_VERSION.to_string()),
    ]
}
