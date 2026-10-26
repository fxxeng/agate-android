package dev.agate.core

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager

/**
 * 网关前台服务（M1 保活壳层，方案 §6.2 三级保活）：
 *
 *  1. 常驻轻保活：specialUse FGS 挂常驻通知，平时**不持** PARTIAL_WAKE_LOCK，
 *     息屏正常休眠省电；
 *  2. 按需持锁：bindLan 局域网模式（0.0.0.0）持 PARTIAL_WAKE_LOCK +
 *     WifiLock(WIFI_MODE_FULL_HIGH_PERF)，保障同网段设备随时接入；
 *     本机模式（127.0.0.1）不持锁；
 *  3. 定点唤醒（B 包 F2/F3）：ACTION_TICK 走 Core 态直调签到 —— 不拉
 *     网关、同步等内核跑完（WakeLock 覆盖全程）；网关没在跑则收尾即退前台。
 *
 * START_STICKY：进程被杀后系统重启服务（intent 为 null → 按当前设置拉内核）；
 * 用户主动停止（ACTION_STOP）走 START_NOT_STICKY，不复活。
 */
class AgateForegroundService : Service() {

    companion object {
        const val ACTION_START = "dev.agate.core.action.START"
        const val ACTION_STOP = "dev.agate.core.action.STOP"
        const val ACTION_TICK = "dev.agate.core.action.TICK"
        private const val CHANNEL_ID = "agate_gateway"
        private const val NOTIFICATION_ID = 1001

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

        /** 闹钟入口：借助 exact-while-idle 广播的 FGS 豁免窗口，深睡里也能拉起。 */
        fun tick(context: Context) {
            context.startForegroundService(
                Intent(context, AgateForegroundService::class.java).setAction(ACTION_TICK)
            )
        }
    }

    private var partialLock: PowerManager.WakeLock? = null

    @Suppress("DEPRECATION") // WifiLock 自 API 33 起废弃，但局域网保活仍依赖它
    private var wifiLock: WifiManager.WifiLock? = null
    private val lockGuard = Any()

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
                    releaseKeepAwakeLocks()
                    stopForeground(STOP_FOREGROUND_REMOVE)
                    stopSelf()
                }.start()
                return START_NOT_STICKY
            }

            ACTION_TICK -> {
                // 先进前台（exact alarm 的 10s 豁免窗口内；已在前台时重调无害）
                enterForeground(currentNotificationText("签到 tick 处理中…"))
                Thread { runTick() }.start()
                return START_STICKY
            }

            else -> {
                // ACTION_START，或 null intent（START_STICKY 被杀重启）
                enterForeground("启动中…")
                Thread { runStart() }.start()
                return START_STICKY
            }
        }
    }

    // ── 启动路径 ─────────────────────────────────────────────────

    private fun runStart() {
        val bindLan = GatewayPrefs.bindLan(this)
        val result = try {
            Gateway.start(
                filesDir.resolve("agate").absolutePath,
                port = GatewayPrefs.port(this),
                bindLan = bindLan,
                apiKey = GatewayPrefs.apiKey(this),
            )
        } catch (t: Throwable) {
            android.util.Log.e("Agate", "内核启动异常（.so 加载失败？）", t)
            null
        }
        when {
            result != null && result.ok -> {
                if (result.bindLan) acquireKeepAwakeLocks()
                updateNotification(
                    "网关运行中 · :${result.port}（内核 v${result.version}）" +
                        if (result.bindLan) " · 局域网 0.0.0.0" else ""
                )
            }

            result != null && result.error == "already_running" -> {
                // 幂等重启：内核已在跑，只对齐锁与通知
                if (result.bindLan) acquireKeepAwakeLocks() else releaseKeepAwakeLocks()
                updateNotification("网关运行中 · :${result.port}（内核 v${result.version}）")
            }

            else -> {
                val reason = result?.let { "${it.stage ?: ""}: ${it.error}" } ?: "异常"
                updateNotification("启动失败：$reason")
            }
        }
    }

    // ── 闹钟 tick 路径 ───────────────────────────────────────────

    private fun runTick() {
        // F3：WakeLock 覆盖整轮签到（120s 内核超时 + 余量），跑完才释放
        val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
        val tickLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "agate:tick").apply {
            setReferenceCounted(false)
            acquire(150_000L)
        }
        try {
            // F2：tick 不再拉起网关 —— Core 态直调签到执行体，网关保持原状
            //（没在跑就继续没在跑；Core 未就绪时这里兜底初始化）
            val init = try {
                Gateway.coreInit(
                    filesDir.resolve("agate").absolutePath,
                    GatewayPrefs.port(this),
                    GatewayPrefs.schedulerOn(this),
                )
            } catch (t: Throwable) {
                android.util.Log.e("Agate", "coreInit 异常", t)
                null
            }
            if (init == null || !init.optBoolean("ok")) {
                updateNotification("闹钟拉起 Core 失败：${init?.optString("error") ?: "异常"}")
                return
            }
            val reply = try {
                Gateway.checkinRunNow(120_000L)
            } catch (t: Throwable) {
                android.util.Log.e("Agate", "checkinRunNow 异常", t)
                null
            }
            android.util.Log.i(
                "Agate",
                "闹钟签到 → ok=${reply?.optBoolean("ok") ?: false} " +
                    "done=${reply?.optBoolean("done") ?: false}" +
                    (reply?.optString("error")?.takeIf { it.isNotEmpty() }?.let { " ($it)" } ?: ""),
            )
            // 签到收尾：网关没在跑就收掉这个短命前台（跑着则保留网关通知）
            val running = try {
                Gateway.status().running
            } catch (_: Throwable) {
                false
            }
            if (!running) {
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf()
            }
        } finally {
            if (tickLock.isHeld) tickLock.release()
        }
    }

    // ── 局域网按需持锁 ───────────────────────────────────────────

    @Suppress("DEPRECATION")
    private fun acquireKeepAwakeLocks() {
        synchronized(lockGuard) {
            if (partialLock?.isHeld != true) {
                val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
                partialLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "agate:lan").apply {
                    setReferenceCounted(false)
                    acquire()
                }
            }
            if (wifiLock?.isHeld != true) {
                val wm = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
                wifiLock = wm.createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "agate:lan").apply {
                    setReferenceCounted(false)
                    acquire()
                }
            }
        }
    }

    @Suppress("DEPRECATION")
    private fun releaseKeepAwakeLocks() {
        synchronized(lockGuard) {
            runCatching { partialLock?.takeIf { it.isHeld }?.release() }
            runCatching { wifiLock?.takeIf { it.isHeld }?.release() }
            partialLock = null
            wifiLock = null
        }
    }

    // ── 通知 ─────────────────────────────────────────────────────

    override fun onDestroy() {
        // 兜底：服务销毁（进程被杀除外）时尽量带走内核与锁
        Thread { try { Gateway.stop() } catch (_: Throwable) {} }.start()
        releaseKeepAwakeLocks()
        super.onDestroy()
    }

    private fun currentNotificationText(fallback: String): String {
        val st = try {
            Gateway.status()
        } catch (_: Throwable) {
            null
        }
        return if (st?.running == true) {
            "网关运行中 · :${st.port}（内核 v${st.version}）" +
                if (st.bindLan) " · 局域网 0.0.0.0" else ""
        } else {
            fallback
        }
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
            .setSmallIcon(R.drawable.ic_agate_logo)
            .setContentTitle("Agate")
            .setContentText(text)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .addAction(Notification.Action.Builder(null, "停止", stopIntent).build())
        // contentTap 打开 App；再挂一个显式「打开」按钮（方案 §2 常驻通知规范）
        packageManager.getLaunchIntentForPackage(packageName)?.let { launch ->
            val openIntent = PendingIntent.getActivity(
                this, 2, launch,
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
            builder.setContentIntent(openIntent)
            builder.addAction(Notification.Action.Builder(null, "打开", openIntent).build())
        }
        return builder.build()
    }
}
