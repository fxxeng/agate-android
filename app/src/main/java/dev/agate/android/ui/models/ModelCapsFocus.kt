package dev.agate.android.ui.models

/**
 * 「从模型卡跳到能力位页」时携带的上下文：打开就落在这一条上。
 *
 * ── 为什么用一个一次性单例而不是导航参数 ────────────────────
 * 能力位页的入口有两处（模型卡、设置里的菜单），都只需要「打开谁」。为一个
 * 可选的 `(provider, model)` 加两条 nav 参数会把路由签名弄复杂，而这件事没有
 * 深链需求 —— 用完即清，也不参与状态恢复。真需要深链时再换参数。
 */
object ModelCapsFocus {
    private var provider: String? = null
    private var model: String? = null

    fun set(providerId: String, modelId: String) {
        provider = providerId
        model = modelId
    }

    /** 取出并清空（只生效一次，返回列表后不会被重复应用）。 */
    fun take(): Pair<String, String>? {
        val p = provider
        val m = model
        provider = null
        model = null
        return if (p.isNullOrBlank() || m.isNullOrBlank()) null else p to m
    }
}
