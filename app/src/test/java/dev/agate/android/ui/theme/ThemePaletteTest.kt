package dev.agate.android.ui.theme

import androidx.compose.ui.graphics.Color
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test

/**
 * 主题调色板回归钉子。三档切换依赖 Dark / Light 两套色集合各自稳定：
 * 改动深浅共用常量时最容易把另一档一起带跑；遮罩与描边散落写字面量时，
 * 「某档下弹层边界看不见 / 遮罩压不住」也只会在某一条路径上暴露。
 * 这里把两套底色、卡片、强调色、描边、遮罩钉成可断言的事实。
 */
class ThemePaletteTest {

    @Test
    fun `深色档卡片靠描边分层 遮罩压得住内容`() {
        val dark = AgateColors.Dark
        assertEquals(Color(0xFF0F172A), dark.bg)
        assertEquals(Color(0xFF1D283A), dark.chromeBg)
        assertEquals(Color(0xFF1E293B), dark.cardBg)
        assertNotEquals("卡片必须能和底色分层", dark.bg, dark.cardBg)
        assertEquals(Color(0xFF38BDF8), dark.accent)
        assertEquals("深色档描边要可辨", Color(0xFF334155), dark.border)
        assertEquals("深色档遮罩要压得住内容", Color(0x99000000), dark.scrim)
    }

    @Test
    fun `浅色档逐值保持稳定 不受深色改动影响`() {
        val light = AgateColors.Light
        assertEquals(Color(0xFFF1F5F9), light.bg)
        assertEquals(Color(0xFFFEFEFE), light.chromeBg)
        assertEquals(Color(0xFFFFFFFF), light.cardBg)
        assertEquals(Color(0xFF0284C7), light.accent)
        assertEquals(Color(0xFF4ADE80), light.badgeGreenText)
        assertEquals("浅色档描边要可辨", Color(0xFFE2E8F0), light.border)
        assertEquals("浅色档遮罩要压得住内容", Color(0xB3000000), light.scrim)
    }

    @Test
    fun `badge 亮色文字 v44 深浅共用`() {
        assertEquals(Color(0xFF4ADE80), AgateColors.Dark.badgeGreenText)
        assertEquals(Color(0xFF4ADE80), AgateColors.Light.badgeGreenText)
    }
}
