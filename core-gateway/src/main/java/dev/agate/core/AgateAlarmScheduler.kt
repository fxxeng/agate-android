package dev.agate.core

import android.app.AlarmManager
import android.content.Context
import android.os.SystemClock

/** 三类轻量刷新的续期：只刷新 Core 状态，不启动 Gateway。 */
object AgateAlarmScheduler {
    fun schedule(context: Context, resource: AlarmResource) {
        if (resource == AlarmResource.Checkin) {
            AlarmScheduler.scheduleNext(context)
            return
        }
        val app = context.applicationContext
        val manager = app.getSystemService(Context.ALARM_SERVICE) as AlarmManager
        val token = AgateAlarmIntents.pendingIntent(app, resource) ?: return
        val at = SystemClock.elapsedRealtime() + when (resource) {
            AlarmResource.UsageRefresh -> 60 * 60 * 1000L
            AlarmResource.GatewayRefresh -> 30 * 60 * 1000L
            AlarmResource.CredentialRefresh -> 6 * 60 * 60 * 1000L
            AlarmResource.Checkin -> error("签到使用独立时刻表")
        }
        manager.setAndAllowWhileIdle(AlarmManager.ELAPSED_REALTIME_WAKEUP, at, token)
    }

    fun cancel(context: Context, resource: AlarmResource) {
        val app = context.applicationContext
        val manager = app.getSystemService(Context.ALARM_SERVICE) as AlarmManager
        AgateAlarmIntents.pendingIntent(app, resource, android.app.PendingIntent.FLAG_NO_CREATE)
            ?.let(manager::cancel)
        if (resource == AlarmResource.Checkin) GatewayPrefs.setNextAlarmAt(app, 0L)
    }
}
