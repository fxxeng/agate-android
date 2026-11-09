package dev.agate.core

/**
 * A tick may stop its foreground service only after Native explicitly reports that
 * the Gateway is stopped. `null` means the status probe failed and must fail closed.
 */
internal fun shouldStopServiceAfterTick(gatewayRunning: Boolean?): Boolean =
    gatewayRunning == false

/**
 * R47/T6: the Service wiring around [shouldStopServiceAfterTick] used to live
 * inline in `AgateForegroundService.runTick` (`try { readStatus().running }
 * catch { null }`), so the policy was unit-tested but the "probe failure means
 * unknown" contract was not. Keep the wiring here, where JVM tests can reach it:
 * any [Throwable] from [probe] is reported to [onProbeFailure] and downgraded to
 * `null` (fail closed — a failed probe is not proof that the Gateway stopped).
 */
internal inline fun stopServiceAfterTick(
    probe: () -> Boolean,
    onProbeFailure: (Throwable) -> Unit = {},
): Boolean =
    try {
        shouldStopServiceAfterTick(probe())
    } catch (failure: Throwable) {
        onProbeFailure(failure)
        shouldStopServiceAfterTick(null)
    }
