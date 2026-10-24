package dev.agate.core

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.PowerManager

/**
 * 电池优化白名单引导（方案 §6.2 保活的配套）。
 *
 * Doze 对前台服务进程本身不冻结 CPU，但会限制网络访问；
 * 加白名单后深睡期网络照常，签到闹钟触发的轮次才能真正打出去。
 * 侧载应用无 Play 政策约束，REQUEST_IGNORE_BATTERY_OPTIMIZATIONS
 * 弹窗直达，无需引导用户去设置里翻。
 */
object BatteryWhitelist {

    fun isIgnoring(ctx: Context): Boolean {
        val pm = ctx.getSystemService(Context.POWER_SERVICE) as PowerManager
        return pm.isIgnoringBatteryOptimizations(ctx.packageName)
    }

    /** 拉起系统弹窗；已在白名单时返回 false（无需申请）。 */
    fun requestIfNeeded(ctx: Context): Boolean {
        if (isIgnoring(ctx)) return false
        ctx.startActivity(
            Intent(
                android.provider.Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                Uri.parse("package:${ctx.packageName}"),
            ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
        )
        return true
    }

    /** Android 12+ 精确闹钟授权页（canScheduleExactAlarms() 为 false 时用）。 */
    fun requestExactAlarmIfNeeded(ctx: Context) {
        if (AlarmScheduler.canExact(ctx)) return
        runCatching {
            ctx.startActivity(
                Intent(
                    android.provider.Settings.ACTION_REQUEST_SCHEDULE_EXACT_ALARM,
                    Uri.parse("package:${ctx.packageName}"),
                ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            )
        }
    }
}
