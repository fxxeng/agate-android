//! ZCode 的**限时套餐领取**（manual claim / start plan）。
//!
//! ── 这个功能替代了本家的「签到」────────────────────────────
//! 其余各家都有每日签到（`core::auto_checkin` 的提供商清单），ZCode 没有签到
//! 活动 —— 它的运营玩法是**限时发放的体验套餐**（周末套餐 / Global Build /
//! ZCode Trust Build），用户在客户端里点一下就领，领到的额度在一段时间内可用。
//!
//! ── 「每天领一次」是**套餐 id 实现的**，不是上游的每日签到 ────
//! 2026-09-28 那期（ZCode Trust Build，9/28–11/7）公告写的是「每天登录领 1 亿」，
//! 而上游没有单独的「每日领取」接口：`preview` 每天返回一个**带日期段的新套餐**
//! （`zcode-v3-start-plan-trust-0928` → `…-0929`），于是「今天领过没」等价于
//! 「今天这个 plan_id 领过没」（同一期再领回 `1003 already claimed`）。
//! 本家的界面据此按自然日落一次状态（`claimAt`），见
//! `core::account_store::StoredAccount::claim_at`。
//!
//! ── 上游协议（两个接口，抄自 ZCode 客户端的 `manualClaimPlan`）──
//! 参考实现：`Acankao/zcode-api` 的 `src/claim/client.ts`，本模块是它的移植；
//! 字段与失败语义另有官方仓库（`zai-org/ZCode`）可对照。
//!
//! ```text
//!   探测  GET  {zcode}/api/v1/zcode-plan/billing/preview?app_version=&platform=
//!   领取  POST {zcode}/api/v1/zcode-plan/billing/claim   body {"plan_id": "..."}
//! ```
//!
//! ── 头集合是「极简 + 一处硬要求」─────────────────────────────
//! preview 与 claim 都只必要地带 `Authorization: Bearer {jwt}`（探测允许匿名），
//! claim 另加 Content-Type 与验证码参数。早先版本发过整套身份头（`X-Device-Mid`
//! 等），两者在不同活动期都出现过。
//!
//! **`X-Device-Mid` 现在是硬要求**（2026-09-28 实测）：缺它、或值不是 UUID 形态，
//! 网关一律回 `400 {"code":3001,"msg":"parameter error"}` —— 连**匿名探测**也一样
//! （只带版本头/平台头都不管用，只加验证码头也无效）。设备标识还必须**跨请求
//! 稳定**（风控据此关联同一设备的请求），因此它随凭证落盘、缺失时由
//! `AccountStore::zcode_device_mid_or_create` 生成一次后长期复用，
//! **不要**在调用点现编一个。
//!
//! ── 验证码为什么是**参数**而不是本模块自己求解 ───────────────
//! claim 通常要带 `X-Aliyun-Captcha-Verify-Param`（阿里云无痕验证）。
//! 参考实现在进程内用 happy-dom 跑阿里云官方混淆 SDK 求解 —— 那是 Node/Bun
//! 生态的产物（需要 DOM 桩、canvas/WebGL/Worker 垫片、同步 XHR 的 worker 变通），
//! 本网关是纯 Rust 且零 Node 依赖，**不重复实现那套求解器**。
//!
//! 本项目已有一条现成的等价链路：AutoClaw 的 OAuth 登录同样要过阿里云无痕
//! 验证，做法是**前端（Tauri webview）用官方 SDK 求解、把 verifyParam 交给
//! 后端**（见 `ui/autoclaw-oauth.js` 与 `api::session::login_oauth_captcha_config`）。
//! 领取走同一条路：前端解出参数 → 调后端的领取接口 → 本模块把它原样转发给上游。
//! 于是本模块的签名里 `captcha` 是**入参**，本模块只负责「带上它去领取」。
//! 代价是**领取无法全自动**：定时任务里没有可用的验证码，这一点在
//! `api::zcode_claim` 的模块头有说明（所以不接自动领取）。
//!
//! ── 失败分类（biz code → 语义）──────────────────────────────
//! 与客户端 `$vt` 映射器逐条对齐（参考实现 `src/claim/types.ts`）：
//! `1001` 不存在 / `1002` 不可领（活动结束或未开始）/ `1003` 已领过 /
//! `1004` 不符合资格（账号或客户端版本不达标）/ `1005` 当日领取额度用尽 /
//! `3001` 参数错误 / `3007` 验证码失败 / `401` 未登录。
//! 这套分类是**调度语义的输入**：见 `outcome_hold`。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::auth_http::send_raw;
use crate::server::core::proxies::ResolvedProxy;
use crate::server::errors::GatewayError;

use super::region::Region;

/// 营销活动发现与领取回执（`marketing/touch` / `touch/action`）。
///
/// 2026-11-03 起每日套餐可**只**由这条链路下发（`billing/preview` 为空），
/// 因此它与 billing 探测并列为两个发现源，见 [`discover`]。
pub(crate) mod marketing;

/// 单次请求超时（参考实现 `DEFAULT_TIMEOUT_MS` 逐字相同）。
///
/// 必须显式设：`egress` 的默认 read_timeout 是 600 秒（给 SSE 长连接留的），
/// 不设的话一个挂住的领取请求能让定时任务转圈十分钟。
const REQUEST_TIMEOUT_MS: u64 = 15_000;

/// 客户端版本号。
///
/// 上游拿它判「这个客户端够不够新」（`1004 ineligible` 就包含版本不达标），
/// 活动也可能要求最低版本。参考实现把它做成配置项（`identity.appVersion`，
/// 环境变量 `ZCODE_APP_VERSION` 可覆盖），默认值是 **ZCode 客户端当前版本**
/// `3.14.0`。
///
/// ★ 别拿参考项目自己的版本号（它 `package.json` 里那个 `4.6.9`）—— 那个数字
/// 冒充不了客户端，`app_version` 与 `X-ZCode-App-Version` 都会对不上，症状是
/// 领取被判 `1004 ineligible`（而这看起来像「账号没资格」，极易误判）。
/// 上游抬门槛时用户可以用环境变量覆盖，不必等发版。
/// **推理请求**的客户端版本号（只进推理身份头，不再给活动发现用）。
///
/// ── 为什么它是 3.16.2，而活动发现另用 3.18.0 ──────────────────
/// 这个值进**推理请求**的 `User-Agent: ZCode/{version}` 与
/// `X-ZCode-App-Version` 头 —— 上游按「客户端形态」识别推理请求，实测
/// 3.17.2 的每条推理都被 `405 request has been blocked`（业务码 3012）拒收，
/// 回退 3.16.2 才恢复；因此**推理这一路不能跟官方版本走**。
///
/// 但活动发现（`billing/preview` / `marketing/touch`）是另一组接口，不受那条
/// 推理风控约束，且有独立的最低版本门槛（见 [`ACTIVITY_APP_VERSION`]）。于是
/// 两路刻意分开：把活动发现也用 3.16.2，症状就是「活动明明在、却一个套餐
/// 都探不到」（上游静默返回空列表，不报错）——正是本次修复的根因之一。
///
/// 仍允许 `ZCODE_APP_VERSION` 覆盖推理值（上游再抬高推理门槛时不必发版）。
const DEFAULT_APP_VERSION: &str = "3.16.2";

/// **活动发现 / 领取接口**的客户端版本号。
///
/// 上游 v3.4.1 实测（2026-11-03）：每日赠送套餐改由 `marketing/touch` 下发后，
/// 同一账号 `3.16.3` 拿到 0 条交付、`3.17.1` 与 `3.18.0` 各拿到 1 条；老
/// `billing/preview` 链路只要 3.16.1+。取两边都够用的 `3.18.0`。
/// 与推理值分开的理由见 [`DEFAULT_APP_VERSION`]。
/// `ZCODE_ACTIVITY_APP_VERSION` 可覆盖（上游抬门槛时不必发版）。
const ACTIVITY_APP_VERSION: &str = "3.18.0";

/// 探测与领取的路径（参考实现 `client.ts` 的常量）
const PREVIEW_PATH: &str = "/api/v1/zcode-plan/billing/preview";
const CLAIM_PATH: &str = "/api/v1/zcode-plan/billing/claim";

/// 客户端配置（风控参数藏在它的 `data.configs.captcha` 里）
const CLIENT_CONFIGS_PATH: &str = "/api/v1/client/configs";

/// 阿里云无痕验证的配置（前端拿它去初始化 SDK）。
///
/// 三个字段都是 SDK 的必填项，任缺一个就没法初始化：
///   - `prefix` / `region` → `window.AliyunCaptchaConfig`（决定打哪个阿里云站点）
///   - `scene_id` → `initAliyunCaptcha({ SceneId })`
///
/// 参考实现从同一个接口取（`fetchCaptchaConfig`，带 60 秒缓存），本家让
/// 调用方决定要不要缓存 —— 这是低频动作（一次领取取一次），不值得在这里
/// 维护一份带 TTL 的进程级缓存。
#[derive(Clone, Debug)]
pub struct CaptchaConfig {
    /// 上游说这一家此刻要不要验证码。`false` 时前端**不该**弹滑块 ——
    /// 硬弹会让用户在本不需要验证的时候被拦一道
    pub enabled: bool,
    /// SDK 站点前缀（`window.AliyunCaptchaConfig.prefix`）
    pub prefix: String,
    /// 场景 id（`initAliyunCaptcha` 的 `SceneId`）
    pub scene_id: String,
    /// SDK 站点地区（`window.AliyunCaptchaConfig.region`）
    pub region: String,
    /// 上游说**推理请求**可以不带验证码头（`configs.captcha.skip_model_request`）。
    ///
    /// 2026-10-01 16:44 UTC 实测：`{"enabled":true,"prefix":"no8xfe","region":"cn",
    /// "sceneId":"11xygtvd","skip_model_request":true}` —— 四个组合（app_version
    /// 3.14.0 / 3.14.4 × platform darwin-arm64 / linux-x64）返回**逐字相同**，
    /// 也就是说这一格与客户端版本无关，是服务端下发的开关。
    ///
    /// `enabled` 与它是**两件事**：前者管领取/登录这类交互流程还要不要滑块，
    /// 后者只管 `…/anthropic/v1/messages` 那条推理路。因此不能拿 `enabled`
    /// 推断推理免码 —— 那样会在上游把这一格翻回 `false` 时继续白铸令牌。
    ///
    /// 缺这一格按 `false` 收（照旧要码）：本家此前的全部行为都建立在「每条
    /// 请求都要一枚令牌」上，把「没下发」读成「免码」等于让一次配置缺失误开门。
    pub skip_model_request: bool,
}

/// 取风控配置（`GET {zcode}/api/v1/client/configs?app_version=&platform=`）。
///
/// 返回 `Ok(None)` = 上游没有下发 captcha 段（或字段不全）：调用方据此
/// **不弹验证码**，而不是弹一个初始化不出来的滑块（那种失败对用户来说
/// 完全无法理解：滑块都没出现就被报「验证码组件不可用」）。
pub async fn captcha_config(
    region: Region,
    proxy: Option<&ResolvedProxy>,
) -> Result<Option<CaptchaConfig>, GatewayError> {
    let url = format!(
        "{}{CLIENT_CONFIGS_PATH}?app_version={}&platform={}",
        region.zcode_origin(),
        urlencode(&app_version()),
        urlencode(platform())
    );
    let response = send_raw("GET", &url, None, &[], proxy, Some(REQUEST_TIMEOUT_MS))
        .await
        .map_err(|error| {
            if error.is_timeout() {
                GatewayError::with_status(504, "获取风控配置超时")
            } else {
                GatewayError::with_status(502, format!("获取风控配置失败: {error}"))
            }
        })?;
    let payload = response.payload.unwrap_or(Value::Null);
    Ok(parse_captcha_config(&payload))
}

/// 从 `GET /api/v1/client/configs` 的**整份响应**里读出 captcha 段（纯函数）。
///
/// 单独成函数的理由：这条链上只有这一处需要「上游那份 JSON 长什么样」的知识，
/// 收成纯函数后，测试可以直接喂一份抓下来的真实响应（见 `captcha_config` 的
/// 测试模块），而不必起一台假服务器 —— 而 `skip_model_request` 这一格恰恰是
/// 整套免码判定的唯一依据，它必须钉在**实测过的字节**上。
///
/// 返回 `None` = 没有 captcha 段、或 SDK 必填的两格（`prefix` / `sceneId`）缺
/// 任一（那时前端连初始化都做不出来，见调用方说明）。
pub(super) fn parse_captcha_config(payload: &Value) -> Option<CaptchaConfig> {
    let captcha = payload
        .get("data")
        .and_then(|value| value.get("configs"))
        .and_then(|value| value.get("captcha"))?;
    let text = |key: &str| {
        captcha
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let prefix = text("prefix");
    let scene_id = text("sceneId");
    if prefix.is_empty() || scene_id.is_empty() {
        return None;
    }
    Some(CaptchaConfig {
        enabled: captcha
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        prefix,
        scene_id,
        // 缺省 `ga`：与 AutoClaw 那条链的兜底一致（阿里云的默认站点）
        region: {
            let value = text("region");
            if value.is_empty() {
                "ga".to_string()
            } else {
                value
            }
        },
        // **缺这一格 = 要码**（见字段说明：宁可白铸一枚令牌，不要发一个
        // 上游可能拒的无头请求 —— 3007 那次往返是要付账号行为分的）
        skip_model_request: captcha
            .get("skip_model_request")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// 领取出错时**优先复核风控配置**（模块头第 3007 档的处置，见 `claim` 的文档）。
///
/// 单独成一个函数是因为它有明确的前置条件与用途：只有「验证码失败」这一档
/// 才值得多花一次往返去问「现在还需要验证码吗」。其他失败（额度用尽、
/// 已领过）复核配置没有意义。
pub fn captcha_hint(config: &CaptchaConfig) -> String {
    if config.enabled {
        "请重新完成验证码后再试".to_string()
    } else {
        // 上游此刻说「不需要验证码」，而领取却因验证码被拒 —— 说明我们带的
        // 那个参数反而成了问题（可能是上一轮的陈旧解）。如实说明，让用户
        // 点一次重试（不带的路径见 `claim` 的调用方）。
        "上游当前未要求验证码，请直接重试一次领取".to_string()
    }
}

/// 客户端版本号（`ZCODE_APP_VERSION` 可覆盖）
/// 内置默认版本号（界面展示「留空 = 用这个」）。
pub fn default_app_version() -> &'static str {
    DEFAULT_APP_VERSION
}

pub fn app_version() -> String {
    // 优先级：配置项（界面可改）> 环境变量 > 内置默认。
    // 配置项排在最前：桌面/安卓形态改不了进程环境，而「上游抬高版本门槛后
    // 自救」必须有个能在界面上改的入口。
    if let Some(value) = crate::server::config::zcode_app_version() {
        return value.to_string();
    }
    std::env::var("ZCODE_APP_VERSION")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_APP_VERSION.to_string())
}

/// 活动发现 / 领取接口的客户端版本号（与推理的 [`app_version`] **刻意分开**）。
///
/// 优先级：环境变量 `ZCODE_ACTIVITY_APP_VERSION` > 内置 `3.18.0`。
/// 这里**不读** `zcode_app_version()` 配置项：那个值是给推理链路躲 405 风控的
/// （3.16.2），界面上改它是为了推理自救；若活动发现也跟着变，用户为推理填一个
/// 低版本就会让套餐探测重新落入「静默空列表」。两件事互不干扰。
///
/// `pub(super)`：新建的 `marketing` 子模块与本节共用同一版本口径。
pub(super) fn activity_app_version() -> String {
    std::env::var("ZCODE_ACTIVITY_APP_VERSION")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| ACTIVITY_APP_VERSION.to_string())
}

/// 平台标识（参考实现的 `${process.platform}-${process.arch}`）。
///
/// 上游把它当「客户端形态」的一部分。macOS / Linux 构建各取所得；其余目标
/// （Android）回落到 `win32-x64`（上游对未知值不报错，只是可能不满足活动的
/// 客户端白名单，那时会如实回 `1004`）。
pub fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "darwin-arm64"
        } else {
            "darwin-x64"
        }
    } else if cfg!(target_os = "linux") {
        if cfg!(target_arch = "aarch64") {
            "linux-arm64"
        } else {
            "linux-x64"
        }
    } else {
        "win32-x64"
    }
}

/// 操作系统的**类别名**（`windows` / `macos` / `linux`）。
///
/// 客户端身份头 `X-Os-Category` 与激活事件体的 `device_os_category` 共用
/// 这一处（两处各写一份 cfg 链，迟早有一处漏改）。与 [`platform`] 的区别：
/// 那个是「平台-架构」（`win32-x64`），这个是纯类别名 —— 上游按它分流、
/// 不看架构。认不出的平台落 `linux`（参考实现 `normalizeOsCategory` 的
/// 默认分支同义；Windows 分支已随桌面版删除）。
pub fn os_category() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// 一条可领取的套餐（上游 `plans[]` 的归一形态）。
///
/// 字段保留上游语义：`starts_at` / `ends_at` 是 **unix 秒**（不是毫秒）——
/// 领取链路全程用秒，换算成毫秒只在展示层做，混用会让「有效期到 1970 年」
/// 这类错误悄悄发生。
#[derive(Clone, Debug)]
pub struct ClaimablePlan {
    /// 上游套餐 id（领取时原样回传）
    pub plan_id: String,
    /// 展示名
    pub name: String,
    /// 描述
    pub description: String,
    /// 优先级（同一批多个套餐时，取最大的那个）
    pub priority: i64,
    /// 生效时间（unix 秒；有些期次的活动套餐是「先领、稍后生效」，因此可能缺失）
    pub starts_at: Option<i64>,
    /// 失效时间（unix 秒）
    pub ends_at: Option<i64>,
    /// 权益条目（进日志与界面，让用户看到领到了什么）
    pub entitlements: Vec<PlanEntitlement>,
    /// 该套餐来自哪些营销活动（`marketing/touch` 的 `campaign_id`）。
    ///
    /// 领取 settle 之后要按这些 id 逐个回执 `touch/action`（见 `marketing` 子模块）。
    /// 只有 billing 链路的套餐这里是空的（无需回执）。
    pub marketing_campaign_ids: Vec<String>,
}

/// 套餐里的一条权益
#[derive(Clone, Debug)]
pub struct PlanEntitlement {
    /// 权益展示名（缺失时回落到 id）
    pub show_name: String,
    /// 计量单位（`tokens` / `requests` …）
    pub unit_type: String,
    /// 授予量（`0` = 上游没给数字）
    pub grant_units: i64,
    /// 周期（`one_time` = 一次性发放，`daily` = 每天续发）。
    ///
    /// 界面拿它区分「活动送的一次性额度」与「每日额度」（Start Plan 的两条权益
    /// 就是 `daily`）—— 2026-09-28 那期 Trust Build 是一次性 1 亿，
    /// 每天一个新套餐，所以两者都可能在同一天出现。
    pub period: String,
    /// 生效时间（unix 秒；缺失表示随套餐立即生效）
    pub effective_at: Option<i64>,
}

/// 探测的结果。
///
/// ── 为什么 404 要单独成一档（这是最容易漏的一处）─────────────
/// 参考实现在 `scheduler.ts` 里明确写着：**404 是「活动还没上线」的正常状态**
/// （活动开抢前，接口尚未部署），要按**正常节奏**轮询而不是走错误退避。
/// 若把 404 当失败处理，退避会让轮询节奏越来越慢，恰好错过开抢那一刻 ——
/// 这个功能的价值全在「上新瞬间抢到」，慢一拍就没意义了。
#[derive(Clone, Debug)]
pub enum PreviewOutcome {
    /// 拿到了可领取的套餐（可能是空列表：接口通了但当前没有可领的）
    Plans(Vec<ClaimablePlan>),
    /// 404：活动接口尚未部署（开抢前的预期状态）
    NotDeployed,
}

/// 领取的失败语义（参考实现 `ClaimFailureKind` 的移植）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClaimFailure {
    /// 套餐不存在（1001）
    NotFound,
    /// 不可领：活动已结束或还没开始（1002）
    Unavailable,
    /// 这个账号已经领过（1003）
    AlreadyClaimed,
    /// 不符合资格：账号或客户端版本不达标（1004）
    Ineligible,
    /// 当日领取额度用尽（1005）
    QuotaExhausted,
    /// 参数错误（3001）—— 最常见的成因是缺 `X-Device-Mid`
    InvalidRequest,
    /// 验证码校验失败（3007）
    Captcha,
    /// 未登录 / JWT 失效（401）
    LoginRequired,
    /// 其它 HTTP 错误
    HttpError,
    /// 认不出的业务码
    Unknown,
}

impl ClaimFailure {
    /// 给用户看的一句话（进日志与界面）
    pub fn label(self) -> &'static str {
        match self {
            Self::NotFound => "套餐不存在",
            Self::Unavailable => "活动已结束或尚未开始",
            Self::AlreadyClaimed => "该账号已领取过",
            Self::Ineligible => "账号或客户端版本不符合活动资格",
            Self::QuotaExhausted => "当日领取额度已用尽",
            Self::InvalidRequest => "参数错误（常见成因：缺少设备标识）",
            Self::Captcha => "验证码校验未通过",
            Self::LoginRequired => "登录态已失效，请重新登录",
            Self::HttpError => "上游 HTTP 错误",
            Self::Unknown => "未知失败",
        }
    }

    /// biz code → 语义（与客户端 `$vt` 映射器逐条对齐）
    fn from_code(code: i64) -> Self {
        match code {
            1001 => Self::NotFound,
            1002 => Self::Unavailable,
            1003 => Self::AlreadyClaimed,
            1004 => Self::Ineligible,
            1005 => Self::QuotaExhausted,
            3001 => Self::InvalidRequest,
            3007 => Self::Captcha,
            401 => Self::LoginRequired,
            _ => Self::Unknown,
        }
    }
}

/// 领取的结果
#[derive(Clone, Debug)]
pub enum ClaimOutcome {
    /// 领到了
    Claimed {
        /// 套餐 id
        plan_id: String,
        /// 生效时间（unix 秒；缺省表示立即生效）
        starts_at: Option<i64>,
        /// 失效时间（unix 秒）
        ends_at: Option<i64>,
    },
    /// 没领到（含「已领过」这类不算错误的结果）
    Failed {
        /// 套餐 id
        plan_id: String,
        /// 失败语义
        failure: ClaimFailure,
        /// 上游业务码（进日志）
        code: i64,
        /// 上游原文
        message: String,
        /// 上游给的下一个可尝试窗口（unix 秒；`1005` 额度用尽时通常有）
        failure_ends_at: Option<i64>,
    },
}

/// 一次领取尝试之后**下次什么时候再试**（调度语义，参考实现 `scheduler.ts`）。
#[derive(Clone, Copy, Debug)]
pub enum NextAttempt {
    /// 本账号在本期活动里无事可做：等到这个 unix 秒（套餐失效时间）之后再说。
    /// 用于 `Claimed` 与 `AlreadyClaimed` —— 两者都表示「这一期已经领到了」。
    HoldUntil(i64),
    /// 换一个窗口再试：等到这个 unix 秒（上游给的下一窗口）。
    /// 用于 `QuotaExhausted`。
    WaitWindow(i64),
    /// 退避：按常规冷却时长再试。用于验证码失败 / 网络错误 / 认不出的失败。
    Cooldown,
}

/// 从领取结果推出下一次尝试时机。
///
/// 为什么单独成函数：这段判断是**调度正确性**的核心（把「已领过」当成失败
/// 去重试会一直打上游、把「额度用尽」当成已领到会整期不再尝试），
/// 放在一处并配上测试视角的注释，比散在调度循环里更难写错。
pub fn outcome_hold(outcome: &ClaimOutcome) -> NextAttempt {
    match outcome {
        // 领到了：本期的目标已达成，等它过期（缺 ends_at 时给 0，由调用方
        // 回落成常规轮询 —— 没有结束时间的套餐不该被无限期 hold 住）
        // 解引用是必需的：这里匹配的是 `&ClaimOutcome`，字段拿到的是
        // `&Option<i64>`（`Option<i64>` 是 Copy，`*` 出来即可）
        ClaimOutcome::Claimed { ends_at, .. } => NextAttempt::HoldUntil((*ends_at).unwrap_or(0)),
        ClaimOutcome::Failed {
            failure,
            failure_ends_at,
            ..
        } => match failure {
            // 已领过 = 本期的目标其实也达成了（可能是用户手动领的 / 另一台设备领的）
            ClaimFailure::AlreadyClaimed => NextAttempt::HoldUntil((*failure_ends_at).unwrap_or(0)),
            // 额度用尽：上游通常给出下一个窗口
            ClaimFailure::QuotaExhausted => match failure_ends_at {
                Some(at) => NextAttempt::WaitWindow(*at),
                None => NextAttempt::Cooldown,
            },
            // 活动没开始 / 不符合资格 / 参数错误 / 验证码 / 网络：都按冷却重试。
            // 其中 `Unavailable`（活动未开始）**不能**长退避 —— 它恰恰是开抢前
            // 最常见的一档，退避太久会错过上新。
            _ => NextAttempt::Cooldown,
        },
    }
}

/// 探测当前可领取的套餐。
///
/// `jwt` 为空时按上游语义发**匿名探测**（参考实现：匿名 preview 不带头）。
/// 匿名也能看到「有没有活动」，因此调度可以在登录态失效时继续探测；
/// 但真正领取必须有 JWT（见 [`claim`]）。
///
/// `device_mid` **事实上是必需的**（2026-09-28 起上游这么要求，见模块头）：
/// 调用方（`api::zcode_claim`）保证传进来的一定是 UUID 形态的稳定标识，
/// 这里只在真的没有时才省略那个头 —— 那种情况会稳定拿到 3001，
/// 而那正是「谁漏了这一步」的信号，不该由本函数偷偷补一个随机值（现编的值
/// 每次都不一样，风控看到的是「同一个账号天天换设备」）。
pub async fn preview_billing(
    region: Region,
    jwt: &str,
    device_mid: Option<&str>,
    proxy: Option<&ResolvedProxy>,
) -> Result<PreviewOutcome, GatewayError> {
    let url = format!(
        "{}{PREVIEW_PATH}?app_version={}&platform={}",
        region.zcode_origin(),
        // 活动发现用活动版本（3.18.0），不是推理的 3.16.2 —— 见常量说明。
        urlencode(&activity_app_version()),
        urlencode(platform())
    );
    let mut headers: Vec<(String, String)> = Vec::new();
    // 匿名探测：一个头都不带（**不是**带一个空 Bearer）
    if !jwt.trim().is_empty() {
        headers.push((
            "Authorization".to_string(),
            format!("Bearer {}", jwt.trim()),
        ));
    }
    if let Some(mid) = device_mid.map(str::trim).filter(|value| !value.is_empty()) {
        headers.push(("X-Device-Mid".to_string(), mid.to_string()));
    }

    let response = send_raw("GET", &url, None, &headers, proxy, Some(REQUEST_TIMEOUT_MS))
        .await
        .map_err(|error| {
            if error.is_timeout() {
                GatewayError::with_status(504, "套餐探测超时")
            } else {
                GatewayError::with_status(502, format!("套餐探测失败: {error}"))
            }
        })?;

    // 404 单独成一档（见 `PreviewOutcome::NotDeployed` 的文档）
    if response.status == 404 {
        return Ok(PreviewOutcome::NotDeployed);
    }
    if response.status == 401 {
        return Err(GatewayError::with_status(401, "登录态已失效，无法探测套餐"));
    }
    let payload = response
        .payload
        .ok_or_else(|| GatewayError::with_status(502, "套餐探测：上游响应不是 JSON"))?;
    let code = payload.get("code").and_then(Value::as_i64).unwrap_or(0);
    let data = payload.get("data");
    // `success: false` 与 code != 0 等价（上游两种写法都会出现）：漏判时会把
    // 一份「业务失败但 code 恰好是 0」的响应当成空套餐列表，症状是活动在却探不到。
    let explicit_failure = payload.get("success").and_then(Value::as_bool) == Some(false);
    if !(200..300).contains(&response.status) || code != 0 || data.is_none() || explicit_failure {
        let message = error_message(&payload, response.status);
        return Err(GatewayError::with_status(
            502,
            format!("套餐探测失败（{code}）：{message}"),
        ));
    }
    let plans = data
        .and_then(|value| value.get("plans"))
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(parse_plan).collect())
        .unwrap_or_default();
    Ok(PreviewOutcome::Plans(plans))
}

/// 双链路发现的合并结果（billing 探测 + 营销发现）。
#[derive(Clone, Debug)]
pub struct Discovery {
    /// 合并去重后的全部可领套餐（按 `priority` 降序）。
    pub plans: Vec<ClaimablePlan>,
    /// 两条链路里**至少一条**成功返回了可解析的响应。
    ///
    /// 只用于区分「上游接口没部署」与「接口返回空列表」——两者对界面是同一句
    /// 「当前没有可领套餐」，但排障时要能分清。billing 404 不再单独成档
    /// （营销可能仍在线上），因此这里取「任一成功」。
    pub deployed: bool,
    /// billing 链路的错误（有它不影响营销结果，供日志与响应透出）。
    pub billing_error: Option<String>,
    /// 营销链路的错误（同上，单独一栏便于排障「活动在却探不到」）。
    pub marketing_error: Option<String>,
}

/// 探测当前可领取的套餐（**双链路**：billing/preview + marketing/touch）。
///
/// ── 为什么要两条链路（本次修复的核心）────────────────────────
/// 2026-11-03 起上游把每日赠送套餐改由 `GET /marketing/touch` 下发，
/// `billing/preview` 可能整段为空。只查 billing 的后果就是「活动明明在，
/// 套餐一个都探不到」。两条链路**并行**发（各自独立、互不阻塞），结果按
/// plan_id 合并去重；一条失败不影响另一条，错误分别落到 `billing_error` /
/// `marketing_error`，只在「两条都失败且没有拿到任何套餐」时才整体报错。
///
/// 认证与设备标识口径与单链路一致：jwt 为空时发匿名请求（上游允许匿名发现），
/// `device_mid` 由调用方保证 UUID 形态。
pub async fn discover(
    region: Region,
    jwt: &str,
    device_mid: Option<&str>,
    proxy: Option<&ResolvedProxy>,
) -> Result<Discovery, GatewayError> {
    let (billing, marketing) = tokio::join!(
        preview_billing(region, jwt, device_mid, proxy),
        marketing::query(region, jwt, device_mid, proxy),
    );
    let mut plans: Vec<ClaimablePlan> = Vec::new();
    let mut deployed = false;
    let mut billing_error = None;
    let mut marketing_error = None;
    match billing {
        Ok(PreviewOutcome::Plans(items)) => {
            if !items.is_empty() {
                deployed = true;
            }
            merge_plans(&mut plans, items);
        }
        // billing 404 是「活动未上线」的正常态，不算错误（营销可能仍在线上）
        Ok(PreviewOutcome::NotDeployed) => {}
        Err(error) => billing_error = Some(error.message),
    }
    match marketing {
        Ok(items) => {
            // 营销链路返回空 deliveries 也是成功（接口在线但此刻没活动）。
            // 只要 HTTP 通了就算 deployed，与 billing 的 `Plans([])` 口径一致。
            deployed = true;
            merge_plans(&mut plans, items);
        }
        // 营销 404 上游已按空列表处理（见 marketing::query），这里收到的一定是
        // 真实错误，落到单独一栏。
        Err(error) => marketing_error = Some(error.message),
    }
    if !deployed && plans.is_empty() {
        let why = [billing_error.as_deref(), marketing_error.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("；");
        return Err(GatewayError::with_status(
            502,
            format!("套餐查询失败：{why}"),
        ));
    }
    Ok(Discovery {
        plans,
        deployed,
        billing_error,
        marketing_error,
    })
}

/// 把一批套餐合并进现有列表（按 `plan_id` 去重，最终按 `priority` 降序）。
///
/// 两条链路可能下发同一个 plan_id：营销侧解析出的元数据（名字/权益/起止）与
/// billing 侧的互为补充，合并时各取非空字段，并把营销 campaign id 收集到一处
/// ——领取后的回执要用它（见 `marketing::confirm`）。
fn merge_plans(target: &mut Vec<ClaimablePlan>, incoming: Vec<ClaimablePlan>) {
    for plan in incoming {
        if let Some(existing) = target.iter_mut().find(|item| item.plan_id == plan.plan_id) {
            merge_plan_fields(existing, plan);
        } else {
            target.push(plan);
        }
    }
    // 稳定排序：优先级相同的条目保持发现顺序（billing 先于 marketing）
    target.sort_by(|a, b| b.priority.cmp(&a.priority));
}

/// 同 plan_id 的字段合并（营销元数据补全 billing，或反过来）。
fn merge_plan_fields(existing: &mut ClaimablePlan, incoming: ClaimablePlan) {
    if existing.name.is_empty() || existing.name == existing.plan_id {
        existing.name = incoming.name;
    }
    if existing.description.is_empty() {
        existing.description = incoming.description;
    }
    existing.priority = existing.priority.max(incoming.priority);
    if existing.starts_at.is_none() {
        existing.starts_at = incoming.starts_at;
    }
    if existing.ends_at.is_none() {
        existing.ends_at = incoming.ends_at;
    }
    if existing.entitlements.is_empty() {
        existing.entitlements = incoming.entitlements;
    }
    for campaign in incoming.marketing_campaign_ids {
        if !existing.marketing_campaign_ids.contains(&campaign) {
            existing.marketing_campaign_ids.push(campaign);
        }
    }
}

/// 领取 settle 之后，按 campaign id 逐个回执 `touch/action`。
///
/// 由 API 层在「领取成功」或「已领过」两种落定结果后调用。回执失败只把原因
/// 收集起来返回（调用方记日志 + 透出到 `marketingError`），**不改领取结果** ——
/// 套餐额度在 claim 那一步已经到账，回执只是消费活动按钮。见 `marketing::confirm`。
pub async fn confirm_campaigns(
    region: Region,
    jwt: &str,
    device_mid: Option<&str>,
    proxy: Option<&ResolvedProxy>,
    campaign_ids: &[String],
) -> Vec<String> {
    let mut errors = Vec::new();
    for campaign in campaign_ids {
        if let Err(error) = marketing::confirm(region, jwt, device_mid, proxy, campaign).await {
            errors.push(error);
        }
    }
    errors
}

/// 领取一个套餐。
///
/// `captcha_verify_param` 由**前端**解出（见模块头：本网关不在 Rust 侧复刻
/// 阿里云 SDK 求解器）。`captcha_region` 是同一套 SDK 给出的地区标识，可选。
///
/// 返回 `Err` 只用于「请求本身没打成」（网络 / 超时 / 响应不是 JSON）；
/// 上游明确拒绝（含验证码失败、已领过）一律走 `Ok(ClaimOutcome::Failed)` ——
/// 那些是**业务结果**，调用方要靠 `failure` 决定下次何时再试。
pub async fn claim(
    region: Region,
    jwt: &str,
    plan_id: &str,
    captcha_verify_param: &str,
    captcha_region: Option<&str>,
    device_mid: Option<&str>,
    proxy: Option<&ResolvedProxy>,
) -> Result<ClaimOutcome, GatewayError> {
    if jwt.trim().is_empty() {
        return Ok(ClaimOutcome::Failed {
            plan_id: plan_id.to_string(),
            failure: ClaimFailure::LoginRequired,
            code: 401,
            message: "领取需要登录态（本账号没有 ZCode JWT）".to_string(),
            failure_ends_at: None,
        });
    }
    let url = format!("{}{CLAIM_PATH}", region.zcode_origin());
    // 头顺序与客户端一致：Authorization → Content-Type → 验证码 → 版本 → 平台
    // → 设备标识。顺序本身不是协议要求，但保持与参考实现同序便于逐条比对。
    //
    // ── 验证码头**只在有值时发**（一处容易想当然的地方）──────────
    // 上游的风控配置可能是 `enabled: false`（那一刻不要验证码），此时前端
    // 根本不会弹滑块、也就没有 verifyParam。若把空串也写成头
    // （`X-Aliyun-Captcha-Verify-Param: `），上游会把它当成「给了一个无效的
    // 验证串」而回 3007 —— 于是「不需要验证码」的那一刻反而领不了，
    // 而报错文案是「验证码校验未通过」，指向完全错误的方向。
    // 因此有值才发；没有就让上游按它自己的规则判（要就回 3007，不要就放行）。
    let mut headers: Vec<(String, String)> = vec![
        (
            "Authorization".to_string(),
            format!("Bearer {}", jwt.trim()),
        ),
        ("Content-Type".to_string(), "application/json".to_string()),
    ];
    if let Some(param) = Some(captcha_verify_param.trim()).filter(|value| !value.is_empty()) {
        headers.push((
            "X-Aliyun-Captcha-Verify-Param".to_string(),
            param.to_string(),
        ));
    }
    if let Some(value) = captcha_region
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        headers.push((
            "X-Aliyun-Captcha-Verify-Region".to_string(),
            value.to_string(),
        ));
    }
    // 领取走活动平面（billing/claim），与发现同口径用活动版本（3.18.0）；
    // 推理身份头的 3.16.2 只服务 anthropic 端点，两者不可混用。
    headers.push(("X-ZCode-App-Version".to_string(), activity_app_version()));
    headers.push(("X-Platform".to_string(), platform().to_string()));
    if let Some(mid) = device_mid.map(str::trim).filter(|value| !value.is_empty()) {
        headers.push(("X-Device-Mid".to_string(), mid.to_string()));
    }

    let body = json!({ "plan_id": plan_id });
    let response = send_raw(
        "POST",
        &url,
        Some(&body),
        &headers,
        proxy,
        Some(REQUEST_TIMEOUT_MS),
    )
    .await
    .map_err(|error| {
        if error.is_timeout() {
            GatewayError::with_status(504, "套餐领取超时")
        } else {
            GatewayError::with_status(502, format!("套餐领取失败: {error}"))
        }
    })?;

    let payload = response.payload.unwrap_or(Value::Null);
    let biz_code = payload.get("code").and_then(Value::as_i64);
    let data = payload.get("data");
    let plan = data.and_then(|value| value.get("plan"));

    // 成功：HTTP 2xx + biz code 0 + success 未明确为 false + data.plan 是对象。
    // 四者缺一不可（对齐上游 v3.4.1）：漏掉 success 会把 200 的业务失败当成功，
    // 漏掉 is_object 会把 plan:null 误判成领到（随后 ends_at 读不到，hold 时长错乱）。
    let explicit_failure = payload.get("success").and_then(Value::as_bool) == Some(false);
    if (200..300).contains(&response.status) && biz_code == Some(0) && !explicit_failure {
        if let Some(plan) = plan.filter(|value| value.is_object()) {
            return Ok(ClaimOutcome::Claimed {
                plan_id: plan_id.to_string(),
                starts_at: plan.get("starts_at").and_then(Value::as_i64),
                ends_at: plan.get("ends_at").and_then(Value::as_i64),
            });
        }
    }

    // 失败：HTTP 层错误（无业务码）与业务码错误分开归类 —— 前者只有状态码
    // 可依（401 判未登录，其余算 HTTP 错误），后者走 biz code 映射
    let failure = match biz_code {
        Some(code) => ClaimFailure::from_code(code),
        None if response.status == 401 => ClaimFailure::LoginRequired,
        None => ClaimFailure::HttpError,
    };
    // 上游在失败响应里也可能带 plan（含 ends_at），那就是「下一个可尝试窗口」
    let failure_ends_at = plan
        .and_then(|value| value.get("ends_at"))
        .and_then(Value::as_i64);
    Ok(ClaimOutcome::Failed {
        plan_id: plan_id.to_string(),
        failure,
        code: biz_code.unwrap_or(i64::from(response.status)),
        message: error_message(&payload, response.status),
        failure_ends_at,
    })
}

/// 从上游响应里取一句人话（参考实现 `unwrapError` 的同效处理）
fn error_message(payload: &Value, status: u16) -> String {
    for key in ["msg", "message"] {
        if let Some(text) = payload.get(key).and_then(Value::as_str) {
            let text = text.trim();
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    format!("HTTP {status}")
}

/// 上游 `plans[]` 里的一条 → [`ClaimablePlan`]（缺 `plan_id` 的条目丢弃）
fn parse_plan(raw: &Value) -> Option<ClaimablePlan> {
    let plan_id = raw
        .get("plan_id")
        .and_then(Value::as_str)?
        .trim()
        .to_string();
    if plan_id.is_empty() {
        return None;
    }
    let name = raw
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&plan_id)
        .to_string();
    let entitlements = raw
        .get("entitlements")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let show_name = item
                        .get("show_name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())?
                        .to_string();
                    Some(PlanEntitlement {
                        show_name,
                        unit_type: item
                            .get("unit_type")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .trim()
                            .to_string(),
                        grant_units: item.get("grant_units").and_then(Value::as_i64).unwrap_or(0),
                        period: item
                            .get("period")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .trim()
                            .to_string(),
                        // 生效时间（unix 秒）：品类同 [`unix_seconds_of`]。
                        // `0` 是官方文档里的「立即生效」哨兵值，原样带出 —— 判定在
                        // 余额侧（`balance::plan_effective_times` 只收 > 0 的）
                        effective_at: item.get("effective_at").and_then(unix_seconds_of),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(ClaimablePlan {
        plan_id,
        name,
        description: raw
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
        priority: raw.get("priority").and_then(Value::as_i64).unwrap_or(0),
        starts_at: raw.get("starts_at").and_then(unix_seconds_of),
        ends_at: raw.get("ends_at").and_then(unix_seconds_of),
        entitlements,
        // billing 链路的条目不挂营销活动；marketing 子模块解析出的条目会在
        // 合并前自行填入 campaign id（见 marketing::query）。
        marketing_campaign_ids: Vec::new(),
    })
}

/// 一个 unix 秒字段 → `i64`。
///
/// 上游这几个时间字段的官方类型是 `number | string | null`
/// （`ZaiStartPlanPlan` / `entitlements[].effective_at` 在官方源码里就是这么写的），
/// 只看 `as_i64` 会让**字符串形态**整条消失 —— 生效时间一丢，界面就退回
/// 「按领取时间 / 立即生效」猜，那正是「生效时间显示成领取时间」那类 bug 的来源。
/// 与余额侧的 `number_of` 同一条取值口径。
fn unix_seconds_of(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// 从一批可领套餐里挑出目标：指定了 `plan_id` 就取它，否则取优先级最高的。
///
/// 参考实现的选取口径：`planId` 配置为空时按 `priority` 降序取第一个。
/// 优先级相同的情形保持**上游给的顺序**（`max_by_key` 取最后一个的语义陷阱
/// 要避开：这里用显式比较，相等时不替换，于是先出现的那个胜出）。
pub fn pick_target<'a>(plans: &'a [ClaimablePlan], plan_id: &str) -> Option<&'a ClaimablePlan> {
    let wanted = plan_id.trim();
    if !wanted.is_empty() {
        return plans.iter().find(|plan| plan.plan_id == wanted);
    }
    let mut best: Option<&ClaimablePlan> = None;
    for plan in plans {
        match best {
            None => best = Some(plan),
            Some(current) if plan.priority > current.priority => best = Some(plan),
            Some(_) => {}
        }
    }
    best
}

/// 最小的 URL 查询参数编码（`url` crate 不在依赖里，这里只处理查询值）。
///
/// 版本号与平台标识都是 ASCII 字母数字与 `-` / `.`，理论上不需要转义；
/// 但 `ZCODE_APP_VERSION` 是用户可覆盖的，一个带 `&` 的值就能拼出畸形 URL。
/// 因此这里按 RFC 3986 的 unreserved 集合放行，其余一律百分号编码。
///
/// `pub(super)`：`oauth` 那边的中转页参数要把**整个地址**当成一个查询值编码
/// （见 `oauth::apply_interstitial`），同一套规则不必再写第二遍。
pub(super) fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~') {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod captcha_config_vectors {
    //! `configs.captcha` 的解析，钉在**抓下来的真实响应**上。
    //!
    //! 这一格现在是整套免码判定的唯一依据（`skip_model_request`），而它的形状
    //! 只有上游能告诉我们 —— 手写一份"想象中的响应"等于把猜测当证据。
    //! 载荷出处：2026-10-01 16:44 UTC 从 `https://zcode.z.ai/api/v1/client/configs`
    //! 抓的 captcha 段，四个组合（app_version 3.14.0/3.14.4 × platform
    //! darwin-arm64/linux-x64）返回逐字相同，所以这里只需一份。

    use serde_json::json;

    use super::parse_captcha_config;

    #[test]
    fn the_live_captcha_segment_parses_as_captured() {
        let config = parse_captcha_config(&json!({
            "code": 0,
            "msg": "",
            "data": {"configs": {"captcha": {
                "enabled": true, "prefix": "no8xfe", "region": "cn",
                "sceneId": "11xygtvd", "skip_model_request": true
            }}}
        }))
        .expect("这份响应是上游真实给过的");
        assert!(
            config.enabled,
            "领取那条路仍然要滑块 —— 它和推理免码是两件事"
        );
        assert_eq!("no8xfe", config.prefix);
        assert_eq!("11xygtvd", config.scene_id);
        assert_eq!("cn", config.region, "站点地区上游给了就别自己兜底成 ga");
        assert!(
            config.skip_model_request,
            "这一格是免码上线的全部依据，不能被解析成 false"
        );
    }

    #[test]
    fn an_absent_skip_flag_still_requires_a_token() {
        // 缺 `skip_model_request`（上游还没下发这一格）= 照旧要码。
        // 判反了的代价不对称：免码判错每条请求撞一次 3007（烧账号行为分），
        // 要码判错只是白铸一枚令牌。
        let config = parse_captcha_config(&json!({
            "data": {"configs": {"captcha": {
                "enabled": true, "prefix": "no8xfe", "sceneId": "11xygtvd"
            }}}
        }))
        .expect("prefix 与 sceneId 齐了就认这份配置");
        assert!(!config.skip_model_request);
        assert_eq!(
            "ga", config.region,
            "没给 region 时的兜底（阿里云默认站点）"
        );
    }

    #[test]
    fn an_incomplete_segment_is_not_a_config_at_all() {
        // 缺 sceneId 时前端连 SDK 都初始化不出来 —— 按「没有配置」处理，
        // 而不是给出一份字段为空的对象（调用方会拿它去弹滑块，然后当场失败）
        assert!(
            parse_captcha_config(&json!({"data": {"configs": {"captcha": {
                "enabled": true, "prefix": "no8xfe"
            }}}}))
            .is_none()
        );
        assert!(parse_captcha_config(&json!({"data": {"configs": {}} })).is_none());
        assert!(parse_captcha_config(&json!({})).is_none());
    }
}

#[cfg(test)]
mod discovery_vectors {
    //! 双链路合并的判据：同一 plan_id 的字段互补、营销 campaign id 汇总、去重与排序。

    use super::{merge_plans, ClaimablePlan};

    fn plan(id: &str, name: &str, priority: i64, campaigns: &[&str]) -> ClaimablePlan {
        ClaimablePlan {
            plan_id: id.to_string(),
            name: name.to_string(),
            description: String::new(),
            priority,
            starts_at: None,
            ends_at: None,
            entitlements: Vec::new(),
            marketing_campaign_ids: campaigns.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn billing_and_marketing_merge_into_one_plan_with_both_campaigns() {
        let mut target = vec![plan("p1", "", 0, &[])];
        merge_plans(&mut target, vec![plan("p1", "周末套餐", 9, &["c1"])]);
        merge_plans(&mut target, vec![plan("p1", "", 3, &["c2", "c1"])]);
        assert_eq!(1, target.len(), "同一 plan_id 始终只有一份");
        let p = &target[0];
        assert_eq!("周末套餐", p.name, "空名被补全");
        assert_eq!(9, p.priority, "优先级取最大");
        assert_eq!(
            vec!["c1".to_string(), "c2".to_string()],
            p.marketing_campaign_ids,
            "两条链路的 campaign id 汇总且去重"
        );
    }

    #[test]
    fn merged_plans_are_sorted_by_priority_desc() {
        let mut target = vec![plan("low", "低", 1, &[])];
        merge_plans(
            &mut target,
            vec![plan("high", "高", 5, &[]), plan("mid", "中", 3, &[])],
        );
        let order: Vec<&str> = target.iter().map(|p| p.plan_id.as_str()).collect();
        assert_eq!(vec!["high", "mid", "low"], order);
    }
}
