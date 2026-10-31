package dev.agate.core

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PendingTickRegistryTest {
    @Test
    fun stopSnapshotDoesNotCaptureTicksQueuedLater() {
        val ticks = PendingTickRegistry()
        ticks.register(10)
        val stopTargets = ticks.snapshot()

        ticks.register(11)
        assertTrue(ticks.hasAny(stopTargets))

        ticks.complete(10)
        assertFalse(ticks.hasAny(stopTargets))
        assertTrue(ticks.snapshot().contains(11))
    }

    @Test
    fun stopSnapshotWaitsForEveryTickThatPrecededStop() {
        val ticks = PendingTickRegistry()
        ticks.register(20)
        ticks.register(21)
        val stopTargets = ticks.snapshot()

        ticks.complete(20)
        assertTrue(ticks.hasAny(stopTargets))
        ticks.complete(21)
        assertFalse(ticks.hasAny(stopTargets))
    }

    @Test
    fun completedTickIsNotKeptInLaterSnapshot() {
        val ticks = PendingTickRegistry()
        ticks.register(30)
        ticks.complete(30)

        assertTrue(ticks.snapshot().isEmpty())
    }

    @Test
    fun completingOneTickDoesNotRemoveAnotherTick() {
        val ticks = PendingTickRegistry()
        ticks.register(40)
        ticks.register(41)

        ticks.complete(40)

        assertFalse(ticks.hasAny(setOf(40)))
        assertTrue(ticks.hasAny(setOf(41)))
    }
}
