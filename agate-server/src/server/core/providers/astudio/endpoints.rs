//! AStudio 的地址与客户端身份常量。
//!
//! ── 三个域 ──────────────────────────────────────────────────
//! ```text
//!   模型域    astronstudio-api-volces-prod.xf-yun.com
//!               POST /model-gateway/v1/responses   （OpenAI Responses 协议）
//!               GET  /api/v1/model-manager/models  （在线目录）
//!   账号域    agent.xfyun.cn/xingchen-studio/…
//!               points/balance、membership/me、membership/plan-config、
//!               points/summary、POST tenant-app/v2/init-app（每日登录）
//! ```
//! 这些地址是桌面端写进它自己内核配置的 `base_url` / `wire_api`，与官方客户端
//! 逐字一致 —— 不是我们猜的。
//!
//! ── 为什么两个模型地址写两遍全字面量 ────────────────────────
//! `concat!` 只吃字面量，`format!` 不能在 const 里用。把它们抽成一个
//! `const HOST` 再拼，只能改成运行期函数（每请求一次分配）或者宏 —— 都不值得。
//! 两个常量各写全，用一条单测钉住「同域」这件事（改了其中一个却忘了另一个，
//! 测试立刻红）。
//!
//! ── 客户端身份头（`clientType` + `studioVersion`）──────────
//! 它们属于**登录链**（上游 `auth.rs` 的 `decorate`）：发短信、登录、`userInfo`
//! 这些接口每个请求都带，模块头写明「少了会按未知客户端处理」。上游插件按宿主
//! 平台给 `21`（Windows）/ `22`（macOS），其它平台**不发**。
//!
//! 我们固定发 `21`：Android 上本来就没有它家的客户端，本家整家都是伪装桌面端
//! （与本项目其它家同一处置）；而「不发」在上游自己的代码里属于没验证过的路径
//! （它只跑过 Windows / macOS 宿主），拿它当默认值不划算。
//!
//! ⚠️ **`studioVersion` 现在也发给账号面**（上游 3.4.2 `client.rs::decorate`）：
//! 登录、余额、会员、每日登录共用同一份桌面端身份。旧结论「账号面不发」
//! 已被上游推翻；`clientType` 仍只在 Windows / macOS 宿主发，Android 不发。
//!
//! 登录链与账号面、以及**将来的登录页**必须共用同一份 `STUDIO_VERSION`：
//! 两处版本号不一致会让上游按两个客户端处理，而极验那次会话只在其中一个
//! 里成立 —— 这与 qwenwork 那条「COSY 头与请求体必须同源」是同一条纪律。

/// 模型域（Responses 与目录共用同一台主机）
pub const MODEL_HOST: &str = "astronstudio-api-volces-prod.xf-yun.com";

/// Responses 端点（模型调用）
pub const RESPONSES_URL: &str =
    "https://astronstudio-api-volces-prod.xf-yun.com/model-gateway/v1/responses";

/// 在线模型目录
pub const MODEL_MANAGER_URL: &str =
    "https://astronstudio-api-volces-prod.xf-yun.com/api/v1/model-manager/models";

/// 账号面根（**结尾带斜杠**：相对路径直接拼在它后面）
pub const STUDIO_API_BASE: &str = "https://agent.xfyun.cn/xingchen-studio/";

/// 当日首次调用触发每日登录奖励（POST，无请求体）；登录与签到共用。
pub const INIT_APP: &str = "tenant-app/v2/init-app";
/// 积分余额
pub const POINTS_BALANCE: &str = "points/balance";
/// 会员信息
pub const MEMBERSHIP_ME: &str = "membership/me";
/// 套餐配置（把 plan id 翻成人能看的名字）
pub const PLAN_CONFIG: &str = "membership/plan-config";
/// 积分明细汇总
pub const POINTS_SUMMARY: &str = "points/summary";

/// 桌面端版本（`studioVersion` 头，取安装包 version）
pub const STUDIO_VERSION: &str = "3.5.1";

/// 客户端类型（`clientType` 头）：21 = Windows、22 = macOS
pub const CLIENT_TYPE: &str = "21";

#[cfg(test)]
mod tests {
    use super::*;

    /// 两个模型地址必须同域：改成不同域（或改了其中一个忘了另一个）会让
    /// 「目录能拉、对话 401」这种最难查的形态出现
    #[test]
    fn the_two_model_urls_share_one_host() {
        for url in [RESPONSES_URL, MODEL_MANAGER_URL] {
            assert!(
                url.contains(MODEL_HOST),
                "模型地址 {url} 不在模型域 {MODEL_HOST} 上"
            );
            assert!(url.starts_with("https://"), "模型地址必须走 https：{url}");
        }
    }

    /// 账号面的四个接口都拼在同一个根上，且根**必须**以斜杠结尾
    /// （否则 `points/balance` 会拼成 `xingchen-studiopoints/balance`）
    #[test]
    fn the_account_paths_hang_off_one_base() {
        assert!(STUDIO_API_BASE.ends_with('/'), "账号面根必须以斜杠结尾");
        for path in [INIT_APP, POINTS_BALANCE, MEMBERSHIP_ME, PLAN_CONFIG, POINTS_SUMMARY] {
            assert!(!path.starts_with('/'), "相对路径不要带前导斜杠：{path}");
            assert!(!path.contains("://"), "这是相对路径：{path}");
        }
    }

    /// 身份头的两个常量都不能是空的（空值等于发一个空头，比不发更可疑）
    #[test]
    fn the_client_identity_headers_have_values() {
        assert!(!STUDIO_VERSION.trim().is_empty());
        assert!(matches!(CLIENT_TYPE, "21" | "22"));
    }
}
