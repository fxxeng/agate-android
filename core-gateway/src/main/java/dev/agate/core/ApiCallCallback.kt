package dev.agate.core

/**
 * 管理面异步直调回调（R50/T10）。
 *
 * Native 在请求完成、取消或超时后**恰好调用一次** `onComplete`；`replyJson`
 * 与同步 `apiCall` 的信封相同（`{ok,status,body}` 或 `{ok:false,error}`）。
 * 回调线程由 Native 决定（Tokio worker 附着到 JVM），实现方不得做重活。
 */
fun interface ApiCallCallback {
    fun onComplete(requestId: Long, replyJson: String)
}
