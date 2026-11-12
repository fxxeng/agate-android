package dev.agate.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * R50/T3：LAN 管理 Key 的格式与随机性锁。
 *
 * 旧实现 `(1..32).map { alphabet.random() }` 用的是 ThreadLocalRandom，不是
 * CSPRNG；这里锁住「24 字节 = 48 位 hex」与多次生成不重复，防止退回弱随机。
 */
class GatewayPrefsApiKeyTest {

    @Test
    fun generatedKeyIsPrefixedHexWithCsprngLength() {
        val key = GatewayPrefs.generateApiKey()
        assertTrue("必须以 agate- 开头: $key", key.startsWith("agate-"))
        val body = key.removePrefix("agate-")
        assertEquals("24 字节应编码为 48 位 hex", 48, body.length)
        assertTrue("只允许小写 hex: $body", body.all { it in "0123456789abcdef" })
    }

    @Test
    fun r49LegacyKeyIsRotatedExactlyOnce() {
        val legacy = "agate-" + "a".repeat(32)
        val replacement = "agate-" + "b".repeat(48)
        var generated = 0
        val resolved = GatewayPrefs.resolveApiKey(legacy, 0) {
            generated += 1
            replacement
        }
        assertEquals(replacement, resolved.key)
        assertTrue(resolved.rotated)
        assertEquals(1, generated)
        val next = GatewayPrefs.resolveApiKey(resolved.key, 2) {
            error("gen2 key 不得再次轮换")
        }
        assertEquals(replacement, next.key)
        assertTrue(!next.rotated)
    }

    @Test
    fun r50CsprngKeyIsRetainedAndMarkedCurrent() {
        val current = "agate-" + "0123456789abcdef".repeat(3)
        val resolved = GatewayPrefs.resolveApiKey(current, 0) {
            error("R50 强随机 Key 不得轮换")
        }
        assertEquals(current, resolved.key)
        assertTrue(!resolved.rotated)
    }

    @Test
    fun malformedOrMissingKeyIsRebuilt() {
        for (stored in listOf(null, "", "agate-123", "agate-" + "z".repeat(32))) {
            val resolved = GatewayPrefs.resolveApiKey(stored, 2) { "agate-" + "c".repeat(48) }
            assertTrue("畸形/缺失 Key 不得沿用: $stored", resolved.rotated)
            assertEquals(54, resolved.key.length)
        }
    }

    @Test
    fun generatedKeysDoNotRepeat() {
        val keys = (1..64).map { GatewayPrefs.generateApiKey() }.toSet()
        assertEquals("64 次生成必须互不相同", 64, keys.size)
    }
}
