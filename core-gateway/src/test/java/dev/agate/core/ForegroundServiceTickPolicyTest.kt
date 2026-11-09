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

    /** A probe failure must be reported and fail closed, not stop the service. */
    @Test
    fun probeFailureFailsClosedAndIsReported() {
        var reported: Throwable? = null
        val stop = stopServiceAfterTick(
            probe = { throw IllegalStateException("JNI unavailable") },
            onProbeFailure = { reported = it },
        )
        assertFalse(stop)
        assertTrue(reported is IllegalStateException)
    }

    /** Normal probe results still flow through the policy unchanged. */
    @Test
    fun probeResultFlowsThroughPolicy() {
        assertTrue(stopServiceAfterTick(probe = { false }))
        assertFalse(stopServiceAfterTick(probe = { true }))
    }
}
