package dev.agate.android.data

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * 三层地基数据层的纯解析测试（不依赖 Android / CoreTransport）。
 * 用手工 JSONObject 模拟三条端点的内核响应，验证字段映射与缺键兜底 ——
 * 这些解析若写错，界面会显示空白或把显示识别当成行为识别，且不会报错。
 */
class ThreeLayerOpsTest {

    // ── ModelAccessOps ──────────────────────────────────────────

    @Test
    fun keyChoice_parses_source_and_env() {
        val regular = ModelAccessOps.KeyChoice(JSONObject().apply {
            put("id", "k1"); put("name", "主 Key"); put("masked", "sk-***1")
            put("key", "sk-full"); put("source", "key"); put("enabled", true)
        })
        assertEquals("k1", regular.id)
        assertEquals("sk-full", regular.key)
        assertFalse(regular.isEnv)

        val env = ModelAccessOps.KeyChoice(JSONObject().apply {
            put("id", "env"); put("name", "环境变量 Key"); put("source", "env")
        })
        assertTrue(env.isEnv)
    }

    @Test
    fun keyChoice_allowedLists_fallback_empty() {
        val key = ModelAccessOps.KeyChoice(JSONObject().put("id", "k"))
        assertTrue(key.allowedProviders.isEmpty())
        assertTrue(key.allowedModels.isEmpty())
    }

    @Test
    fun modelItem_name_falls_back_to_id() {
        val item = ModelAccessOps.ModelItem(JSONObject().put("id", "gpt-4o"))
        assertEquals("gpt-4o", item.id)
        assertEquals("gpt-4o", item.name)
    }

    @Test
    fun modelItem_prefers_explicit_name_and_ownedBy() {
        val item = ModelAccessOps.ModelItem(JSONObject().apply {
            put("id", "m1"); put("name", "My Model"); put("owned_by", "openai")
        })
        assertEquals("My Model", item.name)
        assertEquals("openai", item.ownedBy)
    }

    // ── ClientIdentificationOps ─────────────────────────────────

    private fun clientViewJson(): JSONObject = JSONObject().apply {
        put("config", JSONObject().apply {
            put("acceptDeclarations", true)
            put("clients", JSONArray().put(JSONObject().put("id", "c1").put("name", "My Client")))
            put("rules", JSONArray().put(JSONObject().apply {
                put("id", "r1"); put("enabled", true); put("header", "user-agent")
                put("matchType", "prefix"); put("pattern", "Foo/"); put("clientId", "c1")
            }))
        })
        put("revision", "a".repeat(64))
        put("builtins", JSONArray().put(JSONObject().apply {
            put("id", "codex"); put("name", "Codex"); put("value", "Codex")
            put("ua", "Codex Desktop/1.0"); put("products", JSONArray().put("codex"))
        }))
        put("headers", JSONArray().put("user-agent").put("x-agent2api-client"))
        put("declarationHeader", "x-agent2api-client")
        put("maxRules", 200); put("maxClients", 100)
    }

    @Test
    fun clientView_parses_config_rules_and_builtins() {
        val view = ClientIdentificationOps.parseView(clientViewJson())
        assertTrue(view.config.acceptDeclarations)
        assertEquals("a".repeat(64), view.revision)
        assertEquals(1, view.config.clients.size)
        assertEquals("My Client", view.config.clients[0].name)
        assertEquals(1, view.config.rules.size)
        assertEquals("prefix", view.config.rules[0].matchType)
        assertEquals("Foo/", view.config.rules[0].pattern)
        assertEquals(1, view.builtins.size)
        assertEquals("Codex Desktop/1.0", view.builtins[0].ua)
        assertEquals(2, view.headers.size)
        assertEquals("x-agent2api-client", view.declarationHeader)
        assertEquals(200, view.maxRules)
    }

    @Test
    fun clientView_empty_falls_back() {
        val view = ClientIdentificationOps.parseView(JSONObject())
        assertFalse(view.config.acceptDeclarations)
        assertTrue(view.config.clients.isEmpty())
        assertTrue(view.config.rules.isEmpty())
        assertTrue(view.builtins.isEmpty())
        assertEquals("", view.revision)
        assertEquals(200, view.maxRules)   // optInt 缺省
        assertEquals(100, view.maxClients)
    }

    @Test
    fun clientConfig_toJson_roundtrip() {
        val cfg = ClientIdentificationOps.Config(
            acceptDeclarations = true,
            clients = listOf(ClientIdentificationOps.CustomClient("c1", "C1")),
            rules = listOf(ClientIdentificationOps.ClientRule("r1", true, "user-agent", "prefix", "X/", "c1")),
        )
        val json = cfg.toJson()
        val reparsed = ClientIdentificationOps.parseView(JSONObject().put("config", json)).config
        assertTrue(reparsed.acceptDeclarations)
        assertEquals(1, reparsed.rules.size)
        assertEquals("prefix", reparsed.rules[0].matchType)
    }

    // ── PromptRulesOps ──────────────────────────────────────────

    private fun promptViewJson(): JSONObject = JSONObject().apply {
        put("rules", JSONArray().put(JSONObject().apply {
            put("id", "r1"); put("name", "规则一"); put("enabled", true)
            put("logic", "all")
            put("conditions", JSONArray().put(JSONObject().apply {
                put("id", "c1"); put("field", "provider"); put("op", "is")
                put("values", JSONArray().put("openai"))
            }))
            put("mode", "custom"); put("file", ""); put("text", "你是助手")
            put("effectiveText", "你是助手（生效）"); put("source", "inline")
            put("lines", 1); put("fileError", "")
        }))
        put("fallback", JSONObject().apply {
            put("mode", "passthrough"); put("file", ""); put("text", "")
        })
        put("legacy", 2); put("configured", true)
        put("revision", "b".repeat(64)); put("error", "")
        put("limits", JSONObject().apply {
            put("rules", 200); put("conditions", 32); put("values", 100)
        })
    }

    @Test
    fun promptView_parses_rules_conditions_and_limited_fields() {
        val view = PromptRulesOps.parseView(promptViewJson())
        assertEquals(1, view.rules.size)
        val rv = view.rules[0]
        assertTrue(rv.rule.enabled)
        assertEquals("custom", rv.rule.mode)
        assertEquals("你是助手", rv.rule.text)
        assertEquals("你是助手（生效）", rv.effectiveText)
        assertEquals("inline", rv.source)
        assertEquals(1, rv.rule.conditions.size)
        assertEquals("provider", rv.rule.conditions[0].field)
        assertEquals("is", rv.rule.conditions[0].op)
        assertEquals("openai", rv.rule.conditions[0].values[0])
        assertEquals(2, view.legacy)
        assertTrue(view.configured)
        assertEquals("b".repeat(64), view.revision)
        assertEquals(32, view.limits.conditions)
    }

    @Test
    fun promptView_empty_falls_back() {
        val view = PromptRulesOps.parseView(JSONObject())
        assertTrue(view.rules.isEmpty())
        assertEquals("passthrough", view.fallback.mode)
        assertEquals("", view.revision)
        assertEquals(200, view.limits.rules)
        assertEquals(100, view.limits.values)
    }

    @Test
    fun rule_toJson_roundtrip_conditions() {
        val rule = PromptRulesOps.Rule(
            id = "r", name = "R", enabled = false, logic = "any",
            conditions = listOf(
                PromptRulesOps.Condition("c", "client", "contains", listOf("Codex", "ZCode")),
            ),
            mode = "append", file = "", text = "t",
        )
        val json = rule.toJson()
        assertEquals("any", json.optString("logic"))
        val conditions = json.optJSONArray("conditions")
        assertEquals(1, conditions!!.length())
        assertEquals(2, conditions.getJSONObject(0).getJSONArray("values").length())
        assertEquals("append", json.optString("mode"))
    }
}
