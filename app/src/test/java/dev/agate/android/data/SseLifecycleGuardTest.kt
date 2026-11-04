package dev.agate.android.data

import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SseLifecycleGuardTest {
    @Test
    fun replacingConnectionRejectsLateCallbacksFromPreviousEventSource() {
        val guard = SseLifecycleGuard()
        val firstSource = Any()
        val secondSource = Any()

        val firstGeneration = guard.withLock {
            val started = startLocked()
            beginConnectionLocked(started)!!
        }
        guard.withLock {
            assertTrue(installConnectionLocked(firstGeneration, firstSource))
            assertTrue(isCurrentLocked(firstGeneration, firstSource))
        }

        val secondGeneration = guard.withLock {
            val next = invalidateConnectionLocked(firstGeneration, firstSource)!!
            beginConnectionLocked(next)!!
        }
        guard.withLock {
            assertTrue(installConnectionLocked(secondGeneration, secondSource))
            assertFalse(isCurrentLocked(firstGeneration, firstSource))
            assertFalse(isCurrentLocked(firstGeneration, secondSource))
            assertTrue(isCurrentLocked(secondGeneration, secondSource))
            assertNull(invalidateConnectionLocked(firstGeneration, firstSource))
        }
    }

    @Test
    fun stopInvalidatesPendingRetryAndPollGeneration() {
        val guard = SseLifecycleGuard()
        val pendingGeneration = guard.withLock {
            val started = startLocked()
            beginConnectionLocked(started)!!
        }

        guard.withLock {
            assertTrue(isGenerationActiveLocked(pendingGeneration))
            stopLocked()
            assertFalse(isStartedLocked())
            assertFalse(isGenerationActiveLocked(pendingGeneration))
            assertNull(beginConnectionLocked(pendingGeneration))
        }
    }

    @Test
    fun duplicateFailureCallbacksCanInvalidateOnlyOnce() {
        val guard = SseLifecycleGuard()
        val source = Any()
        val generation = guard.withLock {
            val started = startLocked()
            beginConnectionLocked(started)!!
        }
        guard.withLock {
            assertTrue(installConnectionLocked(generation, source))
            val retryGeneration = invalidateConnectionLocked(generation, source)
            assertNotNull(retryGeneration)
            assertNull(invalidateConnectionLocked(generation, source))
            assertFalse(isCurrentLocked(generation, source))
            assertTrue(isGenerationActiveLocked(retryGeneration!!))
        }
    }

    @Test
    fun aNewManualStartInvalidatesOldRetryAndInstallsNewGeneration() {
        val guard = SseLifecycleGuard()
        val oldGeneration = guard.withLock { startLocked() }
        val newGeneration = guard.withLock { startLocked() }

        guard.withLock {
            assertTrue(newGeneration > oldGeneration)
            assertFalse(isGenerationActiveLocked(oldGeneration))
            assertTrue(isGenerationActiveLocked(newGeneration))
            assertTrue(isStartedLocked())
        }
    }

    @Test
    fun sameGenerationRejectsDifferentEventSourceIdentity() {
        val guard = SseLifecycleGuard()
        val currentSource = Any()
        val differentSource = Any()
        val generation = guard.withLock {
            val started = startLocked()
            beginConnectionLocked(started)!!
        }

        guard.withLock {
            assertTrue(installConnectionLocked(generation, currentSource))
            assertTrue(isCurrentLocked(generation, currentSource))

            // Keep the generation identical: only object identity can reject this callback.
            assertFalse(isCurrentLocked(generation, differentSource))
            assertNull(invalidateConnectionLocked(generation, differentSource))
            assertTrue(isCurrentLocked(generation, currentSource))
        }
    }
}
