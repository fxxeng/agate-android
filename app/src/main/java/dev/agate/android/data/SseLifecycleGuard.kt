package dev.agate.android.data

/**
 * Serializes the repository's SSE lifecycle state and rejects callbacks/jobs from stale
 * connection generations. Call the `*Locked` methods only inside [withLock].
 */
internal class SseLifecycleGuard {
    private var started = false
    private var generation = 0L
    private var currentConnection: Any? = null

    fun <T> withLock(block: SseLifecycleGuard.() -> T): T = synchronized(this) {
        block()
    }

    fun startLocked(): Long {
        check(Thread.holdsLock(this))
        started = true
        currentConnection = null
        generation += 1
        return generation
    }

    fun stopLocked(): Long {
        check(Thread.holdsLock(this))
        started = false
        currentConnection = null
        generation += 1
        return generation
    }

    fun isStartedLocked(): Boolean {
        check(Thread.holdsLock(this))
        return started
    }

    /** Starts a new connection only if the caller still owns the active lifecycle generation. */
    fun beginConnectionLocked(expectedGeneration: Long): Long? {
        check(Thread.holdsLock(this))
        if (!started || generation != expectedGeneration) return null
        currentConnection = null
        generation += 1
        return generation
    }

    fun installConnectionLocked(connectionGeneration: Long, connection: Any): Boolean {
        check(Thread.holdsLock(this))
        if (!started || generation != connectionGeneration) return false
        currentConnection = connection
        return true
    }

    fun isCurrentLocked(connectionGeneration: Long, connection: Any): Boolean {
        check(Thread.holdsLock(this))
        return started && generation == connectionGeneration && currentConnection === connection
    }

    /**
     * Invalidates exactly one current connection and returns the generation token for its
     * retry/poll successor. Duplicate or stale listener callbacks return null.
     */
    fun invalidateConnectionLocked(connectionGeneration: Long, connection: Any): Long? {
        check(Thread.holdsLock(this))
        if (!isCurrentLocked(connectionGeneration, connection)) return null
        currentConnection = null
        generation += 1
        return generation
    }

    /** Retry/poll work is valid only while its generation is current and no SSE is installed. */
    fun isGenerationActiveLocked(expectedGeneration: Long): Boolean {
        check(Thread.holdsLock(this))
        return started && generation == expectedGeneration && currentConnection == null
    }
}
