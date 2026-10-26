package dev.agate.android.data

import dev.agate.core.Gateway
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
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
 */
object CoreTransport {

    @Volatile private var apiKey: String = ""

    fun configure(apiKey: String) {
        this.apiKey = apiKey
    }

    sealed class Reply {
        /** 内核有响应：status 为 HTTP 语义码，text 为响应原文。 */
        data class Ok(val status: Int, val text: String) : Reply()

        /** 链路不可达：内核未初始化 / 分发失败 / 超时（区别于业务错误信封）。 */
        data class Down(val message: String) : Reply()
    }

    suspend fun call(method: String, path: String, body: String? = null): Reply =
        withContext(Dispatchers.IO) {
            val raw = try {
                Gateway.apiCall(method, path, body, apiKey)
            } catch (_: Throwable) {
                null
            } ?: return@withContext Reply.Down("内核未就绪")
            val obj = try {
                JSONObject(raw)
            } catch (_: Exception) {
                return@withContext Reply.Down("内核响应异常")
            }
            if (!obj.optBoolean("ok")) {
                return@withContext Reply.Down(obj.optString("error").ifBlank { "内核未就绪" })
            }
            Reply.Ok(obj.optInt("status", 0), obj.optString("body"))
        }

    suspend fun get(path: String): Reply = call("GET", path)

    suspend fun post(path: String, body: String): Reply = call("POST", path, body)
}
