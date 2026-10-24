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
import android.widget.TextView
import dev.agate.core.AgateForegroundService
import dev.agate.core.Gateway
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONObject
import java.util.concurrent.TimeUnit

/**
 * M0 验收 Demo：无 Compose（M2 才建设计系统），纯框架控件。
 * 验收路径：启动网关 → GET /health → GET /v1/models → POST /v1/chat/completions。
 */
class MainActivity : Activity() {

    private val main = Handler(Looper.getMainLooper())
    private val client = OkHttpClient.Builder()
        .connectTimeout(5, TimeUnit.SECONDS)
        .readTimeout(180, TimeUnit.SECONDS)
        .build()
    private val port = 8080
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
        wrap.addView(
            scroll,
            LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f),
        )
        setContentView(wrap)
        append("Agate M0 Demo 就绪（内核版本待加载后显示）")
        bg { append("Gateway.version() → " + runCatching { Gateway.version() }.getOrElse { "未加载：${it.message}" }) }
    }

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
