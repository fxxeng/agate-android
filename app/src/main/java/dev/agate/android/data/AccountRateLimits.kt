package dev.agate.android.data

import org.json.JSONObject

/**
 * 账号限流（429 冷却）条目的客户端视图。
 *
 * ── 数据从哪来 ──────────────────────────────────────────────
 * 内核在转发遇到上游 429 / 限额时把冷却期**按账号落盘**（账号记录的
 * `rateLimits` 键：模型名 → `{status, code, resetAt, message, at}`，见内核
 * `store_admin::mark_rate_limited`），并随账号公开形态透出到
 * `/api/session.accounts` 的 `raw` 里 —— 客户端**不需要**再发任何请求，
 * 从快照就地解析即可。恢复判定也同步成立：选路按 `resetAt` 自然放行，
 * 到恢复时间这条记录会被内核自动摘掉（成功即清 / 选路时过期不选），
 * 所以这里再按 `resetAt > now` 过滤一遍只是把「内核还没来得及清」的
 * 陈旧条目从展示里藏掉，不改任何行为。
 */
data class RateLimitEntry(
    /** 被限的上游模型名（`rateLimits` 的键） */
    val model: String,
    /** 上游拒绝的状态码（429 居多；个别上游用 403 表达超额） */
    val status: Int,
    /** 恢复时刻（epoch ms） */
    val resetAt: Long,
    /** 上游原文（截断过，≤300 字符） */
    val message: String,
)

/**
 * 从账号 raw 里解析**仍在生效**的限流条目，按恢复时间升序。
 *
 * `raw` 形态异常 / 无键 / 全部过期都返回空表 —— 「没在限流」与「解析不了」
 * 在界面上是同一种表达（不显示限流区），没必要区分。
 */
fun activeRateLimits(raw: JSONObject?, nowMs: Long): List<RateLimitEntry> {
    val limits = raw?.optJSONObject("rateLimits") ?: return emptyList()
    val out = ArrayList<RateLimitEntry>(limits.length())
    for (model in limits.keys()) {
        val entry = limits.optJSONObject(model) ?: continue
        val resetAt = when (val value = entry.opt("resetAt")) {
            is Number -> value.toDouble().toLong()
            is String -> value.trim().toDoubleOrNull()?.toLong()
            else -> null
        } ?: continue
        if (resetAt <= nowMs) continue
        out += RateLimitEntry(
            model = model.trim(),
            status = entry.optInt("status", 429),
            resetAt = resetAt,
            message = entry.optString("message").trim(),
        )
    }
    out.sortBy { it.resetAt }
    return out
}

/** 倒计时文案：与上游 3.0.8 同款式（「还有 7 小时 20 分」）。 */
fun rateCountdown(resetAt: Long, nowMs: Long): String {
    val left = resetAt - nowMs
    if (left <= 60_000L) return "1 分钟内恢复"
    val minutes = left / 60_000L
    val hours = minutes / 60
    return when {
        hours >= 24 -> {
            val days = hours / 24
            val restHours = hours % 24
            if (restHours == 0L) "还有 $days 天" else "还有 $days 天 $restHours 小时"
        }
        hours > 0 -> {
            val restMinutes = minutes % 60
            if (restMinutes == 0L) "还有 $hours 小时" else "还有 $hours 小时 $restMinutes 分"
        }
        else -> "还有 $minutes 分钟"
    }
}

/** 恢复时刻的绝对文案：「今天 23:22」「明天 08:05」「10/2 14:00」。 */
fun rateRecoverAt(resetAt: Long, nowMs: Long): String {
    val fmt = java.text.SimpleDateFormat("HH:mm", java.util.Locale.getDefault())
    val calendar = java.util.Calendar.getInstance()
    val nowDay = (nowMs / 86_400_000L).toInt()
    val resetDay = (resetAt / 86_400_000L).toInt()
    calendar.timeInMillis = resetAt
    val time = fmt.format(calendar.time)
    return when {
        resetDay == nowDay -> "今天 $time"
        resetDay == nowDay + 1 -> "明天 $time"
        else -> {
            val day = java.text.SimpleDateFormat("M/d", java.util.Locale.getDefault())
            "${day.format(calendar.time)} $time"
        }
    }
}
