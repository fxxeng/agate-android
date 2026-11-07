package dev.agate.android.ui.theme

import androidx.compose.runtime.Immutable
import androidx.compose.ui.graphics.Color

/**
 * 设计 tokens。**深色档 2026-11-07 重设计为纯黑 OLED + 冷蓝强调**：
 * 页面底 #000000，卡片用微亮中性灰 #101014 + 细描边分层，页头/底栏 #0A0A0C；
 * 语义色整体提亮一档（旧的 slate 基色在纯黑上偏闷）。浅色档逐值保持 v44 原样。
 *
 * 组件一律经 `agateColors()` 取色，这里是唯一的配色事实源；页面里不应再有
 * 写死的深色专属色值（Hero 卡渐变、徽标描边、报表折线都已收进来）。
 */

// ── 深色（纯黑 OLED） ──
val DarkBg = Color(0xFF000000)          // 页面底：真·纯黑
val DarkInset = Color(0xFF0A0A0C)       // 内嵌底 / 外壳
val DarkCard = Color(0xFF101014)        // 卡片：微亮中性灰，靠它和描边分层
val DarkBorder = Color(0xFF30303A)      // 细描边（纯黑底上提亮一档，卡片/弹层边界更清楚）
val DarkAccent = Color(0xFF4CC9F0)      // 冷蓝强调（比原 sky 稍柔，纯黑上不刺眼）
val DarkTextMain = Color(0xFFF4F6FA)
val DarkTextSub = Color(0xFF9CA3AF)
val DarkTextDim = Color(0xFF6B7280)
val DarkNavBg = Color(0xF20A0A0C)       // 半透明壳叠在纯黑上
val DarkSegBg = DarkBg                  // 分段控件底 = 页面底
val DarkSegActive = DarkCard
val DarkBtnBg = Color(0xFF1C1C22)
val DarkSwitchOff = Color(0xFF26262E)
val DarkDotOff = Color(0xFF26262E)
val DarkDotBorder = Color(0xFF3A3A44)
val DarkPillBg = Color(0x1FF4F6FA)      // 徽标底：白 12%，纯黑上不刺眼
// 外壳色：页头 / 底栏 / 系统栏统一实色，消除半透明底与实底之间的分界缝。
val DarkChrome = Color(0xFF0A0A0C)

// ── 浅色 ──
val LightBg = Color(0xFFF1F5F9)         // --bg-dark
val LightCard = Color(0xFFFFFFFF)       // --card-bg
val LightBorder = Color(0xFFE2E8F0)     // --card-border
val LightInset = Color(0xFFF8FAFC)      // --bg-inset
val LightAccent = Color(0xFF0284C7)     // --accent
val LightTextMain = Color(0xFF0F172A)
val LightTextSub = Color(0xFF475569)    // --text-sub（v18 增量覆盖）
val LightTextDim = Color(0xFF64748B)    // --text-dim（v18 增量覆盖）
val LightNavBg = Color(0xEBFFFFFF)      // rgba(255,255,255,.92)
val LightSegBg = Color(0xFFE2E8F0)
val LightSegActive = Color(0xFFFFFFFF)
val LightBtnBg = Color(0xFFE2E8F0)
val LightSwitchOff = Color(0xFFCBD5E1)
val LightDotOff = Color(0xFFE2E8F0)
val LightDotBorder = Color(0xFFCBD5E1)
val LightPillBg = Color(0x4D0F172A)     // rgba(15,23,42,.30)
// 外壳色：nav-bg rgba(255,255,255,.92) 叠在 bg #F1F5F9 上的精确实色（= #FEFEFE）。
val LightChrome = Color(0xFFFEFEFE)

// ── 语义基色（深色档，纯黑底上整体提亮一档） ──
val GreenDark = Color(0xFF3DDC84)
val RedDark = Color(0xFFFF5C7C)
val AmberDark = Color(0xFFFFB84D)
val BlueDark = Color(0xFF5B9CFF)

// ── 语义基色（浅色档） ──
val GreenLight = Color(0xFF16A34A)
val RedLight = Color(0xFFE11D48)
val AmberLight = Color(0xFFD97706)
val BlueLight = Color(0xFF2563EB)

// ── badge 亮色文字 ──
// 浅色档沿用 v44 原值（浅色主题本次不动）。
val BadgeGreenText = Color(0xFF4ADE80)
val BadgeBlueText = Color(0xFF60A5FA)
val BadgeRedText = Color(0xFFFB7185)
val BadgeAmberText = Color(0xFFFBBF24)
val BadgeGrayText = Color(0xFF94A3B8)
// 纯黑档：跟着新语义基色一起提亮，避免旧 slate 亮色文字在纯黑上发灰。
val BadgeGreenTextDark = Color(0xFF6EE7A8)
val BadgeBlueTextDark = Color(0xFF8DB6FF)
val BadgeRedTextDark = Color(0xFFFF8FA5)
val BadgeAmberTextDark = Color(0xFFFFCF7A)
val BadgeGrayTextDark = Color(0xFF9CA3AF)

/** 控制台 Hero 卡配色。原先硬编码在 ConsolePage 里按底色亮度二选一，
 * 收进主题后深浅各一份，页面不必再猜。 */
@Immutable
data class AgateHeroPalette(
    val bg1: Color,
    val bg2: Color,
    val border: Color,
    val text: Color,
    val sub: Color,
    val pillBg: Color,
    val pillText: Color,
)

/** 全套语义色集合，经 [LocalAgateColors] 注入，组件一律从这里取色。 */
@Immutable
data class AgateColors(
    val bg: Color,
    val insetBg: Color,
    val cardBg: Color,
    val border: Color,
    val accent: Color,
    val onAccent: Color,
    val textMain: Color,
    val textSub: Color,
    val textDim: Color,
    val navBg: Color,
    /** 外壳实色（页头/底栏/系统栏统一，消除分界缝） */
    val chromeBg: Color,
    val segBg: Color,
    val segActiveBg: Color,
    val btnBg: Color,
    val switchOff: Color,
    val dotOff: Color,
    val dotBorder: Color,
    val pillBg: Color,
    val green: Color,
    val red: Color,
    val amber: Color,
    val blue: Color,
    val greenBg: Color,
    val redBg: Color,
    val amberBg: Color,
    val blueBg: Color,
    val badgeGreenText: Color = BadgeGreenText,
    val badgeBlueText: Color = BadgeBlueText,
    val badgeRedText: Color = BadgeRedText,
    val badgeAmberText: Color = BadgeAmberText,
    val badgeGrayText: Color = BadgeGrayText,
    /** 弹层遮罩：纯黑档压得更重，浅色档沿用旧值 */
    val scrim: Color,
    /** 控制台 Hero 卡配色（深浅各一份，页面不再写死） */
    val hero: AgateHeroPalette,
    /** 热力图四级强度（0 空 → 3 满） */
    val heatLevels: List<Color>,
) {
    companion object {
        val Dark = AgateColors(
            bg = DarkBg, insetBg = DarkInset, cardBg = DarkCard, border = DarkBorder,
            accent = DarkAccent, onAccent = Color(0xFF00131C),
            textMain = DarkTextMain, textSub = DarkTextSub, textDim = DarkTextDim,
            navBg = DarkNavBg, chromeBg = DarkChrome, segBg = DarkSegBg, segActiveBg = DarkSegActive,
            btnBg = DarkBtnBg, switchOff = DarkSwitchOff,
            dotOff = DarkDotOff, dotBorder = DarkDotBorder, pillBg = DarkPillBg,
            green = GreenDark, red = RedDark, amber = AmberDark, blue = BlueDark,
            greenBg = Color(0x2E3DDC84), redBg = Color(0x2EFF5C7C),
            amberBg = Color(0x2EFFB84D), blueBg = Color(0x2E5B9CFF),
            scrim = Color(0xB3000000),
            badgeGreenText = BadgeGreenTextDark,
            badgeBlueText = BadgeBlueTextDark,
            badgeRedText = BadgeRedTextDark,
            badgeAmberText = BadgeAmberTextDark,
            badgeGrayText = BadgeGrayTextDark,
            // 纯黑档不再铺一整块蓝：极淡中性渐变 + 冷蓝文字/呼吸点做焦点。
            hero = AgateHeroPalette(
                bg1 = Color(0xFF08080A), bg2 = Color(0xFF111318),
                border = Color(0xFF29323A), text = DarkTextMain, sub = DarkAccent,
                pillBg = Color(0xFF1B2026), pillText = DarkTextMain,
            ),
            heatLevels = listOf(
                Color(0x1A6B7280),
                Color(0x594CC9F0),
                Color(0x994CC9F0),
                DarkAccent,
            ),
        )

        val Light = AgateColors(
            bg = LightBg, insetBg = LightInset, cardBg = LightCard, border = LightBorder,
            accent = LightAccent, onAccent = Color(0xFFFFFFFF),
            textMain = LightTextMain, textSub = LightTextSub, textDim = LightTextDim,
            navBg = LightNavBg, chromeBg = LightChrome, segBg = LightSegBg, segActiveBg = LightSegActive,
            btnBg = LightBtnBg, switchOff = LightSwitchOff,
            dotOff = LightDotOff, dotBorder = LightDotBorder, pillBg = LightPillBg,
            green = GreenLight, red = RedLight, amber = AmberLight, blue = BlueLight,
            greenBg = Color(0x2616A34A), redBg = Color(0x26E11D48),
            amberBg = Color(0x26D97706), blueBg = Color(0x262563EB),
            scrim = Color(0x99000000),
            // 浅色档 Hero 逐值保持 v44 原样（本次只重设计深色）。
            hero = AgateHeroPalette(
                bg1 = Color(0xFFE0F2FE), bg2 = Color(0xFFBAE6FD),
                border = Color(0xFF7DD3FC), text = Color(0xFF0C4A6E), sub = Color(0xFF0369A1),
                pillBg = Color(0x66FFFFFF), pillText = Color(0xFF0C4A6E),
            ),
            heatLevels = listOf(
                Color(0x1A64748B),
                Color(0x590284C7),
                Color(0x990284C7),
                LightAccent,
            ),
        )
    }
}
