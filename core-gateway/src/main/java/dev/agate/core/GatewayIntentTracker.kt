package dev.agate.core

import java.util.concurrent.atomic.AtomicReference

/**
 * Process-wide intent snapshot for guarding delayed Gateway stops.
 *
 * Each accepted START/STOP advances a monotonic generation and records whether
 * the latest delivered intent wants the Gateway to run. Both the STOP command
 * body and the Service destruction cleanup consult [mayStopNow] at their
 * execution time: a START delivered while a stop was queued flips the last
 * intent to "run", and the stale stop is skipped instead of killing the fresh
 * Gateway. Snapshot equality guards (capture-then-compare) were tried before
 * and are structurally weaker — the destruction path itself captures the new
 * START's snapshot, so equality can hold while the stop is still wrong.
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

    /**
     * Whether a stop (STOP command body or destruction cleanup) may actually
     * stop the Gateway at its execution time. Must be read inside the
     * RuntimeController serialized region, so no lifecycle change can slip
     * between this check and the Gateway.stop() JNI call.
     */
    fun mayStopNow(): Boolean = !current.get().shouldRun

    private fun advance(shouldRun: Boolean): GatewayIntentSnapshot {
        while (true) {
            val previous = current.get()
            val next = GatewayIntentSnapshot(previous.generation + 1L, shouldRun)
            if (current.compareAndSet(previous, next)) return next
        }
    }
}
