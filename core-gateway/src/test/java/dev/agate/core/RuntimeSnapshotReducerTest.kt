package dev.agate.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotSame
import org.junit.Assert.assertSame
import org.junit.Test

class RuntimeSnapshotReducerTest {
    @Test
    fun coreAndGatewayHaveIndependentLifecycles() {
        val coreReady = reduce(RuntimeSnapshot(), RuntimeStateEvent.CoreInitializationStarted, 10)
            .let { reduce(it, RuntimeStateEvent.CoreInitializationSucceeded, 20) }
        val gatewayStarting = reduce(coreReady, RuntimeStateEvent.GatewayStartStarted, 30)

        assertEquals(CoreRuntimeState.Ready, gatewayStarting.core)
        assertEquals(GatewayRuntimeState.Starting, gatewayStarting.gateway)
        assertEquals(null, gatewayStarting.schedulerEnabled)
    }

    @Test
    fun coreFailureRetainsStructuredCauseAndCanRetry() {
        val failed = reduce(RuntimeSnapshot(), RuntimeStateEvent.CoreInitializationStarted, 1)
            .let { reduce(it, RuntimeStateEvent.CoreInitializationFailed("bootstrap", "io", "denied"), 2) }

        assertEquals(CoreRuntimeState.Failed("bootstrap", "io", "denied"), failed.core)
        val retry = reduce(failed, RuntimeStateEvent.CoreInitializationStarted, 3)
        assertEquals(CoreRuntimeState.Initializing, retry.core)
        assertEquals(failed.revision + 1, retry.revision)
    }

    @Test
    fun gatewayStartStopTransitionsPreserveNativeDetails() {
        val running = reduce(RuntimeSnapshot(), RuntimeStateEvent.GatewayStartStarted, 1)
            .let { reduce(it, RuntimeStateEvent.GatewayStartSucceeded(8787, true, "3.0.8"), 2) }
        assertEquals(GatewayRuntimeState.Running(8787, true, "3.0.8"), running.gateway)

        val stopped = reduce(running, RuntimeStateEvent.GatewayStopStarted, 3)
            .let { reduce(it, RuntimeStateEvent.GatewayStopSucceeded, 4) }
        assertEquals(GatewayRuntimeState.Stopped, stopped.gateway)
        assertEquals(CoreRuntimeState.NotInitialized, stopped.core)
    }

    @Test
    fun schedulerObservationIsIndependentAndNullableUntilObserved() {
        val initial = RuntimeSnapshot()
        assertEquals(null, initial.schedulerEnabled)
        val observed = reduce(initial, RuntimeStateEvent.SchedulerObserved(true), 7)
        assertEquals(true, observed.schedulerEnabled)
        assertEquals(GatewayRuntimeState.Stopped, observed.gateway)
    }

    @Test
    fun outOfOrderCompletionIsIgnoredWithoutRevisionChange() {
        val initial = RuntimeSnapshot()
        val ignoredCore = reduce(initial, RuntimeStateEvent.CoreInitializationSucceeded, 10)
        val ignoredGateway = reduce(initial, RuntimeStateEvent.GatewayStartSucceeded(8787, false, "x"), 11)

        assertSame(initial, ignoredCore)
        assertSame(initial, ignoredGateway)
    }

    @Test
    fun startCanReconcileAStaleRunningSnapshotBeforeNativeResult() {
        val running = RuntimeSnapshot(
            core = CoreRuntimeState.Ready,
            gateway = GatewayRuntimeState.Running(8787, false, "old"),
            schedulerEnabled = true,
        )
        val starting = reduce(running, RuntimeStateEvent.GatewayStartStarted, 2)
        assertEquals(GatewayRuntimeState.Starting, starting.gateway)

        val refreshed = reduce(
            starting,
            RuntimeStateEvent.GatewayStartSucceeded(8899, true, "new"),
            3,
        )
        assertEquals(GatewayRuntimeState.Running(8899, true, "new"), refreshed.gateway)
    }

    @Test
    fun duplicateStartAndStopEventsAreIdempotent() {
        val starting = reduce(RuntimeSnapshot(), RuntimeStateEvent.GatewayStartStarted, 1)
        assertSame(starting, reduce(starting, RuntimeStateEvent.GatewayStartStarted, 2))
        val running = reduce(starting, RuntimeStateEvent.GatewayStartSucceeded(8787, false, "x"), 3)
        val stopping = reduce(running, RuntimeStateEvent.GatewayStopStarted, 4)
        assertSame(stopping, reduce(stopping, RuntimeStateEvent.GatewayStopStarted, 5))
    }

    @Test
    fun revisionAndTimestampAdvanceOnlyWhenStateChanges() {
        val initial = RuntimeSnapshot()
        val changed = reduce(initial, RuntimeStateEvent.CoreInitializationStarted, 100)
        assertEquals(1L, changed.revision)
        assertEquals(100L, changed.updatedAtMillis)
        val unchanged = reduce(changed, RuntimeStateEvent.CoreInitializationStarted, 200)
        assertSame(changed, unchanged)
        assertNotSame(initial, changed)
    }

    @Test
    fun gatewayFailureRetainsStageCodeAndMessage() {
        val failed = reduce(RuntimeSnapshot(), RuntimeStateEvent.GatewayStartStarted, 1)
            .let { reduce(it, RuntimeStateEvent.GatewayFailed("listen", "address_in_use", "busy"), 2) }
        assertEquals(GatewayRuntimeState.Failed("listen", "address_in_use", "busy"), failed.gateway)
    }


    @Test
    fun stoppingObservationDoesNotCollapseIntoStopped() {
        // R50/T1：status 的 stopping 必须映射为 Stopping，而不是 Stopped。
        val running = RuntimeSnapshot(gateway = GatewayRuntimeState.Running(8787, false, "x"))
        val stopping = reduce(
            running,
            RuntimeStateEvent.NativeStatusObserved(
                coreReady = true,
                gatewayRunning = false,
                port = 8787,
                bindLan = false,
                version = "x",
                schedulerEnabled = false,
                stopping = true,
            ),
            5,
        )
        assertEquals(GatewayRuntimeState.Stopping, stopping.gateway)

        val stopped = reduce(
            stopping,
            RuntimeStateEvent.NativeStatusObserved(
                coreReady = true,
                gatewayRunning = false,
                port = 8787,
                bindLan = false,
                version = "x",
                schedulerEnabled = false,
            ),
            6,
        )
        assertEquals(GatewayRuntimeState.Stopped, stopped.gateway)
    }

    @Test
    fun nativeStatusReconcilesAllThreeIndependentDimensions() {
        val observed = reduce(
            RuntimeSnapshot(
                core = CoreRuntimeState.Initializing,
                gateway = GatewayRuntimeState.Starting,
                schedulerEnabled = null,
            ),
            RuntimeStateEvent.NativeStatusObserved(
                coreReady = true,
                gatewayRunning = true,
                port = 8787,
                bindLan = true,
                version = "3.0.8",
                schedulerEnabled = false,
            ),
            50,
        )

        assertEquals(CoreRuntimeState.Ready, observed.core)
        assertEquals(GatewayRuntimeState.Running(8787, true, "3.0.8"), observed.gateway)
        assertEquals(false, observed.schedulerEnabled)
        assertEquals(1L, observed.revision)
        assertEquals(50L, observed.updatedAtMillis)
    }

    @Test
    fun nativeStoppedStatusDoesNotResetReadyCore() {
        val current = RuntimeSnapshot(
            core = CoreRuntimeState.Ready,
            gateway = GatewayRuntimeState.Running(8787, false, "3.0.8"),
            schedulerEnabled = true,
        )
        val observed = reduce(
            current,
            RuntimeStateEvent.NativeStatusObserved(
                coreReady = true,
                gatewayRunning = false,
                port = 0,
                bindLan = false,
                version = "3.0.8",
                schedulerEnabled = true,
            ),
            60,
        )

        assertEquals(CoreRuntimeState.Ready, observed.core)
        assertEquals(GatewayRuntimeState.Stopped, observed.gateway)
        assertEquals(true, observed.schedulerEnabled)
    }

    @Test
    fun nativeStatusCanRepresentCoreNotReadyAndGatewayNotRunning() {
        val observed = reduce(
            RuntimeSnapshot(
                core = CoreRuntimeState.Ready,
                gateway = GatewayRuntimeState.Running(8787, false, "3.0.8"),
                schedulerEnabled = true,
            ),
            RuntimeStateEvent.NativeStatusObserved(
                coreReady = false,
                gatewayRunning = false,
                port = 0,
                bindLan = false,
                version = "3.0.8",
                schedulerEnabled = false,
            ),
            70,
        )

        assertEquals(CoreRuntimeState.NotInitialized, observed.core)
        assertEquals(GatewayRuntimeState.Stopped, observed.gateway)
        assertEquals(false, observed.schedulerEnabled)
    }

    @Test
    fun identicalNativeStatusDoesNotAdvanceRevision() {
        val current = RuntimeSnapshot(
            core = CoreRuntimeState.Ready,
            gateway = GatewayRuntimeState.Running(8787, false, "3.0.8"),
            schedulerEnabled = true,
        )
        val observed = reduce(
            current,
            RuntimeStateEvent.NativeStatusObserved(
                coreReady = true,
                gatewayRunning = true,
                port = 8787,
                bindLan = false,
                version = "3.0.8",
                schedulerEnabled = true,
            ),
            80,
        )

        assertSame(current, observed)
    }

    private fun reduce(
        snapshot: RuntimeSnapshot,
        event: RuntimeStateEvent,
        nowMillis: Long,
    ): RuntimeSnapshot = RuntimeSnapshotReducer.reduce(snapshot, event, nowMillis)

    @Test
    fun coreFailureCanFollowAnAuthoritativeNotInitializedObservation() {
        val observed = reduce(
            RuntimeSnapshot(),
            RuntimeStateEvent.NativeStatusObserved(
                coreReady = false,
                gatewayRunning = false,
                port = 0,
                bindLan = false,
                version = "test",
                schedulerEnabled = false,
            ),
            1,
        )
        val failed = reduce(
            observed,
            RuntimeStateEvent.CoreInitializationFailed("bootstrap", null, "denied"),
            2,
        )

        assertEquals(CoreRuntimeState.Failed("bootstrap", null, "denied"), failed.core)
        assertEquals(false, failed.schedulerEnabled)
    }

    @Test
    fun stopTimeoutReplacesStoppedWhenReleaseUnconfirmed() {
        // R47/T3.4：released=false 的停止结果走 GatewayStopTimedOut → StopTimeout，
        // 不再假装 Stopped。
        val running = reduce(RuntimeSnapshot(), RuntimeStateEvent.GatewayStartStarted, 1)
            .let { reduce(it, RuntimeStateEvent.GatewayStartSucceeded(8787, false, "3.0.8"), 2) }
        val stopping = reduce(running, RuntimeStateEvent.GatewayStopStarted, 3)
        val timedOut = reduce(stopping, RuntimeStateEvent.GatewayStopTimedOut, 4)
        assertEquals(GatewayRuntimeState.StopTimeout, timedOut.gateway)

        // 乱序/重复保护与 Stopped 同口径：非 Stopping 下的 TimedOut 被忽略
        assertSame(timedOut, reduce(timedOut, RuntimeStateEvent.GatewayStopTimedOut, 5))
    }

    @Test
    fun freshStartCanFollowStopTimeoutAndNativeStatusReconcilesIt() {
        val timedOut = reduce(RuntimeSnapshot(), RuntimeStateEvent.GatewayStartStarted, 1)
            .let { reduce(it, RuntimeStateEvent.GatewayStartSucceeded(8787, false, "x"), 2) }
            .let { reduce(it, RuntimeStateEvent.GatewayStopStarted, 3) }
            .let { reduce(it, RuntimeStateEvent.GatewayStopTimedOut, 4) }
        assertEquals(GatewayRuntimeState.StopTimeout, timedOut.gateway)

        // 快速 STOP→START：新 START 是权威命令，必须能从 StopTimeout 迁移，
        // 否则修掉状态假象会换来卡死在未确认态。
        val starting = reduce(timedOut, RuntimeStateEvent.GatewayStartStarted, 5)
        assertEquals(GatewayRuntimeState.Starting, starting.gateway)

        // 权威探测归位：NativeStatusObserved 把 StopTimeout 收敛为权威态
        val reconciled = reduce(
            timedOut,
            RuntimeStateEvent.NativeStatusObserved(
                coreReady = true,
                gatewayRunning = false,
                port = 0,
                bindLan = false,
                version = "x",
                schedulerEnabled = true,
            ),
            6,
        )
        assertEquals(GatewayRuntimeState.Stopped, reconciled.gateway)
    }
}
