package dev.agate.core

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * R46/T3：Native 回复两层兜底链（Rust `json_reply` 最小信封 + Kotlin `envelope`）
 * 的 Kotlin 侧契约锁。null / 空串 / 非法 JSON 一律收敛成
 * `{ok:false, stage:"jni", error:"malformed native reply"}`，绝不以 NPE 上抛——
 * M4「不把 null 当通用错误吞掉」的收口断言。
 *
 * `Gateway.envelope` 为 internal：仅供本模块测试直呼，公开面仍走各 parse。
 */
class GatewayEnvelopeTest {

    private fun assertMalformed(envelope: JSONObject) {
        assertEquals(false, envelope.optBoolean("ok"))
        assertEquals("jni", envelope.optString("stage"))
        assertEquals("malformed native reply", envelope.optString("error"))
    }

    @Test
    fun envelope_converges_null_blank_and_garbage_to_structured_failure() {
        assertMalformed(Gateway.envelope(null))
        assertMalformed(Gateway.envelope(""))
        assertMalformed(Gateway.envelope("   "))
        assertMalformed(Gateway.envelope("not-json"))
        // 合法 JSON 但不是对象：同样不能当信封用
        assertMalformed(Gateway.envelope("[1,2,3]"))
    }

    @Test
    fun envelope_passes_valid_object_through() {
        val envelope = Gateway.envelope("""{"ok":true,"port":8787,"bind_lan":false}""")
        assertTrue(envelope.optBoolean("ok"))
        assertEquals(8787, envelope.optInt("port"))
    }

    @Test
    fun start_result_parse_survives_null_reply() {
        val result = Gateway.StartResult.parse(null)
        assertEquals(false, result.ok)
        assertEquals("jni", result.stage)
    }

    @Test
    fun status_parse_keeps_error_envelope_semantics() {
        // ok=false 信封仍按 Status.parse 既有语义抛出，且 message 是信封里的
        // error——比 NPE 可诊断（check 抛 IllegalStateException）。
        val boom = try {
            Gateway.Status.parse("{\"ok\":false,\"error\":\"boom\"}")
            fail("ok=false 信封应当抛出")
            throw AssertionError("unreachable")
        } catch (expected: IllegalStateException) {
            expected
        }
        assertEquals("boom", boom.message)

        val malformed = try {
            Gateway.Status.parse(null)
            fail("null 回复应当收敛成 malformed 信封再抛出")
            throw AssertionError("unreachable")
        } catch (expected: IllegalStateException) {
            expected
        }
        assertEquals("malformed native reply", malformed.message)
    }

    @Test
    fun status_parse_carries_bridge_contract_version() {
        // 新内核：bridge=1 显式携带（R46/T2 起契约握手）
        val modern = Gateway.Status.parse(
            """{"ok":true,"running":false,"core":true,"scheduler":true,"bridge":1,"version":"3.0.8"}""",
        )
        assertEquals(1, modern.bridge)

        // 旧内核：无 bridge 字段 → 0（R44 kind 字段的 null 兼容同款先例）
        val legacy = Gateway.Status.parse(
            """{"ok":true,"running":false,"core":true,"scheduler":true,"version":"3.0.7"}""",
        )
        assertEquals(0, legacy.bridge)
    }
}
