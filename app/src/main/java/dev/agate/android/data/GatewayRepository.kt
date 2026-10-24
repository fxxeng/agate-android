package dev.agate.android.data

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.sse.EventSource
import okhttp3.sse.EventSourceListener
import okhttp3.sse.EventSources
import org.json.JSONObject
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/**
 * M3 事件总线仓库（方案 v4.0 §3.2 / O9）：SSE 为主 + 轮询降级的单一数据源。
 *
 * 生命周期：App 级单例；FGS 拉起内核后 configure + start，停机时 stop。
 * 「退后台自动断开」属页面生命周期感知（M3 控制台页接入时再挂），本层
 * 先保证常连语义与 FGS 同寿。
 *
 * 链路状态机：
 *   DISCONNECTED → CONNECTING → CONNECTED
 *        ↘ RETRYING（指数退避 1s→30s，连续 4 败降级）
 *             ↘ POLLING（/health 5s 探测，连续 3 次存活回 CONNECTING）
 *
 * 事件语义与服务端 events_api 对齐：hello / gateway_started / request_done /
 * checkin_done /（lagged 与 ping 不进列表）。
 */
object GatewayRepository {

    enum class LinkState { DISCONNECTED, CONNECTING, CONNECTED, RETRYING, POLLING }

    /** 一条已接收事件；raw 保留原始 JSON 供后续页面按需取字段。 */
    data class RecentEvent(
        val type: String,
        val tsMs: Long,
        val summary: String,
        val raw: String,
    )

    private const val PORT = 8787
    private const val MAX_EVENTS = 20
    private const val MAX_SSE_FAILURES = 4

    private val _link = MutableStateFlow(LinkState.DISCONNECTED)
    val link: StateFlow<LinkState> = _link

    private val _events = MutableStateFlow<List<RecentEvent>>(emptyList())
    val events: StateFlow<List<RecentEvent>> = _events

    @Volatile private var apiKey: String = ""

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** SSE 长连接专用：读超时 45s = 服务端 keepalive(15s) 的 3 倍，只在真断流时判死。 */
    private val sseClient by lazy {
        OkHttpClient.Builder()
            .connectTimeout(5, TimeUnit.SECONDS)
            .readTimeout(45, TimeUnit.SECONDS)
            .retryOnConnectionFailure(false)
            .build()
    }

    /** 降级轮询用短超时客户端。 */
    private val pollClient by lazy {
        OkHttpClient.Builder()
            .connectTimeout(3, TimeUnit.SECONDS)
            .readTimeout(5, TimeUnit.SECONDS)
            .build()
    }

    private var eventSource: EventSource? = null
    private var retryJob: Job? = null
    private var pollJob: Job? = null
    private val failures = AtomicInteger(0)
    @Volatile private var started = false

    /** 壳侧凭证：FGS 启动内核前把 GatewayPrefs 的 Key 同步进来。 */
    fun configure(apiKey: String) {
        this.apiKey = apiKey
    }

    fun start() {
        started = true
        failures.set(0)
        connect()
    }

    fun stop() {
        started = false
        eventSource?.cancel()
        eventSource = null
        retryJob?.cancel()
        retryJob = null
        pollJob?.cancel()
        pollJob = null
        _link.value = LinkState.DISCONNECTED
    }

    private fun connect() {
        if (!started) return
        pollJob?.cancel()
        pollJob = null
        _link.value = if (failures.get() == 0) LinkState.CONNECTING else LinkState.RETRYING
        val builder = Request.Builder().url("http://127.0.0.1:$PORT/api/events")
        if (apiKey.isNotBlank()) builder.header("Authorization", "Bearer $apiKey")
        eventSource?.cancel()
        eventSource = EventSources.createFactory(sseClient).newEventSource(
            builder.build(),
            object : EventSourceListener() {
                override fun onOpen(eventSource: EventSource, response: Response) {
                    failures.set(0)
                    _link.value = LinkState.CONNECTED
                }

                override fun onEvent(eventSource: EventSource, id: String?, type: String?, data: String) {
                    // ping 是心跳；lagged 提示全量刷新（M3 控制台页接入时处理），不进列表
                    if (type == "ping" || type == "lagged") return
                    appendEvent(data)
                }

                override fun onClosed(eventSource: EventSource) {
                    onLinkBroken("closed")
                }

                override fun onFailure(eventSource: EventSource, t: Throwable?, response: Response?) {
                    onLinkBroken(t?.message ?: ("failure " + (response?.code ?: "")))
                }
            },
        )
    }

    private fun onLinkBroken(@Suppress("UNUSED_PARAMETER") reason: String) {
        if (!started) return
        val n = failures.incrementAndGet()
        if (n >= MAX_SSE_FAILURES) {
            degradeToPolling()
            return
        }
        _link.value = LinkState.RETRYING
        // 指数退避 2s→30s（n 从 1 起：1000<<1=2s）
        val delayMs = (1000L shl n.coerceAtMost(5)).coerceAtMost(30_000L)
        retryJob?.cancel()
        retryJob = scope.launch {
            delay(delayMs)
            connect()
        }
    }

    /** O9 降级轮询：/health 是 public 免鉴权端点，探测成本最低。 */
    private fun degradeToPolling() {
        _link.value = LinkState.POLLING
        pollJob?.cancel()
        pollJob = scope.launch {
            var streak = 0
            while (isActive) {
                val alive = try {
                    pollClient.newCall(
                        Request.Builder().url("http://127.0.0.1:$PORT/health").build(),
                    ).execute().use { it.isSuccessful }
                } catch (_: Exception) {
                    false
                }
                streak = if (alive) streak + 1 else 0
                if (streak >= 3) {
                    failures.set(0)
                    connect()
                    return@launch
                }
                delay(5_000L)
            }
        }
    }

    private fun appendEvent(data: String) {
        val obj = try {
            JSONObject(data)
        } catch (_: Exception) {
            null
        } ?: return
        val type = obj.optString("type", "unknown")
        val ts = obj.optLong("tsMs", 0L)
        val body = obj.optJSONObject("data")
        _events.value = (
            listOf(RecentEvent(type, ts, summarize(type, body), data)) + _events.value
        ).take(MAX_EVENTS)
    }

    private fun summarize(type: String, body: JSONObject?): String = when (type) {
        "hello" -> "链路就绪（内核 v${body?.optString("version").orEmpty()}）"
        "gateway_started" -> {
            val port = body?.optInt("port", 0) ?: 0
            val lan = if (body?.optBoolean("bindLan") == true) " · 局域网" else ""
            "网关已启动 :$port$lan"
        }
        "request_done" -> {
            val status = body?.optInt("status", 0) ?: 0
            val model = body?.optString("model").orEmpty().ifBlank { "?" }
            val provider = body?.optString("provider").orEmpty()
            val ms = body?.optLong("durationMs", 0L) ?: 0L
            val flag = if (status in 200..299) "✓" else "✗"
            "$flag $model${if (provider.isNotBlank()) " · $provider" else ""} · ${ms}ms · $status"
        }
        "checkin_done" -> {
            val ok = body?.optInt("succeeded", 0) ?: 0
            val total = body?.optInt("total", 0) ?: 0
            "签到轮次完成 $ok/$total"
        }
        else -> type
    }
}
