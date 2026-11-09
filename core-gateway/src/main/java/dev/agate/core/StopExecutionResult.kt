package dev.agate.core

/**
 * 一次 Gateway STOP 的**执行结果**（R49/T1）。
 *
 * R48 用 Boolean 表达 STOP 结果：`true` 只表示「这条 STOP 没有被更新的 START
 * 抑制」，Native 返回 `ok=false` 时同样是 `true` —— Service 据此释放
 * WakeLock/WifiLock 并 `stopSelfResult()`，把「停止失败」当成「停止成功」。
 *
 * 结果必须自带语义，调用方就不能再猜：
 *
 *  - [SkippedByNewerIntent]：守卫读到更新的 START 意图，**没有执行** STOP；
 *  - [Stopped]：Native 明确已停（`not_running` 幂等成功也归入此态）；
 *  - [StopTimedOut]：停止已发出，但端口释放 5s 内未确认（`released=false`）；
 *  - [Failed]：Native 拒绝或 JNI 抛异常，网关**可能仍在运行**。
 *
 * [Failed.cause] 保留原始异常（JNI 抛错时非空），供调用方记录/上报；
 * 结果化不等于吞掉错误。
 */
internal sealed interface StopExecutionResult {

    data object SkippedByNewerIntent : StopExecutionResult

    data object Stopped : StopExecutionResult

    data object StopTimedOut : StopExecutionResult

    data class Failed(
        val code: String? = null,
        val message: String? = null,
        val cause: Throwable? = null,
    ) : StopExecutionResult
}
