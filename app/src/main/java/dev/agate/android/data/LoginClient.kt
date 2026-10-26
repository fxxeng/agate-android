package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
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



    @Volatile private var apiKey: String = ""

    fun configure(apiKey: String) {
        this.apiKey = apiKey
        CoreTransport.configure(apiKey)
    }

    /** call() 的三态：连不上 / HTTP 状态码 / 拿到 JSON —— 401 不再被吞成「网关未运行」。 */
    private sealed class HttpResult {
        data class Ok(val json: JSONObject) : HttpResult()
        data class Status(val code: Int, val json: JSONObject?) : HttpResult()
        object Unreachable : HttpResult()
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
        val resp = when (val r = postJson("/api/session/login/start", JSONObject().put("provider", provider))) {
            is HttpResult.Ok -> r.json
            else -> return@withContext StartResult.Failed(describe(r))
        }
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
        val resp = when (val r = getJson("/api/session/login/wait?state=$state")) {
            is HttpResult.Ok -> r.json
            else -> return@withContext WaitResult.Failed(describe(r))
        }
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
        val resp = when (val r = postJson(
            "/api/session/login/callback",
            JSONObject().put("callback_url", callbackUrl),
        )) {
            is HttpResult.Ok -> r.json
            else -> return@withContext false
        }
        resp.optBoolean("success")
    }

    // P2：经 CoreTransport 内核直调（不经端口，Core 态可用）
    private suspend fun getJson(path: String): HttpResult = transact(CoreTransport.get(path))

    private suspend fun postJson(path: String, body: JSONObject): HttpResult =
        transact(CoreTransport.post(path, body.toString()))

    /** CoreTransport 回复 → 三态：2xx+JSON=Ok；有响应但非 2xx/坏 JSON=Status（401 语义保留）；链路断=Unreachable。 */
    private suspend fun transact(r: CoreTransport.Reply): HttpResult = when (r) {
        is CoreTransport.Reply.Ok -> {
            val json = runCatching { JSONObject(r.text) }.getOrNull()
            if (r.status in 200..299 && json != null) HttpResult.Ok(json) else HttpResult.Status(r.status, json)
        }
        is CoreTransport.Reply.Down -> HttpResult.Unreachable
    }

    private fun errorOf(resp: JSONObject): String =
        resp.optString("error").ifBlank { "内核返回了无法解析的响应" }

    /** 非 Ok 的可读文案：401 是「Key 没过闸门」，不再说成网关没运行。 */
    private fun describe(r: HttpResult): String = when (r) {
        is HttpResult.Ok -> ""
        is HttpResult.Unreachable -> "无法连接内核"
        is HttpResult.Status -> {
            val detail = r.json?.optString("error").orEmpty()
            when {
                r.code == 401 -> "API Key 校验失败：网关已启用 Key，请核对网关设置中的 Key"
                detail.isNotBlank() -> detail
                r.code == 404 -> "内核没有该登录接口（版本过旧？）"
                else -> "内核返回 HTTP ${r.code}"
            }
        }
    }
}
