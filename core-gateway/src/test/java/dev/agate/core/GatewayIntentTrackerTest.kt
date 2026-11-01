package dev.agate.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class GatewayIntentTrackerTest {
    @Test
    fun cleanupIsAllowedOnlyForTheExactCurrentStoppedGeneration() {
        val tracker = GatewayIntentTracker()
        val stopped = tracker.requestStop()

        assertTrue(tracker.mayCleanup(stopped))

        tracker.requestStart()
        assertFalse("a later START must invalidate old cleanup", tracker.mayCleanup(stopped))
    }

    @Test
    fun laterStopAlsoInvalidatesAnOlderCleanupSnapshot() {
        val tracker = GatewayIntentTracker()
        val firstStop = tracker.requestStop()

        tracker.requestStart()
        tracker.requestStop()

        assertFalse("a newer STOP is handled by its own command", tracker.mayCleanup(firstStop))
        assertTrue(tracker.mayCleanup(tracker.snapshot()))
    }

    @Test
    fun everyAcceptedIntentAdvancesGenerationEvenWhenStateRepeats() {
        val tracker = GatewayIntentTracker()
        val firstStart = tracker.requestStart()
        val secondStart = tracker.requestStart()

        assertTrue(secondStart.generation > firstStart.generation)
        assertEquals(true, secondStart.shouldRun)
        assertFalse(tracker.mayCleanup(firstStart))
    }
}
