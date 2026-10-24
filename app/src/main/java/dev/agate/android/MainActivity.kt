package dev.agate.android

import android.annotation.SuppressLint
import android.app.Activity
import android.content.pm.PackageManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.Gravity
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.Switch
import android.widget.TextView
import dev.agate.core.AgateForegroundService
import dev.agate.core.AlarmScheduler
import dev.agate.core.BatteryWhitelist
import dev.agate.core.Gateway
import dev.agate.core.GatewayPrefs
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONObject
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.concurrent.TimeUnit

/**
 * M1 验收 Demo：无 Compose（M2 才建设计系统），纯框架控件。
 * M0 探测（启动/停止/health/models/chat）+ M1 保活控件
 * （局域网/开机自启/签到闹钟/电池白名单/精确闹钟/tick 链路）。
 */
class MainActivity : Activity() {

    private val main = Handler(Looper.getMainLooper())
    private val client = OkHttpClient.Builder()
        .connectTimeout(5, TimeUnit.SECONDS)
        .readTimeout(180, TimeUnit.SECONDS)
        .build()
    private val port = 8787
    private lateinit var out: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        requestNotificationPermission()

        val wrap = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(48, 64, 48, 32)
        }
        out = TextView(this).apply { setTextIsSelectable(true) }
        val scroll = ScrollView(this).apply { addView(out) }

        fun btn(label: String, fn: () -> Unit): Button =
            Button(this).apply { text = label; setOnClickListener { fn() } }

        fun section(title: String) {
            wrap.addView(TextView(this).apply {
                text = title
                gravity = Gravity.START
                setPadding(0, 24, 0, 8)
            })
        }

        fun toggle(label: String, initial: Boolean, onChange: (Boolean) -> Unit): Switch =
            Switch(this).apply {
                text = label
                isChecked = initial
                setOnCheckedChangeListener { _, checked -> onChange(checked) }
            }

        // ── M0：网关启停与探测 ──
        section("M0 · 网关")
        wrap.addView(btn("启动网关（前台服务）") {
            AgateForegroundService.start(this)
            append("→ 已请求启动，见通知栏；几秒后点下面的探测")
        })
        wrap.addView(btn("停止网关") {
            AgateForegroundService.stop(this)
            append("→ 已请求停止")
        })
        wrap.addView(btn("Gateway.status()") {
            bg {
                append("status → " + runCatching { Gateway.status().toString() }
                    .getOrElse { "异常 ${it.message}" })
            }
        })
        wrap.addView(btn("GET /health") { bg { probe("GET /health") } })
        wrap.addView(btn("GET /v1/models") { bg { probe("GET /v1/models") } })
        wrap.addView(btn("POST /v1/chat/completions（需先配上游账号）") {
            bg {
                val body = JSONObject()
                    .put("model", "gpt-3.5-turbo")
                    .put("stream", false)
                    .put("messages", listOf(mapOf("role" to "user", "content" to "你好，请回复：Agate M0 链路 OK")))
                    .toString()
                probe("POST /v1/chat/completions", body, "application/json".toMediaType())
            }
        })

        // ── M1：保活壳层 ──
        section("M1 · 保活")
        wrap.addView(toggle("局域网监听 0.0.0.0（重启网关生效）", GatewayPrefs.bindLan(this)) { on ->
            GatewayPrefs.setBindLan(this, on)
            append("局域网监听 = $on（重新「启动网关」生效；内核将开面板闸门并在无 API Key 时对 /v1 拒绝服务）")
        })
        wrap.addView(toggle("开机自启", GatewayPrefs.bootAutoStart(this)) { on ->
            GatewayPrefs.setBootAutoStart(this, on)
            append("开机自启 = $on")
        })
        wrap.addView(toggle("签到闹钟（08:00,20:00 北京时间）", GatewayPrefs.checkinAlarmEnabled(this)) { on ->
            GatewayPrefs.setCheckinAlarm(this, on)
            if (on) {
                val at = AlarmScheduler.scheduleNext(this)
                append(
                    if (at != null) {
                        "签到闹钟已注册，下次触发 → " + fmt(at) +
                            if (!AlarmScheduler.canExact(this)) "（非精确：未授精确闹钟权限）" else ""
                    } else {
                        "时刻表非法，未注册"
                    }
                )
            } else {
                AlarmScheduler.cancel(this)
                append("签到闹钟已取消")
            }
        })
        wrap.addView(btn("电池白名单：状态/申请") {
            val ignoring = BatteryWhitelist.isIgnoring(this)
            val asked = BatteryWhitelist.requestIfNeeded(this)
            append(
                when {
                    ignoring -> "已在电池优化白名单（Doze 不限网）"
                    asked -> "已拉起系统申请弹窗，请选「允许」"
                    else -> "未知状态"
                }
            )
        })
        wrap.addView(btn("精确闹钟权限：状态/申请") {
            append(
                if (AlarmScheduler.canExact(this)) {
                    "精确闹钟可用（setExactAndAllowWhileIdle）"
                } else {
                    BatteryWhitelist.requestExactAlarmIfNeeded(this)
                    "未授权精确闹钟 → 已跳设置页（已降级为 setAndAllowWhileIdle）"
                }
            )
        })
        wrap.addView(btn("立即触发 scheduled_tick（闹钟同款链路）") {
            AgateForegroundService.tick(this)
            append("→ 已发 tick（见 logcat tag Agate：闹钟 scheduled_tick → true）")
        })

        wrap.addView(
            scroll,
            LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f),
        )
        setContentView(wrap)
        append("Agate M1 Demo 就绪（内核版本待加载后显示）")
        bg { append("Gateway.version() → " + runCatching { Gateway.version() }.getOrElse { "未加载：${it.message}" }) }
    }

    private fun fmt(epochMs: Long): String =
        SimpleDateFormat("yyyy-MM-dd HH:mm:ss", Locale.CHINA).format(Date(epochMs))

    /** 探测并追加响应（截断到 1500 字符防刷屏）。 */
    private fun probe(label: String, body: String? = null, mediaType: okhttp3.MediaType? = null) {
        val builder = Request.Builder().url("http://127.0.0.1:$port${urlOf(label)}")
        if (body != null) {
            builder.post(body.toRequestBody(mediaType))
        } else {
            builder.get()
        }
        try {
            client.newCall(builder.build()).execute().use { resp ->
                val text = resp.body?.string().orEmpty()
                append(
                    "$label → HTTP ${resp.code}\n" +
                        text.take(1500).let { if (text.length > 1500) "$it…" else it }
                )
            }
        } catch (t: Throwable) {
            append("$label → 失败：${t.message}")
        }
    }

    private fun urlOf(label: String): String = when {
        label.startsWith("GET /health") -> "/health"
        label.startsWith("GET /v1/models") -> "/v1/models"
        else -> "/v1/chat/completions"
    }

    private fun bg(fn: () -> Unit) = Thread { fn() }.start()

    private fun append(line: String) {
        main.post {
            out.append("\n\n────\n$line")
        }
    }

    @SuppressLint("InlinedApi")
    private fun requestNotificationPermission() {
        if (android.os.Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(android.Manifest.permission.POST_NOTIFICATIONS), 1)
        }
    }
}
