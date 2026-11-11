package dev.agate.core

import android.app.AlarmManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri

/** 非导出 Receiver 的 token 路由与 PendingIntent 生命周期。 */
object AgateAlarmIntents {
    const val ACTION_REFRESH = "dev.agate.core.action.ALARM_REFRESH"
    fun resource(action: String?, data: String?): AlarmResource? =
        AgateAlarmRouter.resource(action, data)

    fun cancelAll(context: Context) {
        val app = context.applicationContext
        val manager = app.getSystemService(Context.ALARM_SERVICE) as AlarmManager
        // 升级前是 requestCode=1002、无 data URI、指向旧 Receiver 的 token。
        // 显式 className 不需要保留旧 Kotlin 类；只取消，不新建。
        runCatching {
            val legacy = Intent(app, AlarmReceiver::class.java)
                .setAction(AlarmScheduler.ACTION)
            PendingIntent.getBroadcast(
                app,
                1002,
                legacy,
                PendingIntent.FLAG_NO_CREATE or PendingIntent.FLAG_IMMUTABLE,
            )?.let(manager::cancel)
        }
        AlarmResource.entries.forEach { resource ->
            runCatching {
                pendingIntent(app, resource, PendingIntent.FLAG_NO_CREATE)?.let(manager::cancel)
            }
        }
    }

    /** 测试保留精确 requestCode；没有现存 token 时 FLAG_NO_CREATE 不新建。 */
    internal fun pendingIntent(
        context: Context,
        resource: AlarmResource,
        flags: Int = PendingIntent.FLAG_UPDATE_CURRENT,
    ): PendingIntent? {
        val action = if (resource == AlarmResource.Checkin) {
            AlarmScheduler.ACTION
        } else {
            ACTION_REFRESH
        }
        val data = when (resource) {
            AlarmResource.Checkin -> "agalarm://tick/main"
            AlarmResource.GatewayRefresh -> "agalarm://refresh/gateway"
            AlarmResource.UsageRefresh -> "agalarm://refresh/usage"
            AlarmResource.CredentialRefresh -> "agalarm://refresh/credentials"
        }
        val intent = Intent(context, AgateAlarmReceiver::class.java)
            .setAction(action)
            .setData(Uri.parse(data))
        // requestCode=0 + action + data URI + component 构成稳定身份；不比较 extras。
        return PendingIntent.getBroadcast(
            context,
            0,
            intent,
            flags or PendingIntent.FLAG_IMMUTABLE,
        )
    }
}
