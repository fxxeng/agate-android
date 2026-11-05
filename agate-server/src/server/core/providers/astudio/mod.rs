//! AStudio（科大讯飞「星火 Astron Studio」桌面端）适配器：**Responses 协议**。
//!
//! ── 上游是什么 ──────────────────────────────────────────────
//! 讯飞星火 Astron Studio 的桌面客户端登录后可以调模型。模型调用走 OpenAI
//! **Responses** 协议（`POST {模型域}/model-gateway/v1/responses`），目录走
//! `GET {模型域}/api/v1/model-manager/models`，积分 / 会员 / 签到走
//! `agent.xfyun.cn` 的**网页会话 Cookie**。
//!
//! ── 能力与「没有的东西」────────────────────────────────────
//! ```text
//!   有：对话（流式 / 工具 / 图片 / 思考）、在线模型目录、余额、每日登录积分
//!   无：凭证续期（凭据失效只能重新粘贴）、领取类活动（上游没有接口）
//! ```
//! 「没有续期」不是省事：上游确实没有刷新端点，`modelBearerToken` 与网页会话
//! 都是登录时发的死票。所以本家 `supports_refresh()` 为 false —— 后台续期任务
//! 根本不会问它，界面上也不该出现「刷新 Token」按钮。
//!
//! ── 为什么 `is_stateful()` 为真 ────────────────────────────
//! 上游响应是 Responses 的 SSE 事件（`response.output_text.delta` 等），
//! 网关的 `UpstreamKind` 是**封闭枚举**，没有这一套。由适配器自己读事件、翻成
//! 标准 chat 帧 —— 与 Kiro 的 AWS 二进制帧同一条理由，也是唯一不扩协议的路。
//!
//! ── 两套凭据，各管一段（本家最容易被误解的地方）─────────────
//! ```text
//!   modelBearerToken     只够**调模型**
//!   token + accountId    网页会话：余额 / 会员 / 签到要它（+可选 ssoSessionId）
//! ```
//! 只有前者也能对话 —— 那个账号只是**没有余额行、不参与签到**。这是上游事实，
//! 不是我们省事：公开视图用 `noSession` 如实标出来，别把它说成「账号异常」。
//!
//! ── 本机登录态不接（与 Kiro 同一处置）───────────────────────
//! 上游的「读取桌面端登录态」依赖三样 Android 上不存在的东西：Windows 注册表
//! `HKCU\Software\AStudioStorage`（它还特意用 `RegGetValueW` 而不是 `reg query`，
//! 就是为了绕开 OEM 代码页）、`~/.acode/` 下的 `astron-session.json`、
//! `runtime/acode-home/config.toml`。给了入口只会稳定报「读不到登录态」。
//!
//! ── 登录（手机号验证码 / 账号密码 + 极验滑块）────────────────
//! 上游的做法是自己在 `127.0.0.1` 起一个登录页，页面里加载极验脚本、完成滑块后
//! 把结果交给进程内的表。这套做法在 Android 上**同样成立**（网关本来就跑在
//! 手机上）：`login_page` 起一个临时端口的极简 HTTP 服务 + 一段静态 HTML，
//! 客户端的登录窗打开 `http://127.0.0.1:<端口>/?t=…` 即可 —— 不需要内嵌某个
//! 第三方站点，也不需要读 Cookie 收尾（页面自己把结果带回来）。
//! 编排在 `core::login::astudio`，落库仍走 `add_astudio_account`。
//!
//! ⚠️ 「本机登录态」那条路仍然不接：它读的是 Windows 注册表与 `~/.acode/`，
//! 与登录页是两件事（前者是读桌面端已有的凭据，后者是自己走一遍登录）。
//!
//! ── 子模块分工 ──────────────────────────────────────────────
//! ```text
//!   endpoints    地址与客户端身份常量（纯）
//!   credentials  凭据读/写/公开形态/身份（纯，无网络）
//!   errors       四档分类 + 「输入过长」判定（纯，判据只此一份）
//!   responses    Chat → Responses 请求体 + 流事件解读（纯，可离线测）
//!   client       三套出网（Responses / 目录 Bearer / 账号面 Cookie）
//!   models       目录缓存 + 解析 + 静态兜底
//!   chat         Responses 事件 → chat 帧（流式 / 聚合）
//!   usage        余额（通用积分 / 星火积分 / 会员）
//!   checkin      每日登录积分（用余额差观测）
//!   auth         登录协议：极验配置 / 发码 / 两种登录 / 收尾查模型凭据
//!   login_page   本机登录页（127.0.0.1 上的极简服务 + 静态 HTML）
//!   adapter      ProviderAdapter 实现（注册表接线在这一个文件里收口）
//! ```
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本目录零 unwrap/expect/panic。

pub mod adapter;
pub mod auth;
pub mod chat;
pub mod checkin;
pub mod client;
pub mod credentials;
pub mod endpoints;
pub mod errors;
pub mod login_page;
pub mod models;
pub mod responses;
pub mod usage;

/// provider id（与内核 `PROVIDERS` 注册表、客户端 `PresetRegistry` 同一字面量）
pub const ID: &str = "astudio";
