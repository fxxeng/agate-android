package dev.agate.core

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** 全部周期唤醒入口：按 action/data URI 分类，不用 extras 判资源身份。 */
class AgateAlarmReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val app = context.applicationContext
        when (AgateAlarmIntents.resource(intent.action, intent.dataString)) {
            AlarmResource.Checkin -> {
                if (GatewayPrefs.checkinAlarmEnabled(app)) {
                    runCatching { AlarmScheduler.scheduleNext(app) }
                        .onFailure { android.util.Log.e("Agate", "重排下次签到闹钟失败", it) }
                    AgateForegroundService.tick(app)
                } else {
                    runCatching { AlarmScheduler.cancel(app) }
                        .onFailure { android.util.Log.e("Agate", "取消过期签到闹钟失败", it) }
                }
            }
            AlarmResource.GatewayRefresh -> {
                // 状态刷新只走 Core 状态；不得借此启动/复活监听端口。
                RuntimeController.refreshStatus()
                AgateAlarmScheduler.schedule(app, AlarmResource.GatewayRefresh)
            }
            AlarmResource.UsageRefresh,
            AlarmResource.CredentialRefresh,
            -> {
                // Rust Core 已按配置调度余额与凭证维护；Android 只做状态对齐与 token 续期。
                RuntimeController.refreshStatus()
                val resource = AgateAlarmIntents.resource(intent.action, intent.dataString) ?: return
                AgateAlarmScheduler.schedule(app, resource)
            }
            null -> Unit
        }
    }
}
