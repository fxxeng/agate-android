package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/**
 * 客户端识别配置数据层：`GET/PUT /api/client-identification` + `POST …/preview`。
 *
 * ── 显示识别，不是行为识别（内核 client/mod.rs 最重要的一条分界）──
 * 这里的规则只影响统计列与提示词规则匹配。协议兼容（Codex 推理折叠等）走内核
 * `classify` 的固定表，**不读这里任何一项**。界面要如实告知用户：改名字不会
 * 改变请求形状。preview 并排给 `strict`（行为识别真值）与 `value`（显示识别），
 * 两者不一致是设计常态，不是 bug。
 *
 * PUT 是**整份替换 + revision 防并发覆盖**（规则有序，合并不保留用户排的顺序）。
 */
object ClientIdentificationOps {

    class OpResult<T>(val ok: Boolean, val data: T?, val error: String?)

    class CustomClient(
        val id: String,
        val name: String,
    ) {
        fun toJson(): JSONObject = JSONObject().put("id", id).put("name", name)
    }

    class ClientRule(
        val id: String,
        val enabled: Boolean,
        val header: String,
        val matchType: String,
        val pattern: String,
        val clientId: String,
    ) {
        fun toJson(): JSONObject = JSONObject()
            .put("id", id)
            .put("enabled", enabled)
            .put("header", header)
            .put("matchType", matchType)
            .put("pattern", pattern)
            .put("clientId", clientId)
    }

    /** 整份配置（PUT 与 preview 共用；读出来即可原样提交）。 */
    class Config(
        val acceptDeclarations: Boolean,
        val clients: List<CustomClient>,
        val rules: List<ClientRule>,
    ) {
        fun toJson(): JSONObject = JSONObject()
            .put("acceptDeclarations", acceptDeclarations)
            .put("clients", JSONArray(clients.map { it.toJson() }))
            .put("rules", JSONArray(rules.map { it.toJson() }))
    }

    /** 内置客户端目录项（下拉选项由后端给，前端不抄一份以免分叉）。 */
    class Builtin(o: JSONObject) {
        val id: String = o.optString("id")
        val name: String = o.optString("name")
        val value: String = o.optString("value")
        val ua: String = o.optString("ua")
        val products: List<String> = o.optJSONArray("products")?.let { arr ->
            (0 until arr.length()).map { arr.optString(it) }
        } ?: emptyList()
    }

    /** GET 的完整视图。 */
    class View(
        val config: Config,
        val revision: String,
        val builtins: List<Builtin>,
        val headers: List<String>,
        val declarationHeader: String,
        val maxRules: Int,
        val maxClients: Int,
    )

    /** preview 结果：strict 是协议行为值，value 是显示识别值。 */
    class Preview(
        val strict: String,
        val value: String,
        val label: String,
        val evidence: JSONObject,
    )

    suspend fun load(): OpResult<View> {
        val reply = withContext(Dispatchers.IO) {
            CoreTransport.get("/api/client-identification")
        }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = dataOf(reply) ?: return OpResult(false, null, "响应没有数据")
        return OpResult(true, parseView(data), null)
    }

    /** 整份替换；成功时内核回写生效后的全量视图，直接用它重绘。 */
    suspend fun save(config: Config, revision: String): OpResult<View> {
        val body = JSONObject()
            .put("config", config.toJson())
            .put("revision", revision)
        val reply = withContext(Dispatchers.IO) {
            CoreTransport.call("PUT", "/api/client-identification", body.toString())
        }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = dataOf(reply) ?: return OpResult(false, null, "响应没有数据")
        return OpResult(true, parseView(data), null)
    }

    /**
     * 预览「假如客户端发来这些头」会识别成谁。
     *
     * @param headers 顺序保留、同名可重复（ClientEvidence::capture 的判据正是
     *   「同名头只能有一个」，用 Map 表达不了畸形请求场景）。
     */
    suspend fun preview(
        config: Config,
        userAgent: String,
        headers: List<Pair<String, String>>,
    ): OpResult<Preview> {
        val headerArray = JSONArray(headers.map { (name, value) ->
            JSONArray().put(name).put(value)
        })
        val body = JSONObject()
            .put("config", config.toJson())
            .put("userAgent", userAgent)
            .put("headers", headerArray)
        val reply = withContext(Dispatchers.IO) {
            CoreTransport.post("/api/client-identification/preview", body.toString())
        }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = dataOf(reply) ?: return OpResult(false, null, "响应没有数据")
        return OpResult(
            true,
            Preview(
                strict = data.optString("strict"),
                value = data.optString("value"),
                label = data.optString("label"),
                evidence = data.optJSONObject("evidence") ?: JSONObject(),
            ),
            null,
        )
    }

    /** 纯解析入口（公开便于单测）：把内核视图 JSON 映射成 [View]。 */
    fun parseView(data: JSONObject): View {
        val cfg = data.optJSONObject("config") ?: JSONObject()
        val clients = cfg.optJSONArray("clients")?.let { arr ->
            (0 until arr.length()).mapNotNull { i ->
                arr.optJSONObject(i)?.let { o ->
                    CustomClient(o.optString("id"), o.optString("name"))
                }
            }
        } ?: emptyList()
        val rules = cfg.optJSONArray("rules")?.let { arr ->
            (0 until arr.length()).mapNotNull { i ->
                arr.optJSONObject(i)?.let { o ->
                    ClientRule(
                        id = o.optString("id"),
                        enabled = o.optBoolean("enabled", false),
                        header = o.optString("header"),
                        matchType = o.optString("matchType").ifBlank { "product" },
                        pattern = o.optString("pattern"),
                        clientId = o.optString("clientId"),
                    )
                }
            }
        } ?: emptyList()
        val builtins = data.optJSONArray("builtins")?.let { arr ->
            (0 until arr.length()).mapNotNull { i -> arr.optJSONObject(i)?.let(::Builtin) }
        } ?: emptyList()
        val headers = data.optJSONArray("headers")?.let { arr ->
            (0 until arr.length()).map { arr.optString(it) }
        } ?: emptyList()
        return View(
            config = Config(cfg.optBoolean("acceptDeclarations", false), clients, rules),
            revision = data.optString("revision"),
            builtins = builtins,
            headers = headers,
            declarationHeader = data.optString("declarationHeader"),
            maxRules = data.optInt("maxRules", 200),
            maxClients = data.optInt("maxClients", 100),
        )
    }

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
