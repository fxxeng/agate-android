package dev.agate.android.data

import org.json.JSONObject

/**
 * 余额查询的**客户端缓存、节流与刷新时机判定**。
 *
 * ── 余额查询会真打各家上游（`/api/accounts/usage` → 各 provider 的
 * `query_usage`），官方对这类接口有频次风控。本版本把口径改成
 * **手动刷新为主，主动刷新只在少数明确时机**：
 *   - 手动：用户点「查询余额 / 刷新全部」，走 [AccountOps.usage] 的 force 分支；
 *   - 主动（[shouldProactive]）：冷启动首次打开 app、距上次超过
 *     [PROACTIVE_WINDOW_MS] 再进入额度中心、进入家详情页；
 *   - 其余进入页面 / 首页摘要：只读缓存，绝不打上游。
 *
 * 内核侧缺省也已改为「不自动查」（`usage_records::DEFAULT_QUERY_ENABLED=false`），
 * 显式开启账号自动查询的用户仍按原间隔跑 —— 两条链互不干扰。
 *
 * 缓存是**进程内存**（重启即空），所以「冷启动首次」天然能靠本进程的一次性
 * 标志实现：进程内 [consumeColdStart] 只有第一次返回 true。
 */
object UsageCache {

    /** 主动刷新的时间窗：距上次成功查询超过这么久，进额度中心才值得再查。 */
    const val PROACTIVE_WINDOW_MS: Long = 30 * 60 * 1000L

    /** 强制刷新也至少间隔这么久，防连点。 */
    const val HARD_THROTTLE_MS: Long = 20 * 1000L

    private data class Entry(val at: Long, val body: JSONObject)

    private val entries = HashMap<String, Entry>()

    /** 进程内「冷启动首次主动刷新」令牌：整个进程生命周期只消费一次。 */
    private var coldStartConsumed = false

    private fun key(id: String?) = id?.takeIf { it.isNotBlank() } ?: "*"

    /**
     * 消费冷启动令牌：整个进程只返回一次 true，用于「首次打开 app 主动拉一次」。
     * 进程重启后对象重建 → 令牌复位，正是「每次冷启动一次」的语义。
     */
    @Synchronized
    fun consumeColdStart(): Boolean {
        if (coldStartConsumed) return false
        coldStartConsumed = true
        return true
    }

    /** 是否已有可用缓存（不看新鲜度），用于「先渲染旧数据」。 */
    @Synchronized
    fun cached(id: String?): JSONObject? = entries[key(id)]?.body

    /** 上次成功写入的时间戳（毫秒），没有则 null。 */
    @Synchronized
    fun updatedAt(id: String?): Long? = entries[key(id)]?.at

    /**
     * 是否到了「主动刷新」的时机：从没查过（冷启动首进）→ true；
     * 距上次成功查询超过 [PROACTIVE_WINDOW_MS] → true；否则 false（用缓存）。
     *
     * 与手动强制的 20s 节流分开：主动刷新看的是 30 分钟的「值不值得再打上游」，
     * 手动按钮看的是「用户点了没反应会不会连点」。
     */
    @Synchronized
    fun shouldProactive(id: String?): Boolean {
        val at = entries[key(id)]?.at ?: return true
        return System.currentTimeMillis() - at >= PROACTIVE_WINDOW_MS
    }

    /** 距离下次可强制刷新还有多久（毫秒），0 表示可以。 */
    @Synchronized
    fun throttleRemaining(id: String?): Long {
        val entry = entries[key(id)] ?: return 0
        val elapsed = System.currentTimeMillis() - entry.at
        return (HARD_THROTTLE_MS - elapsed).coerceAtLeast(0)
    }

    @Synchronized
    fun put(id: String?, body: JSONObject) {
        entries[key(id)] = Entry(System.currentTimeMillis(), body)
    }

    /** 账号删除后清掉它的缓存，避免 id 复用时串数据。 */
    @Synchronized
    fun invalidate(id: String) {
        entries.remove(id)
        // 批量缓存里含该账号的行，一并作废（下次会重新批量查）
        entries.remove("*")
    }
}
