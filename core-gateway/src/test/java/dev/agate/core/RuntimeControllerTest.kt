package dev.agate.core

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

class RuntimeControllerTest {
    @Test
    fun lifecycleOperationsAreSerializedAcrossCallingThreads() {
        val active = AtomicInteger(0)
        val maximumActive = AtomicInteger(0)
        val ready = CountDownLatch(8)
        val start = CountDownLatch(1)
        val finished = CountDownLatch(8)

        repeat(8) {
            Thread {
                ready.countDown()
                start.await()
                RuntimeController.runSerialized {
                    val now = active.incrementAndGet()
                    maximumActive.updateAndGet { previous -> maxOf(previous, now) }
                    Thread.sleep(15)
                    active.decrementAndGet()
                }
                finished.countDown()
            }.start()
        }

        assertTrue("workers did not become ready", ready.await(2, TimeUnit.SECONDS))
        start.countDown()
        assertTrue("serialized operations did not finish", finished.await(5, TimeUnit.SECONDS))
        assertEquals(1, maximumActive.get())
    }

    @Test
    fun nestedLifecycleOperationIsReentrant() {
        val result = RuntimeController.runSerialized {
            RuntimeController.runSerialized { "nested-ok" }
        }
        assertEquals("nested-ok", result)
    }

    @Test
    fun interruptedWaiterStillReceivesCommittedLifecycleResult() {
        val blockerEntered = CountDownLatch(1)
        val releaseBlocker = CountDownLatch(1)
        RuntimeController.submitSerialized(
            operation = {
                blockerEntered.countDown()
                releaseBlocker.await(2, TimeUnit.SECONDS)
                Unit
            },
            onComplete = {},
        )
        assertTrue("blocking operation did not start", blockerEntered.await(2, TimeUnit.SECONDS))

        val completed = CountDownLatch(1)
        val result = AtomicReference<String?>()
        val interruptedAfterCompletion = AtomicReference(false)
        val caller = Thread {
            result.set(RuntimeController.runSerialized { "committed" })
            interruptedAfterCompletion.set(Thread.currentThread().isInterrupted)
            completed.countDown()
        }
        caller.start()
        val waitDeadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
        while (caller.state != Thread.State.WAITING && System.nanoTime() < waitDeadline) {
            Thread.yield()
        }
        assertEquals("caller did not wait for the queued operation", Thread.State.WAITING, caller.state)
        caller.interrupt()
        releaseBlocker.countDown()

        assertTrue("interrupted caller did not receive operation result", completed.await(3, TimeUnit.SECONDS))
        assertEquals("committed", result.get())
        assertTrue("interrupt status was not restored", interruptedAfterCompletion.get())
    }

    @Test
    fun operationFailureIsPropagatedWithoutWrappingItsCause() {
        val expected = IllegalStateException("native-failure")
        val actual = try {
            RuntimeController.runSerialized<String> { throw expected }
            null
        } catch (error: Throwable) {
            error
        }
        assertSame(expected, actual)
    }

    @Test
    fun submitSerializedCallbackRunsOffLifecycleWorkerAndCannotBlockIt() {
        val operationThread = AtomicReference<Thread?>()
        val callbackThread = AtomicReference<Thread?>()
        val callbackEntered = CountDownLatch(1)
        val releaseCallback = CountDownLatch(1)
        val nextLifecycleCompleted = CountDownLatch(1)

        RuntimeController.submitSerialized(
            operation = {
                operationThread.set(Thread.currentThread())
                "first"
            },
            onComplete = {
                callbackThread.set(Thread.currentThread())
                callbackEntered.countDown()
                releaseCallback.await(2, TimeUnit.SECONDS)
            },
        )

        assertTrue("completion callback did not start", callbackEntered.await(2, TimeUnit.SECONDS))
        val nextResult = AtomicReference<String?>()
        val nextCaller = Thread {
            nextResult.set(RuntimeController.runSerialized { "second" })
            nextLifecycleCompleted.countDown()
        }
        nextCaller.start()

        val lifecycleContinuedWhileCallbackBlocked =
            nextLifecycleCompleted.await(2, TimeUnit.SECONDS)
        releaseCallback.countDown()

        assertTrue(
            "a blocked completion callback stalled the lifecycle queue",
            lifecycleContinuedWhileCallbackBlocked,
        )
        assertEquals("second", nextResult.get())
        assertTrue("callback must not execute on lifecycle worker", operationThread.get() !== callbackThread.get())
    }

    @Test
    fun submitSerializedDeliversOperationFailureAndIsolatesCallbackFailure() {
        val firstCallback = CountDownLatch(1)
        val secondCallback = CountDownLatch(1)
        val expected = IllegalStateException("async-operation-failed")
        val receivedFailure = AtomicReference<Throwable?>()
        val secondResult = AtomicReference<String?>()

        RuntimeController.submitSerialized(
            operation = { throw expected },
            onComplete = { result ->
                receivedFailure.set(result.exceptionOrNull())
                firstCallback.countDown()
                throw IllegalStateException("callback-failed")
            },
        )
        assertTrue("failed operation result was not delivered", firstCallback.await(2, TimeUnit.SECONDS))
        assertSame(expected, receivedFailure.get())

        RuntimeController.submitSerialized(
            operation = { "queue-still-alive" },
            onComplete = { result ->
                secondResult.set(result.getOrNull())
                secondCallback.countDown()
            },
        )

        assertTrue("callback dispatcher did not survive callback failure",
            secondCallback.await(2, TimeUnit.SECONDS))
        assertEquals("queue-still-alive", secondResult.get())
    }

    @Test
    fun submitSerializedQueueRejectionIsSynchronousAndDoesNotInvokeCallback() {
        val expected = java.util.concurrent.RejectedExecutionException("queue-closed")
        val callbackCalled = AtomicReference(false)

        val actual = try {
            RuntimeController.submitSerializedWith(
                enqueue = { throw expected },
                dispatchCallback = { task -> task.run() },
                operation = { "must-not-run" },
                onComplete = { callbackCalled.set(true) },
            )
            null
        } catch (failure: Throwable) {
            failure
        }

        assertSame(expected, actual)
        assertFalse(callbackCalled.get())
    }

    @Test
    fun readOnlyStatusDoesNotWaitForLifecycleQueueOrPublishSnapshot() {
        val blockerEntered = CountDownLatch(1)
        val releaseBlocker = CountDownLatch(1)
        RuntimeController.submitSerialized(
            operation = {
                blockerEntered.countDown()
                releaseBlocker.await(2, TimeUnit.SECONDS)
                Unit
            },
            onComplete = {},
        )
        assertTrue("blocking lifecycle operation did not start", blockerEntered.await(2, TimeUnit.SECONDS))

        val expected = Gateway.Status(
            running = true,
            port = 8787,
            bindLan = false,
            uptimeSec = 12,
            version = "status-read-test",
            core = true,
            scheduler = true,
        )
        val before = RuntimeController.snapshot()
        val actualThread = AtomicReference<Thread?>()
        val result = AtomicReference<Gateway.Status?>()
        val statusCompleted = CountDownLatch(1)
        val caller = Thread {
            result.set(
                RuntimeController.readStatusWith {
                    actualThread.set(Thread.currentThread())
                    expected
                },
            )
            statusCompleted.countDown()
        }
        caller.start()

        val completedBeforeLifecycleRelease = statusCompleted.await(500, TimeUnit.MILLISECONDS)
        releaseBlocker.countDown()
        assertTrue(
            "read-only status was queued behind lifecycle work",
            completedBeforeLifecycleRelease,
        )
        assertTrue("read-only status caller did not finish", statusCompleted.await(2, TimeUnit.SECONDS))
        assertSame(expected, result.get())
        assertSame("read-only status must execute on its caller thread", caller, actualThread.get())
        assertSame("read-only status must not publish RuntimeSnapshot", before, RuntimeController.snapshot())
    }

    @Test
    fun runtimeSnapshotCanBeReadWithoutEnteringLifecycleQueue() {
        val before = RuntimeController.snapshot()
        assertSame(before, RuntimeController.snapshot())
    }

    @Test
    fun runtimeSnapshotUpdatesAreRestrictedToLifecycleWorker() {
        val error = try {
            RuntimeController.recordStateEvent(
                RuntimeStateEvent.NativeStatusObserved(
                    coreReady = true,
                    gatewayRunning = true,
                    port = 8787,
                    bindLan = false,
                    version = "test",
                    schedulerEnabled = false,
                ),
                nowMillis = 10,
            )
            null
        } catch (failure: IllegalStateException) {
            failure
        }
        assertTrue("state update outside lifecycle worker must be rejected", error != null)
    }

    @Test
    fun runtimeSnapshotIsPublishedAfterSerializedStateReduction() {
        val before = RuntimeController.snapshot()
        val event = RuntimeStateEvent.NativeStatusObserved(
            coreReady = before.core !is CoreRuntimeState.Ready,
            gatewayRunning = before.gateway !is GatewayRuntimeState.Running,
            port = 8787,
            bindLan = true,
            version = "controller-test",
            schedulerEnabled = before.schedulerEnabled != true,
        )

        RuntimeController.runSerialized {
            RuntimeController.recordStateEvent(event, nowMillis = 1234)
        }

        val after = RuntimeController.snapshot()
        assertEquals(before.revision + 1, after.revision)
        assertEquals(1234L, after.updatedAtMillis)
        assertEquals(
            if (event.coreReady) CoreRuntimeState.Ready else CoreRuntimeState.NotInitialized,
            after.core,
        )
        assertEquals(
            if (event.gatewayRunning) {
                GatewayRuntimeState.Running(8787, true, "controller-test")
            } else {
                GatewayRuntimeState.Stopped
            },
            after.gateway,
        )
        assertEquals(event.schedulerEnabled, after.schedulerEnabled)
    }

    @Test
    fun coreInitRunsAndReconcilesNativeStatusInOneSerializedTask() {
        val calls = mutableListOf<String>()
        val before = RuntimeController.snapshot()
        val expectedStatus = Gateway.Status(
            running = before.gateway !is GatewayRuntimeState.Running,
            port = 8787,
            bindLan = true,
            uptimeSec = 0,
            version = "core-init-test",
            core = before.core !is CoreRuntimeState.Ready,
            scheduler = before.schedulerEnabled != true,
        )

        val result = RuntimeController.coreInitWith(
            initialize = {
                calls += "coreInit"
                "native-ok"
            },
            isSuccessful = { it == "native-ok" },
            failureEvent = { error("failure mapper must not run") },
            readStatus = {
                calls += "status"
                expectedStatus
            },
            nowMillis = { 2000L },
        )

        assertEquals("native-ok", result)
        assertEquals(listOf("coreInit", "status"), calls)
        val snapshot = RuntimeController.snapshot()
        assertEquals(
            if (expectedStatus.core) CoreRuntimeState.Ready else CoreRuntimeState.NotInitialized,
            snapshot.core,
        )
        assertEquals(
            if (expectedStatus.running) {
                GatewayRuntimeState.Running(8787, true, "core-init-test")
            } else {
                GatewayRuntimeState.Stopped
            },
            snapshot.gateway,
        )
        assertEquals(expectedStatus.scheduler, snapshot.schedulerEnabled)
        assertEquals(2000L, snapshot.updatedAtMillis)
    }

    @Test
    fun failedCoreInitReconcilesNativeStatusAndRetainsBootstrapFailure() {
        var statusRead = false
        var failureMapped = false

        val result = RuntimeController.coreInitWith(
            initialize = { "native-error" },
            isSuccessful = { false },
            failureEvent = {
                failureMapped = true
                RuntimeStateEvent.CoreInitializationFailed(
                    stage = "bootstrap",
                    code = null,
                    message = it,
                )
            },
            readStatus = {
                statusRead = true
                Gateway.Status(false, 0, false, 0L, "failure-test", false, false)
            },
            nowMillis = { 3000L },
        )

        assertEquals("native-error", result)
        assertTrue(failureMapped)
        assertTrue(statusRead)
        assertEquals(
            CoreRuntimeState.Failed("bootstrap", null, "native-error"),
            RuntimeController.snapshot().core,
        )
        assertEquals(false, RuntimeController.snapshot().schedulerEnabled)
    }

    @Test
    fun gatewayStartPublishesRunningStateFromSuccessfulNativeResult() {
        prepareStoppedGatewaySnapshot()
        val nativeResult = Gateway.StartResult(
            ok = true,
            port = 8899,
            bindLan = true,
            version = "start-test",
            stage = null,
            error = null,
        )

        val result = RuntimeController.startWith(
            start = { nativeResult },
            nowMillis = { 4500L },
        )

        assertSame(nativeResult, result)
        assertEquals(
            GatewayRuntimeState.Running(8899, true, "start-test"),
            RuntimeController.snapshot().gateway,
        )
        assertEquals(4500L, RuntimeController.snapshot().updatedAtMillis)
    }

    @Test
    fun alreadyRunningStartResultIsTreatedAsIdempotentSuccess() {
        prepareStoppedGatewaySnapshot()
        val nativeResult = Gateway.StartResult(
            ok = false,
            port = 8788,
            bindLan = false,
            version = "already-running-test",
            stage = null,
            error = "already_running",
        )

        val result = RuntimeController.startWith(
            start = { nativeResult },
            nowMillis = { 4600L },
        )

        assertSame(nativeResult, result)
        assertEquals(
            GatewayRuntimeState.Running(8788, false, "already-running-test"),
            RuntimeController.snapshot().gateway,
        )
    }

    @Test
    fun failedGatewayStartPublishesFailureAndReturnsNativeResult() {
        prepareStoppedGatewaySnapshot()
        val nativeResult = Gateway.StartResult(
            ok = false,
            port = 0,
            bindLan = false,
            version = "",
            stage = "bind",
            error = "address_in_use",
        )

        val result = RuntimeController.startWith(
            start = { nativeResult },
            nowMillis = { 4700L },
        )

        assertSame(nativeResult, result)
        assertEquals(
            GatewayRuntimeState.Failed("bind", null, "address_in_use"),
            RuntimeController.snapshot().gateway,
        )
    }

    @Test
    fun failedGatewayStartReplacesAStaleRunningSnapshot() {
        RuntimeController.runSerialized {
            RuntimeController.recordStateEvent(
                RuntimeStateEvent.NativeStatusObserved(
                    coreReady = true,
                    gatewayRunning = true,
                    port = 8787,
                    bindLan = false,
                    version = "stale-running",
                    schedulerEnabled = true,
                ),
                nowMillis = 4750L,
            )
        }
        val nativeResult = Gateway.StartResult(
            ok = false,
            port = 0,
            bindLan = false,
            version = "",
            stage = "bind",
            error = "address_in_use",
        )

        RuntimeController.startWith(
            start = { nativeResult },
            nowMillis = { 4775L },
        )

        assertEquals(
            GatewayRuntimeState.Failed("bind", null, "address_in_use"),
            RuntimeController.snapshot().gateway,
        )
    }

    @Test
    fun gatewayStartExceptionPublishesFailureAndRethrowsOriginal() {
        prepareStoppedGatewaySnapshot()
        val expected = IllegalStateException("jni-start-failed")

        val actual = try {
            RuntimeController.startWith(
                start = { throw expected },
                nowMillis = { 4800L },
            )
            null
        } catch (failure: Throwable) {
            failure
        }

        assertSame(expected, actual)
        assertEquals(
            GatewayRuntimeState.Failed("start", null, "jni-start-failed"),
            RuntimeController.snapshot().gateway,
        )
    }

    @Test
    fun gatewayStopGuardAndNativeCallRunInOneSerializedTask() {
        prepareRunningGatewaySnapshot()
        val callerThread = Thread.currentThread()
        val guardThread = AtomicReference<Thread?>()
        val stopThread = AtomicReference<Thread?>()
        val calls = mutableListOf<String>()

        val allowed = RuntimeController.stopWith(
            mayStopNow = {
                guardThread.set(Thread.currentThread())
                calls += "guard"
                true
            },
            stop = {
                stopThread.set(Thread.currentThread())
                calls += "stop"
                RuntimeController.StopOutcome(ok = true)
            },
            nowMillis = { 5100L },
        )

        assertTrue(allowed)
        assertEquals(listOf("guard", "stop"), calls)
        assertSame(guardThread.get(), stopThread.get())
        assertTrue(guardThread.get() !== callerThread)
        assertEquals(GatewayRuntimeState.Stopped, RuntimeController.snapshot().gateway)
        assertEquals(5100L, RuntimeController.snapshot().updatedAtMillis)
    }

    @Test
    fun gatewayStopSkipsNativeCallWhenLatestIntentDisallowsStop() {
        prepareRunningGatewaySnapshot()
        var stopCalled = false

        val allowed = RuntimeController.stopWith(
            mayStopNow = { false },
            stop = {
                stopCalled = true
                RuntimeController.StopOutcome(ok = true)
            },
            nowMillis = { 5200L },
        )

        assertFalse(allowed)
        assertFalse(stopCalled)
        assertEquals(
            GatewayRuntimeState.Running(8787, true, "stop-test"),
            RuntimeController.snapshot().gateway,
        )
    }

    @Test
    fun gatewayStopTreatsNotRunningAsIdempotentSuccess() {
        prepareRunningGatewaySnapshot()

        val allowed = RuntimeController.stopWith(
            mayStopNow = { true },
            stop = { RuntimeController.StopOutcome(ok = false, error = "not_running") },
            nowMillis = { 5300L },
        )

        assertTrue(allowed)
        assertEquals(GatewayRuntimeState.Stopped, RuntimeController.snapshot().gateway)
    }

    @Test
    fun gatewayStopPublishesNativeFailureWithoutChangingCallerDecision() {
        prepareRunningGatewaySnapshot()

        val allowed = RuntimeController.stopWith(
            mayStopNow = { true },
            stop = {
                RuntimeController.StopOutcome(
                    ok = false,
                    code = "state_poisoned",
                    error = "state poisoned",
                )
            },
            nowMillis = { 5400L },
        )

        assertTrue("guard approval remains the return value", allowed)
        assertEquals(
            GatewayRuntimeState.Failed("stop", "state_poisoned", "state poisoned"),
            RuntimeController.snapshot().gateway,
        )
    }

    @Test
    fun gatewayStopExceptionPublishesFailureAndRethrowsOriginal() {
        prepareRunningGatewaySnapshot()
        val expected = IllegalStateException("jni-stop-failed")

        val actual = try {
            RuntimeController.stopWith(
                mayStopNow = { true },
                stop = { throw expected },
                nowMillis = { 5500L },
            )
            null
        } catch (failure: Throwable) {
            failure
        }

        assertSame(expected, actual)
        assertEquals(
            GatewayRuntimeState.Failed("stop", null, "jni-stop-failed"),
            RuntimeController.snapshot().gateway,
        )
    }

    @Test
    fun schedulerSetPublishesNativeEffectiveValueOnLifecycleWorker() {
        val callerThread = Thread.currentThread()
        val nativeThread = AtomicReference<Thread?>()
        // JUnit4 method order is unspecified and the singleton snapshot carries
        // over between tests. Seed schedulerEnabled=true first so the observed
        // transition is a real state change; otherwise reduce() returns the same
        // instance unchanged and updatedAtMillis would not advance.
        RuntimeController.runSerialized {
            RuntimeController.recordStateEvent(
                RuntimeStateEvent.SchedulerObserved(true),
                nowMillis = 5550L,
            )
        }
        val result = RuntimeController.schedulerSetWith(
            set = {
                nativeThread.set(Thread.currentThread())
                org.json.JSONObject()
                    .put("ok", true)
                    .put("scheduler", false)
            },
            nowMillis = { 5600L },
        )

        assertTrue(result.optBoolean("ok"))
        assertTrue(nativeThread.get() !== callerThread)
        assertEquals(false, RuntimeController.snapshot().schedulerEnabled)
        assertEquals(5600L, RuntimeController.snapshot().updatedAtMillis)
    }

    @Test
    fun failedSchedulerSetPreservesLastKnownEffectiveState() {
        RuntimeController.runSerialized {
            RuntimeController.recordStateEvent(
                RuntimeStateEvent.SchedulerObserved(true),
                nowMillis = 5650L,
            )
        }
        val before = RuntimeController.snapshot()
        val result = RuntimeController.schedulerSetWith(
            set = {
                org.json.JSONObject()
                    .put("ok", false)
                    .put("error", "core_not_ready")
            },
            nowMillis = { 5700L },
        )

        assertFalse(result.optBoolean("ok"))
        assertSame(before, RuntimeController.snapshot())
        assertEquals(true, RuntimeController.snapshot().schedulerEnabled)
    }

    @Test
    fun schedulerSetExceptionPropagatesWithoutChangingSnapshot() {
        val before = RuntimeController.snapshot()
        val expected = IllegalStateException("jni-scheduler-failed")
        val actual = try {
            RuntimeController.schedulerSetWith(
                set = { throw expected },
                nowMillis = { 5800L },
            )
            null
        } catch (failure: Throwable) {
            failure
        }

        assertSame(expected, actual)
        assertSame(before, RuntimeController.snapshot())
    }

    @Test
    fun schedulerSetRejectsSuccessfulReplyWithoutEffectiveValue() {
        val before = RuntimeController.snapshot()
        val actual = try {
            RuntimeController.schedulerSetWith(
                set = { org.json.JSONObject().put("ok", true) },
                nowMillis = { 5900L },
            )
            null
        } catch (failure: IllegalStateException) {
            failure
        }

        assertTrue(actual?.message?.contains("scheduler") == true)
        assertSame(before, RuntimeController.snapshot())
    }

    private fun prepareRunningGatewaySnapshot() {
        RuntimeController.runSerialized {
            RuntimeController.recordStateEvent(
                RuntimeStateEvent.NativeStatusObserved(
                    coreReady = true,
                    gatewayRunning = true,
                    port = 8787,
                    bindLan = true,
                    version = "stop-test",
                    schedulerEnabled = true,
                ),
                nowMillis = 5050L,
            )
        }
    }

    private fun prepareStoppedGatewaySnapshot() {
        RuntimeController.runSerialized {
            val current = RuntimeController.snapshot()
            RuntimeController.recordStateEvent(
                RuntimeStateEvent.NativeStatusObserved(
                    coreReady = current.core is CoreRuntimeState.Ready,
                    gatewayRunning = false,
                    port = 0,
                    bindLan = false,
                    version = "controller-test",
                    schedulerEnabled = current.schedulerEnabled ?: false,
                ),
                nowMillis = 4400L,
            )
        }
    }

    @Test
    fun refreshStatusWaitsBehindPreviouslyQueuedLifecycleWork() {
        val blockerEntered = CountDownLatch(1)
        val releaseBlocker = CountDownLatch(1)
        RuntimeController.submitSerialized(
            operation = {
                blockerEntered.countDown()
                releaseBlocker.await(2, TimeUnit.SECONDS)
            },
            onComplete = {},
        )
        assertTrue("blocking operation did not start", blockerEntered.await(2, TimeUnit.SECONDS))

        val before = RuntimeController.snapshot()
        val expectedStatus = Gateway.Status(
            running = before.gateway !is GatewayRuntimeState.Running,
            port = 8787,
            bindLan = false,
            uptimeSec = 0,
            version = "refresh-test",
            core = before.core !is CoreRuntimeState.Ready,
            scheduler = before.schedulerEnabled != true,
        )
        val statusRead = CountDownLatch(1)
        val result = AtomicReference<Gateway.Status?>()
        val caller = Thread {
            result.set(
                RuntimeController.refreshStatusWith(
                    readStatus = {
                        statusRead.countDown()
                        expectedStatus
                    },
                    nowMillis = { 4000L },
                ),
            )
        }
        caller.start()

        assertFalse("status must remain queued behind prior lifecycle work",
            statusRead.await(100, TimeUnit.MILLISECONDS))
        releaseBlocker.countDown()
        assertTrue("queued status reconciliation did not execute",
            statusRead.await(2, TimeUnit.SECONDS))
        caller.join(2000)

        assertEquals("refresh-test", result.get()?.version)
        val snapshot = RuntimeController.snapshot()
        assertEquals(
            if (expectedStatus.core) CoreRuntimeState.Ready else CoreRuntimeState.NotInitialized,
            snapshot.core,
        )
        assertEquals(
            if (expectedStatus.running) {
                GatewayRuntimeState.Running(8787, false, "refresh-test")
            } else {
                GatewayRuntimeState.Stopped
            },
            snapshot.gateway,
        )
        assertEquals(expectedStatus.scheduler, snapshot.schedulerEnabled)
        assertEquals(4000L, snapshot.updatedAtMillis)
    }

}
