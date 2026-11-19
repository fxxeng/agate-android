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

/** 添加账号页的分段归属：Agent（编程/对话客户端）/ Web（网页版）。 */
enum class ProviderCategory { AGENT, WEB }

data class ProviderPreset(
    val id: String,
    val label: String,
    val desc: String,
    /** 兼容字段（旧 UI 仍读它）；新 UI 请读 [channels]。 */
    val kind: LoginKind,
    /** 该家真实支持的通道，顺序即 UI 顺序；首项为默认。 */
    val channels: List<AuthChannel>,
    /** 添加账号页的分段归属；默认 Agent，Web 家逐个声明。 */
    val category: ProviderCategory = ProviderCategory.AGENT,
    /** 粘贴通道的主输入提示（为空表示该家不走粘贴）。 */
    val pasteHint: String? = null,
    val pasteSecondaryHint: String? = null,
    /** 粘贴框是否多行（整份 JSON / Cookie 串）。 */
    val pasteMultiline: Boolean = false,
    /** 能力标签：是否支持自动签到（来自内核 CHECKIN_PROVIDERS）。 */
    val supportsCheckin: Boolean = false,
    /**
     * 登录是否依赖「网关端口」收回调。
     * true 的四家（accio/accio-cn/codearts/antigravity）回调落在
     * `127.0.0.1:<网关端口>`，Core 态不监听该端口 → 网关未运行时登录必失败。
     * 两个例外：`trae` 自己 bind 临时端口；`catpaw` 有 poll-token 兜底
     * （不经过浏览器，不受 PNA/CORS 影响），网关没开也能完成。
     */
    val needsGateway: Boolean = false,
    /**
     * 非空 = 整家置灰不可添加（F12）。如 Kuku：换票引擎是官方 Windows
     * 客户端的 genflowengine.dll，Android 无从加载 → 入口置灰，已有账号不动。
     */
    val disabledNote: String? = null,
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
)

object PresetRegistry {

    private val WEB = listOf(AuthChannel.EMBEDDED, AuthChannel.EXTERNAL)

    val ALL: List<ProviderPreset> = listOf(
        // WorkBuddy：上游推 authUrl → 轮询 auth/token；可粘贴 auth.json（需 uid）
        // 智谱清言（chatglm.cn）：无状态转发 + 双凭证形态 —— 访客免登录可换证、
        // 登录账号有积分。客户端这边只给两条最稳的通道：粘贴 accessToken /
        // refreshToken，或导入本机桌面客户端登录态（`importDesktop: true`，
        // 内核 `api::accounts` 的 Chatglm 分支直接认这两个字段）。
        // 不做内嵌网页登录：那需要跑上游的签名三件套 + 设备号，网页端流程在
        // WebView 里不可靠。
        ProviderPreset(
            "chatglm", "智谱清言", "chatglm.cn · 粘贴 Token 或导入桌面登录态", LoginKind.WEBVIEW,
            listOf(AuthChannel.PASTE, AuthChannel.IMPORT_DESKTOP),
            category = ProviderCategory.WEB,
            pasteHint = "粘贴 accessToken（或 refreshToken）",
            pasteSecondaryHint = "留空则按访客（guest）添加",
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
        // Kuku：粘贴 Cookie；文件导入需二进制 SQLite
        ProviderPreset(
            "kuku", "KukuAI", "百度文库 · 粘贴 Cookie", LoginKind.PASTE,
            listOf(AuthChannel.PASTE, AuthChannel.IMPORT_FILE),
            pasteHint = "Cookie 整串（含 BDUSS 与 STOKEN）", pasteMultiline = true,
            supportsCheckin = true,
            disabledNote = "引擎依赖官方 Windows 客户端，Android 端暂不可用",
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

        // ══ R63 批3 已移植进内核的七家（预设与 main 逐字对齐）══
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
            "atomcode", "AtomCode", "AtomGit · 浏览器授权 / 粘贴令牌", LoginKind.WEBVIEW,
            listOf(
                AuthChannel.EMBEDDED, AuthChannel.EXTERNAL,
                AuthChannel.PASTE, AuthChannel.IMPORT_FILE,
            ),
            pasteHint = "粘贴 accessToken（登录输出里的 access_token）",
            pasteSecondaryHint = "续期令牌 refreshToken（可选；留空则到期后需重新登录）",
            supportsCheckin = true,
        ),
        ProviderPreset(
            "lobsterai", "LobsterAI", "lobsterai.youdao.com · 网页登录", LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED, AuthChannel.EXTERNAL),
            supportsCheckin = true,
            needsGateway = true,
        ),
        ProviderPreset(
            "kiro", "Kiro", "kiro.dev · 浏览器登录 / 粘贴 API key",
            LoginKind.WEBVIEW,
            listOf(AuthChannel.EXTERNAL, AuthChannel.PASTE, AuthChannel.IMPORT_FILE),
            pasteHint = "粘贴 ksk_ 开头的 API key，或整段 kiro-auth-token.json（含 accessToken / refreshToken）",
            pasteMultiline = true,
        ),
        ProviderPreset(
            "astudio", "AStudio", "讯飞星辰 · 手机号登录 / 粘贴 astron-session.json",
            LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED, AuthChannel.PASTE, AuthChannel.IMPORT_FILE),
            pasteHint = "粘贴 modelBearerToken，或整段 astron-session.json（带上 token / accountId 时余额与签到才可用）",
            pasteMultiline = true,
            supportsCheckin = true,
        ),
        ProviderPreset(
            "quwork", "QuWork", "QuWork 1.6.0 · 粘贴凭证 / 导入桌面登录库",
            LoginKind.PASTE,
            listOf(AuthChannel.PASTE, AuthChannel.IMPORT_FILE),
            pasteHint = "模型凭证 llmToken 必填：粘贴凭证 JSON（含 llmToken，可选 accessToken）、纯 llmToken，或 QURIVS_LLM_API_KEY: 的 yaml 行",
            pasteSecondaryHint = "登录凭证 accessToken（可选；填写后可用积分 / 续期 / 每日奖励 / 权益兑换）",
            pasteMultiline = true,
            supportsCheckin = true,
        ),

        // ══ R63 批3-C 移植进内核的四家（6 身份）══
        ProviderPreset(
            "deepseek-web", "DeepSeek 网页", "chat.deepseek.com · 粘贴 userToken",
            LoginKind.PASTE,
            listOf(AuthChannel.PASTE),
            category = ProviderCategory.WEB,
            pasteHint = "粘贴 userToken（浏览器 F12 → localStorage 里 userToken 的 value）",
            pasteMultiline = true,
        ),
        ProviderPreset(
            "doubaowork", "豆包工作", "www.doubao.com · 网页登录 / 粘贴 Cookie",
            LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED, AuthChannel.PASTE, AuthChannel.IMPORT_FILE),
            pasteHint = "Cookie 整串（必须含 sessionid），也支持 Cookie JSON 数组或对象",
            pasteMultiline = true,
            supportsCheckin = true,
        ),
        ProviderPreset(
            "lingxi", "WPS 灵犀", "WPS 灵犀 · 网页登录 / 粘贴 wps_sid",
            LoginKind.WEBVIEW,
            listOf(AuthChannel.EMBEDDED, AuthChannel.PASTE),
            pasteHint = "粘贴整段 Cookie（含 wps_sid）或 V02S 开头的裸 sid",
            supportsCheckin = true,
        ),
        ProviderPreset(
            "tabbit", "Tabbit 国内版", "web.tabbit.com · 手机验证码 / 粘贴 Cookie",
            LoginKind.SMS,
            listOf(AuthChannel.SMS, AuthChannel.PASTE),
            category = ProviderCategory.WEB,
            pasteHint = "粘贴网页 Cookie 整串（token=…; user_id=…），或 DevTools 的 Copy as cURL 整段，或裸 JWT",
            pasteMultiline = true,
            supportsCheckin = true,
        ),
        ProviderPreset(
            "tabbit-intl", "Tabbit 国际版", "web.tabbit.ai · 粘贴 Cookie / 登录 JWT",
            LoginKind.PASTE,
            listOf(AuthChannel.PASTE),
            category = ProviderCategory.WEB,
            pasteHint = "粘贴国际站的网页 Cookie（与国内站不通用），或裸 JWT",
            pasteMultiline = true,
            supportsCheckin = true,
        ),
        ProviderPreset(
            "qwenwork", "千问办公 国内版", "设备授权登录", LoginKind.WEBVIEW,
            listOf(AuthChannel.DEVICE_CODE, AuthChannel.EXTERNAL),
        ),
    )

    fun byId(id: String): ProviderPreset? = ALL.firstOrNull { it.id == id }

    /** 某一分段（Agent / Web）下的全部家，顺序沿用 [ALL]。 */
    fun byCategory(category: ProviderCategory): List<ProviderPreset> =
        ALL.filter { it.category == category }

    /** 未知家（手改数据文件塞进来的 id）回落到 id 本身显示。 */
    fun labelOf(id: String): String = byId(id)?.label ?: id
}