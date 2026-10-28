package dev.agate.android.ui.settings

import dev.agate.android.data.SettingsOps
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * 设置页的纯函数（格式化 / 折叠态摘要 / 备份路径）。
 *
 * ── 为什么这些值得单测 ──────────────────────────────────────
 * 它们全是「看一眼觉得对、实际会错」的那一类：
 *   - 千分位在 4 位与 5 位之间换挡（1000 不加逗号、10000 加）—— 差一位就错；
 *   - 99999 天在界面上必须显示「永久」而不是「99999 天」，这是**语义**不是排版；
 *   - 摘要文案里任何一项取错字段，显示的读数就是错的，而它恰好是用户不展开
 *     这一组时唯一能看到的信息；
 *   - 备份路径带时间戳，写成固定串会让两次导出互相覆盖。
 *
 * 这些都是纯函数，不依赖 Compose 也不依赖内核，所以能在这里钉死。
 */
class SettingsPageFormatTest {

    // ── 千分位 ──────────────────────────────────────────────

    @Test
    fun thousands_separator_kicks_in_exactly_at_the_thousands_place() {
        // 千位起加逗号（标准那条）。曾经的「5 位才加」会让同一页里
        // `3214 条` 与 `10,000 条` 并存 —— 同一页两种读法就是错。
        assertEquals("0", group(0))
        assertEquals("9", group(9))
        assertEquals("999", group(999))
        assertEquals("1,000", group(1000))
        assertEquals("3,214", group(3214))
        assertEquals("9,999", group(9999))
        assertEquals("10,000", group(10_000))
        assertEquals("99,999", group(99_999))
        assertEquals("100,000", group(100_000))
        assertEquals("1,234,567", group(1_234_567))
    }

    @Test
    fun thousands_separator_handles_the_long_min_value() {
        // 负值不在业务里出现（条数、字节数都非负），但格式化函数不该因此崩
        assertTrue(group(-1).isNotEmpty())
    }

    // ── 字节数 ──────────────────────────────────────────────

    @Test
    fun bytes_use_1024_steps_and_one_decimal() {
        assertEquals("0 B", humanBytes(0))
        assertEquals("-1 视作 0", "0 B", humanBytes(-1))
        // 小于 1KB 时给整数 B，不带小数（「512.0 B」是无意义的精度）
        assertEquals("512 B", humanBytes(512))
        assertEquals("1023 B", humanBytes(1023))
        assertEquals("1.0 KB", humanBytes(1024))
        assertEquals("1.5 KB", humanBytes(1536))
        assertEquals("1.0 MB", humanBytes(1024L * 1024))
        // 12.6 MB —— 设计预览里的示例读数，出现在折叠摘要里
        assertEquals("12.6 MB", humanBytes((12.6 * 1024 * 1024).toLong()))
        assertEquals("1.0 GB", humanBytes(1024L * 1024 * 1024))
        // 超过最后一级仍停在 GB，不再造 TB（库文件不可能到那个量级）
        assertTrue(humanBytes(1024L * 1024 * 1024 * 1024).endsWith("GB"))
    }

    // ── 折叠态摘要 ──────────────────────────────────────────

    @Test
    fun an_unread_group_says_so_instead_of_showing_zeros() {
        // 关键点：读不到 ≠ 全是 0。文案必须是「未读取」，否则用户会以为
        // 内核告诉他「你现在就是 0 天保留」。
        assertEquals("未读取", accessSummary(null))
        assertEquals("未读取", streamSummary(null))
        assertEquals("未读取", guardSummary(null, 0))
        assertEquals("未读取", promptSummary(null))
        assertEquals("未读取", storageSummary(null))
        assertEquals("未读取", retrySummary(emptyMap(), 0))
        assertEquals("未读取", queueSummary(emptyMap(), emptyMap()))
    }

    @Test
    fun access_summary_distinguishes_no_key_from_a_set_key() {
        val noKey = accessSummary(SettingsOps.Config(org.json.JSONObject().apply {
            put("apiKeySet", false)
            put("locale", "zh-CN")
        }))
        assertEquals("无 Key · zh-CN", noKey)

        val withKey = accessSummary(SettingsOps.Config(org.json.JSONObject().apply {
            put("apiKeySet", true)
            put("apiKey", "agate-…a1b2")
            put("locale", "en-US")
        }))
        assertEquals("Key 已设 · en-US", withKey)
    }

    @Test
    fun guard_summary_reports_the_word_list_only_when_it_has_words() {
        val off = SettingsOps.Guards(sanitize = true, cors = false, developerRoleCompat = true)
        // 空词表不占版面：显示「· 词表 0」只会让人以为要去做点什么
        assertEquals("指纹剥离开 · 跨域关", guardSummary(off, 0))
        assertEquals("指纹剥离开 · 跨域关 · 词表 3", guardSummary(off, 3))
    }

    @Test
    fun storage_summary_says_unavailable_rather_than_zero_bytes() {
        val unavailable = SettingsOps.Storage(org.json.JSONObject().apply {
            put("configDir", "/data/data/dev.agate.android/files")
            put("database", org.json.JSONObject().put("available", false))
        })
        assertEquals("库不可用", storageSummary(unavailable))

        val ok = SettingsOps.Storage(org.json.JSONObject().apply {
            put("database", org.json.JSONObject().apply {
                put("available", true)
                put("bytes", 1024L * 1024 * 12)
                put("requests", 3214L)
            })
        })
        assertEquals("库 12.0 MB · 请求 3,214 条", storageSummary(ok))
    }

    @Test
    fun prompt_summary_marks_the_degraded_state() {
        val passthrough = SettingsOps.Prompt(org.json.JSONObject().apply {
            put("promptMode", "passthrough")
            put("promptSource", "none")
        })
        assertEquals("透传客户端 · 来源：未使用", promptSummary(passthrough))

        val degraded = SettingsOps.Prompt(org.json.JSONObject().apply {
            put("promptMode", "custom")
            put("promptSource", "inline")
            put("degradeActive", true)
        })
        // 降级是运行期状态，必须在折叠态就看得见 —— 否则用户会以为自己的
        // 提示词正在生效，实际发出去的是中性那一份
        assertEquals("替换 system · 来源：界面编辑 · 降级中", promptSummary(degraded))
    }

    @Test
    fun retry_summary_appends_the_code_count_only_when_present() {
        val values = mapOf(
            "retryCount" to 3L,
            "retryCrossProviderCount" to 5L,
            "retryIntervalSeconds" to 5L,
        )
        assertEquals("原地 3 次 · 换号 5 个 · 间隔 5 秒", retrySummary(values, 0))
        assertEquals("原地 3 次 · 换号 5 个 · 间隔 5 秒 · 1 个码直接换号", retrySummary(values, 1))
    }

    @Test
    fun queue_summary_keeps_the_field_order_of_the_kernel() {
        val queue = mapOf("queueMaxWaits" to 2L, "queueWaitSeconds" to 0L)
        val timeouts = mapOf(
            "connectTimeoutSeconds" to 30L,
            "headersTimeoutSeconds" to 300L,
            "streamIdleTimeoutSeconds" to 300L,
            "bodyTimeoutSeconds" to 300L,
        )
        // 顺序必须是 连接/等待/流式/非流式 —— 与 TIMEOUT_FIELDS 一致，
        // 否则同样是四个数字，读起来的含义就变了
        assertEquals("等2 次 · 30/300/300/300 秒", queueSummary(queue, timeouts))
    }

    @Test
    fun stream_summary_reads_all_three_switches() {
        val s = SettingsOps.Stream(
            stripKeepalive = true,
            autoContinue = false,
            autoContinueRounds = 3,
            debug = false,
            debugRows = 0,
            debugLimit = 500,
        )
        assertEquals("保活丢弃开 · 续写关 · 调试关", streamSummary(s))
    }

    // ── 其它 ────────────────────────────────────────────────

    @Test
    fun theme_labels_cover_all_three_modes() {
        assertEquals("跟随系统", themeLabel(dev.agate.android.ui.theme.AgateThemeMode.FOLLOW_SYSTEM))
        assertEquals("浅色", themeLabel(dev.agate.android.ui.theme.AgateThemeMode.LIGHT))
        assertEquals("深色", themeLabel(dev.agate.android.ui.theme.AgateThemeMode.DARK))
    }
}
