package dev.agate.android.data

import dev.agate.core.ApiCallCallback
import dev.agate.core.CoreRuntimeState
import dev.agate.core.Gateway
import dev.agate.core.RuntimeController
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import kotlin.coroutines.resume
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import org.json.JSONObject

/**
 * 管理面内核直调通道（P2）：method+path+body 经 JNI 走内核内存 Router，
 * 不经 127.0.0.1 端口与网络栈 —— 网关「运行/停止」只是端口开关（B2 语义），
 * Core 态（网关未运行）下管理操作照常可用。
 *
 * [Reply.Ok.status] 透传内核 HTTP 语义（401 未授权等由调用方判定，不吞）；
 * 各 Ops 保持各自的业务信封解析，本通道只管链路。
 *
 * 刻意不走本通道的两处例外：
 * - `KernelOps.isUp()`：探测端口活性，语义上必须走 TCP；
 * - `GatewayRepository` 的 SSE `/api/events`：流式响应不适合 oneshot，
 *   且实时事件只在网关运行时有意义（无网关时已有轮询降级）。
 *
 * ── 四道韧性处理（对应《问题审计》§2/§3 + R50/T10）──────────
 * 1. `core_not_ready` 是「内核仍在 bootstrap」（`AgateApp` 后台线程初始化），
 *    不是业务错误 —— 在 15s 总预算内每 200ms 重试；观察到 Core 进入 Failed
 *    时立刻返回可诊断的失败，而不是把 15s 耗完（R50/T8）。
 * 2. 内核 error 是英文内部串（`core_not_ready` / `timeout after 120s`），
 *    直接透给用户看不懂 —— 统一走 [humanize] 映射。
 * 3. 链路异常（JNI 抛错 / 返回非 JSON）也归 `Down`，与业务错误区分。
 * 4. R50/T10：真正的调用走 `apiCallAsync` + [ApiCallCallback]，调用线程不再
 *    被 120s JNI 阻塞；超时/协程取消会 `nativeApiCancel` 让 Rust 真正 abort，
 *    不再出现「已返回超时、内核任务还在跑」。
 */
object CoreTransport {

    @Volatile private var apiKey: String = ""

    /** `core_not_ready` 的总等待预算与轮询间隔（覆盖冷启动 bootstrap）。 */
    private const val CORE_READY_TIMEOUT_MS = 15_000L
    private const val RETRY_DELAY_MS = 200L

    /** Native 侧 120s 超时之外的 Kotlin 兜底（防回调丢失导致永久挂起）。 */
    private const val NATIVE_TIMEOUT_MS = 125_000L

    private val nextRequestId = AtomicLong(1)
    private val pending = ConcurrentHashMap<Long, CancellableContinuation<Reply>>()

    @Volatile private var callbackInstalled = false

    private val apiCallback = ApiCallCallback { requestId, replyJson ->
        pending.remove(requestId)?.resume(parseReply(replyJson))
    }

    fun configure(apiKey: String) {
        this.apiKey = apiKey
    }

    sealed class Reply {
        /** 内核有响应：status 为 HTTP 语义码，text 为响应原文。 */
        data class Ok(val status: Int, val text: String) : Reply()

        /**
         * 链路不可达：内核未初始化 / 分发失败 / 超时（区别于业务错误信封）。
         * [raw] 保留内核原始错误串，供重试判定（message 已 humanize）。
         */
        data class Down(val message: String, val raw: String? = null) : Reply()
    }

    suspend fun call(method: String, path: String, body: String? = null): Reply =
        withContext(Dispatchers.IO) {
            val deadlineNanos = System.nanoTime() + CORE_READY_TIMEOUT_MS * 1_000_000
            while (true) {
                val reply = withTimeoutOrNull(NATIVE_TIMEOUT_MS) {
                    awaitOnce(method, path, body)
                } ?: Reply.Down("操作超时（超过 2 分钟），请稍后重试")

                // 只有「内核还没就绪」值得等待；业务错误立即回落。
                if (reply is Reply.Down && reply.raw?.contains("core_not_ready") == true) {
                    val core = RuntimeController.snapshot().core
                    if (core is CoreRuntimeState.Failed) {
                        return@withContext Reply.Down("内核初始化失败：${core.message}")
                    }
                    if (System.nanoTime() >= deadlineNanos) {
                        return@withContext Reply.Down("内核启动超时，请稍后重试")
                    }
                    delay(RETRY_DELAY_MS)
                    continue
                }
                return@withContext reply
            }
            // while(true) 在 Kotlin 里是 Unit 表达式，编译器不会按 Nothing 推断；
            // 显式给一个不可达值，保持 call() 的 Reply 返回类型。
            @Suppress("UNREACHABLE_CODE")
            Reply.Down("内核未就绪")
        }

    suspend fun get(path: String): Reply = call("GET", path)

    suspend fun post(path: String, body: String): Reply = call("POST", path, body)

    private suspend fun awaitOnce(method: String, path: String, body: String?): Reply {
        ensureCallback()?.let { return Reply.Down(it) }
        val requestId = nextRequestId.getAndIncrement()
        return suspendCancellableCoroutine { continuation ->
            pending[requestId] = continuation
            continuation.invokeOnCancellation {
                // 协程超时/取消 → 让 Rust 真正 abort，而不是留下一个继续跑的
                // 120s JNI 任务。
                if (pending.remove(requestId) != null) {
                    runCatching { Gateway.cancelApiCall(requestId) }
                }
            }
            val accepted = try {
                JSONObject(Gateway.apiCallAsync(requestId, method, path, body, apiKey))
            } catch (error: Throwable) {
                pending.remove(requestId)
                continuation.resume(Reply.Down(nativeFailure(error)))
                return@suspendCancellableCoroutine
            }
            if (!accepted.optBoolean("ok")) {
                pending.remove(requestId)
                val raw = accepted.optString("error")
                continuation.resume(Reply.Down(humanize(raw), raw))
            }
        }
    }

    /** 注册一次；失败时返回可展示的错误，Native 侧仍无回调，调用方不要继续发请求。 */
    private fun ensureCallback(): String? {
        if (callbackInstalled) return null
        return synchronized(this) {
            if (callbackInstalled) {
                null
            } else {
                val reply = try {
                    JSONObject(Gateway.setApiCallback(apiCallback))
                } catch (error: Throwable) {
                    return@synchronized nativeFailure(error)
                }
                if (reply.optBoolean("ok")) {
                    callbackInstalled = true
                    null
                } else {
                    humanize(reply.optString("error"))
                }
            }
        }
    }

    private fun parseReply(raw: String): Reply = try {
        val obj = JSONObject(raw)
        if (obj.optBoolean("ok")) {
            Reply.Ok(obj.optInt("status", 0), obj.optString("body"))
        } else {
            val error = obj.optString("error")
            Reply.Down(humanize(error), error)
        }
    } catch (_: Exception) {
        Reply.Down("内核响应异常")
    }

    private fun nativeFailure(error: Throwable): String {
        if (error is LinkageError) return "Native 内核库不可用，请重启应用"
        val detail = error.message?.takeIf { it.isNotBlank() }
        return detail?.let { "Native 调用失败：$it" } ?: "Native 调用失败"
    }

    /** 内核内部错误串 → 用户可读文案（未识别的原样返回，便于排查）。 */
    private fun humanize(raw: String?): String {
        val text = raw?.trim().orEmpty()
        return when {
            text.isEmpty() -> "内核未就绪"
            text.contains("core_not_ready") -> "内核启动中，请稍后再试"
            text.startsWith("timeout after") -> "操作超时（超过 2 分钟），请稍后重试"
            text == "cancelled" -> "请求已取消"
            text.contains("router dispatch failed") -> "内核处理失败，请重试"
            text.contains("response body exceeds") -> "响应体过大（超过 16 MiB）"
            text.startsWith("invalid method") ||
                text.startsWith("invalid path") ||
                text.contains("path must start") -> "内核参数错误"
            else -> text
        }
    }
}
