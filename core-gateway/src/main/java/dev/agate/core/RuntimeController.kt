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
 * refreshStatus() 对账。高频/只读 status 通过 readStatus() 在调用线程读取，
 * 不进入生命周期队列且不写 RuntimeSnapshot。长耗时业务请求（apiCall / checkinRunNow）
 * 同样不在此队列，避免阻塞 start/stop。
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

    /**
     * Map a native coreInit failure reply onto the runtime state event.
     *
     * Extracted so the JVM test suite can pin the envelope contract (stage /
     * code / error passthrough) without loading the Android JNI library.
     * `code` carries machine-readable classifications from the Rust side
     * (e.g. `DATA_DIR_MISMATCH`, R47 T2.3); legacy replies without a code
     * map to null. The human-readable reason stays the Rust-side message.
     */
    internal fun coreInitFailureEvent(result: org.json.JSONObject): RuntimeStateEvent =
        RuntimeStateEvent.CoreInitializationFailed(
            stage = result.optString("stage").takeIf { it.isNotEmpty() },
            code = result.optString("code").takeIf { it.isNotEmpty() },
            message = result.optString("error").takeIf { it.isNotEmpty() }
                ?: "Native Core initialization failed",
        )

    /** Public Core initialization entry point; no Android Context is retained here. */
    fun coreInit(dataDir: String, port: Int, schedulerOn: Boolean): org.json.JSONObject =
        coreInitWith(
            initialize = { Gateway.coreInit(dataDir, port, schedulerOn) },
            isSuccessful = { it.optBoolean("ok", false) },
            failureEvent = { result -> coreInitFailureEvent(result) },
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
                    kind = result.kind,
                ),
                nowMillis(),
            )
        }
        result
    }

    /**
     * Evaluate the latest Service intent and, only when permitted, stop Gateway in the
     * same serialized lifecycle task. The guard and Native stop must never be split:
     * a newer START intent arriving while this task is queued must prevent stale STOP.
     *
     * `not_running` is an idempotent stopped outcome. `released=false` (stop issued
     * but port release unconfirmed within 5s) publishes GatewayStopTimedOut instead
     * of pretending Stopped (R47/T3.4). Other Native failures are published to the
     * snapshot but returned unchanged to the existing caller path.
     */
    internal data class StopOutcome(
        val ok: Boolean,
        val error: String? = null,
        val code: String? = null,
        /**
         * Rust stop_gateway 的 released：false = 停止已发出但 5s 内端口释放
         * 未确认（R47/T3.4 三态）。错误信封 / 旧内核无此字段时缺省 true，
         * 不为「无法证明」发明新的失败语义。
         */
        val released: Boolean = true,
    )

    internal fun stopWith(
        mayStopNow: () -> Boolean,
        stop: () -> StopOutcome,
        nowMillis: () -> Long = { System.currentTimeMillis() },
    ): Boolean = runSerialized {
        if (!mayStopNow()) return@runSerialized false

        recordStateEvent(RuntimeStateEvent.GatewayStopStarted, nowMillis())
        val result = try {
            stop()
        } catch (failure: Throwable) {
            recordStateEvent(
                RuntimeStateEvent.GatewayFailed(
                    stage = "stop",
                    code = null,
                    message = failure.message ?: failure.javaClass.simpleName,
                ),
                nowMillis(),
            )
            throw failure
        }

        if (result.ok || result.error == "not_running") {
            // not_running 是幂等停止（本来就没跑）；released=false 才是真正的
            // 三态：停止已发出但端口释放未确认，发布 StopTimeout 而不是假
            // Stopped —— 随后的权威 status 探测（NativeStatusObserved）会把
            // 它归位（R47/T3.4）。
            if (result.ok && !result.released) {
                recordStateEvent(RuntimeStateEvent.GatewayStopTimedOut, nowMillis())
            } else {
                recordStateEvent(RuntimeStateEvent.GatewayStopSucceeded, nowMillis())
            }
        } else {
            recordStateEvent(
                RuntimeStateEvent.GatewayFailed(
                    stage = "stop",
                    code = result.code,
                    message = result.error ?: "Native Gateway stop failed",
                ),
                nowMillis(),
            )
        }
        true
    }

    /** Production STOP entry point; intent validation and JNI stop are atomic. */
    fun stopIfAllowed(mayStopNow: () -> Boolean): Boolean = stopWith(
        mayStopNow = mayStopNow,
        stop = {
            val result = Gateway.stop()
            StopOutcome(
                ok = result.optBoolean("ok", false),
                error = result.optString("error").takeIf { it.isNotEmpty() },
                code = result.optString("code").takeIf { it.isNotEmpty() },
                released = result.optBoolean("released", true),
            )
        },
    )

    /** Public Gateway start entry point; callers must already be on a background thread. */
    fun start(
        dataDir: String,
        port: Int,
        bindLan: Boolean,
        apiKey: String?,
    ): Gateway.StartResult = startWith(
        start = {
            Gateway.start(
                dataDir = dataDir,
                port = port,
                bindLan = bindLan,
                apiKey = apiKey,
            )
        },
    )

    /**
     * Apply the Native scheduler setting and publish only the effective value returned by
     * Rust. A failed Native result leaves the last-known effective snapshot unchanged:
     * the Android preference is the desired setting and may already be persisted by the UI.
     *
     * This internal seam keeps result/state behavior testable without loading JNI.
     */
    internal fun schedulerSetWith(
        set: () -> org.json.JSONObject,
        nowMillis: () -> Long = { System.currentTimeMillis() },
    ): org.json.JSONObject = runSerialized {
        val result = set()
        if (result.optBoolean("ok", false)) {
            val enabled = result.opt("scheduler") as? Boolean
                ?: throw IllegalStateException(
                    "Native schedulerSet returned ok=true without a boolean scheduler value",
                )
            recordStateEvent(RuntimeStateEvent.SchedulerObserved(enabled), nowMillis())
        }
        result
    }

    /** Public scheduler control entry point; callers must already be on a background thread. */
    fun schedulerSet(on: Boolean): org.json.JSONObject = schedulerSetWith(
        set = { Gateway.schedulerSet(on) },
    )

    /**
     * Read Native status for polling/diagnostics without entering the lifecycle queue.
     *
     * This is intentionally different from [refreshStatus]: frequent read-only probes
     * must not queue behind a long Core/start/stop operation, nor publish a potentially
     * stale observation into RuntimeSnapshot. Callers must use a background thread.
     */
    internal fun readStatusWith(readStatus: () -> Gateway.Status): Gateway.Status = readStatus()

    /** Read-only Native status; does not serialize or mutate RuntimeSnapshot. */
    fun readStatus(): Gateway.Status = readStatusWith { Gateway.status() }

    /** Public authoritative status reconcile; observation runs on the lifecycle worker. */
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

    // Completion callbacks must never occupy the lifecycle worker. Keep one daemon
    // dispatcher to preserve callback FIFO order without creating a thread per result.
    // Production FIFO order relies on the lifecycle worker being the sole producer
    // that submits callbacks to this private dispatcher; do not dispatch from other threads.
    private val callbackExecutor = Executors.newSingleThreadExecutor(
        ThreadFactory { runnable ->
            Thread(runnable, "AgateNativeCallbacks").apply {
                isDaemon = true
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

    /**
     * Submit a lifecycle operation in FIFO order. The operation runs on the lifecycle
     * worker; its completion callback is dispatched to a separate single-thread daemon
     * executor so slow UI/result handling cannot stall later lifecycle commands.
     *
     * The callback is not a UI-thread callback: callers must explicitly dispatch to
     * Main when needed. Operation failures are delivered as Result.failure; callback
     * failures are isolated from both executors. If lifecycle enqueue is rejected,
     * rejection is thrown synchronously to the submitting caller and no callback runs.
     *
     * The lifecycle executor is process-lifetime and uses an unbounded FIFO queue.
     * This API does not provide cancellation or backpressure; do not use it for
     * high-volume or long-running business work.
     */
    fun <T> submitSerialized(
        operation: () -> T,
        onComplete: (Result<T>) -> Unit,
    ) = submitSerializedWith(
        enqueue = { task -> executor.execute(task) },
        dispatchCallback = { task -> callbackExecutor.execute(task) },
        operation = operation,
        onComplete = onComplete,
    )

    /**
     * Test seam for the submission contract. Production supplies the lifecycle and
     * callback executors; tests can deterministically exercise synchronous rejection.
     */
    internal fun <T> submitSerializedWith(
        enqueue: (Runnable) -> Unit,
        dispatchCallback: (Runnable) -> Unit,
        operation: () -> T,
        onComplete: (Result<T>) -> Unit,
    ) {
        enqueue(
            Runnable {
                val result = runCatching(operation)
                dispatchCallback(
                    Runnable {
                        runCatching { onComplete(result) }
                    },
                )
            },
        )
    }
}
