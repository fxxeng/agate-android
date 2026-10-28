package dev.agate.core

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.os.Build

/**
 * 「签到结果通知」与「服务异常告警」的发送器（设置页第 10 组的两颗开关）。
 *
 * ── 为什么要独立渠道 ──────────────────────────────────────────
 * 常驻通知走 `agate_gateway`（IMPORTANCE_LOW、ongoing、不可滑掉），
 * 结果与告警如果复用那个渠道，会出现两个问题：
 *   1. 常驻通知是 `setOnlyAlertOnce` 的，同渠道新通知的提示音会被压掉；
 *   2. 用户想「关掉签到通知但保留网关常驻」时无从下手 —— 渠道在系统
 *      设置里是整体开关。
 * 所以这里另建 `agate_result`（结果，DEFAULT）与 `agate_alert`
 * （告警，HIGH）两个渠道。用户在系统设置里能分别静音，互不牵连。
 *
 * ── 与偏好开关的关系 ─────────────────────────────────────────
 * 开关读的是 app 模块的 `KeepalivePrefs`，而本文件在 core-gateway ——
 * core-gateway **不能**反向依赖 app（依赖方向是 app → core-gateway）。
 * 所以这里不读偏好，由调用方（处于 app 侧，或 service 里经反射取不到
 * 就默认发）判断要不要发。调用方不判时本类默认「发」，宁可多一条
 * 也不要让用户以为功能坏了。
 *
 * ── 通知 id 段位 ─────────────────────────────────────────────
 * 常驻用 1001；这里结果用 2001、告警用 3001。同 id 会互相覆盖，
 * 所以每类只保留最新一条 —— 这是刻意的：签到每天多轮，堆一屏通知
 * 只会让人去关掉整个渠道。
 */
object AgateNotify {

    private const val CHANNEL_RESULT = "agate_result"
    private const val CHANNEL_ALERT = "agate_alert"

    private const val ID_RESULT = 2001
    private const val ID_ALERT = 3001

    /**
     * 签到结果通知。
     *
     * @param title 标题，如「定时签到完成」
     * @param body  正文，如「3 个账号成功 · 1 个失败」
     * @param failed 是否含失败项 —— 有失败时用告警渠道，让用户注意到
     */
    fun checkinResult(ctx: Context, title: String, body: String, failed: Boolean = false) {
        // 有失败走告警渠道（HIGH，会响）；全成功走结果渠道（DEFAULT，轻提示）。
        // 一个渠道两种重要度做不到，所以按结果分流 —— 成功不该吵醒用户。
        post(
            ctx = ctx,
            channelId = if (failed) CHANNEL_ALERT else CHANNEL_RESULT,
            channelName = if (failed) "签到失败提醒" else "签到结果",
            importance = if (failed) NotificationManager.IMPORTANCE_HIGH else NotificationManager.IMPORTANCE_DEFAULT,
            id = if (failed) ID_ALERT else ID_RESULT,
            title = title,
            body = body,
        )
    }

    /**
     * 服务异常告警。
     *
     * HIGH 重要度 + 不走 `setOnlyAlertOnce`：网关挂了是用户需要立刻
     * 知道的事，不能因为「刚才已经提示过一次」就静默。
     */
    fun serviceAlert(ctx: Context, title: String, body: String) {
        post(
            ctx = ctx,
            channelId = CHANNEL_ALERT,
            channelName = "服务异常告警",
            importance = NotificationManager.IMPORTANCE_HIGH,
            id = ID_ALERT,
            title = title,
            body = body,
        )
    }

    private fun post(
        ctx: Context,
        channelId: String,
        channelName: String,
        importance: Int,
        id: Int,
        title: String,
        body: String,
    ) {
        val app = ctx.applicationContext
        val nm = app.getSystemService(Context.NOTIFICATION_SERVICE) as? NotificationManager
            ?: return

        if (Build.VERSION.SDK_INT >= 26) {
            // 渠道一旦创建，重要度**不能**再改（系统限制）。重复 create 是幂等的，
            // 所以这里每次都 create —— 已存在时系统忽略，不存在时补齐。
            nm.createNotificationChannel(
                NotificationChannel(channelId, channelName, importance).apply {
                    setShowBadge(true)
                }
            )
        }

        val builder = Notification.Builder(app, channelId)
            .setSmallIcon(R.drawable.ic_agate_logo)
            .setContentTitle(title)
            .setContentText(body)
            .setStyle(Notification.BigTextStyle().bigText(body))
            .setAutoCancel(true)

        app.packageManager.getLaunchIntentForPackage(app.packageName)?.let { launch ->
            builder.setContentIntent(
                PendingIntent.getActivity(
                    app, id, launch,
                    PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                )
            )
        }

        runCatching { nm.notify(id, builder.build()) }
    }
}
