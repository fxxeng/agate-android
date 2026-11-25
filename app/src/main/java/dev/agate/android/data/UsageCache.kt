package dev.agate.android.data

import org.json.JSONObject

/**
 * 余额查询的**客户端缓存、节流与刷新时机判定**。
 *
 * ── 余额查询会真打各家上游（`/api/accounts/usage` → 各 provider 的
 * `query_usage`），官方对这类接口有频次风控。本版本把口径改成
 * **手动刷新为主，主动刷新只在少数明确时机**：
 *   - 手动：用户点「查询余额 / 刷新全部」，走 [AccountOps.usage] 的 MANUAL 分支；
 *   - 主动（[shouldProactive]）：冷启动首次打开 app、距上次超过
 *     [PROACTIVE_WINDOW_MS] 再进入额度中心、进入家详情页；
 *   - 其余进入页面 / 首页摘要：只读缓存，绝不打上游。
 *
 * 手动查成功后另开一道**进程级全局静默**（[MANUAL_QUIET_WINDOW_MS]）：期间任何
 * 页面的主动查询都只读缓存。原因见 [MANUAL_QUIET_WINDOW_MS] —— 单查与批量查的
 * key 不同，各自的 30 分钟窗挡不住对方，必须由跨 key 的静默兜住。
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

    /**
     * 手动查询后的全局静默窗：这段时间内**任何页面**都不发起主动查询。
     *
     * 与 [PROACTIVE_WINDOW_MS] 同长 —— 手动查询拿到的就是此刻最新读数，半小时内
     * 没有信息增量。关键在于它是**进程级、跨所有 key**：手动「刷新全部」只写 `*`
     * 键，若不设全局静默，紧接着进家详情页按单账号 id 查时那个键是空的，照样会真
     * 打一轮上游（反之手动单查后进入度中心同理）。静默窗把这类跨键漏洞一次堵住。
     */
    const val MANUAL_QUIET_WINDOW_MS: Long = PROACTIVE_WINDOW_MS

    /** 强制刷新也至少间隔这么久，防连点。 */
    const val HARD_THROTTLE_MS: Long = 20 * 1000L

    private data class Entry(val at: Long, val body: JSONObject)

    private val entries = HashMap<String, Entry>()

    /** 进程内「冷启动首次主动刷新」令牌：整个进程生命周期只消费一次。 */
    private var coldStartConsumed = false

    /** 上次手动查询成功写入的时刻（毫秒）；0 = 本进程还没手动查过。 */
    private var lastManualQueryAt = 0L

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
     * 是否到了「主动刷新」的时机。两道门禁，任一拦下就只读缓存：
     *  1. **全局静默**：距上次手动查询不足 [MANUAL_QUIET_WINDOW_MS] → false。这道
     *     跨所有 key，是「手动查过之后任何页面都别自动查」的判据；
     *  2. 本 key 距上次成功查询不足 [PROACTIVE_WINDOW_MS] → false。
     *
     * 从没查过且不在静默窗 → true（冷启动首进）。与手动强制的 20s 节流分开：
     * 这里看「值不值得再打上游」，手动按钮看「用户点了没反应会不会连点」。
     */
    @Synchronized
    fun shouldProactive(id: String?): Boolean {
        val now = System.currentTimeMillis()
        if (lastManualQueryAt > 0 && now - lastManualQueryAt < MANUAL_QUIET_WINDOW_MS) {
            return false
        }
        val at = entries[key(id)]?.at ?: return true
        return now - at >= PROACTIVE_WINDOW_MS
    }

    /**
     * 记一次手动查询成功（由 [AccountOps.usage] 的 MANUAL 分支在拿到响应后调用）。
     * 失败不记 —— 没拿到新读数不该让其他页面跟着静默。
     */
    @Synchronized
    fun markManualQuery() {
        lastManualQueryAt = System.currentTimeMillis()
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
