package dev.agate.core

/**
 * A tick may stop its foreground service only after Native explicitly reports that
 * the Gateway is stopped. `null` means the status probe failed and must fail closed.
 */
internal fun shouldStopServiceAfterTick(gatewayRunning: Boolean?): Boolean =
    gatewayRunning == false
