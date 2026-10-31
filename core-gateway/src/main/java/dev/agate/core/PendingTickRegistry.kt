package dev.agate.core

import java.util.concurrent.ConcurrentHashMap

/** Tracks queued/running alarm ticks so STOP can target only ticks already accepted. */
internal class PendingTickRegistry {
    private val pending = ConcurrentHashMap.newKeySet<Int>()

    fun register(startId: Int) {
        pending.add(startId)
    }

    fun complete(startId: Int) {
        pending.remove(startId)
    }

    fun snapshot(): Set<Int> = pending.toSet()

    fun hasAny(snapshot: Set<Int>): Boolean = snapshot.any(pending::contains)
}
