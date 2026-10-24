package dev.agate.core

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * 开机自启（方案 §6.2）。BOOT_COMPLETED 属于前台服务启动豁免的
 * 广播类别，specialUse FGS 可以直接拉。MY_PACKAGE_REPLACED 覆盖
 * 「升级 APK 后进程被杀」的场景，重装后无需手动再点启动。
 *
 * 签到闹钟是单次注册（触发后接力），进程死后系统闹钟仍有效，
 * 这里只需在新进程里把「开关开着」的闹钟重新排上。
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
            Intent.ACTION_BOOT_COMPLETED,
            Intent.ACTION_MY_PACKAGE_REPLACED,
            -> {
                val app = context.applicationContext
                if (GatewayPrefs.bootAutoStart(app)) {
                    runCatching { AgateForegroundService.start(app) }
                }
                if (GatewayPrefs.checkinAlarmEnabled(app)) {
                    runCatching { AlarmScheduler.scheduleNext(app) }
                }
            }
        }
    }
}
