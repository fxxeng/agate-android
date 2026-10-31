package dev.agate.android.data

import dev.agate.core.Gateway
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
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
 *
 * ── 三道韧性处理（对应《问题审计》§2/§3）──────────────
 * 1. `core_not_ready` 是「内核仍在 bootstrap」（`AgateApp` 后台线程初始化），
 *    不是业务错误 —— 重试 3 次、间隔 200ms，冷启动首屏不再直接报错。
 * 2. 内核 error 是英文内部串（`core_not_ready` / `timeout after 120s`），
 *    直接透给用户看不懂 —— 统一走 [humanize] 映射。
 * 3. 链路异常（JNI 抛错 / 返回非 JSON）也归 `Down`，与业务错误区分。
 */
object CoreTransport {

    @Volatile private var apiKey: String = ""

    /** `core_not_ready` 的重试次数与间隔（总计约 0.6s，覆盖冷启动窗口）。 */
    private const val RETRY = 3
    private const val RETRY_DELAY_MS = 200L

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
            var lastError: String? = null
            for (attempt in 0 until RETRY) {
                val raw = try {
                    Gateway.apiCall(method, path, body, apiKey)
                } catch (cancelled: kotlinx.coroutines.CancellationException) {
                    throw cancelled
                } catch (_: LinkageError) {
                    return@withContext Reply.Down("Native 内核库不可用，请重启应用")
                } catch (error: Exception) {
                    val detail = error.message?.takeIf { it.isNotBlank() }
                    return@withContext Reply.Down(
                        detail?.let { "Native 调用失败：$it" } ?: "Native 调用失败"
                    )
                }
                val obj = try {
                    JSONObject(raw)
                } catch (_: Exception) {
                    return@withContext Reply.Down("内核响应异常")
                }
                if (obj.optBoolean("ok")) {
                    return@withContext Reply.Ok(obj.optInt("status", 0), obj.optString("body"))
                }
                val error = obj.optString("error")
                lastError = error
                // 只有「内核还没就绪」值得重试；业务错误立即回落
                if (error.contains("core_not_ready") && attempt < RETRY - 1) {
                    delay(RETRY_DELAY_MS)
                    continue
                }
                return@withContext Reply.Down(humanize(error))
            }
            Reply.Down(humanize(lastError))
        }

    suspend fun get(path: String): Reply = call("GET", path)

    suspend fun post(path: String, body: String): Reply = call("POST", path, body)

    /** 内核内部错误串 → 用户可读文案（未识别的原样返回，便于排查）。 */
    private fun humanize(raw: String?): String {
        val text = raw?.trim().orEmpty()
        return when {
            text.isEmpty() -> "内核未就绪"
            text.contains("core_not_ready") -> "内核启动中，请稍后再试"
            text.startsWith("timeout after") -> "操作超时（超过 2 分钟），请稍后重试"
            text.contains("router dispatch failed") -> "内核处理失败，请重试"
            text.startsWith("invalid method") ||
                text.startsWith("invalid path") ||
                text.contains("path must start") -> "内核参数错误"
            else -> text
        }
    }
}
