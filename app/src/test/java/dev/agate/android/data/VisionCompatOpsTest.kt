package dev.agate.android.data

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** 图片兼容配置解析的纯测试（不依赖 Android / CoreTransport）。 */
class VisionCompatOpsTest {

    private fun viewJson(): JSONObject = JSONObject().apply {
        put("config", JSONObject().apply {
            put("enabled", true)
            put("unknownCapability", "try_original")
            put("timeoutSeconds", 30)
            put("rules", JSONArray().put(JSONObject().apply {
                put("id", "r1"); put("name", "GLM 用 GPT-4o 识图"); put("enabled", true)
                put("mode", "assist")
                put("models", JSONArray().put(JSONObject().put("provider", "zhipu").put("model", "glm-4.7")))
                put("target", JSONObject().put("provider", "openai").put("model", "gpt-4o").put("reasoning", ""))
                put("reuseResults", true); put("preferCurrentRoute", true); put("fallback", "stop")
                put("targets", JSONArray())
            }))
        })
        put("revision", "c".repeat(64))
        put("limits", JSONObject().apply {
            put("rules", 32); put("models", 500); put("targets", 8); put("maxTimeoutSeconds", 300)
        })
    }

    @Test
    fun parses_config_rule_and_target() {
        val view = VisionCompatOps.parseView(viewJson())
        assertTrue(view.config.enabled)
        assertEquals("try_original", view.config.unknownCapability)
        assertEquals(30L, view.config.timeoutSeconds)
        assertEquals(1, view.config.rules.size)
        val rule = view.config.rules[0]
        assertEquals("r1", rule.id)
        assertEquals("assist", rule.mode)
        assertEquals("zhipu", rule.models[0].provider)
        assertNotNull(rule.target)
        assertEquals("gpt-4o", rule.target!!.model)
        assertTrue(rule.reuseResults)
        assertEquals("c".repeat(64), view.revision)
        assertEquals(32, view.limits.rules)
        assertEquals(300L, view.limits.maxTimeoutSeconds)
    }

    @Test
    fun empty_falls_back_to_defaults() {
        val view = VisionCompatOps.parseView(JSONObject())
        assertTrue(view.config.enabled)
        assertTrue(view.config.rules.isEmpty())
        assertEquals("try_original", view.config.unknownCapability)
        assertEquals(30L, view.config.timeoutSeconds)
        assertEquals(32, view.limits.rules)
    }

    @Test
    fun reroute_rule_parses_targets() {
        val json = viewJson()
        val rule = json.getJSONObject("config").getJSONArray("rules").getJSONObject(0)
        rule.put("mode", "reroute")
        rule.put("targets", JSONArray().put(JSONObject().put("provider", "openai").put("model", "gpt-4o")))
        val view = VisionCompatOps.parseView(json)
        assertEquals("reroute", view.config.rules[0].mode)
        assertEquals(1, view.config.rules[0].targets.size)
        assertEquals("gpt-4o", view.config.rules[0].targets[0].model)
    }

    @Test
    fun config_toJson_roundtrip() {
        val cfg = VisionCompatOps.Config(
            enabled = true,
            rules = listOf(
                VisionCompatOps.Rule(
                    id = "r1", name = "R", enabled = true, mode = "assist",
                    models = listOf(VisionCompatOps.ModelRef("p", "m")),
                    target = VisionCompatOps.VisionTarget("v", "vm", ""),
                    reuseResults = true, preferCurrentRoute = true, fallback = "stop",
                    targets = emptyList(),
                )
            ),
            unknownCapability = "compat",
            timeoutSeconds = 45,
        )
        val reparsed = VisionCompatOps.parseView(JSONObject().put("config", cfg.toJson())).config
        assertEquals(1, reparsed.rules.size)
        assertEquals("assist", reparsed.rules[0].mode)
        assertEquals("compat", reparsed.unknownCapability)
        assertEquals(45L, reparsed.timeoutSeconds)
    }
}
