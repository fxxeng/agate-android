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
            // 接力排程失败不能吞掉本次触发：先尽力排下一轮，再独立启动当前 tick。
            runCatching { AlarmScheduler.scheduleNext(app) }
                .onFailure { android.util.Log.e("Agate", "重排下次签到闹钟失败", it) }
            AgateForegroundService.tick(app)
        } else {
            // 开关已关但闹钟还在（竞态）：就地清掉；清理失败也不应导致广播崩溃。
            runCatching { AlarmScheduler.cancel(app) }
                .onFailure { android.util.Log.e("Agate", "取消过期签到闹钟失败", it) }
        }
    }
}
