package dev.agate.android.data

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * 模型行上「思考等级」两个键的解析判据（2026-11-12）。
 *
 * ── 为什么是**两个**键 ─────────────────────────────────────
 * 内核 `manage_view` 把模型自身的状态挂在 `models[]` 行上：
 *   · `reasoningBinding`：用户给它绑的档位，**JSON `null` = 没绑**；
 *   · 「这个键在不在」本身是信息：内置家每条上游模型都带（值可能为 null），
 *     自定义家**不带**（它的等级存在自己那条记录里，映射写接口不认
 *     `custom-xxx`）。界面据此决定要不要给入口 —— 所以解析时 `has` 与「值是
 *     不是 null」必须分开存，混成一个可空字段就没法区分「没绑」与「不可改」。
 *
 * ── 为什么用字面量 `"null"` 当输入 ─────────────────────────
 * 与 [MappingRowParsingTest] 同一个坑：真机 AOSP 的 `optString` 会把
 * `JSONObject.NULL` 读成字符串 `"null"`，单测库返回空串。所以两类形状都要覆盖。
 */
class ManageModelRowParsingTest {

    private fun rows(vararg bodies: String): List<ModelOps.ManageModelRow> =
        ModelOps.ManageModelRow.parse(JSONObject("""{"models":[${bodies.joinToString(",")}]}"""))

    private fun row(body: String) = rows(body).single()

    @Test
    fun bound_level_survives() {
        val r = row("""{"id":"glm-5.3","provider":"zcode","reasoningBinding":"high"}""")
        assertEquals("high", r.reasoningBinding)
        assertTrue(r.reasoningEditable)
    }

    @Test
    fun unbound_level_is_null_but_still_editable() {
        // 内置家「没绑」的形状：键在、值是 JSON null
        val r = row("""{"id":"glm-5.3","provider":"zcode","reasoningBinding":null}""")
        assertNull(r.reasoningBinding)
        assertTrue("键在 = 有入口，不能因为值是 null 就收起入口", r.reasoningEditable)
    }

    @Test
    fun literal_null_string_is_absent() {
        // 真机读 JSON null 得到的就是这个形状（见类注释）
        val r = row("""{"id":"glm-5.3","provider":"zcode","reasoningBinding":"null"}""")
        assertNull(r.reasoningBinding)
        assertTrue(r.reasoningEditable)
    }

    @Test
    fun missing_key_means_not_editable() {
        // 自定义家的形状：内核不发这个键（等级存在它自己那条记录里）
        val r = row("""{"id":"my-model","provider":"custom-abc"}""")
        assertNull(r.reasoningBinding)
        assertFalse(r.reasoningEditable)
    }

    @Test
    fun blank_level_is_treated_as_absent() {
        val r = row("""{"id":"glm-5.3","provider":"zcode","reasoningBinding":"  "}""")
        assertNull(r.reasoningBinding)
        assertTrue(r.reasoningEditable)
    }

    @Test
    fun rows_without_id_are_skipped() {
        assertEquals(1, rows("""{"provider":"zcode","reasoningBinding":"high"}""", """{"id":"glm-5.3"}""").size)
    }
}
