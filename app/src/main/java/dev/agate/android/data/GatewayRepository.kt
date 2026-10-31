package dev.agate.android.data

import dev.agate.android.data.dto.AccountDto
import dev.agate.android.data.dto.AccountDtos
import dev.agate.core.Gateway
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
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
        /**
         * provider id → 该家是否支持余额查询（内核 `ProviderAdapter::supports_usage`
         * 经 /api/session 的 `providers[].usage` 带下来）。界面据此决定要不要显示
         * 「额度设限」入口 —— 不支持的家显示阈值输入框永远看不到效果。
         */
        val providerUsage: Map<String, Boolean> = emptyMap(),
        /** /api/routing/queue → 实时出战队列（首页 v44 hq 卡）；解析失败沿用上份 */
        val routingQueue: List<QueueRow> = emptyList(),
        /** /api/stats/summary overview → 平均时延（字段名随上游，读不到为 null 显示占位） */
        val avgLatencyMs: Long? = null,
        /** /api/session → models（客户端所见清单；M3 模型页数据源） */
        val models: List<ModelRow> = emptyList(),
    )

    /**
     * 一条「客户端所见清单」模型（/api/session.models，与 GET /v1/models 同源）。
     * credits 是上游形态字符串（如 "x0.16 credits" / ""），倍率在界面侧解析。
     */
    data class ModelRow(
        val id: String,
        val provider: String,
        val providerLabel: String,
        val name: String?,
        val credits: String?,
        val maxInput: Int?,
        val maxOutput: Int?,
        val toolCall: Boolean,
        val images: Boolean,
        val reasoning: Boolean,
        val isDefault: Boolean,
        /**
         * 启停状态。`/api/session` 的清单**只含已启用**的模型，所以从那边解析
         * 出来的行恒为 `true`；模型页会把 `/api/models/manage` 里「已禁用」的
         * 条目并进来（那时才是 `false`），用于置灰 + 画开关。
         */
        val enabled: Boolean = true,
        /**
         * 在不在**官方客户端下拉（picker）**里。`true` = 桌面端也列它；
         * `false` = 只有我们列（上游目录里有、桌面端不展示）；
         * `null` = 拿不到名单，不做判断（界面不标注）。
         */
        val official: Boolean? = null,
    )

    /** v44 .qrow 一行：序号由渲染端下标，这里只带语义字段。 */
    data class QueueRow(
        val provider: String,
        val account: String,
        val model: String? = null,
        val note: String? = null,
    )

    /**
     * SSE 与探活用的端口 —— **必须跟设置走**，不能写死。
     *
     * 这里曾经是 `private const val PORT = 8787`：设置页允许改端口，
     * 改了之后 SSE 会连到 8787（没人听）→ `link` 永远 RETRYING、控制台的
     * 实时事件与降级轮询全废，而 UI 上只会显示「连接中」，看不出是端口对不上。
     * 由 [configure] 从 `GatewayPrefs.port` 灌入；未灌入时按默认 8787。
     */
    @Volatile private var port: Int = 8787
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

    /**
     * 领域事件总线：内核 SSE 的业务事件按**种类**转发给页面。
     *
     * ── 解决什么（与 5s 快照轮询的分工）────────────────────────
     * 快照 `SNAPSHOT_INTERVAL_MS = 5s` 是「兜底轮询」：它在跑，但一次改动最多
     * 要等 5 秒才反映到界面。而这个仓库只持有控制台那一份快照 ——
     * 账号列表、模型清单、自定义家名字表分别在各自的页面里，它们连「有东西变了」
     * 都不知道，只能靠进页面时拉一次。
     *
     * 所以：内核在 **core 的变更点**广播事件（见 Rust 侧 `events::publish`），
     * 这里只做「种类转发」，页面各自决定要重拉什么。刻意不在这里做「事件 → 数据」
     * 的解释：那等于让仓库持有所有页面的数据，一处过期就全局不一致。
     *
     * `extraBufferCapacity` 给一点余量 + `DROP_OLDEST`：事件只是「去重拉一次」的
     * 提示，丢掉中间几条不会丢状态（拉的是权威版本）。
     */
    private val _bus = MutableSharedFlow<String>(
        replay = 0,
        extraBufferCapacity = 32,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )
    val bus: SharedFlow<String> = _bus

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
    /**
     * 注入鉴权与端口。调用方是导航宿主（它手里有 `Context`）——
     * 本对象是纯单例，不持 Context（否则要担心泄漏），所以端口只能由外部灌入。
     * [port] 传 0/非法值 = 不改（默认 8787），保持既有调用点兼容。
     */
    fun configure(apiKey: String, port: Int = 0) {
        this.apiKey = apiKey
        if (port in 1..65535) this.port = port
        CoreTransport.configure(apiKey)
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

    /** 页面生命周期感知的快照轮询（方案 §3.1：前台 STARTED 刷新、退后台挂起）。
     *
     * ⚠️ 这个函数**不再是「谁消费谁启动」**：早先只有控制台页调它，于是
     * 提供商页 / 模型页 / 额度中心 / 调用日志这些同读 [snapshot] 的页面，
     * 只要用户没先进过一次控制台，读到的永远是 [ConsoleSnapshot] 的初始值
     * ——「全部 · 0」「还没有任何账号」。实测现象就是**覆盖安装后打开提供商页
     * 一片空**，而同一时刻签到中心（走自己的 `/api/checkin-center`）明明有账号。
     *
     * 现在由 `AgateNavHost` 在导航宿主这一层启动一次，全应用共用（见那里的
     * `DisposableEffect`）；`started` 由 [syncGatewayRunning] 探活维护，
     * 不再依赖某个页面先被访问过。
     */
    fun startPolling() {
        if (snapshotJob?.isActive == true) return
        snapshotJob = scope.launch {
            while (isActive) {
                // 壳层还没探过活时先问一次：`started` 只由探活/网关启停来置位，
                // 页面自己猜「大概在跑」会让快照对着一个没起来的端口轮询。
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
     * 探活并同步 `started`：网关在跑就起 SSE + 拉快照，停了就置错误。
     *
     * 幂等、可频繁调用 —— 壳层每几秒调一次即可。原本这段逻辑写在控制台页的
     * `LaunchedEffect` 里（`Gateway.status()` + [onGatewayRunning]），搬到这里
     * 是因为**页面不保证被访问**：用户在提供商页时那条循环根本不存在。
     */
    fun syncGatewayRunning() {
        scope.launch {
            val status = runCatching { Gateway.status() }.getOrNull()
            onGatewayRunning(status?.running == true)
            // 探活到「在跑」但快照还是空的（比如进程刚拉起来、或上一次全失败
            // 留下的空值）：立刻补拉一次，用户不必等下一个 5 秒刻度。
            if (started && _snapshot.value.fetchedAtMs == 0L) refreshOnce()
        }
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
                    _snapshotError.value = "无法连接内核"
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
                    providerUsage = if (session != null) parsed.providerUsage else prev.providerUsage,
                    routingQueue = parseRoutingQueue(routing) ?: prev.routingQueue,
                    avgLatencyMs = firstNonNull(
                        overview?.optLong("avgLatencyMs", -1L),
                        overview?.optLong("latencyMs", -1L),
                        overview?.optLong("avgDurationMs", -1L),
                    ) ?: prev.avgLatencyMs,
                    models = session?.let { parseModels(it) } ?: prev.models,
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

    /**
     * /api/session.models → 客户端所见清单。字段名按内核 shape.rs 的 list_item：
     * id / owned_by(provider) / name / credits / max_input_tokens / max_output_tokens /
     * supports_tool_call / supports_images / supports_reasoning / is_default；
     * 解析层另接 provider / providerLabel（session_models 注入）。
     */
    private fun parseModels(session: JSONObject): List<ModelRow> {
        val arr = session.optJSONArray("models") ?: return emptyList()
        val out = ArrayList<ModelRow>(arr.length())
        for (i in 0 until arr.length()) {
            val m = arr.optJSONObject(i) ?: continue
            val id = m.optString("id")
            if (id.isBlank()) continue
            val provider = m.optString("provider", m.optString("owned_by", ""))
            out += ModelRow(
                id = id,
                provider = provider,
                providerLabel = m.optString("providerLabel").ifBlank { provider },
                name = m.optString("name").ifBlank { null },
                credits = m.optString("credits").ifBlank { null },
                maxInput = m.optInt("max_input_tokens", 0).takeIf { it > 0 },
                maxOutput = m.optInt("max_output_tokens", 0).takeIf { it > 0 },
                toolCall = m.optBoolean("supports_tool_call"),
                images = m.optBoolean("supports_images"),
                reasoning = m.optBoolean("supports_reasoning"),
                isDefault = m.optBoolean("is_default"),
            )
        }
        return out
    }

    /**
     * 统一带 Bearer Key 的 GET；envelope=true 解 `{success,data}` 信封，否则裸值。
     * P2：经 CoreTransport 内核直调（调用方均在 scope.launch 协程内）。
     * SSE `/api/events` 不走本通道 —— 流式响应不适合 oneshot，且实时事件只在
     * 网关运行时有意义（无网关时轮询降级路径即本函数）。
     */
    private suspend fun getJson(path: String, envelope: Boolean): JSONObject? {
        val r = CoreTransport.get(path)
        val ok = r as? CoreTransport.Reply.Ok ?: return null
        if (ok.status < 200 || ok.status >= 300) return null
        val obj = runCatching { JSONObject(ok.text) }.getOrNull() ?: return null
        return if (envelope) {
            if (!obj.optBoolean("success")) return null
            obj.optJSONObject("data")
        } else {
            obj
        }
    }

    private fun connect() {
        if (!started) return
        pollJob?.cancel()
        pollJob = null
        _link.value = if (failures.get() == 0) LinkState.CONNECTING else LinkState.RETRYING
        val builder = Request.Builder().url("http://127.0.0.1:$port/api/events")
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
                    // 业务事件先上总线（页面按需局部刷新），再走下面原有的
                    // 「进最近事件列表 / 触发快照」逻辑。顺序无关但都在主线程外，
                    // 所以只 tryEmit（满缓冲就丢，见 _bus 的说明）。
                    if (type != null) {
                        when (type) {
                            "accounts_changed",
                            "models_changed",
                            "providers_changed",
                            "checkin_done",
                            "request_done",
                            -> _bus.tryEmit(type)
                        }
                    }
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
                        Request.Builder().url("http://127.0.0.1:$port/health").build(),
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
