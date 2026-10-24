package dev.agate.core

import android.content.Context

/**
 * M1 保活壳层设置（方案 §6.2）。SharedPreferences 即可：
 * 网关唯一的业务状态都在内核 SQLite 里，这里只放「壳怎么活」的偏好。
 *
 * M2 换 Compose 设置页时仍走本对象，UI 只读写、不自持状态。
 */
object GatewayPrefs {

    private const val FILE = "gateway_prefs"

    private const val KEY_BIND_LAN = "bind_lan"
    private const val KEY_BOOT_AUTO_START = "boot_auto_start"
    private const val KEY_CHECKIN_ALARM = "checkin_alarm"
    private const val KEY_ALARM_TIMES = "alarm_times"
    private const val KEY_NEXT_ALARM_AT = "next_alarm_at"

    /** 局域网监听（0.0.0.0 + 内核 panel gate + fail-closed）。默认关。 */
    fun bindLan(ctx: Context): Boolean =
        prefs(ctx).getBoolean(KEY_BIND_LAN, false)

    fun setBindLan(ctx: Context, value: Boolean) {
        prefs(ctx).edit().putBoolean(KEY_BIND_LAN, value).apply()
    }

    /** 开机自启（BOOT_COMPLETED → 拉起前台服务）。默认关，尊重用户。 */
    fun bootAutoStart(ctx: Context): Boolean =
        prefs(ctx).getBoolean(KEY_BOOT_AUTO_START, false)

    fun setBootAutoStart(ctx: Context, value: Boolean) {
        prefs(ctx).edit().putBoolean(KEY_BOOT_AUTO_START, value).apply()
    }

    /** 签到精确闹钟（Doze 兜底）。默认关。 */
    fun checkinAlarmEnabled(ctx: Context): Boolean =
        prefs(ctx).getBoolean(KEY_CHECKIN_ALARM, false)

    fun setCheckinAlarm(ctx: Context, value: Boolean) {
        prefs(ctx).edit().putBoolean(KEY_CHECKIN_ALARM, value).apply()
    }

    /** 闹钟时刻表（北京时间 HH:mm，逗号分隔）。默认对齐方案示例 08:00 / 20:00。 */
    fun alarmTimes(ctx: Context): String =
        prefs(ctx).getString(KEY_ALARM_TIMES, "08:00,20:00") ?: "08:00,20:00"

    fun setAlarmTimes(ctx: Context, csv: String) {
        prefs(ctx).edit().putString(KEY_ALARM_TIMES, csv).apply()
    }

    /** AlarmScheduler 注册时写入的下一次触发时刻（epoch 毫秒），供 UI 展示。 */
    fun nextAlarmAt(ctx: Context): Long =
        prefs(ctx).getLong(KEY_NEXT_ALARM_AT, 0L)

    fun setNextAlarmAt(ctx: Context, atMs: Long) {
        prefs(ctx).edit().putLong(KEY_NEXT_ALARM_AT, atMs).apply()
    }

    private fun prefs(ctx: Context) =
        ctx.applicationContext.getSharedPreferences(FILE, Context.MODE_PRIVATE)
}
