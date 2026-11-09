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
    fun generatedKeysDoNotRepeat() {
        val keys = (1..64).map { GatewayPrefs.generateApiKey() }.toSet()
        assertEquals("64 次生成必须互不相同", 64, keys.size)
    }
}
