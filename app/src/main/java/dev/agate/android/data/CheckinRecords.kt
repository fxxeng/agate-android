package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

/**
 * 签到记录明细台账（任务中心 · 签到记录页）。
 *
 * ```text
 *   GET    /api/checkin-records   查询（?limit=&offset=&outcome=&provider=&keyword=&start=&end=）
 *   DELETE /api/checkin-records   删除（带筛选 = 只删命中的；全清必须显式 all=1）
 * ```
 *
 * ── 这一页为什么是「每账号一行」而不是「每轮一行」────────────
 * 上游 3.0.8 已主动淘汰轮次台账（`checkin-page.tsx` 文件头：「原『最近签到记录』
 * 时间线已迁出，记录长成了明细」）。轮次台账回答不了「浣熊-C 上周三到底失败在哪」，
 * 明细能。轮次视角仍然存在，但降为签到中心总览卡上的一个读数。
 *
 * ── outcome 只有四个值 ──────────────────────────────────────
 * `success` / `already` / `pending` / `failed`。**判据只在内核一处**（落账时判），
 * 界面只按 outcome 上色，绝不自己再判一遍 —— 两套判据迟早会给出两套颜色。
 *
 * ── 清空的护栏 ──────────────────────────────────────────────
 * 不带任何筛选的空请求会被内核 400（要求显式 `all=1`）。这条护栏来自真实的
 * 事故：筛选清空漏传参数曾把全部日志删掉。所以本层把两种删除拆成两个函数，
 * 调用方必须显式选一个 —— 不给「顺手删光」留默认值。
 */
object CheckinRecords {

    /** 结果四值（与内核 `checkin_records::OUTCOMES` 逐字一致）。 */
    enum class Outcome(val value: String, val label: String) {
        SUCCESS("success", "成功领取"),
        ALREADY("already", "今日已领"),
        PENDING("pending", "等待窗口"),
        FAILED("failed", "失败"),
        ;

        companion object {
            fun of(raw: String): Outcome? = entries.firstOrNull { it.value == raw }
        }
    }

    /** 来源五值（`source`）：这一条是谁触发的。 */
    fun sourceLabel(raw: String): String = when (raw) {
        "schedule" -> "定时"
        "startup" -> "启动补签"
        "manual" -> "批量"
        "single" -> "单账号"
        "activity" -> "活动领取"
        else -> raw.ifBlank { "—" }
    }

    /** 时间档位（换算成毫秒区间；`ALL` 不传 start）。 */
    enum class Range(val label: String, val days: Int) {
        TODAY("今天", 0),
        WEEK("7 天", 7),
        MONTH("30 天", 30),
        ALL("全部", -1),
        ;

        /** 区间起点（ms）；`ALL` 或 `days < 0` 时给 null = 不限。 */
        fun startMs(now: Long = System.currentTimeMillis()): Long? = when {
            days < 0 -> null
            // 「今天」按 **北京时间** 的自然日零点：跟手机时区走会让海外设备上
            // 的「今天」与内核落账的 date 字段对不上（issue #138 同一类错位）
            days == 0 -> BeijingDay.startOfTodayMs(now)
            else -> BeijingDay.startOfTodayMs(now) - days * 86_400_000L
        }
    }

    class Entry(o: JSONObject) {
        val rowId: Long = o.optLong("rowId")
        val ts: Long = o.optLong("ts")
        val date: String = o.optString("date")
        val source: String = o.optString("source")
        val reason: String = o.optString("reason")
        val accountId: String = o.optString("accountId")
        val accountName: String = o.optString("accountName")
        val provider: String = o.optString("provider")
        val outcome: String = o.optString("outcome")
        val ok: Boolean = o.optBoolean("ok")
        val message: String? = o.optString("message").takeIf { it.isNotBlank() }
        val error: String? = o.optString("error").takeIf { it.isNotBlank() }
        val keptActive: Boolean = o.optBoolean("keptActive")

        val outcomeEnum: Outcome? get() = Outcome.of(outcome)

        /** 一行的说明文字：错误优先，其次收益说明。 */
        val detail: String get() = error ?: message ?: ""
    }

    class Page(val records: List<Entry>, val total: Long) {
        /** 这一页覆盖的区间文案（`1–50 / 1284` 里的前半段）。 */
        fun windowText(offset: Int): String {
            if (records.isEmpty()) return "0"
            return "${offset + 1}–${offset + records.size}"
        }
    }

    suspend fun query(
        outcome: String? = null,
        provider: String? = null,
        keyword: String? = null,
        range: Range = Range.ALL,
        offset: Int = 0,
        limit: Int = 50,
    ): Page? = withContext(Dispatchers.IO) {
        val params = ArrayList<String>()
        params += "limit=$limit"
        params += "offset=$offset"
        outcome?.takeIf { it.isNotBlank() }?.let { params += "outcome=$it" }
        provider?.takeIf { it.isNotBlank() }?.let { params += "provider=$it" }
        keyword?.takeIf { it.isNotBlank() }?.let {
            params += "keyword=" + java.net.URLEncoder.encode(it, "UTF-8")
        }
        range.startMs()?.let { params += "start=$it" }
        val data = CheckinOps.getData("/api/checkin-records?" + params.joinToString("&"))
            ?: return@withContext null
        val arr = data.optJSONArray("records") ?: return@withContext Page(emptyList(), 0)
        val list = (0 until arr.length()).mapNotNull { i -> arr.optJSONObject(i)?.let(::Entry) }
        Page(list, data.optLong("total", 0L))
    }

    /** 按当前筛选删除（「筛出什么就删什么」）。 */
    suspend fun clearMatching(
        outcome: String? = null,
        provider: String? = null,
        keyword: String? = null,
        range: Range = Range.ALL,
    ): CheckinOps.OpResult = withContext(Dispatchers.IO) {
        val params = ArrayList<String>()
        outcome?.takeIf { it.isNotBlank() }?.let { params += "outcome=$it" }
        provider?.takeIf { it.isNotBlank() }?.let { params += "provider=$it" }
        keyword?.takeIf { it.isNotBlank() }?.let {
            params += "keyword=" + java.net.URLEncoder.encode(it, "UTF-8")
        }
        range.startMs()?.let { params += "start=$it" }
        // 一个筛选都没有时不走这条路（内核会 400）——由调用方改调 clearAll
        if (params.isEmpty()) {
            return@withContext CheckinOps.OpResult(false, null, "没有筛选条件，不能按筛选删除")
        }
        val path = "/api/checkin-records?" + params.joinToString("&")
        val reply = CoreTransport.call("DELETE", path)
        when (reply) {
            is CoreTransport.Reply.Down -> CheckinOps.OpResult(false, null, reply.message)
            is CoreTransport.Reply.Ok -> {
                val obj = runCatching { JSONObject(reply.text) }.getOrNull()
                if (obj?.optBoolean("success") == true) {
                    CheckinOps.OpResult(true, obj.optJSONObject("data"))
                } else {
                    CheckinOps.OpResult(false, null, obj?.optString("error")?.ifBlank { "删除失败" } ?: "删除失败")
                }
            }
        }
    }

    /** 清空全部（**必须**显式 all=1，内核只认这个）。 */
    suspend fun clearAll(): CheckinOps.OpResult = withContext(Dispatchers.IO) {
        val reply = CoreTransport.call("DELETE", "/api/checkin-records?all=1")
        when (reply) {
            is CoreTransport.Reply.Down -> CheckinOps.OpResult(false, null, reply.message)
            is CoreTransport.Reply.Ok -> {
                val obj = runCatching { JSONObject(reply.text) }.getOrNull()
                if (obj?.optBoolean("success") == true) {
                    CheckinOps.OpResult(true, obj.optJSONObject("data"))
                } else {
                    CheckinOps.OpResult(false, null, obj?.optString("error")?.ifBlank { "清空失败" } ?: "清空失败")
                }
            }
        }
    }
}

/**
 * 北京时间的日界（界面侧只用于**选查询区间**，不用于判「今天签没签」）。
 *
 * `checkedInToday` 那个布尔永远来自后端；这里算的只是「今天 00:00 的时间戳」，
 * 用来给 `/api/checkin-records?start=` 传参。两者都用固定 UTC+8 ——
 * 手机所在时区不同也不会把区间切错。
 */
object BeijingDay {

    private const val OFFSET_MS = 8 * 3600_000L
    private const val DAY_MS = 86_400_000L

    /** 北京时间今天 00:00 的时间戳（ms）。 */
    fun startOfTodayMs(now: Long = System.currentTimeMillis()): Long {
        val shifted = now + OFFSET_MS
        return shifted - Math.floorMod(shifted, DAY_MS) - OFFSET_MS
    }

    /** 北京时间 `yyyy-MM-dd`（用于「今天领过几份」这类本地比对）。 */
    fun dateKey(ms: Long): String {
        val shifted = ms + OFFSET_MS
        val days = Math.floorDiv(shifted, DAY_MS)
        // 从 1970-01-01 起的日数与年月日的换算（避免依赖 java.time 的 API 级别）
        var remaining = days
        var year = 1970
        while (true) {
            val len = if (isLeap(year)) 366 else 365
            if (remaining < len) break
            remaining -= len
            year++
        }
        val monthDays = intArrayOf(31, if (isLeap(year)) 29 else 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31)
        var month = 0
        while (month < 12 && remaining >= monthDays[month]) {
            remaining -= monthDays[month]
            month++
        }
        return "%04d-%02d-%02d".format(year, month + 1, remaining + 1)
    }

    private fun isLeap(year: Int): Boolean =
        (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}
