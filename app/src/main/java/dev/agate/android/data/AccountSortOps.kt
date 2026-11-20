package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/**
 * 账号自动排序数据层：`GET/PUT /api/accounts/sort`。
 *
 * ── 与路由档案里的 accountOrder 不是一回事 ───────────────────
 * `RoutingWorkspaceOps` 管的 `accountOrder` 是「路由档案内手动写死的顺序」；这里
 * 是「按额度到期 / 余额 / 加入时间自动排序」的规则（每家最多 3 条，顺序即优先级）。
 * 内核能力（config::account_sort + core::account_order::sorting）早已就绪，这条
 * 数据层只是把它接到管理面。
 *
 * 规则是 tagged enum：`{"key":"credit|balance|added","on":bool,"dir":"..."}`。
 * 这里按原样透传规则 JSON（不建 DTO），避免内核加一维规则就要改客户端。
 */
object AccountSortOps {

    class OpResult<T>(val ok: Boolean, val data: T?, val error: String?)

    /** 一次排序结果（GET/PUT 都回这个）。 */
    class Sorting(o: JSONObject) {
        val updatedAt: Long = o.optLong("updatedAt", 0)
        /** 已显式配置的规则（各家 → 规则数组）；未配置的家不出现。 */
        val configured: Map<String, JSONArray> = objectMap(o.optJSONObject("configured"))
        /** 实际生效的规则（配置 + 每家补齐的禁用维度）。 */
        val rules: Map<String, JSONArray> = objectMap(o.optJSONObject("rules"))
        /** 提供商的展示顺序。 */
        val providerOrder: List<String> = o.optJSONArray("providerOrder")?.let { arr ->
            (0 until arr.length()).map { arr.optString(it) }
        } ?: emptyList()
        /** 各家排好的账号 id 顺序。 */
        val accountOrder: Map<String, List<String>> = stringListMap(o.optJSONObject("accountOrder"))

        companion object {
            private fun objectMap(o: JSONObject?): Map<String, JSONArray> {
                if (o == null) return emptyMap()
                val out = LinkedHashMap<String, JSONArray>()
                o.keys().forEach { key -> o.optJSONArray(key)?.let { out[key] = it } }
                return out
            }

            private fun stringListMap(o: JSONObject?): Map<String, List<String>> {
                if (o == null) return emptyMap()
                val out = LinkedHashMap<String, List<String>>()
                o.keys().forEach { key ->
                    out[key] = o.optJSONArray(key)?.let { arr ->
                        (0 until arr.length()).map { arr.optString(it) }
                    } ?: emptyList()
                }
                return out
            }
        }
    }

    suspend fun load(): OpResult<Sorting> {
        val reply = withContext(Dispatchers.IO) { CoreTransport.get("/api/accounts/sort") }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = dataOf(reply) ?: return OpResult(false, null, "响应没有数据")
        return OpResult(true, Sorting(data), null)
    }

    /**
     * 保存一家的排序规则。
     *
     * @param rules 规则 JSON 数组（tagged enum）；传 null 恢复默认。
     * @param onlyIfUnset 仅在该家从未配置过时写入（首次默认场景）。
     */
    suspend fun save(
        provider: String,
        rules: JSONArray?,
        onlyIfUnset: Boolean = false,
    ): OpResult<Sorting> {
        val body = JSONObject()
            .put("provider", provider)
            .put("rules", rules)
            .put("onlyIfUnset", onlyIfUnset)
        val reply = withContext(Dispatchers.IO) {
            CoreTransport.call("PUT", "/api/accounts/sort", body.toString())
        }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = dataOf(reply) ?: return OpResult(false, null, "响应没有数据")
        return OpResult(true, Sorting(data), null)
    }

    /** 构造一条规则 JSON（界面编辑规则时用）。 */
    fun rule(key: String, on: Boolean, dir: String): JSONObject = JSONObject()
        .put("key", key)
        .put("on", on)
        .put("dir", dir)

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
