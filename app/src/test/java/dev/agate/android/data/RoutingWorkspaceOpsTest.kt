package dev.agate.android.data

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * [RoutingWorkspaceOps] 的解析测试。
 *
 * 这一段代码里**没有一个字节是逻辑**，全是「把内核的 JSON 搬进 Kotlin 字段」——
 * 它值得单测的理由正是这个：搬错的后果是界面显示 0 / 空 / 空白，而不是抛异常。
 * 没人会在真机上发现「粘滞计数恒为 0 是因为键名写成了 sticky_count」。
 *
 * 所以每个用例都拿**内核真会吐出的形态**当输入（字段名逐字抄自
 * `routing_workspace.rs` / `routing_groups.rs` 的 `json!` 字面量），
 * 包括那几处看起来像笔误的地方：
 * - 工作台队列的 `resetAt` 在无冷却时是 **null**，预演里却是个数字；
 * - 工作台的行**不发** `providerLabel`，预演的行发；
 * - 预演队列为空时 `pickedId` 是 **null**，工作台是**空串**。
 *
 * 这些分歧如果只按「我记得是这样」写解析，出事时最难查 —— 解析不报错，
 * 只是值不对。用例把它们钉住，任何一边改了形状，测试先红。
 *
 * 全部纯 [JSONObject] 构造 + 断言：不碰 Android 框架，也不 mock 链路
 * （`workspace()` / `saveProfile()` 那层只做信封解包，解析逻辑全在数据类的
 * 构造函数里，这正是把解析放进构造函数的原因）。
 */
class RoutingWorkspaceOpsTest {

    private fun json(raw: String): JSONObject = JSONObject(raw.trimIndent())

    // ── 工作台 ──────────────────────────────────────────────────

    @Test
    fun `工作台快照按内核形状解出全部渲染字段`() {
        val ws = RoutingWorkspaceOps.Workspace(workspacePayload())

        assertEquals("gpt-4o", ws.model)
        assertTrue(ws.customized)
        assertTrue(ws.globalCustomized)
        // 内核已按小写排序去重，解析不该再动顺序
        assertEquals(listOf("claude-3-7", "gpt-4o"), ws.models)

        assertEquals(3, ws.providers.size)
        val openai = ws.providers[1]
        assertEquals("openai", openai.id)
        assertEquals("OpenAI", openai.label)
        assertEquals(2L, openai.accountCount)
        assertFalse(openai.custom)
        assertTrue(openai.available)
        // 档案里点了但当前不可用的家必须保留（界面标灰，不能过滤掉）
        assertFalse(ws.providers[2].available)
        assertTrue(ws.providers[2].custom)

        // 复杂对象原样保留：界面改一个键再整块提交
        assertEquals("rotate", ws.profile.optString("strategy"))
        assertEquals(2, ws.profile.optJSONArray("providers")!!.length)
        assertEquals("all", ws.inheritedProfile.optString("providerScope"))

        // 64 位十六进制是内核 revision 校验的硬条件，短一位就是 400
        assertEquals(64, ws.revision.length)

        assertEquals("order", ws.settings.optString("routingStrategy"))
        assertEquals("rotate", ws.globalSettings.optString("routingStrategy"))

        assertEquals("acc_1", ws.queue.pickedId)
        assertEquals(2, ws.queue.ordered.size)
        assertEquals(1, ws.queue.blocked.size)
        assertTrue(ws.queue.ordered[0].usable)
        assertFalse(ws.queue.isEmpty)
        assertEquals(1763000000000L, ws.queue.blocked[0].resetAt)

        assertEquals(3L, ws.stickyCount)
        assertEquals(
            0L,
            ws.accountStates.optJSONObject("acc_1")?.optLong("resetAt") ?: -1L,
        )
        assertEquals(1763200000000L, ws.updatedAt)
    }

    @Test
    fun `工作台不带 model 时折成 null 而不是空串`() {
        // 内核在 ?model= 缺省时发的是 JSON null
        assertNull(RoutingWorkspaceOps.Workspace(json("""{"model": null}""")).model)
        // 空串同样折成 null：界面判「全局视角」只该有一个真值
        assertNull(RoutingWorkspaceOps.Workspace(json("""{"model": ""}""")).model)
        assertEquals(
            "gpt-4o",
            RoutingWorkspaceOps.Workspace(json("""{"model": "gpt-4o"}""")).model,
        )
    }

    @Test
    fun `工作台缺键时全部走兜底值且不抛`() {
        val ws = RoutingWorkspaceOps.Workspace(JSONObject("{}"))

        assertNull(ws.model)
        assertFalse(ws.customized)
        assertFalse(ws.globalCustomized)
        assertTrue(ws.models.isEmpty())
        assertTrue(ws.providers.isEmpty())
        // 缺 profile / settings 时给空 JSONObject 而不是 null：
        // 界面可以直接 opt*，不用每个调用点都铺一层空判断
        assertEquals(0, ws.profile.length)
        assertEquals(0, ws.settings.length)
        assertEquals(0, ws.globalSettings.length)
        assertEquals(0, ws.accountStates.length)
        assertEquals("", ws.revision)
        assertTrue(ws.queue.isEmpty)
        assertEquals("", ws.queue.pickedId)
        assertEquals(0L, ws.stickyCount)
        assertEquals(0L, ws.updatedAt)
    }

    // ── 队列 ────────────────────────────────────────────────────

    @Test
    fun `队列行缺 name 与 providerLabel 时回落到 id 与 provider`() {
        val queue = RoutingWorkspaceOps.QueueSnapshot(
            json(
                """
                {
                  "pickedId": "acc_1",
                  "ordered": [
                    {"id": "acc_1", "name": "主号", "provider": "openai", "state": "usable", "reason": "", "resetAt": null},
                    {"id": "acc_2", "provider": "openai", "state": "usable", "reason": ""}
                  ],
                  "blocked": [
                    {"id": "acc_9", "provider": "my-relay", "state": "tried", "reason": "模拟首选成员失败，未实际发送", "resetAt": 123}
                  ]
                }
                """,
            ),
        )

        assertEquals(2, queue.ordered.size)
        assertEquals("主号", queue.ordered[0].name)
        // 工作台的 entry_json 在无冷却时发 null → 落到默认值 0
        assertEquals(0L, queue.ordered[0].resetAt)
        // 两个端点都不发 priority，恒为 -1（保留只为与 RoutingOps.QueueRow 同形）
        assertEquals(-1L, queue.ordered[0].priority)

        // 缺 name / providerLabel：回落到 id / provider，不留空白
        assertEquals("acc_2", queue.ordered[1].name)
        assertEquals("openai", queue.ordered[1].providerLabel)
        assertTrue(queue.ordered[1].usable)

        // 预演里被模拟排除的那条是 tried，不算可用
        assertEquals("tried", queue.blocked[0].state)
        assertFalse(queue.blocked[0].usable)
        assertEquals("my-relay", queue.blocked[0].providerLabel)
        assertEquals(123L, queue.blocked[0].resetAt)
    }

    @Test
    fun `队列 items 不是对象时跳过而不是炸整页`() {
        val queue = RoutingWorkspaceOps.QueueSnapshot(
            json("""{"ordered": [{"id": "acc_1"}, "脏数据", 42], "blocked": []}"""),
        )
        assertEquals(1, queue.ordered.size)
        assertEquals("acc_1", queue.ordered[0].id)
        assertFalse(queue.isEmpty)
    }

    // ── 路由组 ──────────────────────────────────────────────────

    @Test
    fun `路由组快照原样保留组结构与修订号`() {
        val snap = RoutingWorkspaceOps.GroupsSnapshot(groupsPayload())

        assertEquals(1, snap.groups.size)
        val row = snap.groups[0]
        // group 整个原样给出：成员的嵌套名单不展开
        assertEquals("fast", row.group.optString("id"))
        assertEquals("极速组", row.group.optString("name"))
        val members = row.group.optJSONArray("members")!!
        assertEquals(1, members.length)
        assertEquals("openai", members.getJSONObject(0).optString("provider"))
        assertEquals(1, members.getJSONObject(0).optJSONArray("accounts")!!.length)

        assertEquals(64, row.revision.length)
        assertEquals(64, snap.newRevision.length)
        // 新建组与改既有组用的是两个不同的修订号，混用就是 409
        assertNotEquals(row.revision, snap.newRevision)
    }

    @Test
    fun `成员目录行缺 capabilities 时给空对象`() {
        val row = RoutingWorkspaceOps.CatalogRow(
            json("""{"provider": "google", "model": "gemini-2.5-pro"}"""),
        )

        assertEquals("google", row.provider)
        // label / name 缺键时回落到 id 与模型名，界面不该出现空白行
        assertEquals("google", row.providerLabel)
        assertEquals("gemini-2.5-pro", row.name)
        assertEquals(0, row.capabilities.length)
        assertEquals(0L, row.accountCount)
    }

    @Test
    fun `成员目录的能力表里 null 与缺键语义不同`() {
        val row = RoutingWorkspaceOps.CatalogRow(
            json(
                """
                {
                  "provider": "openai",
                  "providerLabel": "OpenAI",
                  "model": "gpt-4o",
                  "name": "GPT-4o",
                  "accountCount": 2,
                  "capabilities": {
                    "maxInputTokens": 200000,
                    "maxOutputTokens": 8192,
                    "supportsToolCall": true,
                    "supportsImages": true,
                    "supportsVideo": null,
                    "supportsReasoning": null
                  }
                }
                """,
            ),
        )

        assertEquals(2L, row.accountCount)
        assertEquals(200000L, row.capabilities.optLong("maxInputTokens"))
        assertTrue(row.capabilities.optBoolean("supportsToolCall"))
        // null = 没声明（不是「不支持」），界面过滤时不能当 false 处理
        assertTrue(row.capabilities.isNull("supportsVideo"))
        assertFalse(row.capabilities.has("unknownKey"))
    }

    // ── 粘滞 ────────────────────────────────────────────────────

    @Test
    fun `粘滞快照解出模式与会话`() {
        val snap = RoutingWorkspaceOps.StickySnapshot(stickyPayload())

        assertEquals("session", snap.sticky)
        assertFalse(snap.isEmpty)
        assertEquals(2, snap.sessions.size)

        val first = snap.sessions[0]
        assertEquals("session:cq-9f2c4a1e6b0d8c3f", first.key)
        // 短标签由内核给（超长取末 12 位 + 省略号），界面不该再截一次
        assertEquals("…4a1e6b0d8c3f", first.label)
        assertEquals("acc_1", first.accountId)
        assertEquals("主号", first.accountLabel)
        assertEquals(1763200000000L, first.lastUsedAt)
    }

    @Test
    fun `粘滞快照缺键按 auto 且视为已清空`() {
        val snap = RoutingWorkspaceOps.StickySnapshot(JSONObject("{}"))

        assertEquals("auto", snap.sticky)
        assertTrue(snap.sessions.isEmpty())
        assertTrue(snap.isEmpty)
    }

    @Test
    fun `粘滞会话缺 label 时回落到 key`() {
        val session = RoutingWorkspaceOps.StickySession(
            json("""{"key": "session:abc", "accountId": "acc_7"}"""),
        )

        assertEquals("session:abc", session.label)
        assertEquals("acc_7", session.accountLabel)
        assertEquals(0L, session.lastUsedAt)
    }

    // ── 预演 ────────────────────────────────────────────────────

    @Test
    fun `预演结果解出命中组与生效设置`() {
        val result = RoutingWorkspaceOps.PreviewResult(previewPayload())

        assertEquals("gpt-4o", result.model)
        assertTrue(result.customized)
        assertEquals("fast", result.group?.optString("id"))
        // 组把自己的策略 / 粘滞叠在基础设置之上
        assertEquals("rotate", result.settings.optString("routingStrategy"))
        assertEquals("acc_1", result.queue.pickedId)
        assertEquals(1, result.queue.ordered.size)
        // 预演的行带 providerLabel（工作台的行没有），这里必须读到
        assertEquals("OpenAI", result.queue.ordered[0].providerLabel)
        assertTrue(result.queue.ordered[0].usable)
        assertFalse(result.queue.blocked[0].usable)
    }

    @Test
    fun `预演未命中组时 group 为 null 且空队列不炸`() {
        val result = RoutingWorkspaceOps.PreviewResult(
            json(
                """
                {
                  "model": "gpt-4o",
                  "customized": false,
                  "group": null,
                  "preview": true,
                  "settings": {},
                  "queue": {"strategy": "order", "sticky": "auto", "pickedId": null, "ordered": [], "blocked": []}
                }
                """,
            ),
        )

        assertNull(result.group)
        assertFalse(result.customized)
        // 预演队列为空时 pickedId 是 null → 折成空串，界面判空不必区分两种
        assertEquals("", result.queue.pickedId)
        assertTrue(result.queue.isEmpty)
    }

    // ── 清粘滞 ──────────────────────────────────────────────────

    @Test
    fun `清粘滞结果解出条数且缺键按成功`() {
        val cleared = RoutingWorkspaceOps.ClearResult(json("""{"ok": true, "cleared": 2}"""))
        assertTrue(cleared.ok)
        assertEquals(2L, cleared.cleared)

        // 缺键：能构造出这个对象说明信封已是 2xx success，ok 按 true
        val bare = RoutingWorkspaceOps.ClearResult(JSONObject("{}"))
        assertTrue(bare.ok)
        assertEquals(0L, bare.cleared)
    }

    // ── 与 RoutingOps 的契约 ────────────────────────────────────

    @Test
    fun `工作台的 settings 可直接套 RoutingOps 的九项解析`() {
        // 模块头承诺的「同一份 routing_json 形状」：不重复映射，直接用
        // RoutingOps.Settings 拿那九项（含「熔断默认关」这类兜底值）。
        val ws = RoutingWorkspaceOps.Workspace(workspacePayload())
        val settings = RoutingOps.Settings(ws.settings)

        assertEquals("order", settings.strategy)
        assertEquals("auto", settings.sticky)
        assertFalse(settings.breakerEnabled)
        assertEquals(5L, settings.breakerThreshold)
        assertEquals(60L, settings.breakerTimeoutSeconds)
    }

    @Test
    fun `预演需求只序列化内核认识的三个键`() {
        // 内核 RequestNeeds 是 deny_unknown_fields：多一个键就 400，
        // 这条用例钉住「别顺手往里塞新字段」。
        val needs = RoutingWorkspaceOps.Needs(tools = true, images = false, video = true)
        val payload = needs.toJson()

        assertEquals(setOf("tools", "images", "video"), payload.keys().asSequence().toSet())
        assertTrue(payload.getBoolean("tools"))
        assertFalse(payload.getBoolean("images"))
        assertTrue(payload.getBoolean("video"))
    }

    // ── 固定输入（字段名逐字抄自内核 json! 字面量）────────────────

    private fun workspacePayload(): JSONObject = json(
        """
        {
          "model": "gpt-4o",
          "customized": true,
          "globalCustomized": true,
          "configuredModels": ["gpt-4o", "claude-3-7"],
          "models": ["claude-3-7", "gpt-4o"],
          "profile": {
            "providerScope": "selected",
            "providers": ["openai", "anthropic"],
            "disabledProviders": [],
            "accountOrder": {"openai": ["acc_1", "acc_2"]},
            "excludedAccounts": ["acc_9"],
            "strategy": "rotate",
            "sticky": "session"
          },
          "inheritedProfile": {
            "providerScope": "all",
            "providers": ["openai", "anthropic", "google"],
            "disabledProviders": ["google"],
            "accountOrder": {},
            "excludedAccounts": []
          },
          "providers": [
            {"id": "anthropic", "label": "Anthropic", "accountCount": 1, "custom": false, "available": true},
            {"id": "openai", "label": "OpenAI", "accountCount": 2, "custom": false, "available": true},
            {"id": "my-relay", "label": "my-relay", "accountCount": 0, "custom": true, "available": false}
          ],
          "accounts": [{"id": "acc_1", "name": "主号"}],
          "queue": {
            "pickedId": "acc_1",
            "ordered": [
              {"id": "acc_1", "name": "主号", "provider": "openai", "state": "usable", "reason": "", "resetAt": null},
              {"id": "acc_2", "name": "备用", "provider": "openai", "state": "usable", "reason": "", "resetAt": null}
            ],
            "blocked": [
              {"id": "acc_3", "name": "小号", "provider": "anthropic", "state": "cooldown", "reason": "账号处于上游冷却", "resetAt": 1763000000000}
            ]
          },
          "settings": {
            "routingStrategy": "order",
            "routingSticky": "auto",
            "routingFailoverBackoff": true,
            "routingSink": true,
            "routingBreakerEnabled": false,
            "routingBreakerThreshold": 5,
            "routingBreakerTimeoutSeconds": 60,
            "routingBreakerSuccessThreshold": 2
          },
          "globalSettings": {
            "routingStrategy": "rotate",
            "routingSticky": "session",
            "routingFailoverBackoff": false,
            "routingSink": true,
            "routingBreakerEnabled": true,
            "routingBreakerThreshold": 9,
            "routingBreakerTimeoutSeconds": 30,
            "routingBreakerSuccessThreshold": 3
          },
          "revision": "9f2c4a1e6b0d8c3f5a7e9b1d4c6f8a0e2b4d6f8a0c2e4b6d8f0a2c4e6b8d0f2a",
          "accountStates": {"acc_1": {"model": "gpt-4o", "resetAt": 0, "limitedModels": 0}},
          "accountConnections": {"acc_1": 2},
          "gateway": {"port": 8787, "host": "127.0.0.1"},
          "stickyCount": 3,
          "updatedAt": 1763200000000
        }
        """,
    )

    private fun groupsPayload(): JSONObject = json(
        """
        {
          "groups": [
            {
              "group": {
                "id": "fast",
                "name": "极速组",
                "model": "gpt-4o",
                "description": "",
                "strategy": "rotate",
                "sticky": "session",
                "members": [
                  {"id": "m1", "provider": "openai", "model": "gpt-4o", "included": true, "accounts": ["acc_1"]}
                ]
              },
              "revision": "ab12cd34ef56ab78cd90ef12ab34cd56ef78ab90cd12ef34ab56cd78ef90ab12"
            }
          ],
          "newRevision": "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
          "catalog": [
            {
              "provider": "openai",
              "providerLabel": "OpenAI",
              "model": "gpt-4o",
              "name": "GPT-4o",
              "capabilities": {
                "maxInputTokens": 200000,
                "maxOutputTokens": 8192,
                "supportsToolCall": true,
                "supportsImages": true,
                "supportsVideo": null,
                "supportsReasoning": null
              },
              "accountCount": 2
            },
            {
              "provider": "anthropic",
              "providerLabel": "Anthropic",
              "model": "claude-3-7-sonnet",
              "name": "Claude 3.7 Sonnet",
              "capabilities": {"supportsToolCall": true},
              "accountCount": 1
            }
          ]
        }
        """,
    )

    private fun stickyPayload(): JSONObject = json(
        """
        {
          "sticky": "session",
          "sessions": [
            {
              "key": "session:cq-9f2c4a1e6b0d8c3f",
              "label": "…4a1e6b0d8c3f",
              "accountId": "acc_1",
              "accountLabel": "主号",
              "lastUsedAt": 1763200000000
            },
            {
              "key": "session:cc-1a2b3c4d5e6f7a8b",
              "label": "…3c4d5e6f7a8b",
              "accountId": "acc_2",
              "accountLabel": "备用",
              "lastUsedAt": 1763199000000
            }
          ],
          "updatedAt": 1763200000000
        }
        """,
    )

    private fun previewPayload(): JSONObject = json(
        """
        {
          "model": "gpt-4o",
          "customized": true,
          "group": {
            "id": "fast",
            "name": "极速组",
            "model": "gpt-4o",
            "description": "",
            "strategy": "rotate",
            "sticky": "session",
            "members": [
              {"id": "m1", "provider": "openai", "model": "gpt-4o", "included": true, "accounts": ["acc_1"]}
            ]
          },
          "preview": true,
          "settings": {
            "routingStrategy": "rotate",
            "routingSticky": "session",
            "routingFailoverBackoff": true,
            "routingSink": true,
            "routingBreakerEnabled": false,
            "routingBreakerThreshold": 5,
            "routingBreakerTimeoutSeconds": 60,
            "routingBreakerSuccessThreshold": 2
          },
          "queue": {
            "strategy": "rotate",
            "sticky": "session",
            "pickedId": "acc_1",
            "ordered": [
              {"id": "acc_1", "memberId": "m1", "model": "gpt-4o", "name": "主号", "provider": "openai", "providerLabel": "OpenAI", "state": "usable", "reason": "", "resetAt": 0}
            ],
            "blocked": [
              {"id": "acc_3", "memberId": "m2", "model": "gpt-4o", "name": "小号", "provider": "anthropic", "providerLabel": "Anthropic", "state": "tried", "reason": "模拟首选成员失败，未实际发送", "resetAt": 0}
            ]
          }
        }
        """,
    )
}
