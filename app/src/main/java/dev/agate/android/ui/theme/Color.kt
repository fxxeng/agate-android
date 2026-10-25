package dev.agate.android.ui.theme

import androidx.compose.runtime.Immutable
import androidx.compose.ui.graphics.Color

/**
 * v44 设计 tokens（123_v44.html :root 变量逐值映射，slate/sky 体系）。
 * 深色为默认态；浅色仅替换背景/文字/语义基色档位（badge 亮色文字深浅共用）。
 */

// ── 深色（默认） ──
val DarkBg = Color(0xFF0F172A)          // --bg-dark / --bg-inset
val DarkCard = Color(0xFF1E293B)        // --card-bg
val DarkBorder = Color(0xFF334155)      // --card-border
val DarkAccent = Color(0xFF38BDF8)      // --accent
val DarkTextMain = Color(0xFFF8FAFC)    // --text-main
val DarkTextSub = Color(0xFFA3B1C6)     // --text-sub（v18 增量覆盖：原 #94A3B8）
val DarkTextDim = Color(0xFF7C8BA1)     // --text-dim（v18 增量覆盖：原 #64748B）
val DarkNavBg = Color(0xE61E293B)       // --nav-bg rgba(30,41,59,.92)
val DarkSegBg = DarkBg                  // --seg-bg
val DarkSegActive = DarkCard            // --seg-active-bg
val DarkBtnBg = Color(0xFF334155)       // --btn-bg
val DarkSwitchOff = Color(0xFF334155)   // --switch-off
val DarkDotOff = Color(0xFF334155)      // --dot-off
val DarkDotBorder = Color(0xFF475569)   // --dot-border
val DarkPillBg = Color(0x59F8FAFC)      // --pill-bg rgba(248,250,252,.35)
// 外壳色：nav-bg rgba(30,41,59,.92) 叠在 bg #0F172A 上的精确实色（= #1D283A）。
// 页头 / 底栏 / 系统栏三者统一用它，消除半透明底与实底之间的分界缝。
val DarkChrome = Color(0xFF1D283A)

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

// ── 语义基色（深色档） ──
val GreenDark = Color(0xFF22C55E)
val RedDark = Color(0xFFF43F5E)
val AmberDark = Color(0xFFF59E0B)
val BlueDark = Color(0xFF3B82F6)

// ── 语义基色（浅色档） ──
val GreenLight = Color(0xFF16A34A)
val RedLight = Color(0xFFE11D48)
val AmberLight = Color(0xFFD97706)
val BlueLight = Color(0xFF2563EB)

// ── badge 亮色文字（v44 深浅共用，比基色亮一档） ──
val BadgeGreenText = Color(0xFF4ADE80)
val BadgeBlueText = Color(0xFF60A5FA)
val BadgeRedText = Color(0xFFFB7185)
val BadgeAmberText = Color(0xFFFBBF24)
val BadgeGrayText = Color(0xFF94A3B8)

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
    /** 热力图四级强度（0 空 → 3 满） */
    val heatLevels: List<Color>,
) {
    companion object {
        val Dark = AgateColors(
            bg = DarkBg, insetBg = DarkBg, cardBg = DarkCard, border = DarkBorder,
            accent = DarkAccent, onAccent = Color(0xFF04111F),
            textMain = DarkTextMain, textSub = DarkTextSub, textDim = DarkTextDim,
            navBg = DarkNavBg, chromeBg = DarkChrome, segBg = DarkSegBg, segActiveBg = DarkSegActive,
            btnBg = DarkBtnBg, switchOff = DarkSwitchOff,
            dotOff = DarkDotOff, dotBorder = DarkDotBorder, pillBg = DarkPillBg,
            green = GreenDark, red = RedDark, amber = AmberDark, blue = BlueDark,
            greenBg = Color(0x2622C55E), redBg = Color(0x26F43F5E),
            amberBg = Color(0x26F59E0B), blueBg = Color(0x263B82F6),
            heatLevels = listOf(
                Color(0x1A94A3B8),
                Color(0x5938BDF8),
                Color(0x9938BDF8),
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
            heatLevels = listOf(
                Color(0x1A64748B),
                Color(0x590284C7),
                Color(0x990284C7),
                LightAccent,
            ),
        )
    }
}
