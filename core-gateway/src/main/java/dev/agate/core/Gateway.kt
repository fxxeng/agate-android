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
                // libagate_server.so —— agent2api-server 内核的 cdylib 产物
                System.loadLibrary("agate_server")
                loaded = true
            }
        }
    }

    // ── JNI 导出（与 agent2api-server/src/jni_bridge.rs 一一对应，勿直接调用）──
    private external fun nativeStart(configJson: String): String
    private external fun nativeStop(): String
    private external fun nativeStatus(): String
    private external fun nativeTriggerTask(task: String): String
    private external fun nativeVersion(): String
    private external fun nativeSetLogCallback(callback: Any?): String

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
    ): StartResult {
        ensureLoaded()
        val cfg = JSONObject()
            .put("data_dir", dataDir)
            .put("port", port)
            .put("bind_lan", bindLan)
        adminToken?.let { cfg.put("admin_token", it) }
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

    /** 内核版本号（与上游 agent2api-server 同源，如 3.0.8）。 */
    fun version(): String {
        ensureLoaded()
        return nativeVersion()
    }

    /** 闹钟唤醒入口：scheduled_tick（签到轮）/ claim_checkin（补领）。 */
    fun triggerTask(task: String): JSONObject {
        ensureLoaded()
        return JSONObject(nativeTriggerTask(task))
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
