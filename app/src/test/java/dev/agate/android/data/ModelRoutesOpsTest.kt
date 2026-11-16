package dev.agate.android.data

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ModelRoutesOps 的纯解析测试（不依赖 Android 框架 / CoreTransport）。
 * 用手工 JSONObject 模拟内核 GET 返回，验证数据类映射与缺键兜底。
 */
class ModelRoutesOpsTest {

    private fun titleJson(): JSONObject = JSONObject().apply {
        put("config", JSONObject().apply {
            put("enabled", true)
            put("rules", org.json.JSONArray().apply {
                put(JSONObject().apply {
                    put("enabled", true)
                    put("clients", org.json.JSONArray().put("zcode"))
                    put("model", "gpt-4o-mini")
                    put("fallback", "original")
                })
            })
        })
        put("revision", "a".repeat(64))
        put("clients", org.json.JSONArray().apply {
            put(JSONObject().put("id", "zcode").put("label", "ZCode").put("supported", true))
            put(JSONObject().put("id", "codex").put("label", "Codex").put("supported", true))
        })
        put("models", org.json.JSONArray().apply {
            put(JSONObject().put("id", "gpt-4o-mini").put("name", "GPT-4o Mini"))
        })
    }

    private fun compactJson(): JSONObject = JSONObject().apply {
        put("config", JSONObject().apply {
            put("enabled", false)
            put("rules", org.json.JSONArray().apply {
                put(JSONObject().apply {
                    put("client", "zcode")
                    put("source", "glm-4")
                    put("target", "glm-4-flash")
                    put("enabled", true)
                })
            })
        })
        put("revision", "b".repeat(64))
        put("clients", org.json.JSONArray().put(
            JSONObject().put("id", "zcode").put("label", "ZCode")
        ))
        put("models", org.json.JSONArray().apply {
            put(JSONObject().put("id", "glm-4").put("name", "GLM-4"))
            put(JSONObject().put("id", "glm-4-flash").put("name", "GLM-4 Flash"))
        })
        put("maxRules", 200)
    }

    @Test
    fun titleConfig_parses_enabled_and_rules() {
        val cfg = ModelRoutesOps.RouteConfig(titleJson())
        assertTrue(cfg.enabled)
        assertEquals(1, cfg.ruleCount)
        assertEquals("a".repeat(64), cfg.revision)
    }

    @Test
    fun titleConfig_parses_clients() {
        val cfg = ModelRoutesOps.RouteConfig(titleJson())
        assertEquals(2, cfg.clients.size)
        assertEquals("zcode", cfg.clients[0].id)
        assertEquals("ZCode", cfg.clients[0].label)
        assertTrue(cfg.clients[0].supported)
    }

    @Test
    fun titleConfig_parses_models() {
        val cfg = ModelRoutesOps.RouteConfig(titleJson())
        assertEquals(1, cfg.models.size)
        assertEquals("gpt-4o-mini", cfg.models[0].id)
        assertEquals("GPT-4o Mini", cfg.models[0].name)
    }

    @Test
    fun titleConfig_maxRules_defaults_zero() {
        val cfg = ModelRoutesOps.RouteConfig(titleJson())
        assertEquals(0, cfg.maxRules)
    }

    @Test
    fun compactConfig_parses_disabled() {
        val cfg = ModelRoutesOps.RouteConfig(compactJson())
        assertFalse(cfg.enabled)
        assertEquals(1, cfg.ruleCount)
        assertEquals(200, cfg.maxRules)
    }

    @Test
    fun compactConfig_parses_models() {
        val cfg = ModelRoutesOps.RouteConfig(compactJson())
        assertEquals(2, cfg.models.size)
        assertEquals("glm-4-flash", cfg.models[1].id)
    }

    @Test
    fun emptyJson_falls_back_gracefully() {
        val cfg = ModelRoutesOps.RouteConfig(JSONObject())
        assertTrue(cfg.enabled) // config 缺省 → optBoolean("enabled", true)
        assertEquals(0, cfg.ruleCount)
        assertEquals("", cfg.revision)
        assertTrue(cfg.clients.isEmpty())
        assertTrue(cfg.models.isEmpty())
        assertEquals(0, cfg.maxRules)
    }

    @Test
    fun client_label_falls_back_to_id() {
        val o = JSONObject().put("id", "test-client")
        val c = ModelRoutesOps.Client(o)
        assertEquals("test-client", c.label)
        assertTrue(c.supported) // 缺省 true
    }

    @Test
    fun model_name_falls_back_to_id() {
        val o = JSONObject().put("id", "some-model")
        val m = ModelRoutesOps.Model(o)
        assertEquals("some-model", m.name)
    }

    @Test
    fun config_jsonObject_preserved_for_edit() {
        val original = titleJson()
        val cfg = ModelRoutesOps.RouteConfig(original)
        // 编辑：直接改 config 里的 rules
        cfg.config.getJSONArray("rules").getJSONObject(0).put("model", "changed")
        assertEquals("changed", cfg.config.getJSONArray("rules").getJSONObject(0).getString("model"))
        // revision 不变（编辑不改修订号，提交时才校验）
        assertEquals("a".repeat(64), cfg.revision)
    }
}
