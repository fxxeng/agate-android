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
import org.json.JSONObject

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
        const val ACTION_RESTART = "dev.agate.core.action.RESTART"
        private const val CHANNEL_ID = "agate_gateway"
        private const val NOTIFICATION_ID = 1001
        /** 后台受限降级通知（与网关常驻通知分开，见 [notifyBlocked]）。 */
        private const val CHANNEL_BLOCKED_ID = "agate_blocked"
        private const val NOTIFICATION_ID_BLOCKED = 1002

        /**
         * 拉起前台服务；**后台受限时不抛**，返回 false 由调用方决定下一步。
         *
         * ── 为什么必须自己吞这个异常 ────────────────────────────
         * Android 12+ 禁止「从后台启动前台服务」，违规直接抛
         * `ForegroundServiceStartNotAllowedException`。这个服务有两个**后台**
         * 入口（闹钟 tick、开机自启），闹钟那一路原来没有 try —— 系统在待机
         * 白名单收紧时会把它变成一次崩溃，而用户看到的是「App 莫名其妙闪退」，
         * 与「签到没跑」这件真事对不上。开机那一路包了 runCatching，但那只是
         * 把崩溃挪到了调用方，仍然没有降级动作（用户不知道要去点一下）。
         *
         * 降级方式：发一条普通通知让用户自己点进来恢复（不静默失败）。
         * 通知本身就是「用户手动点 → 前台 Activity → 启动服务」的合法路径。
         */
        fun start(context: Context): Boolean = launch(context, ACTION_START)

        fun stop(context: Context) {
            context.startService(
                Intent(context, AgateForegroundService::class.java).setAction(ACTION_STOP)
            )
        }

        /**
         * 按**当前配置**重启网关（监听地址 / 端口改了就调它）。
         *
         * 服务没在跑时是 no-op —— 改配置只落盘，**不擅自把网关拉起来**：
         * 「用户关着网关改设置」与「开着网关改设置」是两种意图，后者才需要
         * 让新配置立即生效。
         */
        fun restart(context: Context) {
            context.startService(
                Intent(context, AgateForegroundService::class.java).setAction(ACTION_RESTART)
            )
        }

        /** 闹钟入口：借助 exact-while-idle 广播的 FGS 豁免窗口，深睡里也能拉起。 */
        fun tick(context: Context): Boolean = launch(context, ACTION_TICK)

        /**
         * 统一的启动实现（两个入口共用）。返回 false = 被系统拒绝，已发降级通知。
         */
        private fun launch(context: Context, action: String): Boolean {
            val intent = Intent(context, AgateForegroundService::class.java).setAction(action)
            return try {
                context.startForegroundService(intent)
                true
            } catch (_: Exception) {
                // 这里刻意捕 Exception 而不是只捕
                // ForegroundServiceStartNotAllowedException：那个类只在 API 31+
                // 存在，直接写类型会让 minSdk 较低的构建在类加载期就炸；而且
                // 某些 ROM 抛的是 IllegalStateException（同一原因，不同壳）。
                notifyBlocked(context)
                false
            }
        }

        /**
         * 降级通知：告诉用户「保活没起来，点一下恢复」。
         *
         * 用独立的渠道与 id —— 不能复用网关常驻通知（那条不存在，服务都没起来），
         * 也不该在用户点掉之后还留着（点「恢复」会进 App，由那边重新拉起）。
         */
        private fun notifyBlocked(context: Context) {
            runCatching {
                val nm = context.getSystemService(NOTIFICATION_SERVICE) as NotificationManager
                nm.createNotificationChannel(
                    NotificationChannel(
                        CHANNEL_BLOCKED_ID,
                        "Agate 后台受限提醒",
                        NotificationManager.IMPORTANCE_DEFAULT,
                    )
                )
                val open = context.packageManager.getLaunchIntentForPackage(context.packageName)
                val pending = open?.let {
                    PendingIntent.getActivity(
                        context, 2, it,
                        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                    )
                }
                val builder = Notification.Builder(context, CHANNEL_BLOCKED_ID)
                    .setContentTitle("后台保活被系统限制")
                    .setContentText("点此打开 Agate 恢复网关与定时任务")
                    .setSmallIcon(android.R.drawable.stat_notify_sync)
                    .setAutoCancel(true)
                pending?.let { builder.setContentIntent(it) }
                nm.notify(NOTIFICATION_ID_BLOCKED, builder.build())
            }
        }
    }

    /**
     * 本服务最后一次是否成功把网关带起来（`ACTION_RESTART` 的判据：只在
     * **确实在跑**时才重启，否则变成「改个配置顺手把网关启动了」）。
     * 生命周期与内核一一对应：runStart 成功置位，ACTION_STOP / 启动失败清位
     * —— START_STICKY 重启后进程与内核都死了，字段天然回到 false。
     */
    @Volatile
    private var gatewayRunning: Boolean = false

    private var partialLock: PowerManager.WakeLock? = null

    @Suppress("DEPRECATION") // WifiLock 自 API 33 起废弃，但局域网保活仍依赖它
    private var wifiLock: WifiManager.WifiLock? = null
    private val lockGuard = Any()

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                Thread {
                    gatewayRunning = false
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

            ACTION_RESTART -> {
                enterForeground("按新配置重启中…")
                Thread { runRestart() }.start()
                return START_STICKY
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

    /**
     * 按当前配置重启：先停、再起。内核 `stop` 自带端口释放等待（5s 上限
     * 的探测循环，见 jni_bridge 的 F6），所以 stop 返回后端口就是空的，
     * 紧接着 start 不会撞 bind。
     */
    private fun runRestart() {
        if (!gatewayRunning) return
        try {
            Gateway.stop()
        } catch (t: Throwable) {
            android.util.Log.e("Agate", "重启前停止异常", t)
        }
        releaseKeepAwakeLocks()
        runStart()
    }

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
                gatewayRunning = true
                if (result.bindLan) acquireKeepAwakeLocks()
                updateNotification(
                    "网关运行中 · :${result.port}（内核 v${result.version}）" +
                        if (result.bindLan) " · 局域网 0.0.0.0" else ""
                )
            }

            result != null && result.error == "already_running" -> {
                // 幂等重启：内核已在跑，只对齐锁与通知
                gatewayRunning = true
                if (result.bindLan) acquireKeepAwakeLocks() else releaseKeepAwakeLocks()
                updateNotification("网关运行中 · :${result.port}（内核 v${result.version}）")
            }

            else -> {
                gatewayRunning = false
                val reason = result?.let { "${it.stage ?: ""}: ${it.error}" } ?: "异常"
                updateNotification("启动失败：$reason")
                // 告警只在「用户以为网关起来了、其实没起」时发。这里是启动
                // 路径的失败分支，正是那种情况：常驻通知会一直挂着「启动失败」，
                // 但用户在别的页面时看不到，需要一条会响的通知。
                if (GatewayPrefs.alertNotify(this)) {
                    AgateNotify.serviceAlert(this, "网关启动失败", reason)
                }
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
            // 签到结果通知：读设置页那颗开关（GatewayPrefs 同一个键）。
            // 放在收尾处而不是执行前 —— 通知要报的是结果，不是「开始了」。
            notifyCheckinResult(reply)
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

    // ── 签到结果通知 ─────────────────────────────────────────────

    /**
     * 把 `checkinRunNow` 的返回翻成一条通知。
     *
     * 返回形状（见 `jni_bridge::checkin_run_now` 与 `auto_checkin::record_success`）：
     * ```text
     * { ok, done, error?, result? }
     * result = { succeeded, active, skipped, total, failedCount, failed[] }
     * ```
     *
     * ── 两颗开关各管一半（这条分界别改错） ──────────────────────
     * 「签到结果通知」管**例行播报**：跑成功了、跳过了、还在跑 —— 关掉它是
     * 用户不想每天被念一遍，合理。
     * 「服务异常告警」管**出事了**：整条签到链抛错、或本轮有账号失败 ——
     * 关掉「结果通知」不该让失败也一起消失。所以：
     *   · 成功/跳过/进行中 → 看 `checkinNotify`；
     *   · 失败（含部分账号失败）→ 看 `alertNotify`。
     * 两者都关才是「彻底静音」，这是用户显式关两次的结果，尊重它。
     */
    private fun notifyCheckinResult(reply: JSONObject?) {
        val resultNotify = GatewayPrefs.checkinNotify(this)
        val alertNotify = GatewayPrefs.alertNotify(this)
        if (reply == null) {
            if (alertNotify) {
                AgateNotify.serviceAlert(this, "签到失败", "闹钟拉起签到过程出错，详情见应用日志")
            }
            return
        }
        val error = reply.optString("error").takeIf { it.isNotEmpty() }
        val result = reply.optJSONObject("result")
        when {
            error == "busy" -> if (resultNotify) AgateNotify.checkinResult(
                this, "签到跳过", "上一轮签到还在执行，本次未启动",
            )

            // timeout：内核后台仍在跑，只是本次不等它了 —— 不是失败
            error == "timeout" -> if (resultNotify) AgateNotify.checkinResult(
                this, "签到进行中", "本轮耗时长，已转为后台继续，稍后可在签到记录查看",
            )

            result == null -> if (alertNotify) AgateNotify.serviceAlert(
                this, "签到失败", error?.let { "原因：$it" } ?: "内核未返回结果，详情见应用日志",
            )

            else -> {
                val succeeded = result.optInt("succeeded")
                val total = result.optInt("total")
                val skipped = result.optInt("skipped")
                val failedCount = result.optInt("failedCount")
                val parts = buildList {
                    if (succeeded > 0) add("$succeeded 成功")
                    if (skipped > 0) add("$skipped 已签")
                    if (failedCount > 0) add("$failedCount 失败")
                }
                val body = if (parts.isEmpty()) {
                    "共 $total 个账号，无变化"
                } else {
                    parts.joinToString(" · ")
                }
                // 有失败归告警管（响起），全成功归结果管（轻提示）
                if (failedCount > 0) {
                    if (alertNotify) {
                        AgateNotify.checkinResult(this, "定时签到有失败", body, failed = true)
                    }
                } else {
                    if (resultNotify) {
                        AgateNotify.checkinResult(this, "定时签到完成", body)
                    }
                }
            }
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
