package dev.agate.core

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** 升级前 token 的兼容入口；新资源全部由 AgateAlarmReceiver 分类。 */
@Deprecated("仅用于承接升级前已注册的签到 PendingIntent")
class AlarmReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != AlarmScheduler.ACTION || intent.data != null) return
        val app = context.applicationContext
        if (GatewayPrefs.checkinAlarmEnabled(app)) {
            runCatching { AlarmScheduler.scheduleNext(app) }
            AgateForegroundService.tick(app)
        } else {
            runCatching { AgateAlarmIntents.cancelAll(app) }
        }
    }
}
