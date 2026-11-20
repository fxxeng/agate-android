package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

/**
 * 模型访问数据层：`GET /api/model-access/keys` 与 `GET /api/model-access/models`。
 *
 * ── 与 `/v1/models` 的分工（内核 model_access.rs 模块头）─────────
 * `/v1/models` 是免鉴权探针，拿不到 Key 限制就给全量；这两条是管理面的
 * 「权限查询」：**必须**明确带一把 Key，问的是「这把 Key 能用哪些模型」。
 * 所以 [models] 走 [CoreTransport.callWithKey] 按次传选中的 Key，而不是面板
 * 全局那把 —— 界面选 A、结果按 B 的权限算是这里最容易踩的坑。
 *
 * [keys] 也走同一通道（面板凭证即可），返回当前可用的 Key 清单供界面选择。
 */
object ModelAccessOps {

    class OpResult<T>(val ok: Boolean, val data: T?, val error: String?)

    /** 可选的 Key（明文只回本机管理面；source=key|env）。 */
    class KeyChoice(o: JSONObject) {
        val id: String = o.optString("id")
        val name: String = o.optString("name")
        val masked: String = o.optString("masked")
        val key: String = o.optString("key")
        val source: String = o.optString("source").ifBlank { "key" }
        val enabled: Boolean = o.optBoolean("enabled", false)
        val allowedProviders: List<String> = o.optJSONArray("allowedProviders")?.let { arr ->
            (0 until arr.length()).map { arr.optString(it) }
        } ?: emptyList()
        val allowedModels: List<String> = o.optJSONArray("allowedModels")?.let { arr ->
            (0 until arr.length()).map { arr.optString(it) }
        } ?: emptyList()

        val isEnv: Boolean get() = source == "env" || id == ENV_KEY_ID

        companion object {
            const val ENV_KEY_ID = "env"
        }
    }

    /** 清单条目（`/models` 的 data[]）。界面只用到 id / name，其余保留原文。 */
    class ModelItem(o: JSONObject) {
        val id: String = o.optString("id")
        val name: String = o.optString("name").ifBlank { id }
        val ownedBy: String = o.optString("owned_by").ifBlank { o.optString("ownedBy") }
        val raw: JSONObject = o
    }

    /** 可用 Key 清单（`GET /api/model-access/keys`）。 */
    suspend fun keys(): OpResult<List<KeyChoice>> {
        val reply = withContext(Dispatchers.IO) { CoreTransport.get("/api/model-access/keys") }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = dataOf(reply) ?: return OpResult(false, null, "响应没有数据")
        val arr = data.optJSONArray("keys")
            ?: return OpResult(false, null, "响应缺少 keys")
        val list = (0 until arr.length()).mapNotNull { i ->
            arr.optJSONObject(i)?.let(::KeyChoice)
        }
        return OpResult(true, list, null)
    }

    /**
     * 指定 Key 权限下的模型清单（`GET /api/model-access/models`）。
     *
     * @param key 选中的 Key 明文（来自 [keys] 的 [KeyChoice.key]）；空白视为未选。
     */
    suspend fun models(key: String): OpResult<List<ModelItem>> {
        if (key.isBlank()) return OpResult(false, null, "请先选择一把 API Key")
        val reply = withContext(Dispatchers.IO) {
            CoreTransport.callWithKey("GET", "/api/model-access/models", null, key)
        }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = dataOf(reply) ?: return OpResult(false, null, "响应没有数据")
        // models_response 形态：{ object:"list", data:[{id,...}] }
        val arr = data.optJSONArray("data")
            ?: return OpResult(false, null, "响应缺少 data")
        val list = (0 until arr.length()).mapNotNull { i ->
            arr.optJSONObject(i)?.let(::ModelItem)
        }
        return OpResult(true, list, null)
    }

    // ── 内部：与管理面其它 Ops 同一套信封判据 ──────────────────
    private fun failureOf(reply: CoreTransport.Reply): String? {
        if (reply !is CoreTransport.Reply.Ok) {
            return (reply as? CoreTransport.Reply.Down)?.message ?: "链路错误"
        }
        val parsed = runCatching { JSONObject(reply.text) }.getOrNull()
        if (reply.status !in 200..299 || parsed?.optBoolean("success") == false) {
            return parsed?.optString("error").orEmpty()
                .ifBlank { "查询失败（HTTP ${reply.status}）" }
        }
        return null
    }

    private fun dataOf(reply: CoreTransport.Reply): JSONObject? {
        if (reply !is CoreTransport.Reply.Ok) return null
        val parsed = runCatching { JSONObject(reply.text) }.getOrNull() ?: return null
        return parsed.optJSONObject("data") ?: parsed
    }
}
