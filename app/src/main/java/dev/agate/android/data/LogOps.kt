package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/**
 * 应用日志（运行日志）数据层 —— 「应用日志」页。
 *
 * 内核路由（`agate-server/src/server/api/logs_api.rs` 模块头）：
 * - `GET    /api/logs`           查询（limit / level / category / keyword / sinceId / start / end）
 * - `GET    /api/logs/stats`     各级别 / 分类计数（导航徽标、筛选下拉）
 * - `GET    /api/logs/download`  导出 JSONL 附件
 * - `DELETE /api/logs`           清空（带筛选只删命中项，全清须显式 all=1）
 * - `GET    /api/logs/crash`     读（并清除）上一次运行的崩溃痕迹
 * - `DELETE /api/logs/crash`     丢弃痕迹（用户点掉提示条）
 *
 * 与请求日志（`/api/stats/requests`）的分工：**这里记 App 自己的运行事件**
 * （账号状态、签到、凭证刷新、换号 / 熔断、网关启停），模型调用明细在「调用记录」。
 *
 * ── 级别是「下限」而不是「等于」────────────────────────────
 * 内核 `LEVELS` 按严重度升序（debug < info < warn < error），传 `level=warn`
 * 拿到的是 warn 与 error。界面上「警告 / 错误 / 中断」三档按钮同理。
 */
object LogOps {

    /** 级别（与内核 `logs_store::LEVELS` 逐字对齐）。 */
    enum class Level(val value: String?, val label: String) {
        ALL(null, "全部级别"),
        DEBUG("debug", "调试"),
        INFO("info", "信息"),
        WARN("warn", "警告"),
        ERROR("error", "错误"),
        ;
    }

    /** 一条日志（对应内核 `LogEntry` 的序列化形状）。 */
    class Entry(o: JSONObject) {
        val id: Long = o.optLong("id")
        val ts: Long = o.optLong("ts")
        val level: String = o.optString("level").ifBlank { "info" }
        val category: String = o.optString("category").ifBlank { "server" }
        val message: String = o.optString("message")

        /** 结构化附加字段（如 429 换号的 from / to）；没有时为空。 */
        val data: JSONObject? = o.optJSONObject("data")
    }

    /** 查询结果。 */
    class Page(
        val entries: List<Entry>,
        val total: Long,
        val matched: Long,
    )

    /** 各级别 / 分类计数（筛选 Tab 上的数字）。 */
    class Stats(
        val total: Long,
        val max: Long,
        val byLevel: Map<String, Long>,
        val byCategory: Map<String, Long>,
        val lastId: Long,
    )

    /** 上一次运行的崩溃痕迹。 */
    class Crash(val at: Long, val message: String, val location: String, val thread: String)

    suspend fun page(
        level: Level = Level.ALL,
        category: String? = null,
        keyword: String? = null,
        start: Long? = null,
        end: Long? = null,
        sinceId: Long? = null,
        limit: Int = 200,
    ): OpResult<Page> {
        val params = ArrayList<String>()
        level.value?.let { params += "level=$it" }
        category?.takeIf { it.isNotBlank() }?.let { params += "category=${it.enc()}" }
        keyword?.takeIf { it.isNotBlank() }?.let { params += "keyword=${it.enc()}" }
        start?.let { params += "start=$it" }
        end?.let { params += "end=$it" }
        sinceId?.let { params += "sinceId=$it" }
        params += "limit=$limit"
        val data = get("/api/logs?" + params.joinToString("&")) ?: return OpResult(false, null, "无法连接内核")
        val arr = data.optJSONArray("entries") ?: JSONArray()
        val list = (0 until arr.length()).mapNotNull { arr.optJSONObject(it)?.let(::Entry) }
        return OpResult(
            true,
            Page(list, data.optLong("total"), data.optLong("matched")),
            null,
        )
    }

    suspend fun stats(): OpResult<Stats> {
        val data = get("/api/logs/stats") ?: return OpResult(false, null, "无法连接内核")
        fun mapOf(key: String): Map<String, Long> {
            val o = data.optJSONObject(key) ?: return emptyMap()
            val out = LinkedHashMap<String, Long>()
            val keys = o.keys()
            while (keys.hasNext()) {
                val k = keys.next()
                out[k] = o.optLong(k)
            }
            return out
        }
        return OpResult(
            true,
            Stats(
                total = data.optLong("total"),
                max = data.optLong("max"),
                byLevel = mapOf("byLevel"),
                byCategory = mapOf("byCategory"),
                lastId = data.optLong("lastId"),
            ),
            null,
        )
    }

    /** 导出整份日志为 JSONL 文本。 */
    suspend fun download(): OpResult<String> {
        val reply = withContext(Dispatchers.IO) { CoreTransport.get("/api/logs/download") }
        if (reply !is CoreTransport.Reply.Ok) {
            return OpResult(false, null, (reply as? CoreTransport.Reply.Down)?.message ?: "链路错误")
        }
        if (reply.status !in 200..299) return OpResult(false, null, "导出失败")
        return OpResult(true, reply.text, null)
    }

    /**
     * 清空。`all=true` 时清空全部（对应内核的显式护栏 `all=1`）；
     * 否则按 [level] / [category] / [keyword] 只删命中项。
     */
    suspend fun clear(
        all: Boolean,
        level: Level = Level.ALL,
        category: String? = null,
        keyword: String? = null,
    ): OpResult<Long> {
        val params = ArrayList<String>()
        if (all) params += "all=1"
        level.value?.let { params += "level=$it" }
        category?.takeIf { it.isNotBlank() }?.let { params += "category=${it.enc()}" }
        keyword?.takeIf { it.isNotBlank() }?.let { params += "keyword=${it.enc()}" }
        val reply = withContext(Dispatchers.IO) {
            CoreTransport.call("DELETE", "/api/logs?" + params.joinToString("&"), null)
        }
        if (reply !is CoreTransport.Reply.Ok) {
            return OpResult(false, null, (reply as? CoreTransport.Reply.Down)?.message ?: "链路错误")
        }
        val body = runCatching { JSONObject(reply.text) }.getOrNull()
        if (reply.status !in 200..299) {
            return OpResult(false, null, body?.optString("error").orEmpty().ifBlank { "清空失败" })
        }
        // 响应是 `{success, data:{…统计, removed}}`：清空后内核顺手回了最新统计，
        // `removed` 就压在 `data` 里 —— 直接读顶层会永远拿到 0，界面上表现为
        // 「清空成功」却说删了 0 条。
        val data = body?.optJSONObject("data") ?: body
        return OpResult(true, data?.optLong("removed", 0L) ?: 0L, null)
    }

    /**
     * 读出（并清除）上一次运行的崩溃痕迹。
     *
     * 内核侧 `core::crash_marker` 的判据是**真的发生了 panic**（不是「进程没干净
     * 退出」）—— 在 Android 上后者会把每一次正常的后台回收都误报成崩溃。
     */
    suspend fun takeCrash(): OpResult<Crash?> {
        val data = get("/api/logs/crash") ?: return OpResult(false, null, "无法连接内核")
        if (!data.optBoolean("crashed", false)) return OpResult(true, null, null)
        return OpResult(
            true,
            Crash(
                at = data.optLong("at"),
                message = data.optString("message"),
                location = data.optString("location"),
                thread = data.optString("thread"),
            ),
            null,
        )
    }

    /** 丢弃崩溃痕迹（用户点掉提示条）。 */
    suspend fun dropCrash(): OpResult<Boolean> {
        val reply = withContext(Dispatchers.IO) {
            CoreTransport.call("DELETE", "/api/logs/crash", null)
        }
        if (reply !is CoreTransport.Reply.Ok) {
            return OpResult(false, null, (reply as? CoreTransport.Reply.Down)?.message ?: "链路错误")
        }
        return OpResult(reply.status in 200..299, true, null)
    }

    private suspend fun get(path: String): JSONObject? {
        val reply = withContext(Dispatchers.IO) { CoreTransport.get(path) }
        if (reply !is CoreTransport.Reply.Ok) return null
        val body = runCatching { JSONObject(reply.text) }.getOrNull() ?: return null
        if (!body.optBoolean("success")) return null
        return body.optJSONObject("data") ?: body
    }

    private fun String.enc(): String = java.net.URLEncoder.encode(this, "UTF-8")

    class OpResult<T>(val ok: Boolean, val data: T?, val error: String?)
}
