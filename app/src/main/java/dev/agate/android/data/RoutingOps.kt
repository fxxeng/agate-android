package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

/**
 * 路由设置与候选队列（「路由预演」页）。
 *
 * 内核路由（`agate-server/src/server/api/routing_api.rs` 模块头）：
 * - `GET/PUT /api/routing`        三策略 / 会话粘性 / 退避 / 沉底 / 熔断四参数
 * - `GET    /api/routing/queue`   候选队列可见化（`?model=` 可省）
 *
 * ── 为什么这一页可信 ─────────────────────────────────────────
 * 队列排序**与转发选路是同一个函数**（`routing::plan_route`）—— 界面怎么画，
 * 实际就怎么走。两处若各写一份，界面标的顺序迟早和实际转发分叉，
 * 而「候选队列可见化」要消灭的正是那类问题。
 *
 * ── 写入语义 ─────────────────────────────────────────────────
 * PUT 各键全部可选：不给 = 不动。响应回**生效后的全量值**，界面直接用响应
 * 重绘，不必再拉一次。
 */
object RoutingOps {

    /**
     * 路由设置（内核 `routing_json` 的形状）。
     *
     * 各项兜底值逐一对齐 `RoutingSettings::default()`：熔断**默认关**
     * （`breaker_enabled: false`）、恢复阈值默认 2 —— 这两处照「常识」猜容易
     * 写成 true / 1，一旦后端偶发缺键，界面会显示与实际相反的状态。
     */
    class Settings(o: JSONObject) {
        val strategy: String = o.optString("routingStrategy").ifBlank { "order" }
        val sticky: String = o.optString("routingSticky").ifBlank { "auto" }
        val failoverBackoff: Boolean = o.optBoolean("routingFailoverBackoff", true)
        val sink: Boolean = o.optBoolean("routingSink", true)
        val breakerEnabled: Boolean = o.optBoolean("routingBreakerEnabled", false)
        val breakerThreshold: Long = o.optLong("routingBreakerThreshold", 5L)
        val breakerTimeoutSeconds: Long = o.optLong("routingBreakerTimeoutSeconds", 60L)
        val breakerSuccessThreshold: Long = o.optLong("routingBreakerSuccessThreshold", 2L)

        val strategyLabel: String get() = when (strategy) {
            "rotate" -> "负载均衡"
            "provider" -> "同家轮询"
            else -> "顺序优先"
        }

        val stickyLabel: String get() = when (sticky) {
            "session" -> "按会话"
            "turn" -> "按轮次"
            "off" -> "关闭"
            else -> "自动"
        }

        val strategyDesc: String get() = when (strategy) {
            "rotate" -> "同层账号之间摊平请求，谁用得少谁先上"
            "provider" -> "同一家内多账号轮转，换家之前先把这家走完"
            else -> "严格按队列名次，队首不可用才下探下一位"
        }
    }

    /** 队列里的一位（可用或被跳过，都带原因）。 */
    class QueueRow(o: JSONObject) {
        val id: String = o.optString("id")
        val name: String = o.optString("name").ifBlank { id }
        val provider: String = o.optString("provider")
        val providerLabel: String = o.optString("providerLabel").ifBlank { provider }
        val state: String = o.optString("state")
        val reason: String = o.optString("reason")
        val resetAt: Long = o.optLong("resetAt", 0L)
        val priority: Long = o.optLong("priority", -1L)

        /** 能真正承载请求（其余都是被跳过的各种理由）。 */
        val usable: Boolean get() = state == "usable"
    }

    /** 队列快照。 */
    class Plan(
        val strategy: String,
        val sticky: String,
        val model: String?,
        val lastRequestModel: String?,
        val pickedId: String,
        val ordered: List<QueueRow>,
        val blocked: List<QueueRow>,
    )

    suspend fun settings(): OpResult<Settings> {
        val data = get("/api/routing") ?: return OpResult(false, null, "无法连接内核")
        return OpResult(true, Settings(data), null)
    }

    /** `null` 表示「不动该项」—— 与内核 PUT 的部分更新语义一一对应。 */
    suspend fun save(
        strategy: String? = null,
        sticky: String? = null,
        failoverBackoff: Boolean? = null,
        sink: Boolean? = null,
        breakerEnabled: Boolean? = null,
        breakerThreshold: Long? = null,
        breakerTimeoutSeconds: Long? = null,
        breakerSuccessThreshold: Long? = null,
    ): OpResult<Settings> {
        val body = JSONObject()
        strategy?.let { body.put("routingStrategy", it) }
        sticky?.let { body.put("routingSticky", it) }
        failoverBackoff?.let { body.put("routingFailoverBackoff", it) }
        sink?.let { body.put("routingSink", it) }
        breakerEnabled?.let { body.put("routingBreakerEnabled", it) }
        breakerThreshold?.let { body.put("routingBreakerThreshold", it) }
        breakerTimeoutSeconds?.let { body.put("routingBreakerTimeoutSeconds", it) }
        breakerSuccessThreshold?.let { body.put("routingBreakerSuccessThreshold", it) }
        val reply = withContext(Dispatchers.IO) {
            CoreTransport.call("PUT", "/api/routing", body.toString())
        }
        if (reply !is CoreTransport.Reply.Ok) {
            return OpResult(false, null, (reply as? CoreTransport.Reply.Down)?.message ?: "链路错误")
        }
        val parsed = runCatching { JSONObject(reply.text) }.getOrNull()
        if (reply.status !in 200..299) {
            return OpResult(false, null, parsed?.optString("error").orEmpty().ifBlank { "保存失败" })
        }
        val data = parsed?.optJSONObject("data") ?: parsed
        return OpResult(true, data?.let { Settings(it) }, null)
    }

    /** `model` 为空 = 不限模型的全局队列。 */
    suspend fun queue(model: String? = null): OpResult<Plan> {
        val path = if (model.isNullOrBlank()) "/api/routing/queue" else "/api/routing/queue?model=${model.urlEnc()}"
        val data = get(path) ?: return OpResult(false, null, "无法连接内核")
        fun rows(key: String): List<QueueRow> =
            data.optJSONArray(key)?.let { arr ->
                (0 until arr.length()).mapNotNull { arr.optJSONObject(it)?.let(::QueueRow) }
            } ?: emptyList()
        return OpResult(
            true,
            Plan(
                strategy = data.optString("strategy"),
                sticky = data.optString("sticky"),
                model = data.optString("model").ifBlank { null },
                lastRequestModel = data.optString("lastRequestModel").ifBlank { null },
                pickedId = data.optString("pickedId"),
                ordered = rows("ordered"),
                blocked = rows("blocked"),
            ),
            null,
        )
    }

    private suspend fun get(path: String): JSONObject? {
        val reply = withContext(Dispatchers.IO) { CoreTransport.get(path) }
        if (reply !is CoreTransport.Reply.Ok) return null
        val body = runCatching { JSONObject(reply.text) }.getOrNull() ?: return null
        if (!body.optBoolean("success")) return null
        return body.optJSONObject("data") ?: body
    }

    private fun String.urlEnc(): String = java.net.URLEncoder.encode(this, "UTF-8")

    class OpResult<T>(val ok: Boolean, val data: T?, val error: String?)
}
