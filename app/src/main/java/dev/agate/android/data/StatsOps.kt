package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/**
 * 调用记录（请求日志）数据层 —— 「调用记录」页与其下钻的「请求详情」页。
 *
 * 内核路由（`agate-server/src/server/api/stats_api.rs` 模块头）：
 * - `GET    /api/stats/requests`                 分页 + 模型 / 提供商 / 状态 / 时间过滤
 * - `GET    /api/stats/requests/filters`         筛选下拉的候选清单（出现过的模型 / 提供商）
 * - `GET    /api/stats/requests/raw?id=`         单条请求的原始正文
 * - `GET    /api/stats/requests/clear-preview`    清理弹窗的预览统计
 * - `DELETE /api/stats/requests`                  mode=raw 只删正文 / mode=all 删明细
 * - `POST   /api/stats/requests/compact`          压缩数据库（后台）
 * - `POST   /api/stats/requests/terminate?id=`    终止在途请求
 *
 * ── 为什么筛选候选不自己攒 ───────────────────────────────────
 * 界面只显示「日志里出现过的」模型 / 提供商，这份清单由 `/filters` 端点给出
 * （一次全表 GROUP BY）。前端若自己从当前页的行里去重，翻页后候选项会跟着变，
 * 第 2 页有、第 1 页没的模型会时有时无。
 *
 * ── 为什么 `status` 只认三个值 ───────────────────────────────
 * 存储层 `report::normalize_status_filter` 只接受 `ok` / `error` / `running`，
 * 其余当作没筛。这里沿用同一口径：非法值不发这个参数，而不是发个错的。
 */
object StatsOps {

    /** 一条请求明细（对应内核 `RequestEntry` 的序列化形状）。 */
    class Entry(o: JSONObject) {
        val id: String = o.optString("id")
        val ts: Long = o.optLong("ts")
        val model: String = o.optString("model")
        val provider: String = o.optString("provider")
        val providerLabel: String = o.optString("providerLabel").ifBlank { provider }
        val accountName: String = o.optString("accountName")
        val status: Long = o.optLong("status")
        val durationMs: Long = o.optLong("durationMs")
        val firstResponseMs: Long = o.optLong("firstResponseMs", -1L)
        val attempts: Long = o.optLong("attempts", 1L)
        val error: String? = o.optString("error").takeIf { it.isNotBlank() }
        val promptTokens: Long = o.optLong("promptTokens")
        val completionTokens: Long = o.optLong("completionTokens")
        val totalTokens: Long = o.optLong("totalTokens")
        val cacheReadTokens: Long = o.optLong("cacheReadTokens")
        val clientModel: String = o.optString("clientModel")
        val upstreamModel: String = o.optString("upstreamModel")
        val clientReasoning: String = o.optString("clientReasoning")
        val upstreamReasoning: String = o.optString("upstreamReasoning")
        val phase: String = o.optString("phase")
        val phaseElapsedMs: Long = o.optLong("phaseElapsedMs", -1L)
        val isTest: Boolean = o.optBoolean("isTest", false)
        val client: String = o.optString("client")

        // 尝试明细**自身不带编号**：`AttemptDetail` 只有 provider / account /
        // status / error / retries / notice / bodyBytes，序号只能由数组下标给
        // （第 i 项 = 第 i 次尝试），否则界面上每一行都会印着同一个 0。
        val attemptDetails: List<Attempt> =
            o.optJSONArray("attemptDetails")?.let { arr ->
                (0 until arr.length()).mapNotNull { i ->
                    arr.optJSONObject(i)?.let { Attempt(it, (i + 1).toLong()) }
                }
            } ?: emptyList()

        val sensitiveHits: List<SensitiveHit> =
            o.optJSONArray("sensitiveHits")?.let { arr ->
                (0 until arr.length()).mapNotNull { arr.optJSONObject(it)?.let(::SensitiveHit) }
            } ?: emptyList()

        /** 在途（status=0 = 还没收尾）。 */
        val running: Boolean get() = status == 0L

        /** 成功：2xx（内核把 HTTP 状态码原样落库）。 */
        val ok: Boolean get() = status in 200..299

        /** 是否已采集原始报文（未开启调试模式时为空串 → 详情不给入口）。 */
        val hasRaw: Boolean get() = id.isNotBlank()
    }

    /**
     * 一次上游尝试（`AttemptDetail`）。
     *
     * 字段名逐一对齐内核：`account` 承载账号展示名（`accountName` 是主记录上的，
     * 这里是另一个名字）、`notice` 是这一轮的提示（目前只有「代理回退直连」），
     * 两者照 A 链条的命名习惯推会写错。
     */
    class Attempt(o: JSONObject, val index: Long = 0) {
        val provider: String = o.optString("provider")
        val account: String = o.optString("account")
        /** HTTP 状态码；-1 = 这轮没走到有状态码那一步（传输层失败或未定论）。 */
        val status: Long = o.optLong("status", -1L)
        // 不用 `ifBlank { null }`：optString 返回非空串，ifBlank 的返回类型
        // 会被推成 String，塞 null 直接编译不过。takeIf 才拿得到 String?
        val error: String? = o.optString("error").takeIf { it.isNotBlank() }
        val notice: String? = o.optString("notice").takeIf { it.isNotBlank() }
        /** 这一轮内部又退避重试了几次（不 0 值得在详情里点明）。 */
        val retryCount: Int = o.optJSONArray("retries")?.length() ?: 0

        val ok: Boolean get() = status in 200..299
    }

    /** 命中的敏感词（`SensitiveHit`）。 */
    class SensitiveHit(o: JSONObject) {
        val word: String = o.optString("word")
        val count: Long = o.optLong("count")
    }

    /** 列表一页的结果。 */
    class Page(
        val entries: List<Entry>,
        val total: Long,
        val matched: Long,
        val running: Long,
    )

    /** 筛选下拉的候选清单。 */
    class Filters(
        val models: List<String>,
        val providers: List<ProviderOption>,
    )

    class ProviderOption(val id: String, val label: String)

    /** 清理弹窗的预览统计。 */
    class ClearPreview(
        val all: Long,
        val raw: Long,
        val dbBytes: Long,
        val vacuumRunning: Boolean,
    )

    /** 单条请求的原始正文。 */
    class RawBody(
        val requestBody: String,
        val responseBody: String,
        val truncated: Boolean,
    )

    /** 请求状态筛选（与内核 `normalize_status_filter` 的白名单逐字对齐）。 */
    enum class StatusFilter(val value: String?) {
        ALL(null),
        OK("ok"),
        ERROR("error"),
        RUNNING("running"),
    }

    suspend fun page(
        status: StatusFilter = StatusFilter.ALL,
        provider: String? = null,
        model: String? = null,
        start: Long? = null,
        end: Long? = null,
        limit: Int = 50,
        offset: Int = 0,
    ): OpResult<Page> {
        val params = ArrayList<String>()
        status.value?.let { params += "status=$it" }
        provider?.takeIf { it.isNotBlank() }?.let { params += "provider=${it.enc()}" }
        model?.takeIf { it.isNotBlank() }?.let { params += "model=${it.enc()}" }
        start?.let { params += "start=$it" }
        end?.let { params += "end=$it" }
        params += "limit=$limit"
        params += "offset=$offset"
        val data = get("/api/stats/requests?" + params.joinToString("&")) ?: return OpResult(false, null, "无法连接内核")
        val entries = data.optJSONArray("entries") ?: JSONArray()
        val list = (0 until entries.length()).mapNotNull { entries.optJSONObject(it)?.let(::Entry) }
        return OpResult(
            ok = true,
            data = Page(
                entries = list,
                total = data.optLong("total"),
                matched = data.optLong("matched"),
                running = data.optLong("running"),
            ),
            error = null,
        )
    }

    suspend fun filters(): OpResult<Filters> {
        val data = get("/api/stats/requests/filters") ?: return OpResult(false, null, "无法连接内核")
        val models = data.optJSONArray("models")?.let { arr ->
            (0 until arr.length()).mapNotNull { arr.optString(it).takeIf(String::isNotBlank) }
        } ?: emptyList()
        val providers = data.optJSONArray("providers")?.let { arr ->
            (0 until arr.length()).mapNotNull { arr.optJSONObject(it) }
                .map { ProviderOption(it.optString("id"), it.optString("label").ifBlank { it.optString("id") }) }
        } ?: emptyList()
        return OpResult(true, Filters(models, providers), null)
    }

    suspend fun raw(id: String): OpResult<RawBody> {
        val data = get("/api/stats/requests/raw?id=${id.enc()}") ?: return OpResult(false, null, "无法连接内核")
        return OpResult(
            true,
            RawBody(
                requestBody = data.optString("requestBody"),
                responseBody = data.optString("responseBody"),
                truncated = data.optBoolean("truncated", false),
            ),
            null,
        )
    }

    suspend fun clearPreview(
        status: StatusFilter = StatusFilter.ALL,
        provider: String? = null,
        model: String? = null,
        start: Long? = null,
        end: Long? = null,
    ): OpResult<ClearPreview> {
        val params = ArrayList<String>()
        status.value?.let { params += "status=$it" }
        provider?.takeIf { it.isNotBlank() }?.let { params += "provider=${it.enc()}" }
        model?.takeIf { it.isNotBlank() }?.let { params += "model=${it.enc()}" }
        start?.let { params += "start=$it" }
        end?.let { params += "end=$it" }
        val suffix = if (params.isEmpty()) "" else "?" + params.joinToString("&")
        val data = get("/api/stats/requests/clear-preview" + suffix)
            ?: return OpResult(false, null, "无法连接内核")
        return OpResult(
            true,
            ClearPreview(
                all = data.optLong("all"),
                raw = data.optLong("raw"),
                dbBytes = data.optLong("dbBytes"),
                vacuumRunning = data.optBoolean("vacuumRunning", false),
            ),
            null,
        )
    }

    /** 清理结果：`deleted` 是实际删除条数，`note` 是内核附带的口径说明（部分模式才有）。 */
    class ClearOutcome(val deleted: Long, val note: String?)

    /**
     * `mode` = `raw`（只抹正文）/ `all`（删明细，默认）。
     *
     * ── `all=1` 护栏：不是绕过，是把护栏挪到 UI 上 ──────────────
     * 内核要求「不带任何筛选条件时必须显式带 `all=1`」，否则 400（见
     * `stats_api::clear_stats_requests`：两种 mode 都对实体生效）。这道护栏防的是
     * 「随手一个 DELETE 就清光」，而不是防 UI —— UI 侧已经有一道确认弹层把
     * 「这会清空全部」讲清楚了。用户点过确认之后替他带上 `all=1`，护栏的意图
     * 仍然成立（确认的就是那一层）；反过来若始终不带，用户每次想清空都只会看到
     * 一个莫名其妙的 400，护栏就变成单纯的坏体验。
     */
    suspend fun clear(
        mode: String = "all",
        status: StatusFilter = StatusFilter.ALL,
        provider: String? = null,
        model: String? = null,
        start: Long? = null,
        end: Long? = null,
    ): OpResult<ClearOutcome> {
        val params = ArrayList<String>()
        params += "mode=$mode"
        status.value?.let { params += "status=$it" }
        provider?.takeIf { it.isNotBlank() }?.let { params += "provider=${it.enc()}" }
        model?.takeIf { it.isNotBlank() }?.let { params += "model=${it.enc()}" }
        start?.let { params += "start=$it" }
        end?.let { params += "end=$it" }
        val hasFilter = status.value != null ||
            !provider.isNullOrBlank() ||
            !model.isNullOrBlank() ||
            start != null ||
            end != null
        if (!hasFilter) params += "all=1"
        val reply = withContext(Dispatchers.IO) {
            CoreTransport.call("DELETE", "/api/stats/requests?" + params.joinToString("&"), null)
        }
        if (reply !is CoreTransport.Reply.Ok) {
            return OpResult(false, null, (reply as? CoreTransport.Reply.Down)?.message ?: "链路错误")
        }
        val body = runCatching { JSONObject(reply.text) }.getOrNull()
        if (reply.status !in 200..299) {
            return OpResult(false, null, body?.optString("error").orEmpty().ifBlank { "清理失败" })
        }
        // 管理 API 统一包一层 `{success, data}`，真正的结果在 `data` 里
        // （`{success, mode, deleted, note?}`）—— 直接读顶层会永远拿到 0。
        val data = body?.optJSONObject("data") ?: body
        return OpResult(
            true,
            ClearOutcome(
                deleted = data?.optLong("deleted", 0L) ?: 0L,
                note = data?.optString("note")?.takeIf { it.isNotBlank() },
            ),
            null,
        )
    }

    /** 压缩数据库（后台执行）；同一时间只允许一个，重复触发内核回 409。 */
    suspend fun compact(): OpResult<Boolean> {
        val reply = withContext(Dispatchers.IO) {
            CoreTransport.call("POST", "/api/stats/requests/compact", "{}")
        }
        if (reply !is CoreTransport.Reply.Ok) {
            return OpResult(false, null, (reply as? CoreTransport.Reply.Down)?.message ?: "链路错误")
        }
        if (reply.status == 409) {
            return OpResult(false, null, "数据库压缩正在进行中，请稍后再试")
        }
        val ok = reply.status in 200..299
        val body = runCatching { JSONObject(reply.text) }.getOrNull()
        return OpResult(
            ok,
            true,
            if (ok) null else body?.optString("error").orEmpty().ifBlank { "压缩失败" },
        )
    }

    /** 终止一条在途请求；成功后转发链自己会把明细写成 408「请求已被手动终止」。 */
    suspend fun terminate(id: String): OpResult<Boolean> {
        val reply = withContext(Dispatchers.IO) {
            CoreTransport.call("POST", "/api/stats/requests/terminate?id=${id.enc()}", "{}")
        }
        if (reply !is CoreTransport.Reply.Ok) {
            return OpResult(false, null, (reply as? CoreTransport.Reply.Down)?.message ?: "链路错误")
        }
        if (reply.status == 404) {
            return OpResult(false, null, "这条请求已不在进行中（可能已结束，或来自上次启动遗留的行）")
        }
        val ok = reply.status in 200..299
        val body = runCatching { JSONObject(reply.text) }.getOrNull()
        return OpResult(
            ok,
            true,
            if (ok) null else body?.optString("error").orEmpty().ifBlank { "终止失败" },
        )
    }

    // P2：经 CoreTransport 内核直调；错误信封随 status 一并透传，不吞 4xx/5xx。
    private suspend fun get(path: String): JSONObject? {
        val reply = withContext(Dispatchers.IO) { CoreTransport.get(path) }
        if (reply !is CoreTransport.Reply.Ok) return null
        val body = runCatching { JSONObject(reply.text) }.getOrNull() ?: return null
        if (!body.optBoolean("success")) return null
        return body.optJSONObject("data") ?: body
    }

    /** URL 参数编码；内核按原样收，不编会截断带空格的名字。 */
    private fun String.enc(): String = java.net.URLEncoder.encode(this, "UTF-8")

    class OpResult<T>(val ok: Boolean, val data: T?, val error: String?)
}
