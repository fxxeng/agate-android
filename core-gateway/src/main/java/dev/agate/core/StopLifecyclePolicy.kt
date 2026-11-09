package dev.agate.core

/**
 * [StopExecutionResult] 对应的 Service 动作（R49/T1）。
 *
 * 把「什么时候可以释放保活锁 / 什么时候可以退出前台」从 Service 的回调里抽出来，
 * 做成不依赖 Android 的纯逻辑，JVM 单测即可覆盖失败路径（任务书 §2.7 要求
 * 「不能只测 RuntimeController，而不测 Service 对结果的消费」）。
 */
internal enum class StopServiceAction {
    /** 网关已确认停止：释放 WakeLock/WifiLock 并结束前台服务。 */
    ReleaseAndExit,

    /** 停止未确认 / 失败：保留前台服务与保活锁，进入有界复核。 */
    KeepAndRecheck,

    /** 旧 STOP 被更新的 START 抑制：不停止、不释放，交给新 START。 */
    DoNothing,
}

/** 复核轮次的动作（任务书 §2.5/§2.6：有界、可诊断、不无限重试）。 */
internal enum class StopRecheckAction {
    /** 权威观测确认已停：释放资源并退出前台。 */
    Exit,

    /** 仍未停：还有次数就继续复核。 */
    Retry,

    /** 次数用尽：保留服务与保活资源，通知给出可诊断状态。 */
    KeepService,
}

internal object StopLifecyclePolicy {

    fun actionFor(result: StopExecutionResult): StopServiceAction = when (result) {
        StopExecutionResult.Stopped -> StopServiceAction.ReleaseAndExit
        StopExecutionResult.StopTimedOut -> StopServiceAction.KeepAndRecheck
        is StopExecutionResult.Failed -> StopServiceAction.KeepAndRecheck
        StopExecutionResult.SkippedByNewerIntent -> StopServiceAction.DoNothing
    }

    /**
     * 复核判定。
     *
     * @param stopped   权威 status 是否确认网关已停
     * @param attempt   本次是第几轮复核（从 1 开始）
     * @param maxAttempts 最大轮次；`attempt >= maxAttempts` 即用尽
     */
    fun recheckAction(stopped: Boolean, attempt: Int, maxAttempts: Int): StopRecheckAction = when {
        stopped -> StopRecheckAction.Exit
        attempt >= maxAttempts -> StopRecheckAction.KeepService
        else -> StopRecheckAction.Retry
    }
}
