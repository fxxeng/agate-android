package dev.agate.android.data

import dev.agate.android.data.dto.AccountDto
import dev.agate.android.ui.routing.buildReorderIds
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * 控制台快照里两段「新接的真读数」的解析判据。
 *
 * ── 为什么这两段值得钉住（2026-11-06 真机截图核对）─────────────
 * 控制台原来有三句占位文案，其中「队列数据待 /api/routing/queue 结构核对」
 * 不是功能没做，而是**解析找错了键**：客户端按 `queue / entries / rows /
 * items / candidates` 猜，内核返回的是 `ordered / blocked`，于是队列恒空。
 * 这类 bug 没有编译期信号、界面还自带一句看起来很合理的解释，只有拿内核
 * 真实响应形状做输入才能挡住。
 *
 * 「各家占比」那段同理：`providers[].totalTokens` 一直在报表里用着，控制台
 * 却写着「待 stats 端点字段接入」。这里钉住冠军是按 token 挑、不是按请求数挑。
 */
class ConsoleSnapshotParsingTest {

    private fun queue(body: String) = GatewayRepository.parseRoutingQueue(JSONObject(body))

    private fun entry(
        id: String,
        name: String,
        provider: String,
        label: String,
        state: String,
        reason: String,
    ): String = """{"id":"$id","name":"$name","provider":"$provider",
        |"providerLabel":"$label","state":"$state","reason":"$reason",
        |"resetAt":null,"priority":1}""".trimMargin()

    @Test
    fun `内核真实的 ordered 与 blocked 都被解析`() {
        val ordered = entry("a1", "主号", "zcode", "ZCode", "usable", "优先级最高")
        val blocked = entry("a2", "备用号", "loomy", "Loomy", "cooldown", "上游 429，冷却至 21:03")
        val parsed = queue(
            """{"strategy":"balance","model":null,"lastRequestModel":"glm-4.6",
               |"pickedId":"a1","ordered":[$ordered],"blocked":[$blocked]}""".trimMargin(),
        )
        assertNotNull("旧实现的五个候选键一个都不命中，这里必须解析出来", parsed)
        val q = parsed!!
        assertEquals("glm-4.6", q.model)
        assertEquals(2, q.rows.size)

        val first = q.rows[0]
        assertEquals("主号", first.account)
        assertEquals("ZCode", first.provider)
        assertEquals("glm-4.6", first.model)
        assertEquals("可出战", first.note)
        assertEquals("优先级最高", first.detail)
        assertFalse(first.blocked)

        val second = q.rows[1]
        assertEquals("备用号", second.account)
        assertEquals("冷却中", second.note)
        assertTrue(second.detail!!.contains("429"))
        assertTrue("blocked 数组里的行必须标成被排除", second.blocked)
    }

    @Test
    fun `旧的猜测键不再被静默当成空队列`() {
        // 内核从来没返回过 "queue"。真拿到这种形状时宁可返回 null，
        // 让界面如实说「暂不可用」，也不要显示「当前无候选账号」骗人。
        assertNull(queue("""{"queue":[{"id":"a1","name":"主号"}]}"""))
        assertNull(queue("{}"))
        assertNull(GatewayRepository.parseRoutingQueue(null))
    }

    @Test
    fun `两个空数组是合法的空队列而不是解析失败`() {
        val parsed = queue("""{"ordered":[],"blocked":[]}""")
        assertNotNull(parsed)
        assertTrue(parsed!!.rows.isEmpty())
        assertNull(parsed.model)
    }

    @Test
    fun `显式 model 优先于最近一次请求模型`() {
        val e = entry("a1", "主号", "zcode", "ZCode", "usable", "队首")
        val parsed = queue(
            """{"model":"kimi-k2","lastRequestModel":"glm-4.6",
               |"ordered":[$e],"blocked":[]}""".trimMargin(),
        )
        assertEquals("kimi-k2", parsed!!.model)
        assertEquals("kimi-k2", parsed.rows[0].model)
    }

    @Test
    fun `未知状态回显原值，空状态按是否排除兜底`() {
        val unknown = entry("a1", "甲", "zcode", "ZCode", "maintenance", "维护中")
        val blank = entry("a2", "乙", "zcode", "ZCode", "", "被门禁挡下")
        val parsed = queue("""{"ordered":[$unknown],"blocked":[$blank]}""")!!
        assertEquals("maintenance", parsed.rows[0].note)
        assertEquals("已排除", parsed.rows[1].note)
    }

    @Test
    fun `字面量 null 不得当成模型名 防住真机自锁`() {
        // 真机 AOSP libcore 的 optString 会把 JSON null 读成字符串 "null"，
        // 客户端又把它回传给 ?model=null，端点于是**回显字符串** "null" ——
        // 页面永远显示「模型 null」。单测用的是 org.json:json（返回 default），
        // 所以只有拿这个「已经被污染」的形状做输入才能钉住修复。
        val e = entry("a1", "主号", "zcode", "ZCode", "usable", "队首")
        val body = """{"model":"null","lastRequestModel":"null",
                      |"pickedId":"a1","ordered":[$e],"blocked":[]}""".trimMargin()
        val parsed = queue(body)
        assertNull("字面量 null 不能被显示成模型名", parsed!!.model)
        assertNull(parsed.rows[0].model)
        val plan = RoutingOps.parsePlan(JSONObject(body))
        assertNull(plan.model)
        assertNull(plan.lastRequestModel)
        assertEquals(1, plan.ordered.size)
    }

    @Test
    fun `JSON null 与最近模型同时为空时 两边都给 null`() {
        val e = entry("a1", "主号", "zcode", "ZCode", "usable", "队首")
        val body = """{"model":null,"lastRequestModel":null,"ordered":[$e],"blocked":[]}"""
        assertNull(queue(body)!!.model)
        val plan = RoutingOps.parsePlan(JSONObject(body))
        assertNull(plan.model)
        assertNull(plan.lastRequestModel)
        assertNull("非字符串类型也不该被拼进界面", jsonTextOrNull(JSONObject("""{"model":42}"""), "model"))
    }

    @Test
    fun `各家占比冠军按 totalTokens 挑，不按请求数挑`() {
        val summary = JSONObject(
            """{"providers":[
               |{"id":"a","label":"A家","requests":900,"totalTokens":1200},
               |{"id":"b","label":"B家","requests":12,"totalTokens":98000},
               |{"id":"c","label":"C家","requests":5,"totalTokens":300}
               |]}""".trimMargin(),
        )
        val top = GatewayRepository.parseTopProvider(summary)
        assertNotNull(top)
        assertEquals("B家", top!!.first)
        assertEquals(98_000L, top.second)
    }

    @Test
    fun `零用量不参与冠军，数字串也要认，空数组返回 null`() {
        val summary = JSONObject(
            """{"providers":[
               |{"id":"a","label":"A家","requests":900,"totalTokens":0},
               |{"id":"b","label":"B家","requests":9,"totalTokens":9},
               |{"id":"c","label":"C家","requests":0,"totalTokens":"42"}
               |]}""".trimMargin(),
        )
        val top = GatewayRepository.parseTopProvider(summary)
        assertNotNull(top)
        assertEquals("C家", top!!.first)
        assertEquals("数字串也要认", 42L, top.second)
        assertNull(GatewayRepository.parseTopProvider(JSONObject("""{"providers":[]}""")))
        assertNull(GatewayRepository.parseTopProvider(null))
    }

    @Test
    fun `label 缺失时回退到 id 而不是丢掉这家`() {
        // 内核对空 provider 会给「未知提供商」这类 label，正常不会走到这里；
        // 但手改 / 旧内核少给 label 时，宁可显示 id 也别凭空少一家。
        val summary = JSONObject("""{"providers":[{"id":"zcode","totalTokens":7}]}""")
        val top = GatewayRepository.parseTopProvider(summary)
        assertNotNull(top)
        assertEquals("zcode", top!!.first)
        assertEquals(7L, top.second)
    }

    // ── 候选范围与排序提交（2026-11-07「两套队列 / 429 不换号」）──────

    private fun account(id: String, priority: Int) = AccountDto(
        id = id,
        provider = "zcode",
        displayName = id,
        enabled = true,
        hasCredentials = true,
        chatSupported = true,
        priority = priority,
        edition = null,
        lowBalance = false,
        raw = JSONObject(),
    )

    @Test
    fun `范围字段按内核真值解析 客户端不拿全量账号自己减`() {
        // 真机形状：模型只被一家认，7 个账号里只有 2 个进计划、1 个可用，
        // 429 后候补 0 位。这五个数字必须来自 plan_route，不能由界面猜。
        val body = """{"strategy":"order","sticky":"auto","model":"deepseek-v4.1-flash",
            |"pickedId":"a1","ordered":[{"id":"a1","name":"主号","state":"usable"}],
            |"blocked":[{"id":"a2","name":"备用","state":"cooldown","resetAt":1893456000000}],
            |"modelScoped":true,"scopedAccounts":2,"totalAccounts":7,"outsideAccounts":5,
            |"usableCount":1,"fallbackCount":0,"blockedCount":1,"cooldownCount":1,
            |"earliestResetAt":1893456000000}""".trimMargin()
        val p = RoutingOps.parsePlan(JSONObject(body))
        assertTrue(p.scopeKnown)
        assertTrue(p.modelScoped)
        assertEquals(2, p.scopedAccounts)
        assertEquals(7, p.totalAccounts)
        assertEquals(5, p.outsideAccounts)
        assertEquals(1, p.usableCount)
        assertEquals("429 后没有候补", 0, p.fallbackCount)
        assertEquals(1, p.cooldownCount)
        assertEquals(1893456000000L, p.earliestResetAt)
    }

    @Test
    fun `旧内核没给范围字段时标成未知 不把 0 当算过的数`() {
        val p = RoutingOps.parsePlan(JSONObject("""{"ordered":[],"blocked":[]}"""))
        assertFalse("界面看到这个必须写「暂不可用」，不能写「0 个账号」", p.scopeKnown)
        assertFalse(p.modelScoped)
        assertEquals(0, p.usableCount)
    }

    @Test
    fun `拖出的可用候选置前 其余账号按原优先级补齐成全量排列`() {
        // ordered 拖成 a3 → a1；blocked 是 a2；b1 不承载该模型、在计划外。
        // 提交必须是「全部 4 个账号的一个排列」，否则内核 400 拒。
        val all = listOf(account("a1", 0), account("a2", 1), account("a3", 2), account("b1", 3))
        val full = buildReorderIds(
            ordered = listOf("a3", "a1"),
            blocked = listOf("a2"),
            allAccounts = all,
        )
        assertEquals(listOf("a3", "a1", "a2", "b1"), full)
    }

    @Test
    fun `清单未到或计划与清单对不上时拒绝提交`() {
        val all = listOf(account("a1", 0), account("a2", 1))
        assertNull(buildReorderIds(listOf("a1"), emptyList(), emptyList()))
        assertNull("计划里出现清单没有的账号，不能硬凑", buildReorderIds(listOf("a1", "ghost"), emptyList(), all))
        assertNull("重复 id 不是排列", buildReorderIds(listOf("a1", "a1"), emptyList(), all))
    }
}
