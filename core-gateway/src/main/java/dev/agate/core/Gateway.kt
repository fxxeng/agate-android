package dev.agate.core

import android.os.Handler
import android.os.HandlerThread
import org.json.JSONObject

/**
 * 网关内核（libagate_server.so）的 Kotlin 门面。
 *
 * 双通信面（方案 §3）：数据面全部走 HTTP（127.0.0.1:8787），本对象只管
 * 「电源开关 + 闹钟 + 日志回调」六件事，不碰任何业务数据结构。
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
    private external fun nativeStart(configJson: String): String
    private external fun nativeStop(): String
    private external fun nativeStatus(): String
    private external fun nativeTriggerTask(task: String): String
    private external fun nativeCoreInit(dataDir: String, port: Int, schedulerOn: Boolean): String
    private external fun nativeSchedulerSet(on: Boolean): String
    private external fun nativeCheckinRunNow(timeoutMs: Long): String
    private external fun nativeVersion(): String
    private external fun nativeSetLogCallback(callback: Any?): String

    private external fun nativeApiCall(method: String, path: String, body: String?, apiKey: String): String

    /**
     * 管理面内核直调（P2 通道层）：method+path+body 经 JNI 走内核内存 Router，
     * 不经 127.0.0.1 端口 —— Core 态（网关未运行）下管理操作照常可用。
     * 返回 {ok, status, body} JSON 文本；链路异常返回 null。
     */
    fun apiCall(method: String, path: String, body: String?, apiKey: String): String? =
        try {
            nativeApiCall(method, path, body, apiKey)
        } catch (_: Throwable) {
            null
        }

    data class StartResult(
        val ok: Boolean,
        val port: Int,
        val bindLan: Boolean,
        val version: String,
        val stage: String?,
        val error: String?,
    ) {
        companion object {
            fun parse(raw: String): StartResult {
                val j = JSONObject(raw)
                return StartResult(
                    j.optBoolean("ok"),
                    j.optInt("port", 0),
                    j.optBoolean("bind_lan"),
                    j.optString("version"),
                    j.optString("stage").nullIfEmpty(),
                    j.optString("error").nullIfEmpty(),
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
    ) {
        companion object {
            fun parse(raw: String): Status {
                val j = JSONObject(raw)
                return Status(
                    j.optBoolean("running"),
                    j.optInt("port", 0),
                    j.optBoolean("bind_lan"),
                    j.optLong("uptime_sec", 0),
                    j.optString("version"),
                    j.optBoolean("core"),
                    j.optBoolean("scheduler"),
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
        return JSONObject(nativeStop())
    }

    fun status(): Status {
        ensureLoaded()
        return Status.parse(nativeStatus())
    }

    /** 内核版本号（与上游 agate-server 同源，如 3.0.8）。 */
    fun version(): String {
        ensureLoaded()
        return nativeVersion()
    }

    /** 闹钟唤醒入口：scheduled_tick（签到轮）/ claim_checkin（补领）。 */
    fun triggerTask(task: String): JSONObject {
        ensureLoaded()
        return JSONObject(nativeTriggerTask(task))
    }

    /**
     * Core 初始化（幂等，B 包两级内核）：bootstrap 基础设施 + 后台调度对齐
     * [schedulerOn]。Application 启动与闹钟兜底都走它；不监听端口。
     */
    fun coreInit(dataDir: String, port: Int, schedulerOn: Boolean): JSONObject {
        ensureLoaded()
        return JSONObject(nativeCoreInit(dataDir, port, schedulerOn))
    }

    /** 调度器独立开关（不随网关启停；Core 未就绪时返回 core_not_ready）。 */
    fun schedulerSet(on: Boolean): JSONObject {
        ensureLoaded()
        return JSONObject(nativeSchedulerSet(on))
    }

    /**
     * 同步跑一轮签到并等待完成（F3：闹钟直调内核，替代 HTTP 回环）。
     * 返回前 WakeLock 必须由调用方持有；超时后签到在内核后台继续跑。
     */
    fun checkinRunNow(timeoutMs: Long = 120_000L): JSONObject {
        ensureLoaded()
        return JSONObject(nativeCheckinRunNow(timeoutMs))
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

    private val logThread = HandlerThread("AgateLog").apply { start() }
    private val logHandler = Handler(logThread.looper)

    @Volatile
    private var sink: LogSink? = null

    /**
     * 注册内核日志监听（转发自 logging::console_line 的全量日志）；
     * 传 null 撤销。回调固定在 AgateLog 后台线程执行，不要在回调里做重活。
     * 约定：网关运行期间不换监听（内核侧只持有单个 GlobalRef）。
     */
    @Synchronized
    fun setLogListener(listener: LogListener?) {
        ensureLoaded()
        if (listener == null) {
            sink = null
            nativeSetLogCallback(null)
        } else {
            val s = LogSink { level, tag, message ->
                logHandler.post { listener.onLog(level, tag, message) }
            }
            sink = s // Kotlin 侧也持强引用，防 GC
            nativeSetLogCallback(s)
        }
    }

    private fun String.nullIfEmpty(): String? = if (isEmpty()) null else this
}
