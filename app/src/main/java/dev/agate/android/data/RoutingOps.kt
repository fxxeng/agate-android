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
        val id: String = jsonTextOrNull(o, "id").orEmpty()
        val name: String = jsonTextOrNull(o, "name").takeUnless { it.isNullOrBlank() } ?: id
        val provider: String = jsonTextOrNull(o, "provider").orEmpty()
        val providerLabel: String = jsonTextOrNull(o, "providerLabel").takeUnless { it.isNullOrBlank() } ?: provider
        val state: String = jsonTextOrNull(o, "state").orEmpty()
        val reason: String = jsonTextOrNull(o, "reason").orEmpty()
        val resetAt: Long = o.optLong("resetAt", 0L)
        val priority: Long = o.optLong("priority", -1L)

        /** 能真正承载请求（其余都是被跳过的各种理由）。 */
        val usable: Boolean get() = state == "usable"
    }

    /**
     * 队列快照 + 内核真算过的**候选范围**。
     *
     * 范围字段（2026-11-07）是为了回答「429 之后有没有下一家」：
     * `ordered` 的第 1 位是本轮出战者，其余才是顺延能落到的人；不承载该
     * 模型的账号（`outsideAccounts`）根本不在这条链里。旧版客户端把
     * `/api/session` 的全量账号当排序基线，界面列出一堆账号、真机上却
     * 只有一个能接这个模型 —— 用户看到 429 不换号，以为是路由坏了。
     *
     * `scopeKnown=false` 表示内核没给范围字段（旧内核）：界面一律显示
     * 「范围暂不可用」，不许拿 0 冒充算过的数。
     */
    class Plan(
        val strategy: String,
        val sticky: String,
        val model: String?,
        val lastRequestModel: String?,
        val pickedId: String,
        val ordered: List<QueueRow>,
        val blocked: List<QueueRow>,
        val scopeKnown: Boolean = false,
        val modelScoped: Boolean = false,
        val scopedAccounts: Int = 0,
        val totalAccounts: Int = 0,
        val outsideAccounts: Int = 0,
        val usableCount: Int = 0,
        val fallbackCount: Int = 0,
        val cooldownCount: Int = 0,
        val earliestResetAt: Long = 0L,
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

    /**
     * `model` 为空 = 不限模型的全局队列。
     *
     * 字面量 `"null"` 一律当空：真机曾经把 JSON null 读成这个字符串，再把它
     * 回传给 `?model=null`；端点于是回显**字符串** `"null"`，刷新又带着它 ——
     * 页面永远显示「模型 null」。这里把环从客户端这一端切断。
     */
    suspend fun queue(model: String? = null): OpResult<Plan> {
        val effective = model
            ?.trim()
            ?.takeIf { it.isNotEmpty() && !it.equals("null", ignoreCase = true) }
        val path = if (effective == null) {
            "/api/routing/queue"
        } else {
            "/api/routing/queue?model=${effective.urlEnc()}"
        }
        val data = get(path) ?: return OpResult(false, null, "无法连接内核")
        return OpResult(true, parsePlan(data), null)
    }

    /** 端点响应 → Plan。抽出来是为了单测能直接喂内核的真实 JSON 形状。 */
    internal fun parsePlan(data: JSONObject): Plan {
        fun rows(key: String): List<QueueRow> =
            data.optJSONArray(key)?.let { arr ->
                (0 until arr.length()).mapNotNull { arr.optJSONObject(it)?.let(::QueueRow) }
            } ?: emptyList()
        val ordered = rows("ordered")
        val blocked = rows("blocked")
        return Plan(
            strategy = jsonTextOrNull(data, "strategy").orEmpty(),
            sticky = jsonTextOrNull(data, "sticky").orEmpty(),
            model = jsonTextOrNull(data, "model"),
            lastRequestModel = jsonTextOrNull(data, "lastRequestModel"),
            pickedId = jsonTextOrNull(data, "pickedId").orEmpty(),
            ordered = ordered,
            blocked = blocked,
            scopeKnown = data.has("totalAccounts") && !data.isNull("totalAccounts"),
            modelScoped = data.optBoolean("modelScoped", false),
            scopedAccounts = data.optInt("scopedAccounts", ordered.size + blocked.size),
            totalAccounts = data.optInt("totalAccounts", 0),
            outsideAccounts = data.optInt("outsideAccounts", 0),
            usableCount = data.optInt("usableCount", ordered.size),
            fallbackCount = data.optInt("fallbackCount", (ordered.size - 1).coerceAtLeast(0)),
            cooldownCount = data.optInt("cooldownCount", 0),
            earliestResetAt = data.optLong("earliestResetAt", 0L),
        )
    }

    /**
     * 预演模型候选：`/api/models/list-view` 的**对外名**（与 `/v1/models` 同源，
     * 客户端真正请求得到的那批名字），不读内部管理视图。取前若干个即可 ——
     * 全量清单可能上百条，弹层里铺不下也没必要。
     */
    suspend fun commonModels(limit: Int = 12): List<String> =
        runCatching { AssistantOps.availableModels() }.getOrDefault(emptyList()).take(limit)

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

/**
 * 从 JSONObject 里只取**真正的字符串**，JSON null 与字面量 `"null"` 都当空。
 *
 * ── 为什么不能直接 optString（2026-11-07 真机截图）───────────────
 * `org.json` 有两套实现：单测用的 `org.json:json` 在 `optString(key, default)`
 * 遇到 JSONObject.NULL 时返回 default；**真机 AOSP libcore 的实现**会把
 * `JSONObject.NULL.toString()` 交出来，也就是字符串 `"null"`。于是同一段
 * 解析在单测里全绿、真机上「不限模型」显示成「模型 null」，刷新还把
 * `"null"` 当模型名回传给端点，端点回显字符串 `"null"` —— 自锁。
 * （本仓 `ProviderPage.loadBalances` 早为同一个坑改过 `error` 的读法。）
 *
 * 只认 `as? String` 可以把两套实现拉回同一语义；字面量 `"null"` 一并挡掉，
 * 是为了把已经自锁的那次请求也救回来。路由队列与控制台快照共用这一份。
 */
internal fun jsonTextOrNull(o: JSONObject, key: String): String? {
    val raw = o.opt(key) as? String ?: return null
    val value = raw.trim()
    return value.takeIf { it.isNotEmpty() && !it.equals("null", ignoreCase = true) }
}
