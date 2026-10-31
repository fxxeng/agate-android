package dev.agate.core

import android.app.AlarmManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import java.util.Calendar
import java.util.TimeZone

/**
 * 签到闹钟（方案 §6.2 三级保活的第 3 级「定点唤醒」）。
 *
 * 时区钉 Asia/Shanghai：内核侧调度（core/auto_checkin.rs + core/beijing.rs）
 * 用固定 +08:00 判定「到点」，两侧同一语义，设备时区漂移也不漂。
 *
 * 触发链：AlarmManager（Doze 白名单外的 15 分钟窗口限制对 exact alarm 不适用）
 *   → AlarmReceiver → AgateForegroundService(ACTION_TICK)
 *   → 未运行则先拉内核 → Gateway.triggerTask("scheduled_tick")
 *   → 重排下一个时刻（一次性闹钟，每次触发后自己接力，时刻表改动即时生效）
 */
object AlarmScheduler {

    const val ACTION = "dev.agate.core.action.CHECKIN_ALARM"
    private const val REQUEST_CODE = 1002

    /** Android 12+ 能否用精确闹钟（Android 14 起新装默认拒绝，需引导）。 */
    fun canExact(ctx: Context): Boolean =
        Build.VERSION.SDK_INT < 31 ||
            (ctx.getSystemService(Context.ALARM_SERVICE) as AlarmManager).canScheduleExactAlarms()

    /**
     * 计算并注册下一次触发；返回触发时刻（epoch 毫秒），时刻表全非法返回 null。
     * 网关未启动也可注册（闹钟的职责就是把进程拉起来）。
     */
    fun scheduleNext(ctx: Context): Long? {
        val app = ctx.applicationContext
        val times = parseTimes(GatewayPrefs.alarmTimes(app))
        if (times.isEmpty()) {
            cancel(app)
            GatewayPrefs.setNextAlarmAt(app, 0L)
            return null
        }
        val at = nextOccurrence(times) ?: return null
        val pi = pendingIntent(app)
        val am = app.getSystemService(Context.ALARM_SERVICE) as AlarmManager
        // canExact 可能在检查后被用户撤权；先尝试精确闹钟，再降级为非精确。
        // 两条路径都可能因后台限制/ROM 策略抛异常，不能让 BroadcastReceiver 崩溃，
        // 也不能把一个实际上未注册的时间写进「下次闹钟」状态。
        val scheduled = try {
            if (canExact(app)) {
                try {
                    am.setExactAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, at, pi)
                } catch (exactFailure: Exception) {
                    android.util.Log.w("Agate", "精确闹钟不可用，降级为非精确闹钟", exactFailure)
                    am.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, at, pi)
                }
            } else {
                am.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, at, pi)
            }
            true
        } catch (failure: Exception) {
            android.util.Log.e("Agate", "注册下次签到闹钟失败", failure)
            false
        }
        if (!scheduled) {
            GatewayPrefs.setNextAlarmAt(app, 0L)
            return null
        }
        GatewayPrefs.setNextAlarmAt(app, at)
        return at
    }

    fun cancel(ctx: Context) {
        val app = ctx.applicationContext
        (app.getSystemService(Context.ALARM_SERVICE) as AlarmManager).cancel(pendingIntent(app))
        GatewayPrefs.setNextAlarmAt(app, 0L)
    }

    /** "08:00,20:00" → [(8,0),(20,0)]；非法项丢弃。 */
    internal fun parseTimes(csv: String): List<Pair<Int, Int>> =
        csv.split(',').mapNotNull { item ->
            val parts = item.trim().split(':')
            if (parts.size != 2) return@mapNotNull null
            val h = parts[0].trim().toIntOrNull() ?: return@mapNotNull null
            val m = parts[1].trim().toIntOrNull() ?: return@mapNotNull null
            if (h in 0..23 && m in 0..59) h to m else null
        }

    /** 所有时刻在今天（北京时间）的下次出现里取最早；已过的顺延一天。 */
    private fun nextOccurrence(times: List<Pair<Int, Int>>): Long? {
        val cst = TimeZone.getTimeZone("Asia/Shanghai")
        val now = System.currentTimeMillis()
        var best = Long.MAX_VALUE
        for ((h, m) in times) {
            val cal = Calendar.getInstance(cst).apply {
                set(Calendar.HOUR_OF_DAY, h)
                set(Calendar.MINUTE, m)
                set(Calendar.SECOND, 0)
                set(Calendar.MILLISECOND, 0)
            }
            if (cal.timeInMillis <= now) {
                cal.add(Calendar.DAY_OF_YEAR, 1)
            }
            if (cal.timeInMillis < best) best = cal.timeInMillis
        }
        return if (best == Long.MAX_VALUE) null else best
    }

    private fun pendingIntent(ctx: Context): PendingIntent =
        PendingIntent.getBroadcast(
            ctx,
            REQUEST_CODE,
            Intent(ctx, AlarmReceiver::class.java).setAction(ACTION),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
}
