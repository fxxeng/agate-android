package dev.agate.android.data

/**
 * 官方提供商预置注册表（M3-3）。
 *
 * 顺序与内核 providers::PROVIDERS 数组逐条对齐（内核是该白名单的唯一事实来源，
 * 见 kind_from_id）：同一条产品线的两个版本相邻排列，加新家在末尾追加并同步内核。
 * 展示名直接采用内核 label（界面与内核分组标题永远一致）。
 */

/** 登录通道：决定点卡片后的交互形态。 */
enum class LoginKind { WEBVIEW, SMS, PASTE }

data class ProviderPreset(
    val id: String,
    val label: String,
    val desc: String,
    val kind: LoginKind,
)

object PresetRegistry {

    val ALL: List<ProviderPreset> = listOf(
        ProviderPreset("workbuddy", "WorkBuddy 国内版", "copilot.tencent.com 网页登录", LoginKind.WEBVIEW),
        ProviderPreset("workbuddy-intl", "WorkBuddy 国际版", "workbuddy.ai 网页登录", LoginKind.WEBVIEW),
        ProviderPreset("raccoon", "小浣熊", "JWT 凭证网页登录", LoginKind.WEBVIEW),
        ProviderPreset("catpaw", "CatPaw", "美团 · 网页登录", LoginKind.WEBVIEW),
        ProviderPreset("autoclaw", "AutoClaw 国内版", "智谱 · 手机验证码", LoginKind.SMS),
        ProviderPreset("autoclaw-intl", "AutoClaw 国际版", "autoglm.ai · 手机验证码", LoginKind.SMS),
        ProviderPreset("qoder", "Qoder 中国版", "设备授权登录", LoginKind.WEBVIEW),
        ProviderPreset("qoder-intl", "Qoder 国际版", "设备授权登录", LoginKind.WEBVIEW),
        ProviderPreset("cline-free", "Cline Free", "免费池网页登录", LoginKind.WEBVIEW),
        ProviderPreset("cline-pass", "Cline Pass", "付费池网页登录", LoginKind.WEBVIEW),
        ProviderPreset("accio", "Accio", "国际版网页登录", LoginKind.WEBVIEW),
        ProviderPreset("accio-cn", "Accio 国内版", "国内版网页登录", LoginKind.WEBVIEW),
        ProviderPreset("zcode", "ZCode 国内版", "CLI 授权轮询", LoginKind.WEBVIEW),
        ProviderPreset("zcode-intl", "ZCode 国际版", "CLI 授权轮询", LoginKind.WEBVIEW),
        ProviderPreset("codearts", "CodeArts", "华为云 OAuth 登录", LoginKind.WEBVIEW),
        ProviderPreset("trae", "Trae", "SOLO 网页登录", LoginKind.WEBVIEW),
        ProviderPreset("loomy", "Loomy", "讯飞 · 手机验证码", LoginKind.SMS),
        ProviderPreset("kuku", "KukuAI", "百度文库 · 粘贴 Cookie", LoginKind.PASTE),
        ProviderPreset("monkeycode", "MonkeyCode", "长亭 · 网页登录", LoginKind.WEBVIEW),
        ProviderPreset("monkeycode-intl", "MonkeyCode 国际版", "长亭国际 · 网页登录", LoginKind.WEBVIEW),
        ProviderPreset("commandcode", "Command Code", "粘贴 user_ API Key", LoginKind.PASTE),
        ProviderPreset("antigravity", "Antigravity", "Google · 粘贴 refresh token", LoginKind.PASTE),
        ProviderPreset("officeace", "OfficeAce 果办", "网页登录", LoginKind.WEBVIEW),
        ProviderPreset("orcaterm", "OrcaTerm", "网页登录", LoginKind.WEBVIEW),
    )

    fun byId(id: String): ProviderPreset? = ALL.firstOrNull { it.id == id }

    /** 未知家（手改数据文件塞进来的 id）回落到 id 本身显示。 */
    fun labelOf(id: String): String = byId(id)?.label ?: id
}
