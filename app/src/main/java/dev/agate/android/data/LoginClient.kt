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

    /**
     * POST /api/session/login/start {provider, mode?} → {state, authUrl, edition, provider}
     *
     * [mode] 只有两家读：小浣熊与 CatPaw 的 `"browser"` 表示「在系统浏览器里登录、
     * 回调落本机 http」。其余家传了会被内核明确拒绝（它们内嵌与浏览器打开的是
     * 同一个授权地址，没有第二种形态）—— 因此这里默认 null（= 内嵌），
     * 不让每一家都去回答一次「你支不支持」。
     */
    suspend fun start(provider: String, mode: String? = null): StartResult = withContext(Dispatchers.IO) {
        val body = JSONObject().put("provider", provider)
        if (!mode.isNullOrBlank()) body.put("mode", mode)
        val resp = when (val r = postJson("/api/session/login/start", body)) {
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
     * 壳侧回调转发：`office-raccoon://` 等自定义 scheme 的回跳 WebView 加载不了，
     * 拦下后把完整 URL POST 给内核统一回调端点，由它校验并换凭证。
     *
     * ── 两个字段名必须与内核逐字一致（曾经错在这里）──────────────
     * 内核 `api::session::login_callback` 读的是 **`state`** 与 **`callbackUrl`**
     *（驼峰）。早前这里发的是 `callback_url`（下划线）且**没带 state**，内核第一步
     * 就报 400「缺少 state」—— 表现为小浣熊内嵌登录「点了、页面走完了、却什么都没发生」。
     * 改字段名是不够的，state 必须由调用方（登录会话）给出：内核靠它把这次回调
     * 认到一个**进行中**的任务上，缺了它任何回跳都无主。
     */
    suspend fun forwardCallback(state: String, callbackUrl: String): Boolean = withContext(Dispatchers.IO) {
        val resp = when (val r = postJson(
            "/api/session/login/callback",
            JSONObject().put("state", state).put("callbackUrl", callbackUrl),
        )) {
            is HttpResult.Ok -> r.json
            else -> return@withContext false
        }
        // 成功响应是 `{accountId}`（没有 success 字段）；这里只关心「内核收下了」——
        // 真正的登录结果仍由 /wait 轮询给出（与所有家的口径一致）。
        resp.has("accountId") || resp.optBoolean("success") || resp.optBoolean("continue")
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
