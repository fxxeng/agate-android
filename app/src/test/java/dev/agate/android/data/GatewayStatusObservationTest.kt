package dev.agate.android.data

import dev.agate.core.Gateway
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

class GatewayStatusObservationTest {
    @Test
    fun successfulNativeStoppedStatusRemainsAnAvailableObservation() {
        val stopped = Gateway.Status(
            running = false,
            port = 8787,
            bindLan = false,
            uptimeSec = 0,
            version = "probe-test",
            core = true,
            scheduler = true,
        )

        val result = observeGatewayStatus { stopped }

        assertTrue(result is GatewayStatusObservation.Available)
        assertSame(stopped, (result as GatewayStatusObservation.Available).status)
    }

    @Test
    fun failedNativeStatusReadIsUnavailableNotAStoppedObservation() {
        val failure = IllegalStateException("state poisoned")

        val result = observeGatewayStatus { throw failure }

        assertTrue(result is GatewayStatusObservation.Unavailable)
        val unavailable = result as GatewayStatusObservation.Unavailable
        assertSame(failure, unavailable.cause)
        // The type has no synthetic `running=false` value: callers must retain last-known state.
        assertEquals("state poisoned", unavailable.cause.message)
    }
}
