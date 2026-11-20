package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/**
 * 图片兼容配置数据层：`GET/PUT /api/vision-compat`。
 *
 * 这条配置决定「发图给看不了图的模型时怎么降级」：
 * · `assist`：另调一只视觉模型把图读成文字，替换后交回原模型；
 * · `reroute`：把整个请求交给指定视觉模型。
 *
 * PUT 是整份替换 + revision 防并发覆盖（规则有序，首条命中即止，不能合并）。
 */
object VisionCompatOps {

    class OpResult<T>(val ok: Boolean, val data: T?, val error: String?)

    class ModelRef(
        val provider: String,
        val model: String,
    ) {
        fun toJson(): JSONObject = JSONObject().put("provider", provider).put("model", model)
    }

    class VisionTarget(
        val provider: String,
        val model: String,
        val reasoning: String,
    ) {
        fun toJson(): JSONObject = JSONObject()
            .put("provider", provider)
            .put("model", model)
            .put("reasoning", reasoning)
    }

    class Rule(
        val id: String,
        val name: String,
        val enabled: Boolean,
        val mode: String,
        val models: List<ModelRef>,
        val target: VisionTarget?,
        val reuseResults: Boolean,
        val preferCurrentRoute: Boolean,
        val fallback: String,
        val targets: List<VisionTarget>,
    ) {
        fun toJson(): JSONObject = JSONObject().apply {
            put("id", id)
            put("name", name)
            put("enabled", enabled)
            put("mode", mode)
            put("models", JSONArray(models.map { it.toJson() }))
            put("target", target?.toJson())
            put("reuseResults", reuseResults)
            put("preferCurrentRoute", preferCurrentRoute)
            put("fallback", fallback)
            put("targets", JSONArray(targets.map { it.toJson() }))
        }
    }

    class Config(
        val enabled: Boolean,
        val rules: List<Rule>,
        val unknownCapability: String,
        val timeoutSeconds: Long,
    ) {
        fun toJson(): JSONObject = JSONObject()
            .put("enabled", enabled)
            .put("rules", JSONArray(rules.map { it.toJson() }))
            .put("unknownCapability", unknownCapability)
            .put("timeoutSeconds", timeoutSeconds)
    }

    class Limits(
        val rules: Int,
        val models: Int,
        val targets: Int,
        val maxTimeoutSeconds: Long,
    )

    class View(
        val config: Config,
        val revision: String,
        val limits: Limits,
    )

    suspend fun load(): OpResult<View> {
        val reply = withContext(Dispatchers.IO) { CoreTransport.get("/api/vision-compat") }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = dataOf(reply) ?: return OpResult(false, null, "响应没有数据")
        return OpResult(true, parseView(data), null)
    }

    suspend fun save(config: Config, revision: String): OpResult<View> {
        val body = JSONObject()
            .put("config", config.toJson())
            .put("revision", revision)
        val reply = withContext(Dispatchers.IO) {
            CoreTransport.call("PUT", "/api/vision-compat", body.toString())
        }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = dataOf(reply) ?: return OpResult(false, null, "响应没有数据")
        return OpResult(true, parseView(data), null)
    }

    /** 纯解析入口（公开便于单测）。 */
    fun parseView(data: JSONObject): View {
        val cfg = data.optJSONObject("config") ?: JSONObject()
        val rules = cfg.optJSONArray("rules")?.let { arr ->
            (0 until arr.length()).mapNotNull { i ->
                arr.optJSONObject(i)?.let { o ->
                    val models = o.optJSONArray("models")?.let { ma ->
                        (0 until ma.length()).mapNotNull { j ->
                            ma.optJSONObject(j)?.let { m ->
                                ModelRef(m.optString("provider"), m.optString("model"))
                            }
                        }
                    } ?: emptyList()
                    val target = o.optJSONObject("target")?.let(::targetFrom)
                    val targets = o.optJSONArray("targets")?.let { ta ->
                        (0 until ta.length()).mapNotNull { j ->
                            ta.optJSONObject(j)?.let(::targetFrom)
                        }
                    } ?: emptyList()
                    Rule(
                        id = o.optString("id"),
                        name = o.optString("name"),
                        enabled = o.optBoolean("enabled", true),
                        mode = o.optString("mode").ifBlank { "assist" },
                        models = models,
                        target = target,
                        reuseResults = o.optBoolean("reuseResults", true),
                        preferCurrentRoute = o.optBoolean("preferCurrentRoute", true),
                        fallback = o.optString("fallback").ifBlank { "stop" },
                        targets = targets,
                    )
                }
            }
        } ?: emptyList()
        val limitsObj = data.optJSONObject("limits") ?: JSONObject()
        return View(
            config = Config(
                enabled = cfg.optBoolean("enabled", true),
                rules = rules,
                unknownCapability = cfg.optString("unknownCapability").ifBlank { "try_original" },
                timeoutSeconds = cfg.optLong("timeoutSeconds", 30),
            ),
            revision = data.optString("revision"),
            limits = Limits(
                rules = limitsObj.optInt("rules", 32),
                models = limitsObj.optInt("models", 500),
                targets = limitsObj.optInt("targets", 8),
                maxTimeoutSeconds = limitsObj.optLong("maxTimeoutSeconds", 300),
            ),
        )
    }

    private fun targetFrom(o: JSONObject) = VisionTarget(
        provider = o.optString("provider"),
        model = o.optString("model"),
        reasoning = o.optString("reasoning"),
    )

    private fun failureOf(reply: CoreTransport.Reply): String? {
        if (reply !is CoreTransport.Reply.Ok) {
            return (reply as? CoreTransport.Reply.Down)?.message ?: "链路错误"
        }
        val parsed = runCatching { JSONObject(reply.text) }.getOrNull()
        if (reply.status !in 200..299 || parsed?.optBoolean("success") == false) {
            return parsed?.optString("error").orEmpty()
                .ifBlank { "操作失败（HTTP ${reply.status}）" }
        }
        return null
    }

    private fun dataOf(reply: CoreTransport.Reply): JSONObject? {
        if (reply !is CoreTransport.Reply.Ok) return null
        val parsed = runCatching { JSONObject(reply.text) }.getOrNull() ?: return null
        return parsed.optJSONObject("data") ?: parsed
    }
}
