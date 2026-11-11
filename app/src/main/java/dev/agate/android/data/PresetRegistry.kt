package dev.agate.android.data

/**
 * 官方提供商预置注册表（M3-3）。
 *
 * 顺序与内核 providers::PROVIDERS 数组逐条对齐（内核是该白名单的唯一事实来源，
 * 见 kind_from_id）：同一条产品线的两个版本相邻排列。
 *
 * 【通道**以核实的内核行为为准**】见 docs/提供商接入设计-v1.md：
 * - EXTERNAL（系统浏览器）只在「完成判定由内核轮询 / loopback 收回调」时可用；
 *   需要壳侧拦截回调的（raccoon/catpaw/kuku/orcaterm）不提供。
 * - IMPORT_DESKTOP 读的是本机固定路径，Android 上不展示（仍保留在枚举里）。
 * - 「签到」是能力标签（[supportsCheckin]），不混进通道。
 */

/** 登录通道：决定点卡片后的交互形态。 */
enum class LoginKind { WEBVIEW, SMS, PASTE }

/**
 * 添加账号页的分段归属（与内核 `providers::ProviderCategory` 逐字对齐）。
 *
 * ── 为什么客户端也要有这一份 ─────────────────────────────────
 * 内核 `/api/providers` 的摘要里已经带 `category`（唯一事实来源），但本页的
 * 提供商卡是**离线静态渲染**的（`PresetRegistry.ALL`），不等摘要回来 ——
 * 所以分段必须在注册表里也声明一次。两处的取值由 `docs/` 的移植记录对齐；
 * 新加一家时**两处一起改**，改漏的表现是那家在两个分段都不出现。
 */
enum class ProviderCategory { AGENT, WEB }

/** 登录/接入通道（核实版）。 */
enum class AuthChannel(val label: String) {
    EMBEDDED("内嵌登录"),
    EXTERNAL("浏览器登录"),
    DEVICE_CODE("设备码"),
    SMS("手机验证码"),
    PASTE("粘贴凭证"),
    IMPORT_FILE("导入凭证文件"),
    IMPORT_DESKTOP("桌面登录态"),
}

data class ProviderPreset(
    val id: String,
    val label: String,
    val desc: String,
    /** 兼容字段（旧 UI 仍读它）；新 UI 请读 [channels]。 */
    val kind: LoginKind,
    /** 该家真实支持的通道，顺序即 UI 顺序；首项为默认。 */
    val channels: List<AuthChannel>,
    /** 添加账号页的分段归属（Agent / Web）；默认 Agent，Web 家逐个声明。 */
    val category: ProviderCategory = ProviderCategory.AGENT,
    /** 粘贴通道的主输入提示（为空表示该家不走粘贴）。 */
    val pasteHint: String? = null,
    val pasteSecondaryHint: String? = null,
    /** 粘贴框是否多行（整份 JSON / Cookie 串）。 */
    val pasteMultiline: Boolean = false,
    /**
     * 网页登录的收尾方式是否为「**读登录窗口的 Cookie 再交回**」。
     *
     * true 的家没有授权码可收（凭证就在 Cookie 里）：登录窗口轮询
     * `CookieManager`，读到**本家声明的**登录态 Cookie（`sessionCookieNames`）
     * 后把整串 Cookie 交回内核。
     *
     * 交回端点分两种（由 `cookieCompletePath` 决定）：通用
     * `/api/session/login/callback`（豆包：Cookie 当 `callbackUrl`，
     * 内核各家自己认）与专用端点（KukuAI：`{state, cookie}`）。
     * false 的家仍走原来的「拦自定义 scheme / 等 loopback 回调」那条路。
     */
    val webLoginCookies: Boolean = false,
    /** 能力标签：是否支持自动签到（来自内核 CHECKIN_PROVIDERS）。 */
    val supportsCheckin: Boolean = false,
    /**
     * 登录是否依赖「网关端口」收回调。
     * true 的四家（accio/accio-cn/codearts/antigravity）回调落在
     * `127.0.0.1:<网关端口>`，Core 态不监听该端口 → 网关未运行时登录必失败。
     *
     * 不依赖它的三家各有各的理由：`trae` 与 `kiro` **自己 bind 回环端口**
     * （Trae 随便挑一条，Kiro 必须抢官方 IDE 白名单里的那几条），回调由内核
     * 自己的监听器接，与网关端口无关；`catpaw` 有 poll-token 兜底
     * （不经过浏览器，不受 PNA/CORS 影响），网关没开也能完成。
     */
    val needsGateway: Boolean = false,
    /**
     * 非空 = 整家置灰不可添加（F12）。目前没有家在用（kuku 于 2026-11-06 解除：
     * 换发已改成纯算法签名，不再需要官方 Windows 客户端 —— 见内核
     * `providers/kuku/engine.rs`）；字段留着是因为「这家在 Android 上根本走不通」
     * 确实会发生，而给一个点了报错的入口比少给一条更糟。
     */
    val disabledNote: String? = null,
    /**
     * `webLoginCookies = true` 时，判定「已经登录成功」看哪些 Cookie 名。
     *
     * 各家登录态 Cookie 不同：豆包是 `sessionid` 系，KukuAI 是百度通行证的
     * `BDUSS`。写死一份名单会让「换了家就永远判不成功」（登录页上用户明明
     * 登进去了，客户端却一直认为没登录、最后 5 分钟超时），所以由注册表声明。
     *
     * 客户端这里只做**前置判断**（别在用户还没登录完就交回空 Cookie），
     * 真正的校验仍在内核（各家的 `complete_login` / `normalize_import`）。
     */
    val sessionCookieNames: List<String> = emptyList(),
    /**
     * 读到的 Cookie 交给哪条收尾端点；`null` = 通用
     * `/api/session/login/callback`（body 是 `{state, callbackUrl}`）。
     *
     * KukuAI 是例外：它的收尾是 `/api/session/login/kuku/complete`
     * （body 是 `{state, cookie}`，内核 `finish_kuku_login`），因为百度通行证
     * 登录**不产生授权码**，凭证只在 Cookie 里，走不了「回调 URL」那一套。
     */
    val cookieCompletePath: String? = null,
    /**
     * 「浏览器」通道是否需要内核换一支授权地址（只有小浣熊与 CatPaw 为 true）。
     *
     * ── 为什么要有这个标记，而不是「有 EXTERNAL 就发 mode」──────────
     * 通道列表里带 EXTERNAL 的家很多（accio / codearts / antigravity / workbuddy /
     * zcode / trae …），它们的内嵌窗与系统浏览器打开的是**同一个授权地址**，
     * 内核收到 `mode:"browser"` 会明确拒绝（那是刻意设计：不让调用方以为这个
     * 参数对所有家都有效）。若按「选了浏览器通道就发 mode」实现，那些家会全部
     * 变成 400 —— 一条本该无害的分支把十几个能用的家打坏。
     *
     * 因此「这家有没有第二支授权地址」必须由注册表回答，客户端不猜。
     * 小浣熊为 true 的理由见它自己那条注释（自定义协议 vs redirect）；
     * CatPaw 为 true 只是因为浏览器形态要顺带把网关端口占起来（loopback 回调
     * 需要有人接），授权地址其实与前一条相同。
     */
    val browserLogin: Boolean = false,
    /**
     * 粘贴通道是否同时提供**免登录添加**（只有智谱清言一家为 true）。
     *
     * ── 为什么必须有这个标记，而不是「粘贴框留空即访客」──────────
     * 内核 `api::accounts` 的 Chatglm 分支确实把「没有令牌」当访客
     * （`want_guest` 缺省 = `plain_token.is_none()`），但那是**后端契约**：
     * 界面上「留空点添加」与「点一个叫添加访客账号的按钮」不是同一件事 ——
     * 前者是隐藏动作（用户不知道空着提交会发生什么，也不知道访客没有积分），
     * 后者是明说的入口。上游 3.3 用的就是后者（`guestLogin` 分段 + 独立按钮）。
     *
     * 而当前粘贴表单的主按钮是 `enabled = primary.isNotBlank()` —— 留空根本点不动。
     * 所以这条能力只能由注册表声明、由表单渲染一个独立按钮来表达。
     */
    val guestLogin: Boolean = false,
)

object PresetRegistry {

    private val WEB = listOf(AuthChannel.EMBEDDED, AuthChannel.EXTERNAL)

    val ALL: List<ProviderPreset> = listOf(
        // WorkBuddy：上游推 authUrl → 轮询 auth/token；可粘贴 auth.json（需 uid）
        // 智谱清言（chatglm.cn）：无状态转发 + 双凭证形态 —— 访客免登录可换证、
        // 登录账号有积分。客户端这边给两条通道：粘贴 accessToken（可选填
        // refreshToken），或**免登录添加访客账号**（`guestLogin = true`，
        // 内核 `api::accounts` 的 Chatglm 分支收到 `guest: true` 现换一份凭证）。
        // 不做内嵌网页登录：那需要跑上游的签名三件套 + 设备号，网页端流程在
        // WebView 里不可靠。
        ProviderPreset(
            "chatglm", "智谱清言", "chatglm.cn · 访客免登录 / 粘贴 Token", LoginKind.PASTE,
            listOf(AuthChannel.PASTE),
            category = ProviderCategory.WEB,
            pasteHint = "粘贴 accessToken（JWT；也可直接粘 chatglm_token=… 整行）",
            pasteSecondaryHint = "续期令牌 refreshToken（可选；留空则失效后需重新获取）",
            guestLogin = true,
        ),
        // ModelScope（魔搭）：两个站点**账号体系分开**（令牌打错站会 401），
        // 所以是两家 provider、各填各的令牌。同一枚 `ms-` 令牌两用 ——
        // 既调推理又读身份/魔粒/签到，用户只需复制一次。
        //
        // 两家都归 **Agent 分段**（与外层 `category` 的默认值一致，故不显式声明）。
        //
        // ── 判据是 pack.json，不是「有没有网页站」────────────────────
        // 上游把提供商拆成两个插件包：`agent2api-providers` 不声明
        // `provider_category`（默认 agent，30 个 id）、`agent2api-web-providers`
        // 声明 `"provider_category": "web"`（**只有 chatglm + deepseek-web**）。
        // 魔搭两个站点都在 agent 包内 ⇒ Agent。
        //
        // 曾经按「对话在网页站、登录靠网页令牌」把它归成了 Web —— 那是把
        // 「转发实现走哪条路」和「有没有网页站」混为一谈，归错了。别再按
        // 产品形态猜，去读 pack.json。
        ProviderPreset(
            "modelscope", "ModelScope 国内版", "modelscope.cn · 粘贴 ms- 令牌",
            LoginKind.PASTE,
            listOf(AuthChannel.PASTE),
            pasteHint = "粘贴 ms- 开头的 API 令牌（modelscope.cn「访问令牌」页）",
            supportsCheckin = true,
        ),
        ProviderPreset(
            "modelscope-intl", "ModelScope 国际版", "modelscope.ai · 粘贴 ms- 令牌",
            LoginKind.PASTE,
            listOf(AuthChannel.PASTE),
            pasteHint = "粘贴国际站的 ms- 令牌（modelscope.ai；与国内站不通用）",
            supportsCheckin = true,
        ),
        ProviderPreset(
            "workbuddy", "WorkBuddy 国内版", "copilot.tencent.com 网页登录", LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED, AuthChannel.EXTERNAL, AuthChannel.PASTE),
            pasteHint = "粘贴 auth.json 内容（含 auth.accessToken / account.uid）",
            pasteMultiline = true, supportsCheckin = true,
        ),
        ProviderPreset(
            "workbuddy-intl", "WorkBuddy 国际版", "workbuddy.ai 网页登录", LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED, AuthChannel.EXTERNAL, AuthChannel.PASTE),
            pasteHint = "粘贴 auth.json 内容（含 auth.accessToken / account.uid）",
            pasteMultiline = true, supportsCheckin = true,
        ),
        // 小浣熊：两条通道**走的是两支不同的授权地址**（内核
        // `raccoon::oauth` 的 build_authorize_url / build_browser_authorize_url）：
        //   · 内嵌 —— 带 `login_source=desktop`，官方页跳自定义协议
        //     `office-raccoon://auth/callback`，由 WebView 拦下转交内核；
        //   · 浏览器 —— 改带 `redirect=<本机 http>`，官方页把 `?authorization_code=`
        //     回推到我们自己的端口（手机上自定义协议没人接，这条才是稳的）。
        ProviderPreset(
            "raccoon", "小浣熊", "JWT 凭证网页登录", LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED, AuthChannel.EXTERNAL, AuthChannel.IMPORT_FILE),
            supportsCheckin = true, browserLogin = true,
        ),
        // CatPaw：内嵌与浏览器是**同一套上游流程**（passport 登录后 loopback 回调 +
        // poll-token 兜底），差别只在登录页在哪渲染 —— 与 raccoon 相反，它不需要换
        // 授权地址。有 poll-token 兜底，所以不需要网关在跑（needsGateway 仍为 false）。
        ProviderPreset(
            "catpaw", "CatPaw", "美团 · 网页登录", LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED, AuthChannel.EXTERNAL, AuthChannel.IMPORT_FILE),
            browserLogin = true,
        ),
        // AutoClaw 国内：手机验证码
        ProviderPreset(
            "autoclaw", "AutoClaw 国内版", "智谱 · 手机验证码", LoginKind.SMS,
            listOf(AuthChannel.SMS), supportsCheckin = true,
        ),
        // AutoClaw 国际：loopback 回调
        ProviderPreset(
            "autoclaw-intl", "AutoClaw 国际版", "autoglm.ai · 网页登录", LoginKind.WEBVIEW,
            WEB, supportsCheckin = true,
        ),
        // Qoder：设备码 + PAT 粘贴
        ProviderPreset(
            "qoder", "Qoder 中国版", "设备授权登录 / PAT", LoginKind.WEBVIEW,
            listOf(AuthChannel.DEVICE_CODE, AuthChannel.EXTERNAL, AuthChannel.PASTE),
            pasteHint = "粘贴个人访问令牌 PAT（pt-…）", supportsCheckin = true,
        ),
        ProviderPreset(
            "qoder-intl", "Qoder 国际版", "设备授权登录 / PAT", LoginKind.WEBVIEW,
            listOf(AuthChannel.DEVICE_CODE, AuthChannel.EXTERNAL, AuthChannel.PASTE),
            pasteHint = "粘贴个人访问令牌 PAT（pt-…）",
        ),
        // 千问办公（国内版）：与 Qoder 同一套设备授权协议，只是站点不同
        // （内核 `qoder::endpoints::Site::QwenworkCn`，见该文件模块头）。
        //
        // **没有粘贴通道**：它没有 PAT 交换（`Site::supports_pat` 为假，
        // `/api/v1/jobToken/exchange` 不存在），粘什么都会得到一条 400 ——
        // 给一个必然失败的输入框比不给更糟。
        // **也标不上「签到」**：`sash` 一族活动接口对它不存在
        // （`Site::supports_checkin` 为假），内核的 CHECKIN_PROVIDERS 里也没有它。
        ProviderPreset(
            "qwenwork", "千问办公 国内版", "设备授权登录", LoginKind.WEBVIEW,
            listOf(AuthChannel.DEVICE_CODE, AuthChannel.EXTERNAL),
        ),
        // Cline：设备码轮询；可导入
        ProviderPreset(
            "cline-free", "Cline Free", "设备码授权 · 免费池", LoginKind.WEBVIEW,
            listOf(AuthChannel.DEVICE_CODE, AuthChannel.EXTERNAL, AuthChannel.IMPORT_FILE),
        ),
        ProviderPreset(
            "cline-pass", "Cline Pass", "设备码授权 · 付费池", LoginKind.WEBVIEW,
            listOf(AuthChannel.DEVICE_CODE, AuthChannel.EXTERNAL, AuthChannel.IMPORT_FILE),
        ),
        // Accio：OAuth+PKCE loopback；可粘贴 token
        ProviderPreset(
            "accio", "Accio", "国际版网页登录", LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED, AuthChannel.EXTERNAL, AuthChannel.PASTE),
            pasteHint = "粘贴 accessToken 或 refreshToken", pasteMultiline = true,
            needsGateway = true,
        ),
        ProviderPreset(
            "accio-cn", "Accio 国内版", "国内版网页登录", LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED, AuthChannel.EXTERNAL, AuthChannel.PASTE),
            pasteHint = "粘贴 accessToken 或 refreshToken", pasteMultiline = true,
            needsGateway = true,
        ),
        // ZCode：CLI init 轮询
        ProviderPreset(
            "zcode", "ZCode 国内版", "CLI 授权轮询", LoginKind.WEBVIEW, WEB, supportsCheckin = true,
        ),
        ProviderPreset("zcode-intl", "ZCode 国际版", "CLI 授权轮询", LoginKind.WEBVIEW, WEB),
        // CodeArts：OAuth loopback；可粘贴 AK/SK/STS
        ProviderPreset(
            "codearts", "CodeArts", "华为云 OAuth 登录", LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED, AuthChannel.EXTERNAL, AuthChannel.PASTE),
            pasteHint = "粘贴 AK/SK 或 STS + domain/user", pasteMultiline = true,
            supportsCheckin = true,
            needsGateway = true,
        ),
        // Trae：进程内 loopback + PKCE
        ProviderPreset(
            "trae", "Trae", "SOLO 网页登录", LoginKind.WEBVIEW, WEB, supportsCheckin = true,
        ),
        // Loomy：手机验证码
        ProviderPreset(
            "loomy", "Loomy", "讯飞 · 手机验证码", LoginKind.SMS,
            listOf(AuthChannel.SMS), supportsCheckin = true,
        ),
        // Kuku：内嵌窗口登录（读站点的百度通行证 Cookie）或粘贴 Cookie。
        //
        // ── 为什么现在能用（2026-11-06）──────────────────────────
        // 以前这里挂着 `disabledNote = "引擎依赖官方 Windows 客户端…"`：会话令牌
        // 换发要加载 `genflowengine.dll`，Android 无从加载 ⇒ 网页登录的账号一律
        // userreport errno=-6。内核已改成纯算法签名（`providers/kuku/engine.rs`，
        // 对齐上游 3.4.1），任何平台都能换发，置灰理由不成立。
        //
        // 收尾走专用端点（`cookieCompletePath`）：百度通行证登录不产生授权码，
        // 凭证只有 Cookie（BDUSS / PTOKEN，HttpOnly，页面脚本读不到，但
        // CookieManager 读得到），所以交回 `{state, cookie}`。
        ProviderPreset(
            "kuku", "KukuAI", "百度文库 · 网页登录 / 粘贴 Cookie", LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED, AuthChannel.PASTE, AuthChannel.IMPORT_FILE),
            webLoginCookies = true,
            sessionCookieNames = listOf("BDUSS"),
            cookieCompletePath = "/api/session/login/kuku/complete",
            // 换发要 PTOKEN（通行证级 STOKEN 换不成业务级）；缺了会走「账号已添加
            // 但可能未登录」的警告，所以粘贴时点明要整串。
            pasteHint = "Cookie 整串（含 BDUSS 与 PTOKEN）", pasteMultiline = true,
            supportsCheckin = true,
        ),
        // MonkeyCode：内核不支持网页登录，只能粘贴 session
        ProviderPreset(
            "monkeycode", "MonkeyCode", "长亭 · 粘贴 session", LoginKind.PASTE,
            listOf(AuthChannel.PASTE),
            pasteHint = "粘贴会话 Cookie（session）", pasteMultiline = true,
        ),
        ProviderPreset(
            "monkeycode-intl", "MonkeyCode 国际版", "长亭国际 · 粘贴 session", LoginKind.PASTE,
            listOf(AuthChannel.PASTE),
            pasteHint = "粘贴会话 Cookie（session）", pasteMultiline = true,
        ),
        // Command Code：粘贴 user_ Key
        ProviderPreset(
            "commandcode", "Command Code", "粘贴 user_ API Key", LoginKind.PASTE,
            listOf(AuthChannel.PASTE),
            pasteHint = "API Key（user_ 开头）",
        ),
        // Antigravity：Google OAuth loopback；可粘贴 refresh token
        ProviderPreset(
            "antigravity", "Antigravity", "Google 授权 / 粘贴 refresh token", LoginKind.PASTE,
            listOf(AuthChannel.EMBEDDED, AuthChannel.EXTERNAL, AuthChannel.PASTE),
            pasteHint = "Google refresh token（1// 开头）", pasteMultiline = true,
            pasteSecondaryHint = "Google 账号邮箱（可选，同账号去重）",
            needsGateway = true,
        ),
        // OfficeAce：服务端中介轮询
        ProviderPreset(
            "officeace", "OfficeAce 果办", "网页登录", LoginKind.WEBVIEW, WEB, supportsCheckin = true,
        ),
        // OrcaTerm：回调由壳提交，只能内嵌
        ProviderPreset(
            "orcaterm", "OrcaTerm", "网页登录", LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED),
        ),
        // LobsterAI（网易有道「有道龙虾」）：**只有浏览器登录一条入口**。
        //
        // ── 为什么没有粘贴通道 ──────────────────────────────────────
        // 本家续期要一整组身份载荷件（`uuid` + 两个 `keyfrom`），只在换码那一刻
        // 由上游下发 —— 粘贴给不出这三样。内核 `api::accounts` 对它的粘贴入口
        // **始终回 400**（如实拒绝，不是漏做），所以这里也就不提供粘贴段。
        //
        // ── 回调落在网关端口 ───────────────────────────────────────
        // 门户 302 回 `127.0.0.1:<网关端口>/auth/callback`，所以要 `needsGateway = true`
        // （与 accio / codearts / antigravity 同一档）。
        //
        // 归 **Agent 分段**（用默认值，不显式声明）：本家在上游 agent 包里，
        // 与 ModelScope 同一判据。浏览器授权登录 ≠ 网页提供商。
        ProviderPreset(
            "lobsterai", "LobsterAI", "lobsterai.youdao.com · 网页登录", LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED, AuthChannel.EXTERNAL),
            supportsCheckin = true,
            needsGateway = true,
        ),
        // AtomCode（AtomGit）：**服务端中介的轮询登录** —— 授权回调在
        // `acs.atomgit.com/callback` 终结，不回到本机（与 ZCode / OfficeAce
        // 同一形态）。因此**不需要** `needsGateway`：没有本机回调端口要占。
        //
        // 另有**粘贴通道**：本家续期只要一枚 refreshToken（不像 LobsterAI 还要
        // uuid + keyfrom），用户手里的登录输出就够用，内核 `api::accounts` 的
        // Atomcode 分支确实收粘贴（只给 accessToken 也收，但那类账号不能续期）。
        // 签到 = 领取免费 CodingPlan（内核 CHECKIN_PROVIDERS 里有它），所以
        // `supportsCheckin = true`。归 **Agent 分段**（编程客户端）。
        ProviderPreset(
            "atomcode", "AtomCode", "AtomGit · 浏览器授权 / 粘贴令牌", LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED, AuthChannel.EXTERNAL, AuthChannel.PASTE),
            pasteHint = "粘贴 accessToken（登录输出里的 access_token）",
            pasteSecondaryHint = "续期令牌 refreshToken（可选；留空则到期后需重新登录）",
            supportsCheckin = true,
        ),
        // DeepSeek 网页版（chat.deepseek.com）：**粘贴 userToken**。
        //
        // ── 粘的不是 Cookie，也不是开放平台的 API Key ─────────────
        // 网页版认的是浏览器 localStorage 里 `userToken` 对象的 `.value`
        // —— 这条最容易走偏：把 Cookie 整串粘进来会稳定 401，用 `sk-` 的
        // 平台 Key 更是另一套接口（api.deepseek.com）。
        // 提示语因此写死「userToken」而不是笼统的「令牌」。
        //
        // ── 本家没有续期、没有签到（如实声明，别照抄邻近条目）──────
        // 上游没有 refresh 接口（官方能力表 refresh/checkin/usage 都是
        // `undeclared`），所以 `supportsCheckin` **不置位** —— 置位会让签到
        // 中心多出一行永远「不支持」的账号。
        //
        // 归 **Web 分段**：上游 web 插件包的成员只有 chatglm 与它两家。
        ProviderPreset(
            "deepseek-web", "DeepSeek 网页", "chat.deepseek.com · 粘贴 userToken",
            LoginKind.PASTE,
            listOf(AuthChannel.PASTE),
            category = ProviderCategory.WEB,
            pasteHint = "粘贴 userToken（浏览器 F12 → localStorage 里 userToken 的 value）",
            pasteMultiline = true,
        ),
        // 豆包工作（www.doubao.com）：**粘贴 Cookie**（整串）。
        //
        // ── 归 Agent 分段（用注册表默认值）──────────────────────
        // 上游 agent 插件包里的家（该包不声明 provider_category）。
        //
        // ── 为什么有签到、没有「网页登录」通道 ───────────────────
        // 签到 = 读当日签到态 + 领取活动奖励（内核 `rewards`）；登录本应是
        // 「内嵌窗口取 Cookie」，但那条链在 Android 侧的收尾（从 WebView 读
        // Cookie 再提交）尚未在客户端实现，因此**只提供粘贴入口** ——
        // 给一个点了没反应的通道比少给一条更糟。
        // 通道与上游 beta 的 `add-doubaowork-config.ts` 对齐：
        //   网页登录（内嵌窗口自动读 Cookie）/ 填写凭证 / 导入凭证文件。
        //   ⚠️ 上游还有「导入桌面端登录态」，它要读本机豆包客户端的 Cookie
        //   （且客户端得带 `--remote-debugging-port=9342` 启动）—— Android 上
        //   没有这回事，与其余各家同一处置：枚举里保留、UI 里过滤掉
        //   （见 AddProviderPage 的 `usable`）。
        ProviderPreset(
            "doubaowork", "豆包工作", "www.doubao.com · 网页登录 / 粘贴 Cookie",
            LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED, AuthChannel.PASTE, AuthChannel.IMPORT_FILE),
            webLoginCookies = true,
            // 与内核 `doubaowork::credentials::SESSION_COOKIE_NAMES` 同一组名字
            sessionCookieNames = listOf("sessionid", "sessionid_ss", "sid_guard"),
            // 粘贴框三种形态都收（与内核 `login::normalize_import` 一致）：
            // Cookie 头串 / Cookie JSON 数组 / 扁平键值对象（上游 beta 的
            // 说明里也写了「也支持 Cookie JSON 数组或对象」）
            pasteHint = "Cookie 整串（必须含 sessionid），也支持 Cookie JSON 数组或对象",
            pasteMultiline = true,
            supportsCheckin = true,
        ),

        // ══ 第 4 批新增的四家 ══════════════════════════════════════
        // 内核侧已全部接完（见 docs/上游3.4.1-同步与移植分析.md），这里是客户端
        // 那一半。**顺序与内核 `PROVIDERS` 尾部逐条对齐**：本仓按接入批次排在末尾，
        // 不按上游 order（与 qwenwork / tabbit 同一处置）。
        //
        // ── 四家的登录通道现状（改前先核对内核能力位，别按印象写）──────
        //   tabbit       手机验证码 ✓（两段同步请求，客户端复用 `SmsLoginForm`）
        //   tabbit-intl  ✗（上游对国际站灰度 403，同代码打过去必然失败）
        //   kiro         浏览器登录 ✓（系统浏览器 + 本机白名单端口回调）
        //   astudio      本机登录页 ✓（手机号验证码 / 账号密码 + 极验滑块）
        // 一个通道**点了必然 400** 时不要给 —— 与豆包工作「网页登录通道还没在壳侧
        // 实现，就先不提供」同一处置。

        // Tabbit 两个站点：上游在 **web 插件包**里（`provider_category: "web"`），
        // 两个站账号不通用、签名 key 不同，所以是两个 id、各填各的 Cookie
        // （拿国内站的 Cookie 打国际站只会 401）。
        //
        // 内核侧的事实（改这条注释前先核对 `tabbit/adapter.rs`）：
        // 有真实的每日签到活动（`daily_sign_in`，奖励是额度重置券），
        // 但**没有续期接口**（JWT 7 天有效，失效只能重粘）。
        ProviderPreset(
            "tabbit", "Tabbit 国内版", "web.tabbit.com · 手机验证码 / 粘贴 Cookie",
            LoginKind.SMS,
            // 国内站有手机号+验证码这条链（内核 `tabbit::login`，两段同步请求，
            // 不发窗口、不起任务）：`SmsLoginForm` 直接能用，无需新增表单。
            listOf(AuthChannel.SMS, AuthChannel.PASTE),
            category = ProviderCategory.WEB,
            // 一段文本收三种粘法（内核 `credentials::normalize_payload` 按**内容**
            // 判，不认字段名）：Cookie 头单行 / DevTools 的 Copy as cURL 整段 / 裸 JWT。
            // 客户端**不做**半套解析 —— 两处各拆一次必然分叉，症状是「同一个串
            // 粘贴能加、转发却报缺令牌」。
            pasteHint = "粘贴网页 Cookie 整串（token=…; user_id=…），或 DevTools 的 Copy as cURL 整段，或裸 JWT",
            pasteMultiline = true,
            supportsCheckin = true,
        ),
        ProviderPreset(
            "tabbit-intl", "Tabbit 国际版", "web.tabbit.ai · 粘贴 Cookie / 登录 JWT",
            LoginKind.PASTE,
            listOf(AuthChannel.PASTE),
            category = ProviderCategory.WEB,
            // 国际站**只能导入 Cookie**（上游没有手机号登录），提示里点明这一点：
            // 用户拿着国内站的 Cookie 来填是常见错误，坐实「不通用」比让他自己试出来好。
            pasteHint = "粘贴国际站的网页 Cookie（与国内站不通用），或裸 JWT",
            pasteMultiline = true,
            supportsCheckin = true,
        ),

        // Kiro（AWS 的 AI IDE 订阅，`kiro.dev`）：浏览器登录（门户 OAuth），
        // 或粘贴官方 API key（`ksk_…`）/ 整段 `kiro-auth-token.json`。
        //
        // ── 为什么只有「浏览器登录」，没有内嵌 ────────────────────
        // Google 与 GitHub 都禁止在 WebView 内完成 OAuth（回
        // `disallowed_useragent`），所以 `EMBEDDED` 不在通道里 —— 给了就是个
        // 必然失败的按钮。它与 Trae 的 `EXTERNAL` 是同形（start → 开系统浏览器
        // → 轮询 `/wait`），客户端不需要新表单，也不需要发 `mode`：内核
        // `start_kiro_login` 是专用入口（`browserLogin` 保持 false）。
        //
        // ── 与 Trae 的一个硬差别 ──────────────────────────────────
        // 回调端口**不能**随便挑：门户把它与官方 IDE 注册过的那组固定端口比对，
        // 白名单外的端口不报错、只是不再跳回来（症状是"点了登录什么都没发生"）。
        // 内核会从 10 条候选里逐一抢（`kiro::CALLBACK_PORTS`），全被占才报错。
        //
        // 本次仍不提供「导入桌面端登录态」—— 那份登录态是 kiro-cli 的 SQLite
        // 与 Kiro IDE 的文件，Android 上两者都不存在（内核 `api::accounts` 对该家
        // 的 importDesktop 直接回 400 并说明原因）。
        // `IMPORT_FILE` 给的是**文本 JSON**（`kiro-auth-token.json`）这一条，
        // 与内核「不吃二进制凭证文件」一致。
        //
        // 不标签到：内核 `CHECKIN_PROVIDERS` 里没有它（上游没有可自动领的奖励）——
        // 勾上一个永远「不支持」的家比不勾更糟。
        ProviderPreset(
            "kiro", "Kiro", "kiro.dev · 浏览器登录 / 粘贴 API key",
            LoginKind.WEBVIEW,
            listOf(AuthChannel.EXTERNAL, AuthChannel.PASTE, AuthChannel.IMPORT_FILE),
            pasteHint = "粘贴 ksk_ 开头的 API key，或整段 kiro-auth-token.json（含 accessToken / refreshToken）",
            pasteMultiline = true,
        ),

        // AStudio（科大讯飞「星火 Astron Studio」桌面端）：本机登录页（手机号验证码 /
        // 账号密码 + 极验滑块），或粘贴 `modelBearerToken` / `astronSession.json`。
        //
        // ── 登录页为什么能在这里跑 ──────────────────────────────
        // 上游的登录页**不是某个第三方站点**，而是插件自己发的一段静态 HTML：
        // 它只依赖 `static.geetest.com` 的脚本，接口都打回同一个进程；极验滑块
        // 必须在真实页面里完成，所以网关起一个只监听 127.0.0.1 的小服务
        // （`providers/astudio/login_page.rs`），登录窗打开 `http://127.0.0.1:<端口>/?t=…`
        // 即可。`network_security_config.xml` 本来就为内核回环放行了明文，
        // `AuthWebViewSheet` 对 http(s) 一律放行 —— 两处都不用改。
        // 收尾也不靠读 Cookie（`webLoginCookies` 保持 false）：页面自己带着结果
        // 回来，壳侧只管轮询 `/wait`。
        //
        // ── 两套凭据，粘哪一套都能加 ────────────────────────────
        //   只 `modelBearerToken`      → 能对话；没有余额行、不参与签到
        //   + `token` / `accountId`    → 余额 / 会员 / 签到全通
        // 只粘网页会话是错的（内核会回「没有模型凭据」），所以**一个输入框**就够了
        // —— 整段文件、裸令牌、带 `Bearer ` 外壳的整行都由内核
        // `credentials::normalize_paste` 按内容判。不做「两栏分别填」：那要把两段
        // 拼成内核认的 JSON，等于在客户端再写一遍归一逻辑（与上面同一条纪律）。
        //
        // 签到能标上：内核 `CHECKIN_PROVIDERS` 里有它。但只有模型凭据的账号
        // 由实现直接回 `ineligible`，不会空打 —— 界面上「无网页会话」是如实陈述，
        // 不是「账号异常」（账号列表里读 `noSession` 那个标记）。
        // **登录路径一定两套凭据都有**（登录时顺便查了模型 key），所以它加的
        // 账号不会有「无网页会话」这个提示 —— 只有粘贴路径可能有。
        ProviderPreset(
            "astudio", "AStudio", "讯飞星辰 · 手机号登录 / 粘贴 astron-session.json",
            LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED, AuthChannel.PASTE, AuthChannel.IMPORT_FILE),
            pasteHint = "粘贴 modelBearerToken，或整段 astron-session.json（带上 token / accountId 时余额与签到才可用）",
            pasteMultiline = true,
            supportsCheckin = true,
        ),

        // QuWork（1.6.0）：上游插件 meta.order = 33，归 **Agent 分段**。
        // 只支持**粘贴**（Android 上没有 QuWork 桌面客户端可读登录态，去桌面化）。
        // 两套凭据：模型凭证 llmToken 必填（对话用），登录凭证 accessToken
        // 可选（积分 / 续期 / 每日奖励用；只填 llmToken 也能对话）。
        // 内核 `api::accounts` 的 QuWork 分支把整段凭证交给
        // `quwork::credentials::normalize_paste` 按内容判（JSON / 纯 token /
        // `QURIVS_LLM_API_KEY:` yaml 行），客户端不写第二套解析。
        ProviderPreset(
            "quwork", "QuWork", "QuWork 1.6.0 · 粘贴凭证",
            LoginKind.PASTE,
            listOf(AuthChannel.PASTE),
            pasteHint = "模型凭证 llmToken 必填：粘贴凭证 JSON（含 llmToken，可选 accessToken）、纯 llmToken，或 QURIVS_LLM_API_KEY: 的 yaml 行",
            pasteSecondaryHint = "登录凭证 accessToken（可选；填写后可用积分 / 续期 / 每日奖励）",
            pasteMultiline = true,
            supportsCheckin = true,
        ),
    )

    fun byId(id: String): ProviderPreset? = ALL.firstOrNull { it.id == id }

    /** 某一分段（Agent / Web）下的全部家，顺序沿用 [ALL]。 */
    fun byCategory(category: ProviderCategory): List<ProviderPreset> =
        ALL.filter { it.category == category }

    /** 未知家（手改数据文件塞进来的 id）回落到 id 本身显示。 */
    fun labelOf(id: String): String = byId(id)?.label ?: id
}