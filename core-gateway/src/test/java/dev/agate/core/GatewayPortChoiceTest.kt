package dev.agate.core

import org.junit.Assert.assertEquals
import org.junit.Test

/** R49/T4：探测端口裁决的回归测试（默认 / 自定义 / 运行态优先）。 */
class GatewayPortChoiceTest {

    @Test
    fun defaultPortIsUsedWhenGatewayIsNotRunning() {
        assertEquals(8787, GatewayPortChoice.probePort(configuredPort = 8787, runningPort = null))
    }

    @Test
    fun configuredPortIsUsedWhenGatewayIsNotRunning() {
        assertEquals(9123, GatewayPortChoice.probePort(configuredPort = 9123, runningPort = null))
    }

    @Test
    fun runningGatewayPortWinsOverConfiguredPort() {
        // 用户改了端口但还没重启网关：探测必须打到实际在跑的那个。
        assertEquals(8787, GatewayPortChoice.probePort(configuredPort = 9123, runningPort = 8787))
    }

    @Test
    fun configuredPortTakesOverAgainAfterGatewayStops() {
        assertEquals(9123, GatewayPortChoice.probePort(configuredPort = 9123, runningPort = null))
    }

    @Test
    fun defaultPortContractMatchesGatewayPrefsDefault() {
        assertEquals(8787, GatewayPortChoice.DEFAULT_PORT)
    }
}
