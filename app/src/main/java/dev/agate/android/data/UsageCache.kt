package dev.agate.android.data

import org.json.JSONObject

/**
 * 余额查询的**客户端缓存与节流**。
 *
 * ── 为什么需要它 ────────────────────────────────────────────
 * 余额查询会真的打到各家上游（`/api/accounts/usage` → 各 provider 的
 * `query_usage`），而官方对这类接口有频次风控。页面侧原本的行为是「进入即查」：
 * 每次打开提供商详情、每次重建列表、每次回到额度中心，都会再请求一轮。
 * 用户来回翻几次就等于刷了一串请求。
 *
 * 这里的口径：
 * - **默认只读缓存**（[SOFT_TTL_MS] 内视为新鲜，直接用）；
 * - 只有用户**显式点「查询余额」**才强制刷新；
 * - 强制刷新也有**硬节流**（[HARD_THROTTLE_MS]），防止连点；
 * - 按「账号 id」缓存单查结果，批量查询（id 为空）单独用 `*` 作为键。
 *
 * 阈值取 5 分钟 / 20 秒：余额是分钟级才变的东西，5 分钟内重复查没有信息增量；
 * 20 秒是「用户点了一下没反应会再点」的心理窗口。
 */
object UsageCache {

    /** 缓存新鲜期：此时间内不发起上游请求。 */
    const val SOFT_TTL_MS: Long = 5 * 60 * 1000L

    /** 强制刷新也至少间隔这么久，防连点。 */
    const val HARD_THROTTLE_MS: Long = 20 * 1000L

    private data class Entry(val at: Long, val body: JSONObject)

    private val entries = HashMap<String, Entry>()

    private fun key(id: String?) = id?.takeIf { it.isNotBlank() } ?: "*"

    /** 是否已有可用缓存（不看新鲜度），用于「先渲染旧数据」。 */
    @Synchronized
    fun cached(id: String?): JSONObject? = entries[key(id)]?.body

    /** 缓存是否仍然新鲜。 */
    @Synchronized
    fun isFresh(id: String?): Boolean {
        val entry = entries[key(id)] ?: return false
        return System.currentTimeMillis() - entry.at < SOFT_TTL_MS
    }

    /** 上次成功写入的时间戳（毫秒），没有则 null。 */
    @Synchronized
    fun updatedAt(id: String?): Long? = entries[key(id)]?.at

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
