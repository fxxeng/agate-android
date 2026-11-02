package dev.agate.core

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.assertEquals
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
}
