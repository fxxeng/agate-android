package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/**
 * 数据报表数据层 —— 「数据报表」页。
 *
 * 内核路由（`agate-server/src/server/api/stats_api.rs` 模块头）：
 * - `GET /api/stats/summary?range=&startDate=&endDate=`  一次给全报表
 *
 * ── 为什么一个端点全包 ───────────────────────────────────────
 * 页面上九个板块（概览 / 趋势 / 热力图 / 命中率 / 模型占比 / 三张排行）
 * 用的是**同一份聚合结果**，拆成多个端点会让各板块的"同一个数"来自
 * 不同时刻的快照：`overview.requests` 与三张排行的合计本就设计成可对账
 * （见 `request_stats::aggregate::summarize` 的注释：各维求和的等于总量），
 * 分开请求就会让这个恒等式在页面上看起来不成立。
 *
 * ── range 只有六个合法值 ────────────────────────────────────
 * `today` / `7` / `30` / `month` / `all` / `custom`（与内核 `RANGES` 逐字一致）。
 * 注意 `7` / `30` 是**字符串形式的数字**，不是 `7d`；传错内核直接 400
 * （报表区间是用户显式选的，静默改成别的区间比报错更难排查）。
 * `custom` 另需 `startDate` / `endDate`（`yyyy-MM-dd`，本地自然日）。
 */
object ReportOps {

    /** 时间档位（`value` 直接作为 `range` 查询参数）。 */
    enum class Range(val value: String, val label: String) {
        TODAY("today", "今日"),
        WEEK("7", "近 7 天"),
        MONTH("30", "近 30 天"),
        THIS_MONTH("month", "本月"),
        ALL("all", "全部"),
        CUSTOM("custom", "自定义"),
        ;

        /** 需要额外传起止日的档位。 */
        val isCustom: Boolean get() = this == CUSTOM
    }

    /** 概览：区间总量 + 冠军模型 + 连续天数。 */
    class Overview(
        val requests: Long,
        val successful: Long,
        val tokens: Long,
        val activeDays: Long,
        val streak: Long,
    ) {
        val failures: Long get() = (requests - successful).coerceAtLeast(0)

        /** 成功率；无请求时给 0（不显示 NaN，与内核 `safe_rate` 同一取向）。 */
        val successRate: Double
            get() = if (requests > 0) successful.toDouble() / requests else 0.0
    }

    /** 冠军模型（内核给 `null` = 区间内没有可用模型）。 */
    class TopModel(
        val model: String,
        val tokens: Long,
        val requests: Long,
        val percentage: Double,
    )

    /** 一行排行（providers / accounts 共形；models 少了 success/failures）。 */
    class RankRow(
        val id: String,
        val label: String,
        val requests: Long,
        val success: Long,
        val failures: Long,
        val tokens: Long,
    )

    /** 模型维度（`models` 数组，无 success/failures —— 内核就没给）。 */
    class ModelRow(
        val model: String,
        val label: String,
        val requests: Long,
        val tokens: Long,
    )

    /** 趋势图中的一天（缺失日期由内核补零，所以序列长度 == 区间天数）。 */
    class TrendDay(
        val date: String,
        val tokens: Long,
        val requests: Long,
        val models: List<ModelSlice>,
    )

    /** 热力图的一格（固定 365 天，含今天）。 */
    class HeatCell(val date: String, val requests: Long, val tokens: Long)

    /** 某一档窗口的缓存命中率。 */
    class CacheRate(val hitTokens: Long, val inputTokens: Long, val rate: Double)

    /** 近 24 整点的一格。 */
    class CacheHour(
        val hour: String,
        val hitTokens: Long,
        val inputTokens: Long,
        val totalTokens: Long,
        val rate: Double,
        val models: List<ModelSlice>,
    )

    /** 模型用量切片（趋势图与逐小时图的悬停读数用）。 */
    class ModelSlice(
        val model: String,
        val label: String,
        val tokens: Long,
        val requests: Long,
    )

    /** 一次报表查询的完整结果。 */
    class Summary(
        val range: String,
        val startDate: String,
        val endDate: String,
        val overview: Overview,
        val topModel: TopModel?,
        val providers: List<RankRow>,
        val accounts: List<RankRow>,
        val models: List<ModelRow>,
        val trend: List<TrendDay>,
        val heatmap: List<HeatCell>,
        /**
         * 四档命中率，键是内核的窗口名：`last10m` / `last1h` / `last24h` / `last7d`。
         *
         * 保持为 Map 而不是四个字段：窗口清单在内核 `CACHE_WINDOWS` 里，
         * 将来加一档不该逼客户端改结构（多一档就多一个键，渲染层按顺序取）。
         */
        val cacheRates: Map<String, CacheRate>,
        val cacheTrend: List<CacheHour>,
    )

    suspend fun summary(
        range: Range = Range.WEEK,
        startDate: String? = null,
        endDate: String? = null,
    ): OpResult<Summary> {
        val params = ArrayList<String>()
        params += "range=${range.value}"
        // 只有 custom 才读这两项。非 custom 档传了内核也不用（见 `summarize` 的
        // `custom` 参数注释），这里干脆不传，免得让人以为切档时要跟着发。
        if (range.isCustom) {
            startDate?.takeIf { it.isNotBlank() }?.let { params += "startDate=$it" }
            endDate?.takeIf { it.isNotBlank() }?.let { params += "endDate=$it" }
        }
        val data = get("/api/stats/summary?" + params.joinToString("&"))
            ?: return OpResult(false, null, "无法连接内核")
        return OpResult(true, parse(data), null)
    }

    /** 解出整份报表；任何一块缺失都退化成空，不让整页失败。 */
    fun parse(data: JSONObject): Summary {
        val overview = data.optJSONObject("overview")
        return Summary(
            range = data.optString("range"),
            startDate = data.optString("startDate"),
            endDate = data.optString("endDate"),
            overview = Overview(
                requests = overview?.optLong("requests") ?: 0L,
                successful = overview?.optLong("successful") ?: 0L,
                tokens = overview?.optLong("tokens") ?: 0L,
                activeDays = overview?.optLong("activeDays") ?: 0L,
                streak = overview?.optLong("streak") ?: 0L,
            ),
            // 冠军模型在 `overview.topModel` **里面**（不是顶层）；区间内没有
            // 可用模型时内核给 `Value::Null` —— optJSONObject 对 null 返回 null，
            // 正好落到「没有冠军」。
            topModel = overview?.optJSONObject("topModel")?.let {
                TopModel(
                    model = it.optString("model"),
                    tokens = it.optLong("tokens"),
                    requests = it.optLong("requests"),
                    percentage = it.optDouble("percentage", 0.0),
                )
            },
            providers = rankRows(data.optJSONArray("providers")),
            accounts = rankRows(data.optJSONArray("accounts")),
            models = modelRows(data.optJSONArray("models")),
            trend = array(data.optJSONArray("dailyTrend")).map { day ->
                TrendDay(
                    date = day.optString("date"),
                    tokens = day.optLong("tokens"),
                    requests = day.optLong("requests"),
                    models = modelSlices(day.optJSONArray("models")),
                )
            },
            heatmap = array(data.optJSONArray("heatmap")).map { cell ->
                HeatCell(
                    date = cell.optString("date"),
                    requests = cell.optLong("requests"),
                    tokens = cell.optLong("tokens"),
                )
            },
            cacheRates = cacheRates(data.optJSONObject("cacheRates")),
            cacheTrend = array(data.optJSONArray("cacheTrend24h")).map { hour ->
                CacheHour(
                    hour = hour.optString("hour"),
                    hitTokens = hour.optLong("hitTokens"),
                    inputTokens = hour.optLong("inputTokens"),
                    totalTokens = hour.optLong("totalTokens"),
                    rate = hour.optDouble("rate", 0.0),
                    models = modelSlices(hour.optJSONArray("models")),
                )
            },
        )
    }

    private fun array(arr: JSONArray?): List<JSONObject> {
        if (arr == null) return emptyList()
        return (0 until arr.length()).mapNotNull { arr.optJSONObject(it) }
    }

    private fun rankRows(arr: JSONArray?): List<RankRow> = array(arr).mapNotNull { o ->
        val id = o.optString("id")
        val label = o.optString("label")
        if (id.isBlank() && label.isBlank()) return@mapNotNull null
        RankRow(
            id = id,
            label = label.ifBlank { id },
            requests = o.optLong("requests"),
            success = o.optLong("success"),
            failures = o.optLong("failures"),
            tokens = o.optLong("totalTokens"),
        )
    }

    private fun modelRows(arr: JSONArray?): List<ModelRow> = array(arr).mapNotNull { o ->
        val model = o.optString("model")
        val label = o.optString("label")
        if (model.isBlank() && label.isBlank()) return@mapNotNull null
        ModelRow(
            model = model,
            label = label.ifBlank { model },
            requests = o.optLong("requests"),
            tokens = o.optLong("totalTokens"),
        )
    }

    private fun modelSlices(arr: JSONArray?): List<ModelSlice> = array(arr).map { o ->
        ModelSlice(
            model = o.optString("model"),
            label = o.optString("label").ifBlank { o.optString("model") },
            tokens = o.optLong("tokens"),
            requests = o.optLong("requests"),
        )
    }

    private fun cacheRates(o: JSONObject?): Map<String, CacheRate> {
        if (o == null) return emptyMap()
        val out = LinkedHashMap<String, CacheRate>()
        val keys = o.keys()
        while (keys.hasNext()) {
            val key = keys.next()
            val slot = o.optJSONObject(key) ?: continue
            out[key] = CacheRate(
                hitTokens = slot.optLong("hitTokens"),
                inputTokens = slot.optLong("inputTokens"),
                rate = slot.optDouble("rate", 0.0),
            )
        }
        return out
    }

    private suspend fun get(path: String): JSONObject? {
        val reply = withContext(Dispatchers.IO) { CoreTransport.get(path) }
        if (reply !is CoreTransport.Reply.Ok) return null
        val body = runCatching { JSONObject(reply.text) }.getOrNull() ?: return null
        if (!body.optBoolean("success")) return null
        return body.optJSONObject("data") ?: body
    }

    class OpResult<T>(val ok: Boolean, val data: T?, val error: String?)
}
