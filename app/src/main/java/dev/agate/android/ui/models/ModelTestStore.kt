package dev.agate.android.ui.models

import dev.agate.android.data.ModelOps
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * 模型测速的**共享状态**（模型页的行内测速与「模型测速」页共用一份）。
 *
 * ── 为什么放在单例里而不是各页面自己 remember ──────────────────
 * 同一个模型在两处都能测（列表行上的「测」、测速页的一行），结果必须一致：
 * 在行上测完再进测速页，"这一条已经测过、结论是什么"要还在。放页面里就会
 * 各自一份，用户看到两个矛盾的读数。
 *
 * ── 为什么一次只跑一条（按 key 去重）────────────────────────
 * 同一条重复点击会真的打两次上游、花两份额度。`running` 既是去重锁也是
 * 界面的忙态来源。
 */
object ModelTestStore {

    private val _results = MutableStateFlow<Map<String, ModelOps.TestResult>>(emptyMap())

    /** `provider|model` → 最近一次结论（没有键 = 还没测过）。 */
    val results: StateFlow<Map<String, ModelOps.TestResult>> = _results

    private val _running = MutableStateFlow<Set<String>>(emptySet())

    /** 正在测的 key 集合（界面据此显示「测试中…」并禁掉按钮）。 */
    val running: StateFlow<Set<String>> = _running

    fun key(provider: String, model: String): String = "$provider|$model"

    /** 测一条。已经在该条上跑着时直接返回（不重复消耗额度）。 */
    suspend fun run(provider: String, model: String) {
        val k = key(provider, model)
        if (k in _running.value) return
        _running.value = _running.value + k
        try {
            // 通道异常（内核没起来）也要落一条结论：否则按钮会永远停在「测试中」
            val result = runCatching { ModelOps.test(provider, model) }.getOrElse { error ->
                ModelOps.TestResult(
                    ok = false,
                    status = null,
                    error = error.message ?: "测试失败",
                    ttfbMs = null,
                    durationMs = null,
                    completionTokens = null,
                    totalTokens = null,
                    reply = null,
                )
            }
            _results.value = _results.value + (k to result)
        } finally {
            _running.value = _running.value - k
        }
    }

    fun clear() {
        _results.value = emptyMap()
    }
}

/** 首字 / 总耗时 / 吞吐的短标签（两处共用同一套写法，别各写一份）。 */
internal fun ModelOps.TestResult.summary(): String {
    if (!ok) {
        val code = status?.let { " $it" } ?: ""
        return "失败$code" + (error?.let { " · $it" } ?: "")
    }
    val parts = ArrayList<String>(3)
    ttfbMs?.let { parts += "首字 ${fmtMs(it)}" }
    durationMs?.let { parts += "总 ${fmtMs(it)}" }
    // 吞吐只算「首字之后」的生成段：把等待首字算进去会让结论随网络抖动
    val gen = (durationMs ?: 0) - (ttfbMs ?: 0)
    val tok = completionTokens ?: 0
    if (gen > 0 && tok > 0) parts += "${(tok * 1000.0 / gen).toInt()} tok/s"
    return parts.joinToString(" · ")
}

internal fun fmtMs(ms: Long): String =
    if (ms < 1000) "${ms}ms" else String.format("%.1fs", ms / 1000.0)
