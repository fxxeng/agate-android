package dev.agate.android.data

import dev.agate.core.Gateway

/** A Native status probe has three meanings: running, stopped, or unavailable. */
internal sealed interface GatewayStatusObservation {
    data class Available(val status: Gateway.Status) : GatewayStatusObservation
    data class Unavailable(val cause: Throwable) : GatewayStatusObservation
}

/** Keep transport/query failure distinct from a valid Native `running=false` result. */
internal fun observeGatewayStatus(
    readStatus: () -> Gateway.Status,
): GatewayStatusObservation = try {
    GatewayStatusObservation.Available(readStatus())
} catch (failure: Throwable) {
    GatewayStatusObservation.Unavailable(failure)
}
