package dev.agate.android.ui.theme

import androidx.compose.ui.graphics.Color
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test

/**
 * 深色档 2026-11-07 重设计为纯黑 OLED 后，最容易出的两种回归：
 * ① 有人把纯黑底「顺手调回」slate 蓝灰；② 改 badge / Hero 这类深浅共用
 * 常量时把浅色档一起带跑。这里把两条边界钉成可断言的事实。
 */
class ThemePaletteTest {

    @Test
    fun `深色档是纯黑底 卡片靠微亮灰分层`() {
        val dark = AgateColors.Dark
        assertEquals(Color(0xFF000000), dark.bg)
        assertEquals(Color(0xFF0A0A0C), dark.chromeBg)
        assertEquals(Color(0xFF101014), dark.cardBg)
        assertNotEquals("纯黑底上卡片必须能分层", dark.bg, dark.cardBg)
        assertEquals(Color(0xFF4CC9F0), dark.accent)
    }

    @Test
    fun `浅色档逐值保持 v44 不受深色重设计影响`() {
        val light = AgateColors.Light
        assertEquals(Color(0xFFF1F5F9), light.bg)
        assertEquals(Color(0xFFFFFFFF), light.cardBg)
        assertEquals(Color(0xFF0284C7), light.accent)
        assertEquals(Color(0xFF4ADE80), light.badgeGreenText)
    }

    @Test
    fun `badge 亮色文字深浅两套独立`() {
        assertEquals(Color(0xFF6EE7A8), AgateColors.Dark.badgeGreenText)
        assertEquals(Color(0xFF4ADE80), AgateColors.Light.badgeGreenText)
        assertNotEquals(AgateColors.Dark.badgeGreenText, AgateColors.Light.badgeGreenText)
    }

    @Test
    fun `Hero 卡配色收进主题且深浅各自一份`() {
        // 深色档不许再出现整块 slate 蓝渐变（旧值 #12324D / #0C4A6E）。
        assertEquals(Color(0xFF08080A), AgateColors.Dark.hero.bg1)
        assertEquals(Color(0xFF4CC9F0), AgateColors.Dark.hero.sub)
        assertEquals(Color(0xFFE0F2FE), AgateColors.Light.hero.bg1)
        assertNotEquals(AgateColors.Dark.hero.bg1, AgateColors.Light.hero.bg1)
    }
}
