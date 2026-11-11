package dev.agate.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class GatewayCheckinStatusTest {
    @Test
    fun checkinProgressUsesBridgeJsonFieldNames() {
        val status = Gateway.Status.parse(
            """{"ok":true,"running":false,"core":true,"bridge":4,
                "checkin_revision":17,"checkin_running":true}""",
        )
        assertEquals(17L, status.checkinRevision)
        assertTrue(status.checkinRunning)
    }

    @Test
    fun oldCoreDoesNotInventProgress() {
        val status = Gateway.Status.parse("""{"ok":true,"running":false,"core":true}""")
        assertEquals(0L, status.checkinRevision)
        assertFalse(status.checkinRunning)
        assertEquals(0, status.bridge)
    }
}
