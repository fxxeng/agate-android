package dev.agate.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Test

/**
 * R49/T1：Service 对 STOP 结果的消费契约。
 *
 * Service 本身无法在 JVM 单测里实例化（本模块只有 junit + org.json，没有
 * Robolectric / android.jar 运行时），因此把「放锁 / 退出 / 保留 / 复核」
 * 的判定抽进 [StopLifecyclePolicy]，在这里覆盖失败路径与竞态矩阵；
 * Service 内只剩把 4 个分支接到真实回调上的接线。
 */
class StopLifecyclePolicyTest {

    @Test
    fun confirmedStopReleasesKeepAwakeLocksAndExitsService() {
        assertEquals(
            StopServiceAction.ReleaseAndExit,
            StopLifecyclePolicy.actionFor(StopExecutionResult.Stopped),
        )
    }

    @Test
    fun unconfirmedStopKeepsForegroundServiceAndLocks() {
        assertEquals(
            StopServiceAction.KeepAndRecheck,
            StopLifecyclePolicy.actionFor(StopExecutionResult.StopTimedOut),
        )
    }

    @Test
    fun failedStopKeepsForegroundServiceAndLocks() {
        val failed = StopExecutionResult.Failed(code = "state_poisoned", message = "state poisoned")
        assertEquals(
            StopServiceAction.KeepAndRecheck,
            StopLifecyclePolicy.actionFor(failed),
        )
    }

    @Test
    fun staleStopYieldsToNewerStartWithoutTouchingResources() {
        assertEquals(
            StopServiceAction.DoNothing,
            StopLifecyclePolicy.actionFor(StopExecutionResult.SkippedByNewerIntent),
        )
    }

    @Test
    fun recheckExitsAsSoonAsNativeConfirmsStop() {
        assertEquals(
            StopRecheckAction.Exit,
            StopLifecyclePolicy.recheckAction(stopped = true, attempt = 1, maxAttempts = 3),
        )
        assertEquals(
            StopRecheckAction.Exit,
            StopLifecyclePolicy.recheckAction(stopped = true, attempt = 3, maxAttempts = 3),
        )
    }

    @Test
    fun recheckRetriesWhileAttemptsRemain() {
        assertEquals(
            StopRecheckAction.Retry,
            StopLifecyclePolicy.recheckAction(stopped = false, attempt = 1, maxAttempts = 3),
        )
        assertEquals(
            StopRecheckAction.Retry,
            StopLifecyclePolicy.recheckAction(stopped = false, attempt = 2, maxAttempts = 3),
        )
    }

    @Test
    fun recheckGivesUpIntoDiagnosableStateInsteadOfExitingForegroundService() {
        // 任务书 §2.4/§2.6：Gateway 仍运行不能退出前台；达到上限要保留可诊断状态。
        assertEquals(
            StopRecheckAction.KeepService,
            StopLifecyclePolicy.recheckAction(stopped = false, attempt = 3, maxAttempts = 3),
        )
        assertEquals(
            StopRecheckAction.KeepService,
            StopLifecyclePolicy.recheckAction(stopped = false, attempt = 9, maxAttempts = 3),
        )
    }

    @Test
    fun failedResultRetainsNativeCodeMessageAndCause() {
        val cause = IllegalStateException("jni")
        val failed = StopExecutionResult.Failed(code = "boom", message = "native", cause = cause)

        assertEquals("boom", failed.code)
        assertEquals("native", failed.message)
        assertSame(cause, failed.cause)
    }
}
