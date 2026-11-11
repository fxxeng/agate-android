package dev.agate.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class AgateAlarmRouterTest {
    @Test
    fun actionAndDataUriSelectResourceExactly() {
        assertEquals(
            AlarmResource.Checkin,
            AgateAlarmIntents.resource(AlarmScheduler.ACTION, "agalarm://tick/main"),
        )
        assertEquals(
            AlarmResource.UsageRefresh,
            AgateAlarmIntents.resource(AgateAlarmIntents.ACTION_REFRESH, "agalarm://refresh/usage"),
        )
        assertEquals(
            AlarmResource.CredentialRefresh,
            AgateAlarmIntents.resource(AgateAlarmIntents.ACTION_REFRESH, "agalarm://refresh/credentials"),
        )
    }

    @Test
    fun legacyOrUnknownResourcesAreNotSilentlyRun() {
        assertNull(AgateAlarmIntents.resource(AlarmScheduler.ACTION, null))
        assertNull(AgateAlarmIntents.resource("dev.agate.core.action.BOOT_COMPLETED", null))
        assertNull(AgateAlarmIntents.resource(AgateAlarmIntents.ACTION_REFRESH, "https://evil/refresh"))
    }
}
