package dev.agate.core

import android.os.Handler
import android.os.HandlerThread
import org.json.JSONException
import org.json.JSONObject

/**
 * 网关内核（libagate_server.so）的 Kotlin 门面。
 *
 * 双通信面（方案 §3）：数据面默认走 HTTP（127.0.0.1:8787）。本对象提供
 * 12 个 JNI 导出 —— 电源（start/stop/status/coreInit）、闹钟与签到
 * （triggerTask/schedulerSet/checkinRunNow/checkinRunAndWait/
 * requestCheckinStop）、日志回调（setLogCallback）、版本（version），以及
 * 管理面直调 apiCall（Core 态经内核内存 Router，不经 loopback 端口）。
 * 全部按 JSON 文本进出，本对象不解释业务数据结构。
 *
 * 线程：JNI 导出可在任意线程调用（bootstrap/start 是同步调用、毫秒级返回，
 * 内部落到内核自己的 Tokio 运行时）；但建议在后台线程调 start/stop，
 * 避免阻塞调用线程。
 */
object Gateway {

    @Volatile
    private var loaded = false
    private val loadLock = Any()

    private fun ensureLoaded() {
        if (loaded) return
        synchronized(loadLock) {
            if (!loaded) {
                // libagate_server.so —— agate-server 内核的 cdylib 产物
                System.loadLibrary("agate_server")
                loaded = true
            }
        }
    }

    // ── JNI 导出（与 agate-server/src/jni_bridge.rs 一一对应，勿直接调用）──
    // 返回类型 String?：JNI 层面分配失败确实可能拿到 null（见 json_reply 的
    // 两层兜底链），如实声明，统一由 envelope() 收敛成结构化失败信封。
    private external fun nativeStart(configJson: String): String?
    private external fun nativeStop(): String?
    private external fun nativeStatus(): String?
    private external fun nativeTriggerTask(task: String): String?
    private external fun nativeCoreInit(dataDir: String, port: Int, schedulerOn: Boolean): String?
    private external fun nativeSchedulerSet(on: Boolean): String?
    private external fun nativeCheckinRunNow(timeoutMs: Long): String?
    private external fun nativeCheckinRunAndWait(): String?
    private external fun nativeRequestCheckinStop(): String?
    private external fun nativeVersion(): String?
    private external fun nativeSetLogCallback(callback: Any?): String?

    private external fun nativeApiCall(method: String, path: String, body: String?, apiKey: String): String?

    /**
     * 管理面内核直调（P2 通道层）：method+path+body 经 JNI 走内核内存 Router，
     * 不经 127.0.0.1 端口 —— Core 态（网关未运行）下管理操作照常可用。
     * 返回 {ok, status, body} JSON 文本。Native 加载/调用异常向上传递，
     * 由 CoreTransport 转换为明确的链路错误；null 回复（两层兜底都失败的
     * 极端情况）在此收敛为结构化失败信封，CoreTransport 按 ok=false 处理，
     * 不会把错误吞成 null。
     */
    fun apiCall(method: String, path: String, body: String?, apiKey: String): String {
        ensureLoaded()
        return envelope(nativeApiCall(method, path, body, apiKey)).toString()
    }

    data class StartResult(
        val ok: Boolean,
        val port: Int,
        val bindLan: Boolean,
        val version: String,
        val stage: String?,
        val error: String?,
        /** bind 失败的机器可读分类（Rust `ConflictKind::as_str`：occupied/reserved/other）；旧内核或非 bind 失败为 null。 */
        val kind: String?,
    ) {
        companion object {
            fun parse(raw: String?): StartResult {
                val j = envelope(raw)
                return StartResult(
                    j.optBoolean("ok"),
                    j.optInt("port", 0),
                    j.optBoolean("bind_lan"),
                    j.optString("version"),
                    j.optString("stage").nullIfEmpty(),
                    j.optString("error").nullIfEmpty(),
                    j.optString("kind").nullIfEmpty(),
                )
            }
        }
    }

    data class Status(
        val running: Boolean,
        val port: Int,
        val bindLan: Boolean,
        val uptimeSec: Long,
        val version: String,
        /** B 包两级状态：core = 进程内基础设施就绪（随进程）；running = 网关在跑。 */
        val core: Boolean,
        /** 签到调度器开关状态（独立于网关）。 */
        val scheduler: Boolean,
        /**
         * JNI 控制面契约版本（status_json "bridge" = jni_bridge::BRIDGE_CONTRACT_VERSION）。
         * 0 = 旧内核（未携带该字段的历史版本，按 R44 `kind` 的 null 兼容先例降级）；
         * >1 = 未知新内核，只允许取已知字段。默认 0 仅供手工构造（测试桩）。
         */
        val bridge: Int = 0,
    ) {
        companion object {
            fun parse(raw: String?): Status {
                val j = envelope(raw)
                // status_json() wraps lock/Native failures as {ok:false,error:...}.
                // Never turn that error envelope into a plausible all-false runtime
                // snapshot through optBoolean defaults.
                check(j.optBoolean("ok", false)) {
                    j.optString("error").takeIf { it.isNotEmpty() }
                        ?: "Native status query failed"
                }
                return Status(
                    j.optBoolean("running"),
                    j.optInt("port", 0),
                    j.optBoolean("bind_lan"),
                    j.optLong("uptime_sec", 0),
                    j.optString("version"),
                    j.optBoolean("core"),
                    j.optBoolean("scheduler"),
                    j.optInt("bridge", 0),
                )
            }
        }
    }

    /**
     * 拉起网关。[dataDir] 是内核数据目录（SQLite/日志/备份），调用方传
     * context.filesDir 下的子目录；[port] 默认 8787；[bindLan] 局域网监听
     * （M0 默认 false，M1 接设置）；[adminToken] M1 局域网守卫用，M0 可空。
     */
    fun start(
        dataDir: String,
        port: Int = 8787,
        bindLan: Boolean = false,
        adminToken: String? = null,
        apiKey: String? = null,
    ): StartResult {
        ensureLoaded()
        val cfg = JSONObject()
            .put("data_dir", dataDir)
            .put("port", port)
            .put("bind_lan", bindLan)
        adminToken?.let { cfg.put("admin_token", it) }
        apiKey?.takeIf { it.isNotBlank() }?.let { cfg.put("api_key", it) }
        return StartResult.parse(nativeStart(cfg.toString()))
    }

    /** 优雅停机（在途请求有 graceful shutdown 收尾）。 */
    fun stop(): JSONObject {
        ensureLoaded()
        return envelope(nativeStop())
    }

    fun status(): Status {
        ensureLoaded()
        return Status.parse(nativeStatus())
    }

    /** 内核版本号（与上游 agate-server 同源，如 3.0.8）。 */
    fun version(): String {
        ensureLoaded()
        // 裸字符串契约（唯一非信封入口）；分配失败按空串收敛，UI 层自行判空。
        return nativeVersion() ?: ""
    }

    /** 闹钟唤醒入口：scheduled_tick（签到轮）/ claim_checkin（补领）。 */
    fun triggerTask(task: String): JSONObject {
        ensureLoaded()
        return envelope(nativeTriggerTask(task))
    }

    /**
     * Core 初始化（幂等，B 包两级内核）：bootstrap 基础设施 + 后台调度对齐
     * [schedulerOn]。Application 启动与闹钟兜底都走它；不监听端口。
     */
    fun coreInit(dataDir: String, port: Int, schedulerOn: Boolean): JSONObject {
        ensureLoaded()
        return envelope(nativeCoreInit(dataDir, port, schedulerOn))
    }

    /** 调度器独立开关（不随网关启停；Core 未就绪时返回 core_not_ready）。 */
    fun schedulerSet(on: Boolean): JSONObject {
        ensureLoaded()
        return envelope(nativeSchedulerSet(on))
    }

    /**
     * 同步跑一轮签到并等待完成（F3：闹钟直调内核，替代 HTTP 回环）。
     * 返回前 WakeLock 必须由调用方持有；超时后签到在内核后台继续跑。
     */
    fun checkinRunNow(timeoutMs: Long = 120_000L): JSONObject {
        ensureLoaded()
        return envelope(nativeCheckinRunNow(timeoutMs))
    }

    /**
     * FGS 专用：等待 Rust 签到任务真正完成后返回。
     * STOP 可通过 [requestCheckinStop] 合作式停止后续账号；当前账号仍会自然收尾。
     * 调用方必须在后台线程调用，并在整个等待期间持有 WakeLock。
     */
    fun checkinRunAndWait(): JSONObject {
        ensureLoaded()
        return envelope(nativeCheckinRunAndWait())
    }

    /** 请求当前签到轮次在当前账号自然收尾后停止；不会中断在途 Provider 请求。 */
    fun requestCheckinStop(): JSONObject {
        ensureLoaded()
        return envelope(nativeRequestCheckinStop())
    }

    // ── 日志回调 ──────────────────────────────────────────────────

    fun interface LogListener {
        fun onLog(level: String, tag: String, message: String)
    }

    /** Rust 侧经 JNI 反射调用的桥对象（jni_bridge.rs::call_on_log）。 */
    private class LogSink(private val listener: LogListener) {
        fun onLog(level: String, tag: String, message: String) {
            listener.onLog(level, tag, message)
        }
    }

    // lazy：Gateway 对象在 JVM 单测里也会被触碰（envelope/parse 纯函数测试），
    // 而 HandlerThread 在单元测试 classpath 是 android.jar 桩（方法体 throw
    // "Stub!"）——放类初始化器会让整只对象在测试里炸掉。首次注册日志监听时
    // 才真正起线程，生产语义不变（setLogListener 是唯一入口）。
    private val logThread: HandlerThread by lazy { HandlerThread("AgateLog").apply { start() } }
    private val logHandler: Handler by lazy { Handler(logThread.looper) }

    private val logListenerRegistry = LogListenerRegistry<LogSink>()

    /**
     * 注册内核日志监听（转发自 logging::console_line 的全量日志）；
     * 传 null 撤销。回调固定在 AgateLog 后台线程执行，不要在回调里做重活。
     * 约定：网关运行期间不换监听（内核侧只持有单个 GlobalRef）。
     * 返回 Native 结构化结果；ok=false 时保留上一次成功注册的 Kotlin 强引用，
     * 调用方应记录 error，而不能把失败当作注册/撤销成功。
     */
    @Synchronized
    fun setLogListener(listener: LogListener?): LogCallbackUpdateResult {
        ensureLoaded()
        val candidate = listener?.let { target ->
            LogSink { level, tag, message ->
                logHandler.post { target.onLog(level, tag, message) }
            }
        }
        return logListenerRegistry.update(candidate) { callback ->
            // nativeSetLogCallback 现返回 String?（R46/T3）：经 envelope 收敛成
            // 结构化信封再解析；null/畸形 → ok=false → registry 保留旧监听。
            parseLogCallbackUpdateResponse(envelope(nativeSetLogCallback(callback)).toString())
        }
    }

    /**
     * Native 回复统一解析入口：null / 空串 / 非法 JSON / 非信封结构一律收敛成
     * 结构化失败信封（{ok:false, stage:"jni", error:"malformed native reply"}），
     * 绝不向上抛 NPE/JSONException —— 与 jni_bridge.rs::json_reply 的最小兜底
     * 配套，覆盖两层各自的分配/传输异常（R46/T3，M4「不把 null 当通用错误
     * 吞掉」的 Kotlin 侧）。internal 供 JVM 测试直接断言。
     *
     * 信封结构契约（R47/T3.3 从严）：
     *   · ok 必须存在且为布尔 —— optBoolean 会把缺字段吞成 false，无法区分
     *     「没有 ok」与「ok=false」，必须显式判型；
     *   · ok=false 时 error 必须非空 ——「失败信封必带可诊断 error」是 Rust/
     *     Kotlin 两侧共同契约（全库 ok=false 回复点已逐一审计），违者视为
     *     契约破坏，与畸形回复同路收敛且不保留原内容，避免漂移静默存活。
     * 只捕 JSONException：OOM 等 Error 不伪装成 malformed，向上传播由调用方
     * 的 Throwable 兜底处理（诚实失败优于假成功）。
     */
    internal fun envelope(raw: String?): JSONObject {
        val parsed = try {
            JSONObject(raw ?: "")
        } catch (_: JSONException) {
            return malformedEnvelope()
        }
        val ok = parsed.opt("ok") as? Boolean
        return if (ok != null && (ok || parsed.optString("error").isNotEmpty())) {
            parsed
        } else {
            malformedEnvelope()
        }
    }

    private fun malformedEnvelope(): JSONObject =
        JSONObject("{\"ok\":false,\"stage\":\"jni\",\"error\":\"malformed native reply\"}")

    private fun String.nullIfEmpty(): String? = if (isEmpty()) null else this
}
