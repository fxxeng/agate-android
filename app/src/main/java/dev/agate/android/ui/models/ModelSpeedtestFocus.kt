package dev.agate.android.ui.models

/**
 * 「点胶囊进测速页」的定位意图（与 [ModelCapsFocus] 同一手法）。
 *
 * ── 为什么不用路由参数 ──────────────────────────────────────
 * 本仓的二级页路由是不带参的常量字符串（`Routes.MODEL_SPEEDTEST`），加一个
 * 可选参数要同时改路由声明、`composable` 的 arguments 与三处调用点，而
 * [ModelCapsFocus] 已经验证过「一次性焦点」这条路更省事：设置 → 导航 →
 * 目标页 `take()` 取走即清空。
 *
 * ── 为什么 take 会清空 ──────────────────────────────────────
 * 用户从胶囊进来看到「只看 xxx」，随后点「显示全部」、返回、再从页头「测速」
 * 进来 —— 那一次**不该**又被过滤一次。焦点是一次性的意图，不是页面的状态。
 * 想再按某个模型看，回模型页再点一次胶囊即可。
 */
object ModelSpeedtestFocus {
    private var model: String? = null

    fun set(modelId: String) {
        model = modelId
    }

    /** 取出并清空；没有焦点时返回空串。 */
    fun take(): String {
        val value = model.orEmpty()
        model = null
        return value
    }
}
