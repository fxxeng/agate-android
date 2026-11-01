package dev.agate.core

import java.util.concurrent.atomic.AtomicReference

/**
 * Process-wide intent snapshot for guarding delayed Service destruction cleanup.
 *
 * Each accepted START/STOP advances a monotonic generation. A cleanup may stop the
 * Gateway only when the exact snapshot captured by that Service instance is still
 * current and represents a stopped intent.
 */
internal data class GatewayIntentSnapshot(
    val generation: Long,
    val shouldRun: Boolean,
)

internal class GatewayIntentTracker {
    private val current = AtomicReference(GatewayIntentSnapshot(0L, false))

    fun requestStart(): GatewayIntentSnapshot = advance(shouldRun = true)

    fun requestStop(): GatewayIntentSnapshot = advance(shouldRun = false)

    fun snapshot(): GatewayIntentSnapshot = current.get()

    fun mayCleanup(captured: GatewayIntentSnapshot): Boolean {
        val latest = current.get()
        return latest == captured && !latest.shouldRun
    }

    private fun advance(shouldRun: Boolean): GatewayIntentSnapshot {
        while (true) {
            val previous = current.get()
            val next = GatewayIntentSnapshot(previous.generation + 1L, shouldRun)
            if (current.compareAndSet(previous, next)) return next
        }
    }
}
