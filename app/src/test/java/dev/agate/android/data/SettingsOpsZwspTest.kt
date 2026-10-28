package dev.agate.android.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * 零宽空格词表的本地校验（`SettingsOps.validateZwspWord`）。
 *
 * ── 为什么钉这几条 ──────────────────────────────────────────
 * 内核那边有一份同名判据（`api::zwsp::validated_words`），**内核是最终裁决**。
 * 这里的判据只为两件事存在：
 *   1. 用户点保存之前就能看到原因，不用等一次 400；
 *   2. 界面上「加词」按钮的可用状态需要同一套判据。
 * 所以这里的每条都必须与内核一致 —— 不一致的后果是「界面放行、内核 400」，
 * 那比没有本地校验更糟。
 *
 * 其中**空词条**这条尤其不能松：内核拿它做 `str::find("")`，会在每一个字节
 * 位置命中，等于把整段文本打成筛子。
 */
class SettingsOpsZwspTest {

    @Test
    fun a_plain_word_is_accepted_and_trimmed() {
        assertEquals("敏感词", SettingsOps.sanitizeZwspWord("  敏感词  "))
        assertEquals("敏感词", SettingsOps.sanitizeZwspWord("敏感词"))
    }

    @Test
    fun an_empty_or_blank_word_is_rejected() {
        for (blank in listOf("", " ", "\t", "\n", "   \r\n ")) {
            val result = SettingsOps.validateZwspWord(blank)
            assertTrue("「$blank」应当被拒", result.isFailure)
            assertTrue(
                "文案要说清是空",
                result.exceptionOrNull()?.message?.contains("不能为空") == true,
            )
        }
    }

    @Test
    fun an_already_broken_word_is_rejected() {
        val broken = "敏感" + SettingsOps.ZWSP + "词"
        val result = SettingsOps.validateZwspWord(broken)
        assertTrue("含零宽空格的词条应当被拒", result.isFailure)
        assertTrue(
            "文案要说明填原始词",
            result.exceptionOrNull()?.message?.contains("零宽空格") == true,
        )
    }

    @Test
    fun the_length_cap_matches_the_kernel() {
        // 恰好到上限：收
        val atCap = "字".repeat(SettingsOps.ZWSP_MAX_WORD_CHARS)
        assertEquals(atCap, SettingsOps.sanitizeZwspWord(atCap))

        // 超一个字符：拒
        val overCap = "字".repeat(SettingsOps.ZWSP_MAX_WORD_CHARS + 1)
        assertNull(SettingsOps.sanitizeZwspWord(overCap))
    }

    @Test
    fun the_caps_are_the_kernel_numbers() {
        // 这两个数字与内核 config/types.rs 的常量逐字对应；改一边必须改另一边，
        // 所以在这里钉死，任何一侧的漂移都会先撞在这条断言上。
        assertEquals(200, SettingsOps.ZWSP_MAX_WORDS)
        assertEquals(64, SettingsOps.ZWSP_MAX_WORD_CHARS)
        assertEquals("\u200B", SettingsOps.ZWSP)
    }

    @Test
    fun a_word_of_exactly_the_cap_length_in_characters_not_bytes() {
        // 中文一个字 3 字节：按字节算上限会把 22 个汉字就判超长。
        // 判据必须是**字符数**（内核用 `chars().count()`）。
        val cjk = "敏".repeat(SettingsOps.ZWSP_MAX_WORD_CHARS)
        assertTrue("按字符数算，正好到上限应当收下", cjk.toByteArray().size > SettingsOps.ZWSP_MAX_WORD_CHARS)
        assertEquals(cjk, SettingsOps.sanitizeZwspWord(cjk))
    }
}
