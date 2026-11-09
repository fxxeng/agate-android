package dev.agate.core

import android.app.AlarmManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * 精确闹钟授权变化接收器（R50/T7）。
 *
 * Android 12+ 用户可以事后授予或撤销 `SCHEDULE_EXACT_ALARM`。旧实现没有
 * 监听这个广播：撤销后系统会清掉已注册的精确闹钟，授予后也不会自动补排，
 * 签到链就断了。这里在授权变化时按当前开关重排一次；撤销时 scheduleNext
 * 会走非精确降级或抛异常，按失败记录，不让广播崩溃。
 */
class ExactAlarmPermissionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != AlarmManager.ACTION_SCHEDULE_EXACT_ALARM_PERMISSION_STATE_CHANGED) {
            return
        }
        val app = context.applicationContext
        if (!GatewayPrefs.checkinAlarmEnabled(app)) return
        runCatching { AlarmScheduler.scheduleNext(app) }
            .onFailure {
                android.util.Log.w("Agate", "精确闹钟授权变化后重排失败", it)
            }
    }
}
