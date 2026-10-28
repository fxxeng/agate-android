package dev.agate.android.data

import android.content.Context
import dev.agate.core.AlarmScheduler
import dev.agate.core.BatteryWhitelist
import dev.agate.core.Gateway
import dev.agate.core.GatewayPrefs

/**
 * 「保活与通知」组的**读层**（设置页第 10 组）。
 *
 * ── 为什么不像其他 Ops 那样自己管偏好 ────────────────────────
 * 这一组的两颗开关（签到结果通知 / 异常告警）的真实消费者在 core-gateway：
 * `AgateForegroundService.runTick` 签到收尾时读开关决定发不发通知。
 * 依赖方向是 app → core-gateway，core-gateway **读不到** app 的偏好对象。
 * 所以键与读写都落在 `GatewayPrefs`（同一个 SharedPreferences 文件
 * `gateway_prefs`），本对象只做三件事：
 *   1. 把「只读的系统实况」包成设置页好用的形状；
 *   2. 把三项读数合成一句副标题；
 *   3. 收口「哪些能改、哪些只能看」这条语义。
 *
 * ── 「前台服务常驻」为什么不是开关 ──────────────────────────
 * `AgateForegroundService` 是网关运行的**唯一载体**：`Gateway.start` 在
 * app 侧没有任何直接调用，`ConsolePage` / `GatewayControlPage` /
 * `AddProviderPage` 三个入口全都走 `AgateForegroundService.start`。
 * 关掉它 = 关掉网关。做成开关既与「网关控制」页的启停按钮重复，又会
 * 让用户以为关掉之后网关还在跑。所以设置页把它渲染成只读状态行。
 *
 * ── 「忽略电池优化」「精确闹钟」同理 ───────────────────────
 * 两者都是**系统授予**的权限，App 无权自行开关，只能读实况 + 拉起系统
 * 弹窗。做成开关会让人以为能在 App 内控制，实际点了只会跳出去。
 */
object KeepalivePrefs {

    /** 签到结果通知（读写委托 GatewayPrefs，键在那里定义一次）。 */
    fun checkinNotify(ctx: Context): Boolean = GatewayPrefs.checkinNotify(ctx)

    fun setCheckinNotify(ctx: Context, value: Boolean) =
        GatewayPrefs.setCheckinNotify(ctx, value)

    /** 服务异常告警（读写委托 GatewayPrefs）。 */
    fun alertNotify(ctx: Context): Boolean = GatewayPrefs.alertNotify(ctx)

    fun setAlertNotify(ctx: Context, value: Boolean) =
        GatewayPrefs.setAlertNotify(ctx, value)

    // ── 系统实况（只读，每次进设置页重读，不缓存） ──────────────

    /**
     * 网关是否在跑。
     *
     * 用 `Gateway.status().running` 而不是问 Service 自身：前台服务可能
     * 因为「一次性 tick」短暂存在（`runTick` 末尾会 `stopSelf`），那时
     * 服务对象活着但网关没在跑。「常驻」对用户而言是**网关在跑**。
     */
    fun serviceRunning(): Boolean = runCatching { Gateway.status().running }
        .getOrDefault(false)

    /** 忽略电池优化的授权实况。 */
    fun ignoringBatteryOptimization(ctx: Context): Boolean =
        runCatching { BatteryWhitelist.isIgnoring(ctx) }.getOrDefault(false)

    /** 拉起系统「忽略电池优化」授权弹窗；已授权时返回 false（无弹窗）。 */
    fun requestIgnoreBatteryOptimization(ctx: Context): Boolean =
        runCatching { BatteryWhitelist.requestIfNeeded(ctx) }.getOrDefault(false)

    /** Android 12+ 精确闹钟授权实况（低版本恒 true）。 */
    fun canExactAlarm(ctx: Context): Boolean =
        runCatching { AlarmScheduler.canExact(ctx) }.getOrDefault(true)

    /** 拉起系统「精确闹钟」授权页（已授权时为空操作）。 */
    fun requestExactAlarm(ctx: Context) {
        runCatching { BatteryWhitelist.requestExactAlarmIfNeeded(ctx) }
    }

    /** 下一次定点唤醒时刻（epoch ms，0 = 未排程）。 */
    fun nextAlarmAt(ctx: Context): Long =
        runCatching { GatewayPrefs.nextAlarmAt(ctx) }.getOrDefault(0L)
}
