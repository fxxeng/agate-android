package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONObject
import java.util.concurrent.TimeUnit

/**
 * 登录三段式客户端（M3-3）：login/start → wait 轮询 → cancel，
 * 外加壳侧回调转发（POST /api/session/login/callback，内核 session.rs 的
 * 「壳侧捕获」通道 —— WebView 拦到自定义 scheme 回跳时把完整 URL 交给内核）。
 *
 * 与 GatewayRepository 同一处 configure（apiKey 过 protected 闸门）。
 * 全部 suspend + IO；不持有状态（登录会话状态归 UI 层）。
 */
object LoginClient {

    private const val PORT = 8787
    private val JSON_MEDIA = "application/json; charset=utf-8".toMediaType()

    private val client by lazy {
        OkHttpClient.Builder()
            .connectTimeout(5, TimeUnit.SECONDS)
            .readTimeout(8, TimeUnit.SECONDS)
            .build()
    }

    @Volatile private var apiKey: String = ""

    fun configure(apiKey: String) {
        this.apiKey = apiKey
    }

    sealed class StartResult {
        data class Started(val state: String, val authUrl: String, val provider: String) : StartResult()
        data class Failed(val reason: String) : StartResult()
    }

    sealed class WaitResult {
        object Pending : WaitResult()
        data class Done(val session: JSONObject?) : WaitResult()
        data class Failed(val reason: String) : WaitResult()
    }

    /** POST /api/session/login/start {provider} → {state, authUrl, edition, provider} */
    suspend fun start(provider: String): StartResult = withContext(Dispatchers.IO) {
        val resp = postJson("/api/session/login/start", JSONObject().put("provider", provider))
            ?: return@withContext StartResult.Failed("无法连接内核（网关未运行？）")
        val data = resp.optJSONObject("data")
            ?: return@withContext StartResult.Failed(errorOf(resp))
        val state = data.optString("state")
        val url = data.optString("authUrl")
        if (state.isBlank() || url.isBlank()) {
            StartResult.Failed("内核未返回授权地址")
        } else {
            StartResult.Started(state, url, data.optString("provider", provider))
        }
    }

    /** GET /api/session/login/wait?state= — 三态 pending / done+error / done+session */
    suspend fun wait(state: String): WaitResult = withContext(Dispatchers.IO) {
        val resp = getJson("/api/session/login/wait?state=$state")
            ?: return@withContext WaitResult.Failed("无法连接内核")
        val data = resp.optJSONObject("data")
            ?: return@withContext WaitResult.Failed(errorOf(resp))
        when {
            data.optBoolean("pending") -> WaitResult.Pending
            data.has("error") -> WaitResult.Failed(data.optString("error").ifBlank { "登录失败" })
            else -> WaitResult.Done(data.optJSONObject("session"))
        }
    }

    /** POST /api/session/login/cancel {state}（尽力而为，失败静默）。 */
    suspend fun cancel(state: String) {
        withContext(Dispatchers.IO) { postJson("/api/session/login/cancel", JSONObject().put("state", state)) }
    }

    /**
     * 壳侧回调转发：zcode:// 等自定义 scheme 的回跳 WebView 加载不了，
     * 拦下后把完整 URL POST 给内核统一回调端点，由它决定下一跳。
     */
    suspend fun forwardCallback(callbackUrl: String): Boolean = withContext(Dispatchers.IO) {
        val resp = postJson(
            "/api/session/login/callback",
            JSONObject().put("callback_url", callbackUrl),
        ) ?: return@withContext false
        resp.optBoolean("success")
    }

    private fun getJson(path: String): JSONObject? = call(
        Request.Builder().url("http://127.0.0.1:$PORT$path").get().build(),
    )

    private fun postJson(path: String, body: JSONObject): JSONObject? = call(
        Request.Builder()
            .url("http://127.0.0.1:$PORT$path")
            .post(body.toString().toRequestBody(JSON_MEDIA))
            .build(),
    )

    private fun call(request: Request): JSONObject? = try {
        client.newCall(request).execute().use { resp ->
            if (!resp.isSuccessful) return null
            val text = resp.body?.string() ?: return null
            if (text.isBlank()) null else JSONObject(text)
        }
    } catch (_: Exception) {
        null
    }

    private fun errorOf(resp: JSONObject): String =
        resp.optString("error").ifBlank { "内核返回了无法解析的响应" }
}
