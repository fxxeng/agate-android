//! 豆包工作（`www.doubao.com`）的基址、路径常量、**桌面端伪装头**与风控 query 参数。
//!
//! ── 上游长什么样（证据分三级，逐条标注）─────────────────────────
//! 本模块的每一条常量都按下面的证据等级标注，**没有一条是从别家抄来的**：
//!
//! ```text
//!   [二进制] 官方插件 `agentpack_x/bin/agent2api-providers.exe` 的字符串表逐字提取
//!   [用户核实] 任务书里已核对一致的取值（风控参数清单、桌面端版本号）
//!   [Go参考] 本地第三方实现 `.workbuddy/tmp/dbw/internal/doubao/`（**网页版**伪装）
//!   [推断]   以上三处都没有、但上游协议要求必须给值时的取值（每条都写明依据与风险）
//! ```
//!
//! 端点（前三条是[二进制]里逐字出现的路径）：
//! ```text
//!   POST /chat/completion                                  主推理入口（SSE 流式）
//!   POST /alice/user/launch                                启动握手（拿会话/风控刷新）
//!   POST /alice/resource/prepare_upload                    申请上传凭证（STS）
//!   GET  /alice/commerce/sale/subscription/overview/       额度 / 订阅读数
//!   GET  /alice/ug/campaign/v3/detail/m2                   活动详情（签到态在这里）
//!   POST /alice/ug/campaign/item/reward/claim/<code>       领取奖励
//!   GET  /alice/slot/action_bar_v3/brief_list              能力槽位（目录线索之一）
//!   GET  /passport/account/info/v2/                        账号信息（登录取 userId）
//!   POST /api/biz/customer/getCustomerInfo                 企业客户信息
//!   POST /api/biz/v1/organization/<id>/projects/...        组织 / 项目（企业版）
//!   POST /api/v2/chat/completions                          备用推理入口（另一条通道）
//! ```
//!
//! ── 桌面端伪装：**这套头才是关键差异**（不是网页版那套）────────
//! 官方插件伪装的是**桌面客户端**（`DoubaoWork` 的 Chromium 外壳），而不是
//! 网页版。二进制里桌面端专有的 query 键与网页版公共键**混在同一张字符串表里**，
//! 这是本家最容易照错的点：
//!
//! ```text
//!   [二进制] clientVersion  version_code  web_platform  doubao_device_platform
//!            client_platform  pkg_type  runtime  runtime_version  pc_version
//!            doubao_pc_version  sys_region  web_id  webId  chromium_version
//!            chromiumVersion
//! ```
//! 注意 `web_id`/`webId`、`chromium_version`/`chromiumVersion` 是**同一含义的两种
//! 拼法同时出现**（网页版写 snake_case、桌面端写 camelCase）—— 因此本实现
//! **两套都发**，这是[二进制]直接支持的结论，不是猜的。
//!
//! UA 也是[二进制]逐字：
//! `Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko)
//! Chrome/147.0.0.0 Safari/537.36`。
//!
//! ── 与第三方 Go 参考（**网页版**伪装）的取值差异，逐条说明取舍 ──────
//! Go 参考的 `SecurityParams()` 用 `device_platform=web` / `runtime=web` /
//! `pc_version=2.1.7`。本实现**只借它的参数清单与结构，不借取值** ——
//! 取值按二进制那套（用户已在任务书里点明这一点）。凡取值存在分歧的地方，
//! 本文件都把两种取值都写下来并说明取了哪个、为什么。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::fmt::Write as _;

/// 默认基址（[二进制]：字符串表里的 `https://www.doubao.com`）
pub const DEFAULT_BASE_URL: &str = "https://www.doubao.com";

/// 推理入口路径（[二进制] `/chat/completion`，**没有 `/api` 前缀、没有版本段**）
pub const CHAT_COMPLETION_PATH: &str = "/chat/completion";

/// 启动握手路径（[二进制] `/alice/user/launch`）
pub const LAUNCH_PATH: &str = "/alice/user/launch";

/// 申请上传凭证路径（[二进制] `/alice/resource/prepare_upload`）
pub const PREPARE_UPLOAD_PATH: &str = "/alice/resource/prepare_upload";

/// 订阅 / 额度总览路径（[二进制]，**注意结尾带斜杠** —— 上游就是这么登记的）
pub const SUBSCRIPTION_OVERVIEW_PATH: &str = "/alice/commerce/sale/subscription/overview/";

/// 订阅入口配置路径（[二进制] `/alice/commerce/sale/subscription/entry/config`）。
///
/// 二进制里这一串与 `window_limit_section` / `enterprise_window_limit_section` /
/// `member_info` / `hasActiveSubscription` / `hasEnterpriseSubscription` /
/// `quota_package_section` / `incentive_quota_section` 紧邻 —— 那些是**响应字段名**，
/// 说明额度读数的字段形态就在这里（见 [`super::quota`] 的模块头）。
pub const SUBSCRIPTION_ENTRY_CONFIG_PATH: &str = "/alice/commerce/sale/subscription/entry/config";

/// 活动 / 签到详情路径（[二进制] `/alice/ug/campaign/v3/detail/m2`）。
///
/// `m2` 是**必需的路径段**（不是可选参数）：二进制里它以独立短串的形式紧跟
/// `/alice/ug/campaign/v3/detail/` 之后，与 `daily_checkin` / `checked_in_today` /
/// `task_status` 三个响应字段同处一表。
pub const CAMPAIGN_DETAIL_PATH: &str = "/alice/ug/campaign/v3/detail/m2";

/// 活动列表路径（[二进制] `/alice/ug/campaign/list/`，结尾带斜杠）
pub const CAMPAIGN_LIST_PATH: &str = "/alice/ug/campaign/list/";

/// 领取活动奖励路径的**前缀**（[二进制] `/alice/ug/campaign/item/reward/claim/`）。
///
/// 真实路径是前缀 + 奖励条目 id（`item_id` / `campaign_item_code`），
/// 因此这里给的是拼接用的常量而不是完整路径。
pub const CAMPAIGN_REWARD_CLAIM_PREFIX: &str = "/alice/ug/campaign/item/reward/claim/";

/// 能力槽位简报路径（[二进制] `/alice/slot/action_bar_v3/brief_list`）。
///
/// 二进制里它与 `default_select_mode_map` / `new_chat` / `agent-` /
/// `default_mode` 紧邻 —— 那就是**目录线索**：`mode_list` / `model_list`
/// 这一族键说明本家的「模型」以**模式**形态下发（见 [`super::models`] 的模块头）。
pub const ACTION_BAR_BRIEF_LIST_PATH: &str = "/alice/slot/action_bar_v3/brief_list";

/// 账号信息路径（[二进制] `/passport/account/info/v2/`，结尾带斜杠）
pub const ACCOUNT_INFO_PATH: &str = "/passport/account/info/v2/";

/// 企业客户信息路径（[二进制] `/api/biz/customer/getCustomerInfo`）
pub const CUSTOMER_INFO_PATH: &str = "/api/biz/customer/getCustomerInfo";

/// 组织 / 项目路径的**前缀**（[二进制] `/api/biz/v1/organization/`，企业版）
pub const ORGANIZATION_PATH_PREFIX: &str = "/api/biz/v1/organization/";

/// 备用推理入口（[二进制] `/api/v2/chat/completions`）。
///
/// 本实现**不走它**（主入口是 [`CHAT_COMPLETION_PATH`]）：它是另一条通道
/// （OpenAI 形态的路径命名，二进制里与 `remainQuota` / `totalQuota` /
/// `departmentFullName` / `refreshExpiresAt` 同处，看着是企业版的网关）。
/// 留在常量表里是为了「下次核对端点时不必再翻一遍二进制」。
#[allow(dead_code)]
pub const V2_CHAT_COMPLETIONS_PATH: &str = "/api/v2/chat/completions";

/// imageX 上传网关路径前缀（[二进制] `/top/v1`，见 [`super::upload`]）
pub const IMAGEX_TOP_PATH: &str = "/top/v1";

// ─── 桌面端伪装常量（取值来源逐条标注）──────────────────────────

/// 桌面客户端版本（[二进制] `clientVersion` 的值，逐字 `2.33.5`）
pub const CLIENT_VERSION: &str = "2.33.5";

/// 桌面端内嵌 Chromium 在 UA 里的版本（[二进制] UA 串里逐字是 `Chrome/147.0.0.0`）。
///
/// ⚠️ 与 [`CHROMIUM_VERSION`] **不是一个值**，这是刻意的：UA 里是
/// `147.0.0.0`（Chromium 对外报的简化版号），`chromium_version` 参数里是完整
/// 四段版本 `147.0.7727.149`（用户核实的官方值）。两处混用会让「自称的
/// 内核版本」与「UA 里的内核版本」对不上，那本身就是可观测的破绽。
pub const UA_CHROMIUM_VERSION: &str = "147.0.0.0";

/// `chromium_version` / `chromiumVersion` 参数的取值（[用户核实] `147.0.7727.149`）
pub const CHROMIUM_VERSION: &str = "147.0.7727.149";

/// 桌面端 User-Agent（[二进制] 逐字，Windows + 上文那个 Chromium 版本）
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
                              AppleWebKit/537.36 (KHTML, like Gecko) \
                              Chrome/147.0.0.0 Safari/537.36";

/// 中台 appId（[二进制] `aid582478real_aid` 里逐字出现 `582478`）。
///
/// `aid` 与 `real_aid` **同值**（[Go参考] 的 `set("aid", AppID)` /
/// `set("real_aid", AppID)` 也是这么发的）。
pub const APP_ID: &str = "582478";

/// 账号 SDK 版本（[二进制] `sdk_version2.2.5`）
pub const ACCOUNT_SDK_VERSION: &str = "2.2.5";

/// 账号 SDK 来源（[二进制] `account_sdk_source` 键紧随 `sdk_version` 之后，
/// 同处一表的取值短串是 `webview`）
pub const ACCOUNT_SDK_SOURCE: &str = "webview";

/// `runtime` 参数的取值（[二进制] `webviewchromedoubaowork-chat` 三个短串连排，
/// 取 `chrome` 作 runtime、`doubaowork-chat` 作应用标识）。
///
/// ⚠️ 与 [Go参考] 的 `runtime=web` **不同**（那是网页版）。用户明确要求用
/// 官方二进制那套，这里取 `chrome`。
pub const RUNTIME: &str = "chrome";

/// 应用标识（[二进制] `doubaowork-chat`，随 `runtime` 一起出现）
pub const APP_RUNTIME_ID: &str = "doubaowork-chat";

/// `pkg_type` 的取值（[Go参考] `release_version`；二进制里该键在表内但取值
/// 未单独出现，故沿用参考实现 —— 它是**发行渠道**标识，两端一致）
pub const PKG_TYPE: &str = "release_version";

/// `version_code` 的取值（[Go参考] `20800`；二进制里未直接出现）。
///
/// 与 [`CLIENT_VERSION`] 的关系：`version_code` 是**构建号**、`clientVersion`
/// 是**语义版本**，两者必须同源（同一版客户端）。这里保留参考实现给的构建号，
/// 可由环境变量 `DOUBAOWORK_VERSION_CODE` 覆盖 —— 上游换版时会用到。
pub const VERSION_CODE: &str = "20800";

/// `pc_version` 的取值。桌面端与网页版**共用键名但取值不同**：
///   - [Go参考]（网页版）：`2.1.7`
///   - 桌面端：本实现取 [`CLIENT_VERSION`]（`2.33.5`）
///
/// 依据：二进制里 `pc_version` 与 `doubao_pc_version` **相邻**（同一组两个键），
/// 而 `doubao_pc_version` 显然是「豆包 PC 版本」，即 [`CLIENT_VERSION`]。
/// 因此 `pc_version` 取同值最自洽 —— 这是[推断]，可由环境变量覆盖。
pub const PC_VERSION: &str = CLIENT_VERSION;

/// `doubao_pc_version` 的取值（[二进制] 键逐字出现；取 [`CLIENT_VERSION`]，
/// 理由见 [`PC_VERSION`]）
pub const DOUBAO_PC_VERSION: &str = CLIENT_VERSION;

/// `web_platform` 的取值（[二进制] 键逐字出现；取 `web`，与内核形态一致）。
///
/// [推断]：二进制只给了键、没给取值。取 `web` 而不是 `pc` 是因为这是
/// **渲染平台**（Chromium webview）而不是操作系统平台 —— 后者已由
/// [`DOUBAO_DEVICE_PLATFORM`] 表达。可由环境变量覆盖。
pub const WEB_PLATFORM: &str = "web";

/// `device_platform` / `doubao_device_platform` 的取值（[二进制] 键逐字出现；
/// 取 `windows`）。
///
/// ⚠️ 与 [Go参考] 的 `device_platform=web` **不同**（那是网页版）。桌面端报的
/// 是真实操作系统平台（UA 也是 Windows），这里取 `windows`。[推断]，可覆盖。
pub const DOUBAO_DEVICE_PLATFORM: &str = "windows";

/// `client_platform` 的取值（[Go参考] `pc_client`；二进制里键逐字出现但取值
/// 未单独出现）。
///
/// 取 `pc_client` 而不是 `web`：本家伪装的是桌面客户端，`pc_client` 与
/// [`DOUBAO_DEVICE_PLATFORM`] 同族。可由环境变量覆盖。
pub const CLIENT_PLATFORM: &str = "pc_client";

/// `runtime_version` 的取值（[Go参考] `3.5.4`，samantha runtime 版本）。
///
/// 二进制里 `runtime_version` 键逐字出现但取值未单独出现；参考实现给的值是
/// 唯一可用的依据。可由环境变量覆盖。
pub const RUNTIME_VERSION: &str = "3.5.4";

/// 语言（[Go参考] `zh`，与桌面端一致）
pub const LANGUAGE: &str = "zh";

/// 地区（[Go参考] `CN`）
pub const REGION: &str = "CN";

/// 系统地区（[Go参考] `CN`）
pub const SYS_REGION: &str = "CN";

/// 本家各接口的默认超时（毫秒）。
///
/// 与别家的 30 秒同档：额度 / 活动这类控制面接口在弱网下偶有 2~3 秒的抖动，
/// 给得太紧会把一次本可成功的查询变成「上游不可达」。
pub const REQUEST_TIMEOUT_MS: u64 = 30_000;

/// 目录 / 额度这类**维护动作**的超时（毫秒）：不值得让用户等太久
pub const CATALOG_TIMEOUT_MS: u64 = 20_000;

/// 上传相关请求的超时（毫秒）：要传字节，给得宽一些
pub const UPLOAD_TIMEOUT_MS: u64 = 60_000;

/// 读环境变量覆盖（空值视为未设置），去掉尾部斜杠。
///
/// ⚠️ 与别家的 `env_override` 相比**多一步大写归一**：桌面端那些版本常量在
/// 上游换版时会整体替换，运维现场手抄时大小写经常不一致（`147.0.7727.149`
/// 与 `147.0.7727.149` 肉眼无差，但误敲成别的字符就发不出去了）。这里只做
/// 去空白与去尾斜杠，**不做大小写变换** —— 版本号本身大小写无关，但 cookie 之类
/// 会经过同一函数的调用点（本模块只用于基址与版本，故安全）。
fn env_override(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

/// 基址：`DOUBAOWORK_BASE_URL` / `DOUBAO_BASE_URL` 可覆盖，前者优先。
pub fn base_url() -> String {
    env_override("DOUBAOWORK_BASE_URL")
        .or_else(|| env_override("DOUBAO_BASE_URL"))
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
}

/// 桌面客户端版本：`DOUBAOWORK_CLIENT_VERSION` 可覆盖
/// （上游换版时不必改代码，也不必等新构建）
pub fn client_version() -> String {
    env_override("DOUBAOWORK_CLIENT_VERSION").unwrap_or_else(|| CLIENT_VERSION.to_string())
}

/// `version_code`：`DOUBAOWORK_VERSION_CODE` 可覆盖
pub fn version_code() -> String {
    env_override("DOUBAOWORK_VERSION_CODE").unwrap_or_else(|| VERSION_CODE.to_string())
}

/// `pc_version` / `doubao_pc_version`：`DOUBAOWORK_PC_VERSION` 可覆盖
pub fn pc_version() -> String {
    env_override("DOUBAOWORK_PC_VERSION").unwrap_or_else(|| PC_VERSION.to_string())
}

/// `chromium_version`：`DOUBAOWORK_CHROMIUM_VERSION` 可覆盖
pub fn chromium_version() -> String {
    env_override("DOUBAOWORK_CHROMIUM_VERSION").unwrap_or_else(|| CHROMIUM_VERSION.to_string())
}

/// 全 URL（基址 + 路径）。
///
/// 只做「`/` 拼 `/`」这一种归一（基址句尾斜杠已在 [`env_override`] 去掉），
/// 不做其它改写：路径里的**结尾斜杠是有意义的**（见 [`SUBSCRIPTION_OVERVIEW_PATH`] /
/// [`ACCOUNT_INFO_PATH`]），任何「顺手规范化」都会把它们改坏。
pub fn url(path: &str) -> String {
    format!("{}{path}", base_url())
}

/// 推理入口全 URL（[`CHAT_COMPLETION_PATH`]）
pub fn chat_url() -> String {
    url(CHAT_COMPLETION_PATH)
}

/// 启动握手全 URL
pub fn launch_url() -> String {
    url(LAUNCH_PATH)
}

/// 申请上传凭证全 URL
pub fn prepare_upload_url() -> String {
    url(PREPARE_UPLOAD_PATH)
}

/// 订阅 / 额度总览全 URL
pub fn subscription_overview_url() -> String {
    url(SUBSCRIPTION_OVERVIEW_PATH)
}

/// 订阅入口配置全 URL
pub fn subscription_entry_config_url() -> String {
    url(SUBSCRIPTION_ENTRY_CONFIG_PATH)
}

/// 活动详情全 URL（签到态读它）
pub fn campaign_detail_url() -> String {
    url(CAMPAIGN_DETAIL_PATH)
}

/// 活动列表全 URL
pub fn campaign_list_url() -> String {
    url(CAMPAIGN_LIST_PATH)
}

/// 领取奖励全 URL（`item_id` 由调用方给，会被转义）
pub fn reward_claim_url(item_id: &str) -> String {
    format!(
        "{}{}{}",
        base_url(),
        CAMPAIGN_REWARD_CLAIM_PREFIX,
        urlencode(item_id)
    )
}

/// 能力槽位简报全 URL
pub fn action_bar_brief_list_url() -> String {
    url(ACTION_BAR_BRIEF_LIST_PATH)
}

/// 账号信息全 URL
pub fn account_info_url() -> String {
    url(ACCOUNT_INFO_PATH)
}

/// imageX 上传网关全 URL
pub fn imagex_top_url() -> String {
    url(IMAGEX_TOP_PATH)
}

/// 极小的 query 转义（本家的 item_id / campaign_code 都是 ASCII 记号）。
///
/// 不引 `urlencoding` 依赖：输入来自上游返回的条目 id，但仍按 RFC3986 转义 ——
/// 一旦 id 里出现 `&` / `=` / `?`，不转义会当场把 query 截断（邀请码这类
/// 上游自己拼的串完全可能带上保留字符）。
pub fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => {
                // `write!` 到 String 不会失败（String 的 fmt::Write 实现是
                // 无错的），因此这里不需要处理 Result —— 用 `let _ =` 明确
                // 表达「有意忽略」，与 `panic=abort` 下不能 unwrap 的约束一致。
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// 一份账号的**风控指纹**（query 参数里「跟谁在说话」的那几个值）。
///
/// 它们不随请求变化（同一账号恒用同一组），因此随账号落盘（见
/// [`super::credentials`]）。缺值时由 [`Fingerprint::derive`] 现造一组
/// —— 上游对「一个账号一直在用同一组指纹」是认的，对「同一账号每请求换指纹」
/// 才会风控。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fingerprint {
    /// `device_id` / `tea_uuid`（两者同值；[Go参考] 的 `a.DeviceID` 就是这么发的）
    pub device_id: String,
    /// `web_id` / `webId`
    pub web_id: String,
    /// `fp`
    pub fp: String,
    /// `msToken`（可空：有的账号 Cookie 里没带）
    pub ms_token: String,
}

impl Fingerprint {
    /// 从账号记录里读指纹；缺失的项**就地从设备标识派生**。
    ///
    /// ── 为什么缺失时是「派生」而不是「报错」─────────────────────
    /// 粘贴 Cookie 的用户手里往往只有一条 `sessionid`，拿不到桌面端写在
    /// Local Storage 里的 `device_id` / `web_id`（[Go参考] 的
    /// `readDeviceIDs` 要靠 regexp 从 leveldb 里挖）。此时**不造一组值就发不出
    /// 请求**；而「报错让用户自己去找 device_id」是一条没人走得通的路。
    /// 因此按需求量级现造：`device_id` 15 位数字、`web_id` 19 位数字
    /// （[Go参考] `RandomDigits(15)` / `RandomDigits(19)`）、`fp` 按
    /// [`default_fp`] 的形态。**一旦账号里已有值就原样沿用**，不覆盖 ——
    /// 「同一账号换指纹」才是风控信号。
    pub fn derive(
        device_id: Option<&str>,
        web_id: Option<&str>,
        fp: Option<&str>,
        ms_token: Option<&str>,
    ) -> Self {
        let pick = |value: Option<&str>, generator: fn() -> String| {
            value
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .unwrap_or_else(generator)
        };
        Self {
            device_id: pick(device_id, || random_digits(15)),
            web_id: pick(web_id, || random_digits(19)),
            fp: pick(fp, default_fp),
            // msToken 缺失时**留空**：它是 Cookie 里带出来的东西，
            // 造一个假的发上去只会让上游的校验看起来「更可疑」
            ms_token: ms_token
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .unwrap_or("")
                .to_string(),
        }
    }
}

/// 风控 query 参数（键序 = 发送序，见模块头关于「两种拼法都发」的说明）。
///
/// ── 为什么返回 `Vec<(String,String)>` 而不是 `HashMap` ──────────
/// 上游对 query 参数**不要求顺序**，但排障时要能一眼看出「这次到底发了哪些参数」
/// —— 详细日志里打印一个乱序 map 与打印一张固定顺序的表，可读性差一个量级。
/// 而且 `HashMap` 迭代序随进程随机，两次请求的日志对不上，事后无法 diff。
pub fn security_params(fingerprint: &Fingerprint) -> Vec<(String, String)> {
    let device_id = fingerprint.device_id.as_str();
    let web_id = fingerprint.web_id.as_str();
    let fp = fingerprint.fp.as_str();
    let tab_id = uuid_v4();

    let mut params: Vec<(String, String)> = vec![
        // ── 中台标识 ──
        ("aid".to_string(), APP_ID.to_string()),
        ("real_aid".to_string(), APP_ID.to_string()),
        // ── 设备指纹（device_id 与 tea_uuid 同值，[Go参考] 同款）──
        ("device_id".to_string(), device_id.to_string()),
        ("tea_uuid".to_string(), device_id.to_string()),
        ("web_id".to_string(), web_id.to_string()),
        ("webId".to_string(), web_id.to_string()),
        // ── 平台 / 语言 / 地区 ──
        ("device_platform".to_string(), DOUBAO_DEVICE_PLATFORM.to_string()),
        ("doubao_device_platform".to_string(), DOUBAO_DEVICE_PLATFORM.to_string()),
        ("client_platform".to_string(), CLIENT_PLATFORM.to_string()),
        ("web_platform".to_string(), WEB_PLATFORM.to_string()),
        ("language".to_string(), LANGUAGE.to_string()),
        ("region".to_string(), REGION.to_string()),
        ("sys_region".to_string(), SYS_REGION.to_string()),
        // ── 版本（桌面端那套）──
        ("pkg_type".to_string(), PKG_TYPE.to_string()),
        ("version_code".to_string(), version_code()),
        ("clientVersion".to_string(), client_version()),
        ("pc_version".to_string(), pc_version()),
        ("doubao_pc_version".to_string(), pc_version()),
        ("chromium_version".to_string(), chromium_version()),
        ("chromiumVersion".to_string(), chromium_version()),
        ("runtime".to_string(), RUNTIME.to_string()),
        ("runtime_version".to_string(), RUNTIME_VERSION.to_string()),
        // ── 开关位 ──
        ("samantha_web".to_string(), "1".to_string()),
        ("use-olympus-account".to_string(), "1".to_string()),
        // ── 风控指纹 ──
        ("fp".to_string(), fp.to_string()),
        ("web_tab_id".to_string(), tab_id),
    ];
    // msToken 在 Cookie 里没带时**整键不发**（[Go参考] 也是 `if a.MsToken != ""`）：
    // 发一个空值等于宣称「我有一个空的 msToken」，与「没有」是两件事。
    if !fingerprint.ms_token.is_empty() {
        params.push(("msToken".to_string(), fingerprint.ms_token.clone()));
    }
    params
}

/// 把 query 参数拼成 `k=v&k=v`（**不排序**，保持 [`security_params`] 的键序）。
///
/// 与 SigV4 那条链上的 `encodeQuery`（排序）刻意不同：这里拼的是**业务 query**，
/// 顺序无语义、但可读性有值（见 [`security_params`]）。
pub fn encode_query(params: &[(String, String)]) -> String {
    params
        .iter()
        .map(|(key, value)| format!("{}={}", urlencode(key), urlencode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// 一家共用的基础请求头（`Cookie` 之外的那些）。
///
/// `Accept` 给 `application/json`，SSE 调用点会**覆盖**成 `text/event-stream`
/// （见 [`super::chat`]）—— 与 `send_raw` 的默认头同值，这里显式给出是为了
/// 让「本家到底发了哪些头」在一处可读。
pub fn base_headers() -> Vec<(String, String)> {
    vec![
        ("User-Agent".to_string(), USER_AGENT.to_string()),
        ("Origin".to_string(), base_url()),
        ("Referer".to_string(), format!("{}/chat/", base_url())),
        ("Accept-Language".to_string(), "zh-CN,zh;q=0.9".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
    ]
}

/// SSE 请求所需的头（`Accept` 必须是 `text/event-stream`，否则上游不走流）
pub fn sse_headers() -> Vec<(String, String)> {
    vec![
        ("Accept".to_string(), "text/event-stream".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
    ]
}

/// JSON 请求体所需的头
pub fn json_headers() -> Vec<(String, String)> {
    vec![("Content-Type".to_string(), "application/json".to_string())]
}

/// 鉴权头：整串 Cookie（本家**不用** `Authorization: Bearer`）。
///
/// ── 与第三方 Go 参考的一处关键差异（必须写下来）────────────────
/// [Go参考] 的注释里提到用 `Authorization: Bearer <sessionid>`
/// （`cookie_windows.go` 那族代码里 `Cookie` / `doubao.com` / `www.doubao.com`
/// 三个串是取 Cookie 用的）。**本实现发的是完整 Cookie 头**，因为：
///   - 二进制里的账号表里逐字出现在 `Cookie ` / `Cookie:` / ` sessionid=` /
///     `; sessionid_ss=` / `Cookie JSON` 这几串，说明桌面端发的是 Cookie
///     整串（`sessionid` 只是其中最必需的一项，`sessionid_ss` 是备用名）；
///   - 只有 `sessionid` 时上游无法校验 `passport_csrf_token` 一类的伴随项，
///     而桌面端 Cookie 里本来就有它们。
/// `csrf` 是可选头（二进制里该头名未出现，取自[Go参考] 的 `passport_csrf_token`
/// 读法）：有就带上，没有就不带 —— 见 [`super::credentials::Credentials::csrf_token`]。
pub fn auth_headers(cookie_header: &str, csrf_token: Option<&str>) -> Vec<(String, String)> {
    let mut headers = vec![("Cookie".to_string(), cookie_header.to_string())];
    if let Some(token) = csrf_token.map(str::trim).filter(|text| !text.is_empty()) {
        headers.push(("x-tt-passport-csrf-token".to_string(), token.to_string()));
    }
    headers
}

/// 生成一个标准 UUID v4（`block_id` / `local_message_id` / `web_tab_id` 都要它）。
///
/// ── 为什么必须现造而不是复用别家的 id 生成器 ────────────────────
/// 上游信封里的这些 id 是**客户端生成的**（`local_message_id` 这个名字本身就
/// 说明「本地生成、不由服务端分配」），形态是标准 UUID（含连字符、版本位与
/// 变体位）。`protocol::random_id` 给的是 `<prefix>_<32hex>` 形态（那是 OpenAI
/// 那套协议的 id 风格），形态不对。
///
/// 随机源走 `getrandom`（本项目已是直接依赖）；失败时退回时间戳拼的**合法
/// UUID 形态**（版本位仍置对）—— 宁可唯一性降级，也不要生成一个形态非法的值
/// （上游对 `local_message_id` 的解析一旦失败，整条消息可能被丢弃）。
pub fn uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::getrandom(&mut bytes).is_err() {
        // 兜底：毫秒时间戳铺满前半段，后半段用一次线性同余展开 ——
        // 只为「形态合法 + 大概率不重」，不声称密码学强度。
        let now = crate::server::logging::now_ms() as u64;
        let mut state = now ^ 0x9E37_79B9_7F4A_7C15;
        for (index, slot) in bytes.iter_mut().enumerate() {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let base = if index < 8 {
                (now >> (index * 8)) as u8
            } else {
                (state >> 32) as u8
            };
            *slot = base;
        }
    }
    // RFC4122 v4：版本位 0100、变体位 10
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

/// `n` 位随机十进制数字串（首位非 0）——设备指纹占位用（[Go参考] `RandomDigits`）。
pub fn random_digits(n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let mut bytes = vec![0u8; n];
    if getrandom::getrandom(&mut bytes).is_err() {
        // 兜底：时间戳逐位取模。精度低但形态合法（这是指纹占位，不是凭据）。
        let mut out = String::with_capacity(n);
        let mut state = crate::server::logging::now_ms() as u64;
        for index in 0..n {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let digit = if index == 0 {
                1 + (state >> 33) % 9
            } else {
                (state >> 33) % 10
            };
            out.push(char::from(b'0' + digit as u8));
        }
        return out;
    }
    let mut out = String::with_capacity(n);
    out.push(char::from(b'1' + bytes[0] % 9));
    for byte in bytes.iter().skip(1) {
        out.push(char::from(b'0' + byte % 10));
    }
    out
}

/// `n` 字节的随机十六进制串（[Go参考] `RandomHex`）。
/// 生成 `n` 个**十六进制字符**的随机串（**不是 n 个字节**）。
///
/// ── 为什么参数口径必须是「字符数」（踩过）──────────────────────
/// 早先这里是「取 `n` 个随机字节、每个字节渲染成两位 hex」⇒ 实际产出
/// **`2n` 个字符**。于是 `default_fp()` 的 `random_hex(5)` 给的是 10 个字符，
/// 而它自己的文档写着 `<hex5>`（5 个字符）—— 文档与实现当场矛盾，而且两者
/// 都「看起来对」。这类错不会崩、不会报错，只会让指纹形状与参考实现不一致
/// （被上游风控侧悄悄判成「不像官方客户端」）。
///
/// 现在参数就是**字符数**（`(n + 1) / 2` 个字节，奇数时多取半字节），
/// 与 `fp` / `local_conversation_id` 各调用点的直觉一致。
pub fn random_hex(n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let bytes_needed = n.div_ceil(2);
    let mut bytes = vec![0u8; bytes_needed];
    if getrandom::getrandom(&mut bytes).is_err() {
        // 随机源不可用（极罕见）：用时间戳兜底，**仍按字符数截断**
        let fallback = format!("{:x}", crate::server::logging::now_ms());
        return fallback.chars().take(n).collect();
    }
    let mut out = String::with_capacity(bytes_needed * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out.truncate(n);
    out
}

/// `fp` 指纹的占位形态（[Go参考] `defaultFP` 的逐字复刻）：
/// `verify_<hex5>_<hex5>_<hex4>_<hex4>_<hex4>_<hex10>`。
///
/// 上游只把 `fp` 当作「这台设备的一个稳定标识」，形态要求就是这一段固定结构。
/// 缺省账号（只粘了 Cookie 的用户）用它现造一组。
pub fn default_fp() -> String {
    format!(
        "verify_{}_{}_{}_{}_{}_{}",
        random_hex(5),
        random_hex(5),
        random_hex(4),
        random_hex(4),
        random_hex(4),
        random_hex(10),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 路径拼接：结尾斜杠**必须保留**（上游就是这么登记的，去掉会被 404）
    #[test]
    fn paths_keep_their_trailing_slashes() {
        assert!(subscription_overview_url().ends_with("/overview/"));
        assert!(account_info_url().ends_with("/info/v2/"));
        assert!(campaign_list_url().ends_with("/list/"));
        // 不带斜杠的那几个也要原样
        assert_eq!(
            chat_url(),
            "https://www.doubao.com/chat/completion"
        );
        assert!(prepare_upload_url().ends_with("/alice/resource/prepare_upload"));
    }

    /// ⚠️ UA 里的 Chromium 版本与 `chromium_version` 参数**不是一个值**（见常量注释）
    #[test]
    fn ua_version_and_param_version_stay_distinct() {
        assert!(USER_AGENT.contains("Chrome/147.0.0.0"));
        assert_eq!(CHROMIUM_VERSION, "147.0.7727.149");
        assert!(!USER_AGENT.contains(CHROMIUM_VERSION));
    }

    /// 风控参数：桌面端专有键与网页版公共键**都在**，且两种拼法都发
    #[test]
    fn security_params_carry_both_spellings() {
        let fingerprint = Fingerprint {
            device_id: "111111111111111".to_string(),
            web_id: "2222222222222222222".to_string(),
            fp: "verify_aaaaa_bbbbb_cccc_dddd_eeee_ffffffffff".to_string(),
            ms_token: "tok".to_string(),
        };
        let params = security_params(&fingerprint);
        let keys: Vec<&str> = params.iter().map(|(key, _)| key.as_str()).collect();
        for required in [
            "aid", "real_aid", "device_id", "tea_uuid", "web_id", "webId",
            "device_platform", "language", "region", "sys_region", "pkg_type",
            "version_code", "clientVersion", "pc_version", "doubao_pc_version",
            "chromium_version", "chromiumVersion", "client_platform", "runtime",
            "runtime_version", "samantha_web", "use-olympus-account", "fp",
            "web_tab_id", "msToken", "doubao_device_platform", "web_platform",
        ] {
            assert!(keys.contains(&required), "缺少风控参数：{required}");
        }
        // device_id 与 tea_uuid 同值（参考实现的约定）
        let device = params.iter().find(|(k, _)| k.as_str() == "device_id").map(|(_, v)| v.clone());
        let tea = params.iter().find(|(k, _)| k.as_str() == "tea_uuid").map(|(_, v)| v.clone());
        assert_eq!(device, tea);
        // web_id 与 webId 同值
        let web = params.iter().find(|(k, _)| k.as_str() == "web_id").map(|(_, v)| v.clone());
        let web_camel = params.iter().find(|(k, _)| k.as_str() == "webId").map(|(_, v)| v.clone());
        assert_eq!(web, web_camel);
        // web_tab_id 是 UUID 形态（每次不同）
        let tab = params
            .iter()
            .find(|(k, _)| k.as_str() == "web_tab_id")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        assert_eq!(tab.len(), 36);
        assert_eq!(tab.chars().filter(|ch| *ch == '-').count(), 4);
    }

    /// `msToken` 为空时**整键不发**（不等于发一个空值）
    #[test]
    fn ms_token_key_is_omitted_when_absent() {
        let fingerprint = Fingerprint {
            device_id: "1".to_string(),
            web_id: "2".to_string(),
            fp: "verify_x".to_string(),
            ms_token: String::new(),
        };
        let keys: Vec<String> = security_params(&fingerprint)
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert!(!keys.iter().any(|key| key.as_str() == "msToken"));
    }

    /// 指纹派生：**已有的值原样沿用**（同账号换指纹才是风控信号），缺失才现造
    #[test]
    fn fingerprint_derive_keeps_existing_values() {
        let kept = Fingerprint::derive(Some("dev"), Some("web"), Some("fp"), Some("ms"));
        assert_eq!(kept.device_id, "dev");
        assert_eq!(kept.web_id, "web");
        assert_eq!(kept.fp, "fp");
        assert_eq!(kept.ms_token, "ms");
        // 空白视为缺失
        let made = Fingerprint::derive(Some("  "), None, None, None);
        assert_eq!(made.device_id.len(), 15);
        assert_eq!(made.web_id.len(), 19);
        assert!(made.fp.starts_with("verify_"));
        assert!(made.ms_token.is_empty());
    }

    /// UUID 形态：36 字符、4 个连字符、版本位 4、变体位合法
    #[test]
    fn uuid_has_rfc4122_shape() {
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 4);
        assert_eq!(parts[3].len(), 4);
        assert_eq!(parts[4].len(), 12);
        assert_eq!(parts[2].chars().next(), Some('4'));
        assert!(matches!(parts[3].chars().next(), Some('8' | '9' | 'a' | 'b')));
        assert!(id.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-'));
    }

    /// 随机数字：位数对、首位非 0、全是数字
    #[test]
    fn random_digits_shape() {
        let digits = random_digits(19);
        assert_eq!(digits.len(), 19);
        assert!(digits.chars().all(|ch| ch.is_ascii_digit()));
        assert_ne!(digits.chars().next(), Some('0'));
        assert_eq!(random_digits(0), "");
    }

    /// query 转义：保留字符必须转义（不转义会截断 query）
    #[test]
    fn urlencode_escapes_reserved_characters() {
        assert_eq!(urlencode("a&b=c"), "a%26b%3Dc");
        assert_eq!(urlencode("ok-_.~"), "ok-_.~");
        assert_eq!(urlencode("中文"), "%E4%B8%AD%E6%96%87");
        // reward claim 路径上的条目 id 会被转义
        let url = reward_claim_url("id&x=1");
        assert!(url.ends_with("/alice/ug/campaign/item/reward/claim/id%26x%3D1"));
    }

    /// cookie 头与 csrf 头：csrf 空时**不发**该头
    #[test]
    fn auth_headers_include_csrf_only_when_present() {
        let without = auth_headers("sessionid=abc", None);
        assert_eq!(without.len(), 1);
        assert_eq!(without[0].0, "Cookie");
        let with = auth_headers("sessionid=abc", Some("  csrf-token  "));
        assert_eq!(with.len(), 2);
        assert_eq!(with[1].1, "csrf-token");
        // 空白等价于没有
        assert_eq!(auth_headers("sessionid=abc", Some("   ")).len(), 1);
    }

    /// `fp` 形态：`verify_` 前缀 + 6 段下划线分隔（[Go参考] `defaultFP` 的逐字复刻）
    #[test]
    fn default_fp_shape_matches_reference() {
        let fp = default_fp();
        assert!(fp.starts_with("verify_"));
        let parts: Vec<&str> = fp.trim_start_matches("verify_").split('_').collect();
        assert_eq!(parts.len(), 6);
        assert_eq!(parts[0].len(), 5);
        assert_eq!(parts[1].len(), 5);
        assert_eq!(parts[2].len(), 4);
        assert_eq!(parts[3].len(), 4);
        assert_eq!(parts[4].len(), 4);
        assert_eq!(parts[5].len(), 10);
    }

    /// encode_query：键序保持（排障时两次请求要能 diff）
    #[test]
    fn encode_query_preserves_order() {
        let params = vec![
            ("b".to_string(), "2".to_string()),
            ("a".to_string(), "1".to_string()),
        ];
        assert_eq!(encode_query(&params), "b=2&a=1");
    }
}
