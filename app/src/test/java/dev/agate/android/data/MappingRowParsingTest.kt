package dev.agate.android.data

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * 映射行里两个**可空**字符串键的解析判据（2026-11-12 真机截图）。
 *
 * ── 为什么必须用字面量 `"null"` 当输入 ──────────────────────
 * 内核（`catalog::manage_view`）发的 `provider` / `reasoning` 是 JSON `null`。
 * `org.json` 有两套实现：单测用的 `org.json:json` 在 `optString` 遇到
 * `JSONObject.NULL` 时返回空串，**真机 AOSP libcore** 返回 `"null"` —— 于是
 * 同一段解析在单测里全绿、真机上 7 条映射的「思考等级」全显示 `null`。
 *
 * 所以这里刻意构造**字面量是字符串 `"null"`** 的输入（等价于真机读到的形状），
 * 两套实现下的结果就一致了。顺带钉住「正常值不被吞」与「前后空格」两条，
 * 免得把判据写成「见到 n/u/l 就丢」。
 */
class MappingRowParsingTest {

    private fun row(body: String) = ModelOps.MappingRow.from(JSONObject(body))

    @Test
    fun reasoning_literal_null_is_absent() {
        // 真机读 JSON null 得到的就是这个形状
        assertNull(row("""{"alias":"glm-5.3","target":"sn-glm-5-3","reasoning":"null"}""").reasoning)
    }

    @Test
    fun provider_literal_null_is_absent() {
        // 全局映射（provider 缺省）在真机上会被读成 "null"，行标签就会显示 null
        assertNull(row("""{"alias":"glm-5.3","target":"sn-glm-5-3","provider":"null"}""").provider)
    }

    @Test
    fun real_json_null_is_absent() {
        // 单测库里 NULL 走的是另一条分支，一并覆盖
        assertNull(row("""{"alias":"a","target":"b","reasoning":null,"provider":null}""").reasoning)
        assertNull(row("""{"alias":"a","target":"b","provider":null}""").provider)
    }

    @Test
    fun real_values_survive() {
        val r = row("""{"alias":"glm-5.3","target":"sn-glm-5-3","provider":"raccoon","reasoning":"high"}""")
        assertEquals("raccoon", r.provider)
        assertEquals("high", r.reasoning)
    }

    @Test
    fun surrounding_whitespace_is_trimmed() {
        val r = row("""{"alias":"a","target":"b","provider":" raccoon ","reasoning":" high "}""")
        assertEquals("raccoon", r.provider)
        assertEquals("high", r.reasoning)
    }

    @Test
    fun missing_keys_are_absent() {
        val r = row("""{"alias":"a","target":"b"}""")
        assertNull(r.provider)
        assertNull(r.reasoning)
    }

    // ── `effective`（2026-11-12 #129）────────────────────────────
    // 别名的生效真值由内核请求侧入口算好随行下发，界面只读不算。
    @Test
    fun effective_flag_is_read_from_kernel() {
        assertTrue(row("""{"alias":"a","target":"b","effective":true}""").effective)
        assertFalse(row("""{"alias":"a","target":"b","effective":false}""").effective)
    }

    @Test
    fun missing_effective_flag_degrades_to_inactive() {
        // 内核没算过就不能谎报生效：界面按「未生效」降级
        assertFalse(row("""{"alias":"a","target":"b"}""").effective)
    }
}
