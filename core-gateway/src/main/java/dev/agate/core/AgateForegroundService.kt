package dev.agate.core

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder

/**
 * 网关前台服务（M0 最小版）：specialUse FGS + 常驻通知，负责拉起/停止内核。
 *
 * M0 刻意省略（M1 补齐，见方案 §6.2）：开机自启、电池白名单引导、
 * AlarmManager 精确签到唤醒、按需 WakeLock/WifiLock、局域网模式。
 * START_NOT_STICKY：进程被杀后不自动重启（保活重启是 M1 的验收项）。
 */
class AgateForegroundService : Service() {

    companion object {
        const val ACTION_START = "dev.agate.core.action.START"
        const val ACTION_STOP = "dev.agate.core.action.STOP"
        private const val CHANNEL_ID = "agate_gateway"
        private const val NOTIFICATION_ID = 1001

        /** M0：端口/局域网开关用默认值（8787/关）；M1 起接 Compose 设置页。 */
        fun start(context: Context) {
            context.startForegroundService(
                Intent(context, AgateForegroundService::class.java).setAction(ACTION_START)
            )
        }

        fun stop(context: Context) {
            context.startService(
                Intent(context, AgateForegroundService::class.java).setAction(ACTION_STOP)
            )
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                Thread {
                    try {
                        Gateway.stop()
                    } catch (t: Throwable) {
                        android.util.Log.e("Agate", "内核停止异常", t)
                    }
                    stopForeground(STOP_FOREGROUND_REMOVE)
                    stopSelf()
                }.start()
                return START_NOT_STICKY
            }
            else -> {
                // 先进前台（Android 12+ 的 FGS 5 秒窗口），再拉内核
                enterForeground("启动中…")
                Thread {
                    val result = try {
                        Gateway.start(filesDir.resolve("agate").absolutePath, port = 8787, bindLan = false)
                    } catch (t: Throwable) {
                        android.util.Log.e("Agate", "内核启动异常（.so 加载失败？）", t)
                        null
                    }
                    if (result != null && result.ok) {
                        updateNotification("网关运行中 · :${result.port}（内核 v${result.version}）")
                    } else {
                        val reason = result?.let { "${it.stage ?: ""}: ${it.error}" } ?: "异常"
                        updateNotification("启动失败：$reason")
                    }
                }.start()
                return START_NOT_STICKY
            }
        }
    }

    override fun onDestroy() {
        // 兜底：服务销毁（进程被杀除外）时尽量带走内核，避免留下无人管的监听端口
        Thread { try { Gateway.stop() } catch (_: Throwable) {} }.start()
        super.onDestroy()
    }

    private fun enterForeground(text: String) {
        val nm = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, "Agate 网关", NotificationManager.IMPORTANCE_LOW)
        )
        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(NOTIFICATION_ID, buildNotification(text), ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        } else {
            startForeground(NOTIFICATION_ID, buildNotification(text))
        }
    }

    private fun updateNotification(text: String) {
        val nm = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        nm.notify(NOTIFICATION_ID, buildNotification(text))
    }

    private fun buildNotification(text: String): Notification {
        val stopIntent = PendingIntent.getService(
            this, 1,
            Intent(this, AgateForegroundService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val builder = Notification.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle("Agate")
            .setContentText(text)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .addAction(Notification.Action.Builder(null, "停止", stopIntent).build())
        packageManager.getLaunchIntentForPackage(packageName)?.let { launch ->
            builder.setContentIntent(
                PendingIntent.getActivity(this, 0, launch, PendingIntent.FLAG_IMMUTABLE)
            )
        }
        return builder.build()
    }
}
