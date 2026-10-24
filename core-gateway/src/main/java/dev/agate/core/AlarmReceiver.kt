package dev.agate.core

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * 签到闹钟触发入口。exact-while-idle 闹钟的 BroadcastReceiver 自带
 * 约 10 秒的前台服务启动豁免窗口，Doze 深睡里也能把网关拉起来。
 *
 * 先重排下一个时刻再触发（防本轮回调被杀后整条链断掉）。
 */
class AlarmReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != AlarmScheduler.ACTION) return
        val app = context.applicationContext
        if (GatewayPrefs.checkinAlarmEnabled(app)) {
            AlarmScheduler.scheduleNext(app)
            AgateForegroundService.tick(app)
        } else {
            // 开关已关但闹钟还在（竞态）：就地清掉
            AlarmScheduler.cancel(app)
        }
    }
}
