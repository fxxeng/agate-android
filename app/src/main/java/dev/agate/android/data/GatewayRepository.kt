package dev.agate.android.data

import dev.agate.android.data.dto.AccountDto
import dev.agate.android.data.dto.AccountDtos
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
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong

/**
 * M3 事件总线仓库（方案 v4.0 §3.2 / O9）：SSE 为主 + 轮询降级的单一数据源。
 *
 * 生命周期：App 级单例；FGS 拉起内核后 configure + start，停机时 stop。
 * 「退后台自动断开」属页面生命周期感知：控制台页在前台时 [startPolling]，
 * DisposableEffect 销毁时 [stopPolling]（SSE 常连不随页面走）。
 *
 * 链路状态机：
 *   DISCONNECTED → CONNECTING → CONNECTED
 *        ↘ RETRYING（指数退避 1s→30s，连续 4 败降级）
 *             ↘ POLLING（/health 5s 探测，连续 3 次存活回 CONNECTING）
 *
 * 事件语义与服务端 events_api 对齐：hello / gateway_started / request_done /
 * checkin_done /（ping 不进列表；lagged 触发快照全量刷新，不进列表）。
 *
 * M3-2 起：控制台快照轮询（/api/session + /health + /api/stats 各端点）也由本层
 * 承载 —— 页面只读 StateFlow，保证「单一数据源」（方案 §7.1）。
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

    /**
     * 控制台首屏快照：聚合四个只读端点。字段独立解析、失败项沿用上一份
     * 快照的值（内核滚动升级/瞬时抖动时不闪成 0）。
     */
    data class ConsoleSnapshot(
        val fetchedAtMs: Long = 0,
        /** /api/stats/summary?range=today → overview */
        val requestsToday: Long = 0,
        val successfulToday: Long = 0,
        val tokensToday: Long = 0,
        /** /api/session → accounts（enabled 计活跃，v44 语义「活跃账号 X/Y」） */
        val accountsTotal: Int = 0,
        val accountsEnabled: Int = 0,
        val routedAccountId: String? = null,
        val routedAccountName: String? = null,
        /** /api/session → health.upstreamConfigured / session.loggedIn */
        val upstreamConfigured: Boolean? = null,
        val loggedIn: Boolean? = null,
        val defaultModel: String? = null,
        /** /health（裸值信封） */
        val authRequired: Boolean? = null,
        val modelCount: Int = 0,
        /** /api/stats/requests → running（在途请求数 = 排队水位） */
        val inFlight: Int = 0,
        /** M3-3：全量账号（按家分组渲染用），解析统一走 AccountDtos */
        val accountList: List<AccountDto> = emptyList(),
        /** provider id → 账号总数（含禁用，分组标题口径） */
        val providerCounts: Map<String, Int> = emptyMap(),
        /** provider id → 该家队首账号 id（★ 判据） */
        val currentAccountIds: Map<String, String> = emptyMap(),
        /** /api/routing/queue → 实时出战队列（首页 v44 hq 卡）；解析失败沿用上份 */
        val routingQueue: List<QueueRow> = emptyList(),
        /** /api/stats/summary overview → 平均时延（字段名随上游，读不到为 null 显示占位） */
        val avgLatencyMs: Long? = null,
    )

    /** v44 .qrow 一行：序号由渲染端下标，这里只带语义字段。 */
    data class QueueRow(
        val provider: String,
        val account: String,
        val model: String? = null,
        val note: String? = null,
    )

    private const val PORT = 8787
    private const val MAX_EVENTS = 20
    private const val MAX_SSE_FAILURES = 4
    private const val SNAPSHOT_INTERVAL_MS = 5_000L
    /** request_done 触发的快照刷新节流：流式请求收尾很密，逐条刷新没有意义。 */
    private const val EVENT_REFRESH_THROTTLE_MS = 2_000L

    private val _link = MutableStateFlow(LinkState.DISCONNECTED)
    val link: StateFlow<LinkState> = _link

    private val _events = MutableStateFlow<List<RecentEvent>>(emptyList())
    val events: StateFlow<List<RecentEvent>> = _events

    private val _snapshot = MutableStateFlow(ConsoleSnapshot())
    val snapshot: StateFlow<ConsoleSnapshot> = _snapshot

    /** 快照整体拉取失败的原因（网络层失败，非字段缺失）；成功后清空。 */
    private val _snapshotError = MutableStateFlow<String?>(null)
    val snapshotError: StateFlow<String?> = _snapshotError

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

    /** 降级轮询与快照共用的短超时客户端（全是 loopback 小响应）。 */
    private val pollClient by lazy {
        OkHttpClient.Builder()
            .connectTimeout(3, TimeUnit.SECONDS)
            .readTimeout(5, TimeUnit.SECONDS)
            .build()
    }

    private var eventSource: EventSource? = null
    private var retryJob: Job? = null
    private var pollJob: Job? = null
    private var snapshotJob: Job? = null
    private val failures = AtomicInteger(0)
    private val refreshing = AtomicBoolean(false)
    private val lastEventRefresh = AtomicLong(0)
    @Volatile private var started = false

    /** 壳侧凭证：FGS 启动内核前把 GatewayPrefs 的 Key 同步进来。 */
    fun configure(apiKey: String) {
        this.apiKey = apiKey
    }

    fun start() {
        started = true
        failures.set(0)
        connect()
        refreshOnce()
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

    /** 内核运行态对齐（控制台页 5s 轮询 JNI status 后回填）：起 SSE / 断 SSE。 */
    fun onGatewayRunning(running: Boolean) {
        if (running && !started) {
            start()
        } else if (!running && started) {
            stop()
            _snapshotError.value = "网关未运行"
        }
    }

    /** 页面生命周期感知的快照轮询（方案 §3.1：前台 STARTED 刷新、退后台挂起）。 */
    fun startPolling() {
        if (snapshotJob?.isActive == true) return
        snapshotJob = scope.launch {
            while (isActive) {
                if (started) refreshOnce()
                delay(SNAPSHOT_INTERVAL_MS)
            }
        }
    }

    fun stopPolling() {
        snapshotJob?.cancel()
        snapshotJob = null
    }

    /**
     * 拉一份控制台快照。四个端点逐一独立解析：失败的项沿用上一份快照的值；
     * 全部失败才置 [snapshotError]。在途刷新时重复调用直接跳过（5s 定时 +
     * 事件触发可能撞车）。
     */
    fun refreshOnce() {
        if (!refreshing.compareAndSet(false, true)) return
        scope.launch {
            try {
                val prev = _snapshot.value
                var anySuccess = false
                var session: JSONObject? = null
                var health: JSONObject? = null
                var summary: JSONObject? = null
                var queue: JSONObject? = null

                getJson("/api/session", envelope = true)?.let { session = it; anySuccess = true }
                getJson("/health", envelope = false)?.let { health = it; anySuccess = true }
                getJson("/api/stats/summary?range=today", envelope = true)?.let { summary = it; anySuccess = true }
                getJson("/api/stats/requests?limit=1", envelope = true)?.let { queue = it; anySuccess = true }
                var routing: JSONObject? = null
                getJson("/api/routing/queue", envelope = true)?.let { routing = it }

                if (!anySuccess) {
                    _snapshotError.value = "无法连接内核（网关未运行？）"
                    return@launch
                }
                _snapshotError.value = null

                // accounts 是对象信封 {accounts[], providers[], currentAccountIds{}}
                // （内核 account_store::snapshot）。M3-2 曾按裸数组解析：零账号时恰好
                // 都对，有账号后 KPI 恒 0 —— M3-3 起解析统一收口 AccountDtos。
                val parsed = AccountDtos.parseSession(session)
                val total = parsed.accounts.size
                var enabled = 0
                var routedName: String? = null
                if (total > 0) {
                    val routedId = session?.optString("routedAccountId", "").orEmpty()
                    for (acc in parsed.accounts) {
                        if (acc.enabled) enabled++
                        if (routedId.isNotEmpty() && routedId == acc.id) routedName = acc.displayName
                    }
                }
                val overview = summary?.optJSONObject("overview")
                _snapshot.value = ConsoleSnapshot(
                    fetchedAtMs = System.currentTimeMillis(),
                    requestsToday = overview?.optLong("requests") ?: prev.requestsToday,
                    successfulToday = overview?.optLong("successful") ?: prev.successfulToday,
                    tokensToday = overview?.optLong("tokens") ?: prev.tokensToday,
                    accountsTotal = if (session != null) total else prev.accountsTotal,
                    accountsEnabled = if (session != null) enabled else prev.accountsEnabled,
                    routedAccountId = session?.optString("routedAccountId", "")?.ifBlank { null }
                        ?: prev.routedAccountId,
                    routedAccountName = routedName ?: prev.routedAccountName,
                    upstreamConfigured = session?.optJSONObject("health")?.optBoolean("upstreamConfigured")
                        ?: prev.upstreamConfigured,
                    loggedIn = session?.optJSONObject("session")?.let { it.optBoolean("loggedIn") }
                        ?: prev.loggedIn,
                    defaultModel = session?.optString("defaultModel", "")?.ifBlank { null }
                        ?: prev.defaultModel,
                    authRequired = health?.let { it.optBoolean("authRequired") } ?: prev.authRequired,
                    modelCount = health?.optInt("models") ?: prev.modelCount,
                    inFlight = queue?.optInt("running") ?: prev.inFlight,
                    accountList = if (session != null) parsed.accounts else prev.accountList,
                    providerCounts = if (session != null) parsed.providerCounts else prev.providerCounts,
                    currentAccountIds = if (session != null) parsed.currentAccountIds else prev.currentAccountIds,
                    routingQueue = parseRoutingQueue(routing) ?: prev.routingQueue,
                    avgLatencyMs = firstNonNull(
                        overview?.optLong("avgLatencyMs", -1L),
                        overview?.optLong("latencyMs", -1L),
                        overview?.optLong("avgDurationMs", -1L),
                    ) ?: prev.avgLatencyMs,
                )
            } finally {
                refreshing.set(false)
            }
        }
    }

    /** 队列端点字段名未定案，按候选键防御解析；端点缺失/结构变化返回 null（沿用上份）。 */
    private fun parseRoutingQueue(obj: JSONObject?): List<QueueRow>? {
        if (obj == null) return null
        val arr = listOf("queue", "entries", "rows", "items", "candidates")
            .firstNotNullOfOrNull { key -> obj.optJSONArray(key) } ?: return null
        val rows = ArrayList<QueueRow>(arr.length())
        for (i in 0 until arr.length()) {
            val r = arr.optJSONObject(i) ?: continue
            rows += QueueRow(
                provider = r.optString("provider", r.optString("providerId", "?")),
                account = r.optString("account", r.optString("displayName", r.optString("accountId", "?"))),
                model = r.optString("model", "").ifBlank { null },
                note = r.optString("reason", r.optString("status", "")).ifBlank { null },
            )
        }
        return rows
    }

    private fun firstNonNull(vararg v: Long?): Long? = v.firstOrNull { it != null && it >= 0 }

    /** 统一带 Bearer Key 的 GET；envelope=true 解 `{success,data}` 信封，否则裸值。 */
    private fun getJson(path: String, envelope: Boolean): JSONObject? {
        val builder = Request.Builder().url("http://127.0.0.1:$PORT$path")
        if (apiKey.isNotBlank()) builder.header("Authorization", "Bearer $apiKey")
        return try {
            pollClient.newCall(builder.build()).execute().use { resp ->
                if (!resp.isSuccessful) return null
                val body = resp.body?.string() ?: return null
                val obj = JSONObject(body)
                if (envelope) {
                    if (!obj.optBoolean("success")) return null
                    obj.optJSONObject("data")
                } else {
                    obj
                }
            }
        } catch (_: Exception) {
            null
        }
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
                    when (type) {
                        // ping 是心跳；lagged 提示全量刷新（M3-2 控制台接入），不进列表
                        "ping" -> return
                        "lagged" -> {
                            refreshOnce()
                            return
                        }
                        "request_done" -> scheduleEventRefresh()
                    }
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

    /** request_done 节流刷新：2s 窗口内的连续收尾合并成一次快照拉取。 */
    private fun scheduleEventRefresh() {
        val now = System.currentTimeMillis()
        val last = lastEventRefresh.get()
        if (now - last < EVENT_REFRESH_THROTTLE_MS) return
        if (!lastEventRefresh.compareAndSet(last, now)) return
        refreshOnce()
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
