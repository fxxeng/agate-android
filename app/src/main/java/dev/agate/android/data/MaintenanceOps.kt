package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

/**
 * 维护记录（内核 `api/maintenance.rs`）：
 * - `GET  /api/maintenance`                      概览：任务 + 最近一次 + 记录分页
 * - `GET  /api/maintenance/records/{id}`          单次详情（含逐条结果）
 * - `POST /api/maintenance/run/{taskId}`          手动跑（异步，返回 running 起始详情）
 * - `POST /api/maintenance/records/{id}/retry`    重试失败项（可带 itemId）
 *
 * 契约字段（camelCase）与上游 `maintenance-types.ts` 逐字对齐；内核的
 * `types.rs` 已按同一形状 serde(rename_all="camelCase")，这里只做只读解析。
 * 界面显示识别规则不参与维护执行 —— 本 Ops 只负责取数与动作，不做业务判断。
 */
object MaintenanceOps {

    const val TASK_CREDENTIAL = "credential_maintenance"
    const val TASK_MODEL = "model_refresh"

    /** 一次执行（Run）：概览的记录项与详情共用。 */
    data class Run(
        val id: String,
        val taskId: String,
        val source: String,
        val retryOf: String?,
        val startedAt: Long,
        val finishedAt: Long?,
        val durationMs: Long?,
        val status: String,
        val refreshed: Int,
        val skipped: Int,
        val failed: Int,
        val total: Int,
        val error: String?,
    ) {
        val isRunning: Boolean get() = status == "running"

        companion object {
            fun from(o: JSONObject): Run = Run(
                id = o.optString("id"),
                taskId = o.optString("taskId"),
                source = o.optString("source"),
                retryOf = o.optString("retryOf").ifBlank { null },
                startedAt = o.optLong("startedAt"),
                finishedAt = o.optLong("finishedAt", 0L).takeIf { it > 0 },
                durationMs = o.optLong("durationMs", 0L).takeIf { it > 0 },
                status = o.optString("status"),
                refreshed = o.optInt("refreshed"),
                skipped = o.optInt("skipped"),
                failed = o.optInt("failed"),
                total = o.optInt("total"),
                error = o.optString("error").ifBlank { null },
            )
        }
    }

    /** 逐条结果（Item）。 */
    data class Item(
        val id: String,
        val provider: String,
        val providerLabel: String,
        val accountId: String?,
        val accountName: String?,
        val status: String,
        val message: String?,
        val credentialChanged: Boolean?,
        val durationMs: Long?,
        val forced: Boolean,
    ) {
        companion object {
            fun from(o: JSONObject): Item = Item(
                id = o.optString("id"),
                provider = o.optString("provider"),
                providerLabel = o.optString("providerLabel"),
                accountId = o.optString("accountId").ifBlank { null },
                accountName = o.optString("accountName").ifBlank { null },
                status = o.optString("status"),
                message = o.optString("message").ifBlank { null },
                credentialChanged = if (o.has("credentialChanged") && !o.isNull("credentialChanged")) {
                    o.optBoolean("credentialChanged")
                } else {
                    null
                },
                durationMs = o.optLong("durationMs", 0L).takeIf { it > 0 },
                forced = o.optBoolean("forced"),
            )
        }
    }

    data class Detail(val run: Run, val results: List<Item>) {
        companion object {
            fun from(o: JSONObject): Detail = Detail(
                run = Run.from(o),
                results = o.optJSONArray("results")?.let { arr ->
                    (0 until arr.length()).mapNotNull { i ->
                        arr.optJSONObject(i)?.let(Item::from)
                    }
                }.orEmpty(),
            )
        }
    }

    data class Page(
        val items: List<Run>,
        val total: Int,
        val offset: Int,
        val limit: Int,
    ) {
        companion object {
            fun from(o: JSONObject): Page = Page(
                items = o.optJSONArray("items")?.let { arr ->
                    (0 until arr.length()).mapNotNull { i ->
                        arr.optJSONObject(i)?.let(Run::from)
                    }
                }.orEmpty(),
                total = o.optInt("total"),
                offset = o.optInt("offset"),
                limit = o.optInt("limit"),
            )
        }
    }

    data class Overview(
        val records: Page,
        val retentionDays: Int,
        val maxRecords: Int,
        val latest: Map<String, Run>,
    )

    sealed class Outcome<out T> {
        data class Ok<T>(val value: T) : Outcome<T>()
        data class Err(val message: String) : Outcome<Nothing>()
    }

    // 传入已取得的 Reply 做纯解析，便于单测钉形状（无需真的起内核）。
    fun <T> parseReply(reply: CoreTransport.Reply, block: (JSONObject) -> T?): Outcome<T> {
        val ok = reply as? CoreTransport.Reply.Ok
            ?: return Outcome.Err((reply as? CoreTransport.Reply.Down)?.message ?: "链路不可达")
        val obj = runCatching { JSONObject(ok.text) }.getOrNull()
        // 管理 API 的错误信封是「真实 HTTP 状态码 + body {success:false,error}」，
        // 文案（如「未知的维护任务」）比 `HTTP 400` 有用得多：非 2xx 时也先读 body，
        // 只有读不到才回落状态码。body 不是 JSON 的场合同样回落到 HTTP 状态。
        if (ok.status < 200 || ok.status >= 300) {
            val message = obj?.optString("error").orEmpty().ifBlank { "HTTP ${ok.status}" }
            return Outcome.Err(message)
        }
        if (obj == null) return Outcome.Err("响应不是合法 JSON")
        if (!obj.optBoolean("success", false)) {
            return Outcome.Err(obj.optString("error").ifBlank { "请求失败" })
        }
        val data = obj.optJSONObject("data") ?: return Outcome.Err("响应缺少 data")
        val value = block(data) ?: return Outcome.Err("响应 data 形状不符")
        return Outcome.Ok(value)
    }

    private fun buildQuery(
        taskId: String?,
        source: String?,
        status: String?,
        keyword: String?,
        offset: Int,
        limit: Int,
    ): String {
        val parts = mutableListOf<String>()
        fun add(key: String, value: String?) {
            if (!value.isNullOrEmpty()) {
                parts += "$key=${java.net.URLEncoder.encode(value, "UTF-8")}"
            }
        }
        add("taskId", taskId)
        add("source", source)
        add("status", status)
        add("keyword", keyword)
        if (offset > 0) parts += "offset=$offset"
        if (limit > 0) parts += "limit=$limit"
        return if (parts.isEmpty()) "" else "?" + parts.joinToString("&")
    }

    /** 概览（筛选可选；status 取 failed/success，source 取 automatic/manual）。 */
    suspend fun overview(
        taskId: String? = null,
        source: String? = null,
        status: String? = null,
        keyword: String? = null,
        offset: Int = 0,
        limit: Int = 20,
    ): Outcome<Overview> = withContext(Dispatchers.IO) {
        val query = buildQuery(taskId, source, status, keyword, offset, limit)
        val reply = CoreTransport.get("/api/maintenance$query")
        parseReply(reply) { data ->
            val page = Page.from(data.optJSONObject("records") ?: return@parseReply null)
            val latest = HashMap<String, Run>()
            data.optJSONObject("latest")?.let { obj ->
                val keys = obj.keys()
                while (keys.hasNext()) {
                    val key = keys.next()
                    obj.optJSONObject(key)?.let { latest[key] = Run.from(it) }
                }
            }
            Overview(
                records = page,
                retentionDays = data.optInt("retentionDays", 30),
                maxRecords = data.optInt("maxRecords", 2000),
                latest = latest,
            )
        }
    }

    /** 单次详情（手动跑后轮询此接口拿终态）。 */
    suspend fun detail(id: String): Outcome<Detail> = withContext(Dispatchers.IO) {
        val reply = CoreTransport.get("/api/maintenance/records/$id")
        parseReply(reply, Detail::from)
    }

    /** 手动跑（返回 running 起始详情；随后用 detail(id) 轮询）。 */
    suspend fun run(taskId: String): Outcome<Detail> = withContext(Dispatchers.IO) {
        val reply = CoreTransport.post("/api/maintenance/run/$taskId", "{}")
        parseReply(reply, Detail::from)
    }

    /** 重试失败项；itemId 为空表示重试该次全部失败项。 */
    suspend fun retry(runId: String, itemId: String? = null): Outcome<Detail> =
        withContext(Dispatchers.IO) {
            val body = if (itemId.isNullOrEmpty()) {
                "{}"
            } else {
                JSONObject().put("itemId", itemId).toString()
            }
            val reply = CoreTransport.post("/api/maintenance/records/$runId/retry", body)
            parseReply(reply, Detail::from)
        }
}
