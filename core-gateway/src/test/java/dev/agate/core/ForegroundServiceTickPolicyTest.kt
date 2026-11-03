package dev.agate.core

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ForegroundServiceTickPolicyTest {
    @Test
    fun onlyAnExplicitStoppedObservationAllowsServiceShutdown() {
        assertTrue(shouldStopServiceAfterTick(false))
        assertFalse(shouldStopServiceAfterTick(true))
        assertFalse(shouldStopServiceAfterTick(null))
    }
}
