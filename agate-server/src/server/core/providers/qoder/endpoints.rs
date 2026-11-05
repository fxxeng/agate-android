//! Qoder / 千问办公的**站点**定义；站点不接受任意 URL。
//!
//! ── 站点即 provider 身份（2026-10 拆家 → 2026-11 扩到千问办公）────────
//! 与 AutoClaw / Accio / ZCode / ModelScope 同款：一个站点是**一家 provider**。
//!
//! | 站点 | provider id | 产品 | 版本 |
//! |---|---|---|---|
//! | [`Site::QoderCn`] | `qoder` | Qoder | 中国版 |
//! | [`Site::QoderIntl`] | `qoder-intl` | Qoder | 国际版 |
//! | [`Site::QwenworkCn`] | `qwenwork` | 千问办公 | 国内版 |
//!
//! 站点 → 身份的互查只在本文件（`kind` / `provider_id` / `from_provider_id`），
//! **别处不要再写 `"qwenwork"` 这类字面量**。存量身份只增不改：
//! `qoder`（中国版）当年不带后缀，`qwenwork`（国内版）沿用网关停用清单里的
//! 那一个 id，存量账号因此原地复活而不是变成另一个家。
//!
//! ── 为什么没有 `qwenwork-intl`（国际版）──────────────────────
//! 上游 3.4.1 的插件里有四个站点，第四个是千问办公国际版
//! （`gateway.qwenwork.ai`）。它**没有**移植过来，原因不在协议：国际站的
//! 授权页只接受它自己登记的回调地址，设备码入口又被上游整体关掉，用户点
//! 「添加账号」会稳定失败。上游因此把它放进了停用名单
//! （`core/providers/availability.rs::SUSPENDED_IDS`）。我们这里少一个永远
//! 构造不出来的枚举分支，比留一个「能选但一定失败」的入口诚实。
//! 上游哪天放开回调白名单，加一个变体 + 一条 `PROVIDERS` 条目即可，
//! 其余代码不用动（每个 `match self` 都会编译期报错提醒）。
//!
//! ── 千问办公和 Qoder 是什么关系（为什么共用一个模块）────────────────
//! **同一套协议栈**：同一个聊天路径
//! （`/algo/api/v2/service/pro/sse/agent_chat_generation`）、同一套
//! `Bearer COSY.<payload>.<sig>` 信封、同一把服务端 RSA 公钥、同一个
//! `client_id`（国内版与 Qoder CN 逐字相同）。差异只有一批**客户端身份常量**
//! 与账号接口路径。因此实现只有一份、按本站点取值（`Site` 的那些方法），
//! 而不是复制一份 7000 行。
//!
//! ── 常量各自的依据（写清来源，改动前先复核）──────────────────────
//! Qoder 一侧来自现有的 `Qoder-Proxy` 移植（已在跑）；千问办公一侧来自
//! **三方实测证据**，按可信度排：
//!   1. `dumate2api` 的 `src/qwenwork/`（2026-10-14 直连 gateway.qwenwork.cn
//!      签名成功、19 帧正常出流；2026-10-31 复核模型目录）—— 提供
//!      `cosyVersion 1.1.52` / `clientType 6` / `data_policy agree` /
//!      `business qoder_work` / `scene qwork` / `session_type qoder_work` /
//!      `user-agent qoderwork/1.1.0` / 账号上下文与钱包路径；
//!   2. `magpie-plugins` 的 `packages/qoder`（0.2.37，对着 QwenWorkCN.app
//!      1.2.1 写）—— 提供 `client_id` / `redirect_uri` / `walletsPath` /
//!      `modelScene=listAllModels`；
//!   3. 上游归档的 Bun 参考（`runtime/vendor/qoder/qwenwork-*.mjs`）——
//!      同一份协议的本项目实现，逐字段与上面两份互相印证。
//!
//! ── 2026-11-03 复核：三个站点都走设备授权（务必读完再改）──────────
//! 上面三份参考用的都是**设备授权**：浏览器打开
//! `gateway.qwenwork.cn/device/selectAccounts`，授权后自己轮询
//! `/api/v1/deviceToken/poll`。
//!
//! 曾经有一版把它判成「入口已停用」并改走授权码 + 本机回调 —— 那个结论是错的：
//! 当时本模块把 `nonce` 的 UUID 横线去掉了，上游按 UUID 解析它，于是整条 query
//! 被判成非法（`400 INVALID_DEVICE_FLOW`、`details.field=query`、
//! `reason=not_allowed`）。补回横线之后站点的入口全部正常（302 进登录页），
//! 轮询对未完成的流程回 404（= 等待中）。
//!
//! 授权码那条路对本项目不可用：授权页只认客户端自己登记的
//! `https://gateway.qwenwork.{cn,ai}/oauth/callback`，本机回环与深链一律
//! `redirect_uri does not match`，授权码回不到我们手里。所以这里**不要**再
//! 接回授权码 / 本机回调。

use serde_json::Value;

use crate::server::core::providers::{kind_id, ProviderKind};
use crate::server::errors::GatewayError;

/// 本家站点（Qoder 两地 + 千问办公国内版）。
///
/// `Copy` 且无堆分配：它随凭证快照一起传遍转发链路，调用点全是值传递。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Site {
    /// Qoder 中国版（`qoder`）
    QoderCn,
    /// Qoder 国际版（`qoder-intl`）
    QoderIntl,
    /// 千问办公国内版（`qwenwork`，`gateway.qwenwork.cn`）
    QwenworkCn,
}

impl Site {
    /// 三个站点（注册表顺序：中国版 / 国内版在各自成对里靠前 —— 存量账号都归它们）。
    pub const ALL: [Site; 3] = [Site::QoderCn, Site::QoderIntl, Site::QwenworkCn];

    /// 本家是不是千问办公（协议同源，但客户端身份常量与账号接口不同）。
    pub fn is_qwenwork(self) -> bool {
        matches!(self, Site::QwenworkCn)
    }

    /// 本家是不是 Qoder（决定走 Qoder 的账号接口与活动领取）。
    pub fn is_qoder(self) -> bool {
        !self.is_qwenwork()
    }

    // ── 身份 ────────────────────────────────────────────────────

    /// 本家站点对应哪个 provider kind（站点 → 身份的**唯一**映射）
    pub fn kind(self) -> ProviderKind {
        match self {
            Self::QoderCn => ProviderKind::Qoder,
            Self::QoderIntl => ProviderKind::QoderIntl,
            Self::QwenworkCn => ProviderKind::Qwenwork,
        }
    }

    /// 本家站点的 provider id（**落盘契约**，存量 id 只增不改）。
    pub fn provider_id(self) -> &'static str {
        kind_id(self.kind())
    }

    /// 这个 kind 是不是本模块的站点（三家都算）—— 判据只在这里写一份
    pub fn from_kind(kind: ProviderKind) -> Option<Self> {
        Self::ALL.into_iter().find(|site| site.kind() == kind)
    }

    /// provider id → 站点（不认识的 id 返回 None）。
    ///
    /// 「id ↔ 站点」的唯一反向映射：跨站点的账号迁移 / 排障要靠它，
    /// 而不是在别处再写一份字面量清单。
    pub fn from_provider_id(provider_id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|site| site.provider_id() == provider_id)
    }

    /// 产品名（账号页分组、日志前缀用）。
    pub fn product(self) -> &'static str {
        match self {
            Site::QoderCn | Site::QoderIntl => "Qoder",
            Site::QwenworkCn => "千问办公",
        }
    }

    /// 版本名（中国版 / 国际版 / 国内版）。
    ///
    /// 千问办公沿用本项目「国内版」的既有措辞；Qoder 保持「中国版 / 国际版」
    /// 不动（那是已发布的叫法）。
    pub fn label(self) -> &'static str {
        match self {
            Site::QoderCn => "中国版",
            Site::QoderIntl => "国际版",
            Site::QwenworkCn => "国内版",
        }
    }

    /// 展示名（`Qoder 中国版` / `千问办公 国内版`）。
    pub fn display_name(self) -> String {
        format!("{} {}", self.product(), self.label())
    }

    /// 日志前缀（`[Qoder]` / `[千问办公]`）。
    ///
    /// 日志里写的东西必须是**真发生的事**：千问办公的请求、目录刷新、额度查询
    /// 挂在 `[Qoder]` 名下会让排障时找错产品。凡是拿得到站点的日志都用它。
    pub fn log_tag(self) -> &'static str {
        match self {
            Site::QoderCn | Site::QoderIntl => "[Qoder]",
            Site::QwenworkCn => "[千问办公]",
        }
    }

    /// 凭证记录里的 `mode` 字段（兼容旧记录的落盘形态）。
    ///
    /// **它不是权威**：站点一律以 provider 身份为准（见 `refresh::snapshot`），
    /// 这个字段只在「同一条记录被两家站点读到」时提供一个人可读的地区标注。
    pub fn id(self) -> &'static str {
        match self {
            Site::QoderCn | Site::QwenworkCn => "cn",
            Site::QoderIntl => "global",
        }
    }

    /// 账号公开形态里的 `edition`（前端按它分组 / 回显）。
    ///
    /// 与 [`Self::id`] 的差别是历史包袱：`mode` 当年只有 `cn` / `global` 两档，
    /// 而 `edition` 是更早的 `cn` / `intl` 两档。三站之后两者都不足以当身份
    /// （千问办公也是 `cn`），身份一律用 [`Self::provider_id`]。
    pub fn edition(self) -> &'static str {
        match self {
            Site::QoderCn | Site::QwenworkCn => "cn",
            Site::QoderIntl => "intl",
        }
    }

    /// 新账号 id 的前缀（`qoder-<地区>-<sha256(userId) 前 16 位>`）。
    ///
    /// 存量记录的前缀由「`qoder-` + [`Self::id`]」拼出（`qoder-cn-…` /
    /// `qoder-global-…`），**必须保持不变** —— id 是请求日志与报表的引用键。
    /// 千问办公另起一个前缀：它与 Qoder 中国版同为 `cn`，沿用 `qoder-cn-`
    /// 会让「同一个人在两家产品」撞 id（两家的 userId 各有自己的命名空间，
    /// 撞不撞不由我们决定）。
    pub fn account_id_prefix(self) -> &'static str {
        match self {
            Site::QoderCn => "qoder-cn",
            Site::QoderIntl => "qoder-global",
            Site::QwenworkCn => "qwenwork-cn",
        }
    }

    /// 解析 payload 里的 `mode` / `edition` 兼容字段。
    ///
    /// 只在**不知道 provider 身份**的入口用（例如手工粘贴一份凭证）；
    /// 拿着 provider 身份的入口必须用 [`Self::from_provider_id`] / [`Self::kind`]。
    pub fn parse(value: &str) -> Result<Self, GatewayError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "global" | "intl" => Ok(Site::QoderIntl),
            "cn" => Ok(Site::QoderCn),
            _ => Err(GatewayError::with_status(
                400,
                "Qoder 地区必须为 global（国际版）或 cn（中国版）",
            )),
        }
    }

    pub fn from_payload(payload: &Value) -> Result<Self, GatewayError> {
        let mode = payload.get("mode").or_else(|| payload.get("edition"));
        match mode {
            None | Some(Value::Null) => Ok(Self::QoderIntl),
            Some(Value::String(value)) => Self::parse(value),
            _ => Err(GatewayError::with_status(400, "Qoder 地区必须是字符串")),
        }
    }

    // ── 主机 ────────────────────────────────────────────────────

    /// 网页门户基址（人看的页面：登录授权页所在的那台主机）。
    pub fn web_origin(self) -> &'static str {
        match self {
            Site::QoderCn => "https://qoder.com.cn",
            Site::QoderIntl => "https://qoder.com",
            Site::QwenworkCn => "https://gateway.qwenwork.cn",
        }
    }

    /// 账号接口基址（`openapi`：设备授权轮询、续期、用户资料、额度）。
    ///
    /// 千问办公把网页、账号接口与推理网关**都放在 gateway 这一台主机上**，
    /// 所以它三项取同一个值。
    pub fn open_api(self) -> &'static str {
        match self {
            Site::QoderCn => "https://openapi.qoder.com.cn",
            Site::QoderIntl => "https://openapi.qoder.sh",
            Site::QwenworkCn => "https://gateway.qwenwork.cn",
        }
    }

    /// 推理 / 模型目录基址（`/algo/...` 这一族的根，含尾斜杠）。
    pub fn gateway(self) -> &'static str {
        match self {
            Site::QoderCn => "https://gateway.qoder.com.cn/",
            Site::QoderIntl => "https://api3.qoder.sh/",
            Site::QwenworkCn => "https://gateway.qwenwork.cn/",
        }
    }

    /// 设备授权页的完整地址。
    ///
    /// Qoder 在**门户**上（`qoder.com/device/selectAccounts`）；千问办公在
    /// **gateway** 上（它会 302 到 `qwenwork.cn` 的标准 OAuth 页）。
    pub fn device_login_url(self) -> String {
        format!("{}{}", self.web_origin(), DEVICE_LOGIN_PATH)
    }

    /// **对话链路**实际使用的推理基址。
    ///
    /// ── 为什么按令牌前缀分流（这不是取舍，是上游的约束）──────────
    /// 国际版有两台推理主机：`api3.qoder.sh` 认设备流令牌（`dt-`），
    /// `api2.qoder.sh` 才认 PAT 换来的作业令牌（`jt-`）—— 把 `jt-` 打到
    /// api3 会被判「Login expired」（403）。官方 qodercli 也是这么分的：
    /// PAT 先换作业令牌、再走 api2。中国版与千问办公只有一台网关，
    /// 两种令牌都收，不需要分流。
    ///
    /// 依据：9router 的 `qoderInferenceBase`（注释写明「Job-token (jt-...)
    /// traffic must hit api2.qoder.sh — api3 rejects jt- with "Login expired"」）、
    /// CLIProxyAPI 的 qoder2api 插件同款分流、OmniRoute 的 issue #4683。
    pub fn inference_base(self, access_token: &str) -> &'static str {
        match self {
            Site::QoderIntl => {
                if access_token.starts_with("jt-") {
                    "https://api2.qoder.sh/"
                } else {
                    "https://api3.qoder.sh/"
                }
            }
            other => other.gateway(),
        }
    }

    /// 设备流授权的 `client_id`。
    ///
    /// `None` = 历史上不带它也能登录（Qoder 两个站点就是这种，改它会动到
    /// 已发布的登录链路），因此保持原样；千问办公必须带。
    pub fn device_client_id(self) -> Option<&'static str> {
        match self {
            Site::QoderCn | Site::QoderIntl => None,
            // 桌面客户端的；与 Qoder CN 的 CLI 生产 client_id 逐字相同
            Site::QwenworkCn => Some("e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb"),
        }
    }

    /// 设备流授权的 `redirect_uri`（`None` = 不发这个参数）。
    pub fn device_redirect(self) -> Option<&'static str> {
        match self {
            Site::QoderCn | Site::QoderIntl => None,
            Site::QwenworkCn => Some("qwenwork-cn://"),
        }
    }

    /// 授权页允许的域名（网关据此拦截把用户引去别处的登录地址）。
    pub fn allowed_login_hosts(self) -> Vec<String> {
        match self {
            Site::QoderCn | Site::QoderIntl => vec![
                "qoder.sh".to_string(),
                "qoder.com".to_string(),
                "qoder.com.cn".to_string(),
            ],
            Site::QwenworkCn => vec![
                "qwenwork.cn".to_string(),
                "gateway.qwenwork.cn".to_string(),
            ],
        }
    }

    // ── 客户端身份（COSY 头 + 请求体必须一致的那一组）──────────────

    /// `Cosy-Version` / payload 里的 `cosyVersion`。
    pub fn cosy_version(self) -> &'static str {
        match self {
            Site::QoderCn | Site::QoderIntl => COSY_VERSION_QODER,
            // dumate2api：1.1.0 桌面端实测 1.1.52
            Site::QwenworkCn => "1.1.52",
        }
    }

    /// `Cosy-ClientType`。
    pub fn cosy_client_type(self) -> &'static str {
        match self {
            Site::QoderCn | Site::QoderIntl => "5",
            Site::QwenworkCn => "6",
        }
    }

    /// `Cosy-Data-Policy`。
    ///
    /// Qoder 是 `disagree`（既有实现，不动）；千问办公的客户端是 `agree`，
    /// 而 dumate2api 只在 `agree` 下打通了签名链路，故照客户端取值。
    pub fn cosy_data_policy(self) -> &'static str {
        match self {
            Site::QoderCn | Site::QoderIntl => "disagree",
            Site::QwenworkCn => "agree",
        }
    }

    /// `Cosy-Scene`（`None` = 不发这个头）。
    pub fn cosy_scene(self) -> Option<&'static str> {
        if self.is_qwenwork() { Some("qwork") } else { None }
    }

    /// `Cosy-Business-Product`（`None` = 不发这个头）。
    pub fn cosy_business_product(self) -> Option<&'static str> {
        if self.is_qwenwork() { Some("qoder_work") } else { None }
    }

    /// `Cosy-Business-Type`（`None` = 不发这个头）。
    pub fn cosy_business_type(self) -> Option<&'static str> {
        if self.is_qwenwork() { Some("agent") } else { None }
    }

    /// 请求体里的 `session_type`。
    ///
    /// **它必须与 `Cosy-ClientType` 一致**：千问办公的服务端会因为
    /// 「body 说 qodercli、头说 6」这种自相矛盾**直接拒绝**（dumate2api
    /// 的 constants 里写着这条实测结论），这也是本模块必须按站点取值、
    /// 不能照抄 Qoder 那套常量的直接原因。
    pub fn session_type(self) -> &'static str {
        match self {
            Site::QoderCn | Site::QoderIntl => "qodercli",
            Site::QwenworkCn => "qoder_work",
        }
    }

    /// 请求体 `business.product`。
    pub fn business_product(self) -> &'static str {
        match self {
            Site::QoderCn | Site::QoderIntl => "cli",
            Site::QwenworkCn => "qoder_work",
        }
    }

    /// 请求体 `business.version`。
    pub fn business_version(self) -> &'static str {
        match self {
            Site::QoderCn | Site::QoderIntl => "1.0.0",
            Site::QwenworkCn => self.cosy_version(),
        }
    }

    /// 账号类请求的 `User-Agent`（`None` = 沿用既有的那个）。
    pub fn user_agent(self) -> Option<&'static str> {
        if self.is_qwenwork() { Some("qoderwork/1.1.0") } else { None }
    }

    // ── 账号接口 ────────────────────────────────────────────────

    /// 用户资料路径。
    pub fn user_info_path(self) -> &'static str {
        match self {
            Site::QoderCn | Site::QoderIntl => USER_INFO_PATH,
            Site::QwenworkCn => ACCOUNT_CONTEXT_PATH,
        }
    }

    /// 额度（账号上下文）路径（`None` = 走 Qoder 的 `sash` 用量接口）。
    pub fn usage_path(self) -> Option<&'static str> {
        if self.is_qwenwork() { Some(ACCOUNT_CONTEXT_QUOTA_PATH) } else { None }
    }

    /// 积分池路径（`None` = 这家没有 / 不读）。
    ///
    /// 只有千问办公**国内版**的公有云个人账号有这三档钱包；Qoder 根本没有
    /// 这个接口。
    pub fn wallets_path(self) -> Option<&'static str> {
        match self {
            Site::QwenworkCn => Some(WALLETS_PATH),
            Site::QoderCn | Site::QoderIntl => None,
        }
    }

    /// 续期请求体里的 `target`（`None` = 只发 `refresh_token`）。
    pub fn refresh_target(self) -> Option<&'static str> {
        match self {
            Site::QwenworkCn => Some("c"),
            Site::QoderCn | Site::QoderIntl => None,
        }
    }

    /// 设备令牌本身是不是对话令牌。
    ///
    /// 千问办公**没有**作业令牌交换（`/api/v1/jobToken/exchange` 不存在），
    /// 设备授权拿到的这对令牌就是对话用的那一对，续期也按设备令牌续。
    pub fn device_only(self) -> bool {
        self.is_qwenwork()
    }

    /// 有没有 PAT（个人访问令牌）交换。
    pub fn supports_pat(self) -> bool {
        self.is_qoder()
    }

    /// 有没有可领取的每日活动。
    pub fn supports_checkin(self) -> bool {
        self.is_qoder()
    }

    // ── 模型目录 ────────────────────────────────────────────────

    /// 目录响应里要读的那个场景（`None` = 走 Qoder 的 `chat` 数组）。
    ///
    /// 千问办公的目录是 `{"qwork": [...]}`，且**把整份目录都放在这一个场景里**
    /// （含上游标了 `enable: false` 的条目），所以还要 [`Self::list_all_models`]。
    pub fn model_scene(self) -> Option<&'static str> {
        if self.is_qwenwork() { Some("qwork") } else { None }
    }

    /// 目录里那些 `enable: false` 的条目要不要一起列出来。
    pub fn list_all_models(self) -> bool {
        self.is_qwenwork()
    }

    /// `model_config` 里要按站点补的字段。
    ///
    /// 千问办公的客户端会带上 `display_name` / `format` / `is_vl` 等；
    /// dumate2api 逐字段抄了它（那里的注释写明「与客户端一致」才不被拒）。
    pub fn model_config_style(self) -> ModelConfigStyle {
        if self.is_qwenwork() {
            ModelConfigStyle::Qwenwork
        } else {
            ModelConfigStyle::Qoder
        }
    }

    /// 上下文窗口上限（千问办公客户端声明的值）。
    ///
    /// 用在「目录条目自己没声明 `max_input_tokens`」的兜底上（见
    /// `models::context_window_of`）：那时按客户端声明值取，而不是退回一个
    /// 更小的默认值 —— 后者会让本可以放下的长对话被判成超长。
    pub fn max_input_tokens(self) -> i64 {
        // 千问办公客户端声明的值；dumate2api 二分实测到 ~1.02M（超过即整轮失败，
        // 不是截断），所以这种声明只能如实写，不能往上抬。
        const QWENWORK_MAX_INPUT: i64 = 1_024_000;
        match self {
            Site::QwenworkCn => QWENWORK_MAX_INPUT,
            Site::QoderCn | Site::QoderIntl => 0,
        }
    }
}

/// `model_config` 的两种形状（见 [`Site::model_config_style`]）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModelConfigStyle {
    /// Qoder：`key` + 几个可选位（`slim_model_config`）。
    Qoder,
    /// 千问办公：客户端那一份更宽的字段集。
    Qwenwork,
}

/// Qoder 的 COSY 协议版本（既有实现，不动）。
const COSY_VERSION_QODER: &str = "1.1.38";

/// 设备授权页路径。**三站同名同参**，只有主机名不同。
///
/// `nonce` 必须是标准 UUID（带横线），否则上游整条判非法（见模块头）。
pub const DEVICE_LOGIN_PATH: &str = "/device/selectAccounts";
pub const DEVICE_POLL_PATH: &str = "/api/v1/deviceToken/poll";
pub const EXCHANGE_PATH: &str = "/api/v1/jobToken/exchange";
pub const USER_INFO_PATH: &str = "/api/v1/userinfo";
pub const USAGE_PATH: &str = "/api/v2/quota/usage";

/// 千问办公的账号上下文（用户资料 + 计划 + 额度都在这一份里）。
pub const ACCOUNT_CONTEXT_PATH: &str =
    "/api/v1/adapter/user/account-context?include=user,plan,quota,page,data_sharing";
/// 千问办公的额度读取路径（只要 user/plan/quota 三项 —— 与客户端一致）。
pub const ACCOUNT_CONTEXT_QUOTA_PATH: &str =
    "/api/v1/adapter/user/account-context?include=user,plan,quota";
/// 千问办公的三档积分池（每日 / 每月 / 长期）。
pub const WALLETS_PATH: &str = "/api/v1/adapter/user/wallets";

/// 套餐查询（`GET`，返回 `plan_tier_name` 等）。与额度接口同在 openapi 主机上，
/// 用同一套头；余额面板的「套餐 Pro Trial」一行就来自这里。
pub const PLAN_PATH: &str = "/api/v2/user/plan";

/// 活动/签到接口（`sash` 一族）的路径前缀。
///
/// **与 openapi 同主机、但鉴权口径完全不同**：只要一个 `Bearer <dt->` 加
/// `cosy-clienttype: 10`，**不打 COSY 签名**（见 [`sash_headers`]）。
/// 每日签到的领取走 `GET {CAMPAIGNS_PATH}` 找 `actionType == "CLAIM_BENEFIT"`
/// 的活动、再 `POST {CAMPAIGNS_PATH}/{campaignId}/claim` 领取 —— 详情与证据见
/// `checkin.rs` 的模块头。**千问办公没有这条链路**（见 [`Site::supports_checkin`]）。
pub const CAMPAIGNS_PATH: &str = "/sash/api/v1/me/campaigns";
/// 活动列表的查询串：`forceRefresh=true` 让上游跳过缓存给实时的 `claimStatus`。
pub const CAMPAIGNS_QUERY: &str = "?forceRefresh=true";

/// `cosy-clienttype` 的值：桌面端（QoderWork）形态。
///
/// **这个头在门控返回内容**，不是可有可无的装饰：实测同一个账号
/// （中国版，2026-09-27）用 `Cosy-ClientType: 5`（IDE 形态）打活动接口，
/// 上游返回 `showCampaign:false` + 空活动列表；换成 `10` 才给出完整活动包
/// （含可领取的签到活动）。参考实现（CLIProxyAPI 的 qoder2api 插件、
/// CPA 的 qoder 插件）都用 10。
pub const SASH_CLIENT_TYPE: &str = "10";

/// `user-agent`：抓包确认的客户端标识（参考实现同值）
pub const SASH_USER_AGENT: &str = "Qoder";

/// 活动/签到接口的请求头：**最小束、无签名**。
///
/// 小写头名是照抄抓包结果（HTTP 头名大小写不敏感，但保持一致便于对拍）。
pub fn sash_headers(token: &str) -> Vec<(String, String)> {
    vec![
        ("authorization".to_string(), format!("Bearer {token}")),
        ("cosy-clienttype".to_string(), SASH_CLIENT_TYPE.to_string()),
        ("accept".to_string(), "application/json".to_string()),
        ("accept-language".to_string(), "zh-CN".to_string()),
        ("user-agent".to_string(), SASH_USER_AGENT.to_string()),
    ]
}

/// 设备流凭据（`dt-` / `drt-`）的续期端点，挂在 [`Site::open_api`] 上。
///
/// ── 为什么不是 `{center}/algo/api/v3/user/refresh_token` ──────────
/// 那条「认证中心」端点是给 PAT 换来的作业令牌（`jt-` / `jrt-`）用的，且要求
/// `appcode: cosy` + `Date` + `signature = md5("cosy&<secret>&<date>")` 这套签名头
/// （`center.qoder.sh` 与 `gateway.qoder.com.cn` 同名同签）。我们的设备流凭据走
/// 那条路会**连签名都没有**，被 WAF 直接丢掉 —— 实测两个地区都稳定返回
/// `403 {"errorMessage":"Request discarded","errorCode":"Forbidden"}`，与凭证
/// 有效性、地区选择都无关。补上签名头后同一请求会从 403 变成 400 业务校验，
/// 足以证明 403 来自缺签名而不是令牌本身。
///
/// 设备流凭据的续期在 **openapi 主机**上、且**不需要任何签名**（对照实现里
/// 只带 `content-type` / `accept` 两个头就能刷新成功）。千问办公的续期也在这
/// 一条路径上（dumate2api 实测）。
pub const DEVICE_REFRESH_PATH: &str = "/api/v1/deviceToken/refresh";

/// 作业令牌（`jrt-`，PAT 换来的那一族）的续期端点。
///
/// 与 [`DEVICE_REFRESH_PATH`] 同在 openapi 主机上、同样不要签名，只有路径不同。
/// 两者**不可互换**：把 `drt-` 交给作业令牌端点、或把 `jrt-` 交给设备端点，
/// 上游都按「令牌无效」拒绝。千问办公没有作业令牌，这条对它不适用。
pub const JOB_REFRESH_PATH: &str = "/api/v1/jobToken/refresh";

/// 续期请求的头：**不带任何签名，也不带 Authorization**。
///
/// 两条续期路径的鉴权凭据都在请求体里（`{"refresh_token": "…"}`），带旧令牌
/// 没有意义。多带 `Cosy-*` 也不会让 center 那一族接受（那需要的是 appcode 签名），
/// 因此这里只保留一个可辨识的 UA。
pub fn refresh_headers() -> Vec<(String, String)> {
    vec![("User-Agent".to_string(), "qoder-local-proxy".to_string())]
}

/// 续期请求的头（按站点）。
///
/// 千问办公要带客户端自己的 UA、`Login-Version` 与一个请求 id（dumate2api 的
/// 实测形态）；Qoder 沿用既有的单 UA 形态。
pub fn refresh_headers_for(site: Site) -> Vec<(String, String)> {
    match site.user_agent() {
        None => refresh_headers(),
        Some(agent) => vec![
            ("User-Agent".to_string(), agent.to_string()),
            ("Login-Version".to_string(), "v2".to_string()),
            (
                "X-Request-Id".to_string(),
                super::machine::random_uuid().unwrap_or_default(),
            ),
        ],
    }
}

pub fn open_api_headers(token: Option<&str>) -> Vec<(String, String)> {
    let mut headers = vec![
        ("Cosy-Version".to_string(), "1.0.1".to_string()),
        ("Cosy-ClientType".to_string(), "5".to_string()),
        ("User-Agent".to_string(), "qoder-local-proxy".to_string()),
    ];
    if let Some(token) = token {
        headers.push(("Authorization".to_string(), format!("Bearer {token}")));
    }
    headers
}

/// 账号类请求的头（`Authorization` 之外的那几个）。
///
/// ── 千问办公走「最小头束」──────────────────────────────────
/// 它的账号接口（`account-context` / `wallets` / 设备轮询）是普通 REST，
/// **不做 COSY 签名**，上游看到的客户端身份来自请求头。三份参考实现
/// （dumate2api / magpie / 本项目归档）都只带 `Accept` + 自己的 `User-Agent`
/// （+ `Bearer`），没有一份带 `Cosy-*`。因此这里按最小束走 —— 带 Qoder 那套
/// 头没有依据，还可能让「头说 IDE、body 说桌面端」这类自相矛盾重新出现。
///
/// Qoder 沿用既有的固定头（它一直这么跑）。
pub fn account_headers(site: Site, token: Option<&str>) -> Vec<(String, String)> {
    if site.is_qwenwork() {
        let mut headers = vec![
            ("Accept".to_string(), "application/json".to_string()),
            (
                "User-Agent".to_string(),
                site.user_agent().unwrap_or("qoderwork/1.1.0").to_string(),
            ),
        ];
        if let Some(token) = token {
            headers.push(("Authorization".to_string(), format!("Bearer {token}")));
        }
        return headers;
    }
    open_api_headers(token)
}

#[cfg(test)]
mod tests {
    //! 站点表的判据：**身份与主机必须一一对应**。
    //!
    //! 这里钉的是「加站点时最容易错的那几件事」：id 拼错、主机串站、客户端
    //! 身份常量跟着串站。它们都不会让编译失败，只会让请求打到错的产品上，
    //! 而症状是「凭证明明有效却 401」—— 从日志里看不出来。

    use super::*;

    #[test]
    fn every_site_has_a_distinct_identity() {
        for site in Site::ALL {
            assert_eq!(Site::from_provider_id(site.provider_id()), Some(site));
            assert_eq!(Site::from_kind(site.kind()), Some(site));
            assert!(!site.provider_id().is_empty());
        }
        // 三家两两不同，且 id 与注册表逐字一致
        let ids: Vec<&str> = Site::ALL.iter().map(|site| site.provider_id()).collect();
        assert_eq!(ids, vec!["qoder", "qoder-intl", "qwenwork"]);
    }

    #[test]
    fn qwenwork_never_shares_a_host_with_qoder() {
        let qwenwork = Site::QwenworkCn;
        for qoder in [Site::QoderCn, Site::QoderIntl] {
            assert_ne!(qwenwork.gateway(), qoder.gateway());
            assert_ne!(qwenwork.open_api(), qoder.open_api());
            assert_ne!(qwenwork.web_origin(), qoder.web_origin());
        }
        // 千问办公三项同主机（它把网页 / 账号 / 网关放一台）
        assert_eq!(qwenwork.gateway().trim_end_matches('/'), qwenwork.open_api());
        assert_eq!(qwenwork.open_api(), qwenwork.web_origin());
    }

    #[test]
    fn client_identity_never_mixes_products() {
        // Qoder 的 body 与头必须自洽：session_type=qodercli ↔ client_type=5
        for qoder in [Site::QoderCn, Site::QoderIntl] {
            assert_eq!(qoder.session_type(), "qodercli");
            assert_eq!(qoder.cosy_client_type(), "5");
            assert_eq!(qoder.business_product(), "cli");
            assert!(qoder.cosy_scene().is_none());
            assert!(qoder.wallets_path().is_none());
        }
        // 千问办公：session_type=qoder_work ↔ client_type=6，且三个 business 头都有
        let qwenwork = Site::QwenworkCn;
        assert_eq!(qwenwork.session_type(), "qoder_work");
        assert_eq!(qwenwork.cosy_client_type(), "6");
        assert_eq!(qwenwork.business_product(), "qoder_work");
        assert_eq!(qwenwork.business_version(), qwenwork.cosy_version());
        assert_eq!(qwenwork.cosy_scene(), Some("qwork"));
        assert_eq!(qwenwork.cosy_business_product(), Some("qoder_work"));
        assert_eq!(qwenwork.cosy_business_type(), Some("agent"));
    }

    #[test]
    fn only_qwenwork_has_the_wider_model_config_and_wallets() {
        assert_eq!(Site::QwenworkCn.model_config_style(), ModelConfigStyle::Qwenwork);
        assert_eq!(Site::QwenworkCn.model_scene(), Some("qwork"));
        assert!(Site::QwenworkCn.list_all_models());
        assert!(Site::QwenworkCn.wallets_path().is_some());
        assert!(Site::QwenworkCn.max_input_tokens() > 0);

        for qoder in [Site::QoderCn, Site::QoderIntl] {
            assert_eq!(qoder.model_config_style(), ModelConfigStyle::Qoder);
            assert!(qoder.model_scene().is_none());
            assert!(!qoder.list_all_models());
            assert_eq!(qoder.max_input_tokens(), 0);
        }
    }

    #[test]
    fn qwenwork_has_no_pat_no_checkin_and_no_job_token() {
        // 这三条决定了界面上的入口：给一个一定失败的按钮比不给更糟
        assert!(Site::QwenworkCn.device_only());
        assert!(!Site::QwenworkCn.supports_pat());
        assert!(!Site::QwenworkCn.supports_checkin());
        assert!(Site::QoderCn.supports_pat());
        assert!(Site::QoderIntl.supports_checkin());
    }

    #[test]
    fn device_login_carries_client_id_only_where_required() {
        // Qoder 历史上不带 client_id / redirect_uri 也能登录，改它会动到已发布
        // 的链路；千问办公必须带，缺了授权页进不去
        assert!(Site::QoderCn.device_client_id().is_none());
        assert!(Site::QoderIntl.device_redirect().is_none());
        assert_eq!(
            Site::QwenworkCn.device_client_id(),
            Some("e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb")
        );
        assert_eq!(Site::QwenworkCn.device_redirect(), Some("qwenwork-cn://"));
        // 授权页地址必须落在本家允许的域名里（否则会被自己的白名单拦掉）
        for site in Site::ALL {
            let url = url::Url::parse(&site.device_login_url()).expect("授权地址可解析");
            let host = url.host_str().expect("有主机名").to_string();
            assert!(
                site.allowed_login_hosts().iter().any(|allowed| allowed == &host),
                "{} 的授权页主机 {host} 不在白名单里",
                site.display_name()
            );
        }
    }

    #[test]
    fn account_ids_never_share_a_prefix_across_sites() {
        // 账号 id = `<前缀>-<sha256(userId) 前 16 位>`。两站共用前缀 ⇒ 同一个人
        // 在两家产品会撞 id，而 id 是请求日志 / 报表的引用键（撞了以后归谁家
        // 都说不清）。千问办公与 Qoder 中国版的 `mode` 都是 `cn`，这条尤其要钉。
        for (index, site) in Site::ALL.iter().enumerate() {
            for other in &Site::ALL[index + 1..] {
                assert_ne!(
                    site.account_id_prefix(),
                    other.account_id_prefix(),
                    "{} 与 {} 的账号 id 前缀相同",
                    site.display_name(),
                    other.display_name()
                );
                assert!(
                    !other.account_id_prefix().starts_with(site.account_id_prefix()),
                    "{} 的前缀是 {} 的前缀串，会让 id 分不清归属",
                    other.display_name(),
                    site.display_name()
                );
            }
        }
        // 存量前缀**必须**保持原样：改了会让历史请求记录对不上账号
        assert_eq!(Site::QoderCn.account_id_prefix(), "qoder-cn");
        assert_eq!(Site::QoderIntl.account_id_prefix(), "qoder-global");
        assert_eq!(Site::QwenworkCn.account_id_prefix(), "qwenwork-cn");
    }

    #[test]
    fn payload_mode_keeps_its_historical_meaning() {
        // `mode` 只是兼容字段：历史记录里没有它是常态，缺省必须落回国际版
        // （拆家前 `qoder` 就是国际版，存量记录全靠这条规则）
        assert_eq!(Site::parse("").expect("空"), Site::QoderIntl);
        assert_eq!(Site::parse("intl").expect("intl"), Site::QoderIntl);
        assert_eq!(Site::parse("global").expect("global"), Site::QoderIntl);
        assert_eq!(Site::parse("cn").expect("cn"), Site::QoderCn);
        assert!(Site::parse("qwenwork").is_err());
    }
}
