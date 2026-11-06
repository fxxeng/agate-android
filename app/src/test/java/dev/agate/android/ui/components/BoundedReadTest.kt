package dev.agate.android.ui.components

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import java.io.ByteArrayInputStream

/**
 * 取图字节上限的单测。
 *
 * ── 为什么要单独测这个 ──────────────────────────────────────────
 * `boundedRead` 守的是「一次 OOM」：参考图来自相册，用户选一张 50 MP 的图，
 * `decodeSampled` 里那句 `inJustDecodeBounds` 只省**解码**内存，省不了
 * **读字节**的内存。上限差一位的后果是进程被系统杀掉，而不是一句报错。
 *
 * 三条边界都要钉住：正常小图原样返回、刚好等于上限放行、超过上限返回 null。
 */
class BoundedReadTest {

    private fun read(bytes: ByteArray): ByteArray? =
        AgateImageLoader.boundedRead(ByteArrayInputStream(bytes))

    @Test
    fun small_payload_is_returned_verbatim() {
        val payload = ByteArray(1024) { (it % 251).toByte() }
        assertArrayEquals(payload, read(payload))
    }

    @Test
    fun payload_exactly_at_the_limit_is_accepted() {
        val payload = ByteArray(AgateImageLoader.MAX_IMAGE_BYTES)
        assertArrayEquals(payload, read(payload))
    }

    @Test
    fun payload_one_byte_over_the_limit_is_refused() {
        val payload = ByteArray(AgateImageLoader.MAX_IMAGE_BYTES + 1)
        assertNull(read(payload))
    }

    @Test
    fun empty_stream_yields_empty_array() {
        assertEquals(0, read(ByteArray(0))?.size)
    }

    private fun assertArrayEquals(expected: ByteArray, actual: ByteArray?) {
        // 自己比而不是用 JUnit 的 assertArrayEquals：后者对 null 的报错信息
        // 只说「expected not null」，看不出是长度问题还是内容问题。
        if (actual == null) {
            throw AssertionError("应当返回字节，实际是 null（长度 ${expected.size}）")
        }
        assertEquals("长度不一致", expected.size, actual.size)
        for (index in expected.indices) {
            if (expected[index] != actual[index]) {
                throw AssertionError("第 $index 字节不一致")
            }
        }
    }
}