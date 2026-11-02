package dev.agate.core

import java.util.concurrent.Callable
import java.util.concurrent.ExecutionException
import java.util.concurrent.Executors
import java.util.concurrent.ThreadFactory
import java.util.concurrent.atomic.AtomicReference

/**
 * Android 进程内 Native Core 生命周期操作的唯一串行入口。
 *
 * 串行化会改变 Core / Gateway 生命周期状态的控制面操作，以及显式的
 * refreshStatus() 对账。长耗时业务请求（apiCall / checkinRunNow）不在此队列，
 * 避免一次签到或管理请求阻塞网关启停。页面的普通 status 读取尚未全部迁移。
 *
 * 调用方不得在主线程直接执行 JNI；当前入口是同步的，调用方必须来自后台线程。
 * 同一控制线程内允许重入，避免组合操作自我等待造成死锁。
 */
object RuntimeController {
    private val worker = AtomicReference<Thread?>()
    private val runtimeSnapshot = AtomicReference(RuntimeSnapshot())

    /**
     * Thread-safe immutable view of the latest Kotlin runtime state.
     *
     * Reads never enter the lifecycle queue. Mutations are restricted to the lifecycle
     * worker through [recordStateEvent], so observers cannot publish competing state.
     */
    fun snapshot(): RuntimeSnapshot = runtimeSnapshot.get()

    /**
     * Reduce and publish one lifecycle outcome/authoritative observation.
     *
     * This is deliberately internal and worker-confined. NativeStatusObserved must only
     * be recorded at a serialized reconciliation boundary with no lifecycle command in
     * flight; callers must not feed arbitrary status polling results here.
     */
    internal fun recordStateEvent(event: RuntimeStateEvent, nowMillis: Long) {
        check(Thread.currentThread() === worker.get()) {
            "Runtime state may only be updated on the lifecycle worker"
        }
        val current = runtimeSnapshot.get()
        runtimeSnapshot.set(RuntimeSnapshotReducer.reduce(current, event, nowMillis))
    }

    /**
     * Initialize the native Core and reconcile its authoritative status in one
     * lifecycle-queue task. A failed native result is returned unchanged; thrown
     * JNI/parse failures are published as CoreInitializationFailed and rethrown.
     *
     * This internal seam keeps the queue/state contract JVM-testable without loading
     * the Android JNI library. Production callers should use [coreInit].
     */
    internal fun <T> coreInitWith(
        initialize: () -> T,
        isSuccessful: (T) -> Boolean,
        failureEvent: (T) -> RuntimeStateEvent,
        readStatus: () -> Gateway.Status,
        nowMillis: () -> Long = { System.currentTimeMillis() },
    ): T = runSerialized {
        recordStateEvent(RuntimeStateEvent.CoreInitializationStarted, nowMillis())
        val result = try {
            initialize()
        } catch (failure: Throwable) {
            recordStateEvent(
                RuntimeStateEvent.CoreInitializationFailed(
                    stage = "core_init",
                    code = null,
                    message = failure.message ?: failure.javaClass.simpleName,
                ),
                nowMillis(),
            )
            throw failure
        }

        if (!isSuccessful(result)) {
            // A failed coreInit can mean bootstrap failed, or Core exists but the
            // scheduler alignment failed. Reconcile Native facts first; if Core is
            // still absent, retain the structured initialization failure afterward.
            val status = try {
                readStatus()
            } catch (statusFailure: Throwable) {
                recordStateEvent(failureEvent(result), nowMillis())
                throw statusFailure
            }
            recordStateEvent(status.toRuntimeStateEvent(), nowMillis())
            if (!status.core) {
                recordStateEvent(failureEvent(result), nowMillis())
            }
            return@runSerialized result
        }

        recordStateEvent(RuntimeStateEvent.CoreInitializationSucceeded, nowMillis())
        // Keep the Native observation in the same serialized task as coreInit.
        // This prevents a later status result from interleaving with another
        // lifecycle command and overwriting its transitional state.
        val status = readStatus()
        recordStateEvent(status.toRuntimeStateEvent(), nowMillis())
        result
    }

    /**
     * Explicit, serialized Native status reconciliation. Since this is queued behind
     * previously submitted lifecycle commands, the observation cannot overtake them.
     */
    internal fun refreshStatusWith(
        readStatus: () -> Gateway.Status,
        nowMillis: () -> Long = { System.currentTimeMillis() },
    ): Gateway.Status = runSerialized {
        val status = readStatus()
        recordStateEvent(status.toRuntimeStateEvent(), nowMillis())
        status
    }

    /** Public Core initialization entry point; no Android Context is retained here. */
    fun coreInit(dataDir: String, port: Int, schedulerOn: Boolean): org.json.JSONObject =
        coreInitWith(
            initialize = { Gateway.coreInit(dataDir, port, schedulerOn) },
            isSuccessful = { it.optBoolean("ok", false) },
            failureEvent = { result ->
                RuntimeStateEvent.CoreInitializationFailed(
                    stage = result.optString("stage").takeIf { it.isNotEmpty() },
                    code = result.optString("code").takeIf { it.isNotEmpty() },
                    message = result.optString("error").takeIf { it.isNotEmpty() }
                        ?: "Native Core initialization failed",
                )
            },
            readStatus = { Gateway.status() },
        )

    /**
     * Execute Gateway.start and publish its lifecycle outcome on the same serialized
     * worker. `already_running` is an idempotent success from the caller's perspective:
     * Rust returns ok=false but includes the authoritative running endpoint details.
     *
     * StartResult itself contains the fields needed for the snapshot, so this operation
     * does not add a second JNI status call or risk replacing a successful start with a
     * stale observation.
     */
    internal fun startWith(
        start: () -> Gateway.StartResult,
        nowMillis: () -> Long = { System.currentTimeMillis() },
    ): Gateway.StartResult = runSerialized {
        recordStateEvent(RuntimeStateEvent.GatewayStartStarted, nowMillis())
        val result = try {
            start()
        } catch (failure: Throwable) {
            recordStateEvent(
                RuntimeStateEvent.GatewayFailed(
                    stage = "start",
                    code = null,
                    message = failure.message ?: failure.javaClass.simpleName,
                ),
                nowMillis(),
            )
            throw failure
        }

        if (result.ok || result.error == "already_running") {
            recordStateEvent(
                RuntimeStateEvent.GatewayStartSucceeded(
                    port = result.port,
                    bindLan = result.bindLan,
                    version = result.version,
                ),
                nowMillis(),
            )
        } else {
            recordStateEvent(
                RuntimeStateEvent.GatewayFailed(
                    stage = result.stage,
                    code = null,
                    message = result.error ?: "Native Gateway start failed",
                ),
                nowMillis(),
            )
        }
        result
    }

    /** Public Gateway start entry point; callers must already be on a background thread. */
    fun start(
        dataDir: String,
        port: Int,
        bindLan: Boolean,
        apiKey: String?,
    ): Gateway.StartResult = startWith {
        Gateway.start(
            dataDir = dataDir,
            port = port,
            bindLan = bindLan,
            apiKey = apiKey,
        )
    }

    /** Public status refresh; observation is performed only on the lifecycle worker. */
    fun refreshStatus(): Gateway.Status = refreshStatusWith(readStatus = { Gateway.status() })

    private fun Gateway.Status.toRuntimeStateEvent() = RuntimeStateEvent.NativeStatusObserved(
        coreReady = core,
        gatewayRunning = running,
        port = port,
        bindLan = bindLan,
        version = version,
        schedulerEnabled = scheduler,
    )

    private val executor = Executors.newSingleThreadExecutor(
        ThreadFactory { runnable ->
            Thread(runnable, "AgateNativeLifecycle").apply {
                isDaemon = true
                worker.set(this)
            }
        },
    )

    /** 在专用控制线程上执行一个生命周期变更，并把异常原样交还调用方。 */
    fun <T> runSerialized(operation: () -> T): T {
        if (Thread.currentThread() === worker.get()) return operation()

        val future = executor.submit(Callable(operation))
        // 生命周期变更一旦入队就不能因为调用线程被中断而“假取消”：
        // Future.get() 被中断并不会自动取消任务，若立即返回，start/stop 仍可能
        // 在调用方认为失败之后悄悄执行。这里等待已提交操作完成，再恢复中断标记，
        // 让调用方得到真实结果并由上层决定如何处理线程中断。
        var interrupted = false
        try {
            while (true) {
                try {
                    return future.get()
                } catch (_: InterruptedException) {
                    interrupted = true
                } catch (failed: ExecutionException) {
                    throw failed.cause ?: failed
                }
            }
        } finally {
            if (interrupted) Thread.currentThread().interrupt()
        }
    }

    /** 从任意线程按调用顺序入队；完成回调在控制线程执行，调用方自行切回 UI。 */
    fun <T> submitSerialized(
        operation: () -> T,
        onComplete: (Result<T>) -> Unit,
    ) {
        executor.execute {
            val result = runCatching(operation)
            runCatching { onComplete(result) }
        }
    }
}
