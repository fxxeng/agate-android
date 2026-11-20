package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

/**
 * 第三方回答完自动折叠过程开关数据层：`GET/PUT /api/reasoning-collapse`。
 *
 * 仅对严格识别为 Codex 的 Responses 请求补 phase 阶段标记（final_answer /
 * commentary），让客户端折叠思考与工具过程。默认关。
 * 与 `/api/developer-role-compat` 同形（单开关，GET 读 / PUT 写）。
 */
object ReasoningCollapseOps {

    class OpResult<T>(val ok: Boolean, val data: T?, val error: String?)

    suspend fun load(): OpResult<Boolean> {
        val reply = withContext(Dispatchers.IO) { CoreTransport.get("/api/reasoning-collapse") }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = dataOf(reply) ?: return OpResult(false, null, "响应没有数据")
        return OpResult(true, data.optBoolean(KEY, false), null)
    }

    suspend fun save(enabled: Boolean): OpResult<Boolean> {
        val body = JSONObject().put(KEY, enabled)
        val reply = withContext(Dispatchers.IO) {
            CoreTransport.call("PUT", "/api/reasoning-collapse", body.toString())
        }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = dataOf(reply) ?: return OpResult(false, null, "响应没有数据")
        return OpResult(true, data.optBoolean(KEY, enabled), null)
    }

    private const val KEY = "thirdPartyReasoningCollapse"

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
