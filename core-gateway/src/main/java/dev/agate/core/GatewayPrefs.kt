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
    private const val KEY_API_KEY = "api_key"
    private const val KEY_BOOT_AUTO_START = "boot_auto_start"
    private const val KEY_CHECKIN_ALARM = "checkin_alarm"
    private const val KEY_ALARM_TIMES = "alarm_times"
    private const val KEY_NEXT_ALARM_AT = "next_alarm_at"

    private const val KEY_PROVIDER_ORDER = "provider_order"
    private const val KEY_RECENT_PROVIDERS = "recent_providers"
    private const val KEY_WIZARD_DISMISSED = "wizard_dismissed"
    private const val KEY_THEME_MODE = "theme_mode"
    private const val KEY_PORT = "port"
    private const val KEY_SCHEDULER_ON = "scheduler_on"
    private const val KEY_CHECKIN_NOTIFY = "checkin_notify_enabled"
    private const val KEY_ALERT_NOTIFY = "alert_notify_enabled"

    /** 局域网监听（0.0.0.0 + 内核 panel gate + fail-closed）。默认关。 */
    /**
     * 本机网关 Key（M3）：首次调用生成 `agate-` + 32 位 hex 并持久化，
     * 之后跨重启稳定。经 nativeStart 的 api_key 注入内核（env 兼容模式），
     * 作用：① 局域网形态下壳自身的 /api、/v1 请求凭它过闸门；② 展示在
     * 「访问密钥」页供局域网设备配置。仅存 SharedPreferences（M3/M5 的
     * KeyStore 加密为增强项，见方案 §3.7）。
     */
    fun apiKey(ctx: Context): String {
        prefs(ctx).getString(KEY_API_KEY, null)?.let { return it }
        val alphabet = "0123456789abcdef"
        val key = "agate-" + (1..32).map { alphabet.random() }.joinToString("")
        prefs(ctx).edit().putString(KEY_API_KEY, key).apply()
        return key
    }

    fun bindLan(ctx: Context): Boolean =
        prefs(ctx).getBoolean(KEY_BIND_LAN, false)

    fun setBindLan(ctx: Context, value: Boolean) {
        prefs(ctx).edit().putBoolean(KEY_BIND_LAN, value).apply()
    }

    /** 提供商页分组显示顺序（M3-3R）：家 id 逗号串，缺省按 PresetRegistry 顺序。 */
    fun providerOrder(ctx: Context): List<String> =
        prefs(ctx).getString(KEY_PROVIDER_ORDER, null)?.split(',')?.filter { it.isNotBlank() } ?: emptyList()

    fun setProviderOrder(ctx: Context, order: List<String>) {
        prefs(ctx).edit().putString(KEY_PROVIDER_ORDER, order.joinToString(",")).apply()
    }

    /** 最近添加的家（M3-3R 新增提供商页快捷 chips）：家 id 逗号串，最新在前，至多 4 个。 */
    fun recentProviders(ctx: Context): List<String> =
        prefs(ctx).getString(KEY_RECENT_PROVIDERS, null)?.split(',')?.filter { it.isNotBlank() } ?: emptyList()

    /** 首页新手向导卡是否被 ✕ 关闭（v44 obDismiss 语义，跨重启记住）。 */
    fun wizardDismissed(ctx: Context): Boolean = prefs(ctx).getBoolean(KEY_WIZARD_DISMISSED, false)

    /** App 主题档（F13）：存壳层 AgateThemeMode.name，默认跟随系统；枚举映射在壳层。 */
    fun themeMode(ctx: Context): String =
        prefs(ctx).getString(KEY_THEME_MODE, "FOLLOW_SYSTEM") ?: "FOLLOW_SYSTEM"

    fun setThemeMode(ctx: Context, value: String) {
        prefs(ctx).edit().putString(KEY_THEME_MODE, value).apply()
    }

    /**
     * 内核端口（B 包①单一来源）：Application 初始化 Core、FGS 启动网关、
     * 设置页展示共用这一份；改后重启网关生效。默认 8787。
     */
    fun port(ctx: Context): Int = prefs(ctx).getInt(KEY_PORT, 8787)

    fun setPort(ctx: Context, value: Int) {
        prefs(ctx).edit().putInt(KEY_PORT, value.coerceIn(1, 65535)).apply()
    }

    /**
     * 后台任务调度开关（B 包⑤拍板：独立于网关，默认开）。
     * App 进程活着就跑（凭证维护/模型刷新/签到调度）；关掉即纯手动。
     */
    fun schedulerOn(ctx: Context): Boolean = prefs(ctx).getBoolean(KEY_SCHEDULER_ON, true)

    fun setSchedulerOn(ctx: Context, value: Boolean) {
        prefs(ctx).edit().putBoolean(KEY_SCHEDULER_ON, value).apply()
    }

    fun setWizardDismissed(ctx: Context, value: Boolean) {
        prefs(ctx).edit().putBoolean(KEY_WIZARD_DISMISSED, value).apply()
    }

    fun pushRecentProvider(ctx: Context, id: String) {
        val cur = recentProviders(ctx).toMutableList()
        cur.remove(id)
        cur.add(0, id)
        prefs(ctx).edit().putString(KEY_RECENT_PROVIDERS, cur.take(4).joinToString(",")).apply()
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

    /**
     * 签到结果通知开关（设置页「保活与通知」）。
     *
     * 放在 GatewayPrefs 而不是 app 模块：读它的是
     * `AgateForegroundService.runTick` 的收尾处，那是**没有 UI 的壳层组件**，
     * 而依赖方向是 app → core-gateway，core-gateway 读不到 app 的偏好对象。
     * 设置页那边经同一个 SharedPreferences 文件读写，键不重复定义。
     *
     * 默认开：定时签到多在息屏时跑完，不给通知用户根本不知道结果。
     */
    fun checkinNotify(ctx: Context): Boolean =
        prefs(ctx).getBoolean(KEY_CHECKIN_NOTIFY, true)

    fun setCheckinNotify(ctx: Context, value: Boolean) {
        prefs(ctx).edit().putBoolean(KEY_CHECKIN_NOTIFY, value).apply()
    }

    /** 服务异常告警开关（网关启动失败 / 全部账号不可用）。默认开。 */
    fun alertNotify(ctx: Context): Boolean =
        prefs(ctx).getBoolean(KEY_ALERT_NOTIFY, true)

    fun setAlertNotify(ctx: Context, value: Boolean) {
        prefs(ctx).edit().putBoolean(KEY_ALERT_NOTIFY, value).apply()
    }

    private fun prefs(ctx: Context) =
        ctx.applicationContext.getSharedPreferences(FILE, Context.MODE_PRIVATE)
}
