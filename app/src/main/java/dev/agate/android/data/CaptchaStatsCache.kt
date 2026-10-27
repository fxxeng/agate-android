package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

/**
 * ZCode 活动套餐**验证码令牌池**的库存缓存。
 *
 * ── 为什么要缓存（而不是每个账号各拉一次）───────────────────
 * 令牌池是**网关全局**的一份（不是每账号一份），而账号列表里每个 ZCode 账号
 * 都要显示「令牌 N / M」这一行。若每个账号各发一次请求，10 个账号就是 10 次
 * 往返，而且它们读的是同一份数字 —— 纯浪费。
 *
 * 所以缓存一份、5 秒内复用（与 3.0.8 界面轮询的节奏同量级）。过期后下一次
 * 取用会重新拉；拉失败时**沿用上次的值**（不把界面刷成「读取中…」），
 * 真的从未拉到过才返回 null（调用方显示「读取中…」，不编数字）。
 */
object CaptchaStatsCache {

    /** `(ready, target)`；null = 从未成功拉到过。 */
    @Volatile private var cached: Pair<Int, Int>? = null
    @Volatile private var fetchedAt: Long = 0L
    @Volatile private var inFlight: Boolean = false

    private const val TTL_MS = 5_000L

    fun get(): Pair<Int, Int>? = cached

    /** 过期就异步刷新（不阻塞渲染）。调用方通常是 Composable 的副作用。 */
    suspend fun refreshIfStale() {
        val now = System.currentTimeMillis()
        if (inFlight || (cached != null && now - fetchedAt < TTL_MS)) return
        inFlight = true
        try {
            val r = withContext(Dispatchers.IO) { CoreTransport.get("/api/zcode/captcha") }
            if (r is CoreTransport.Reply.Ok) {
                val data = runCatching { JSONObject(r.text) }.getOrNull()?.optJSONObject("data")
                if (data != null) {
                    val ready = data.optInt("ready", 0)
                    val target = data.optInt("target", 3)
                    cached = ready to target
                    fetchedAt = System.currentTimeMillis()
                }
            }
        } finally {
            inFlight = false
        }
    }
}
