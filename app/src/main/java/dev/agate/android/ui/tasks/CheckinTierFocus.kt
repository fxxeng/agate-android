package dev.agate.android.ui.tasks

/**
 * 「任务中心 → 签到中心」携带的目标任务档：打开签到中心就落在该档。
 *
 * ── 为什么是一次性单例而不是导航参数 ──────────────────────────
 * 与 [dev.agate.android.ui.models.ModelCapsFocus] 同理：这是一次性的跳转意图，
 * 没有深链需求、不参与状态恢复。用完即清，返回任务中心再点别的档不会被
 * 上一次的残留值影响。档位值集中在此，任务中心与签到中心共用一份，避免
 * 两处各写一份字面量漂移。
 */
object CheckinTierFocus {
    const val ALL = "all"
    const val DAILY = "daily"
    const val ONBOARDING = "onboarding"
    const val PLANS = "plans"
    const val GROWTH = "growth"

    /** 标签顺序与文案，任务中心标签组与签到中心任务档共用。 */
    val TIERS: List<Pair<String, String>> = listOf(
        ALL to "全部",
        DAILY to "每日签到",
        ONBOARDING to "新手福利",
        PLANS to "活动福利",
        GROWTH to "成长任务",
    )

    private var tier: String? = null

    fun set(tier: String) {
        this.tier = tier.takeIf { it.isNotBlank() }
    }

    /** 取出并清空（只生效一次）；空白返回 null，签到中心回落到「全部」档。 */
    fun take(): String? {
        val value = tier
        tier = null
        return value?.takeIf { it.isNotBlank() }
    }

    /** 合法性兜底：只认已知档位，其余回落到「全部」。 */
    fun normalize(tier: String?): String =
        if (tier != null && TIERS.any { it.first == tier }) tier else ALL
}
