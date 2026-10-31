package dev.agate.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertNotNull
import org.junit.Test

class ServiceCommandCoalescerTest {
    @Test
    fun coalescesOnlyAdjacentDuplicateActions() {
        val queue = ServiceCommandCoalescer()
        val firstStart = queue.tryAcquire("START")
        assertNotNull(firstStart)
        assertNull(queue.tryAcquire("START"))

        val stop = queue.tryAcquire("STOP")
        assertNotNull(stop)
        val startAfterStop = queue.tryAcquire("START")
        assertNotNull("START after STOP must not be swallowed by old START", startAfterStop)
        assertNull(queue.tryAcquire("START"))

        // Older completions must not clear the latest queued action's coalescing marker.
        queue.complete(firstStart!!)
        queue.complete(stop!!)
        assertNull(queue.tryAcquire("START"))
        queue.complete(startAfterStop!!)
        assertEquals("START can be accepted after the latest START completes", true, queue.tryAcquire("START") != null)
    }


    @Test
    fun startIdScopedStopAndTickCommandsRemainDistinct() {
        val queue = ServiceCommandCoalescer()
        val firstStop = queue.tryAcquire("STOP#10")
        val secondStop = queue.tryAcquire("STOP#11")
        val firstTick = queue.tryAcquire("TICK#12")
        val secondTick = queue.tryAcquire("TICK#13")

        assertNotNull(firstStop)
        assertNotNull(secondStop)
        assertNotNull(firstTick)
        assertNotNull(secondTick)
    }

    @Test
    fun differentActionsFormOrderingBarriers() {
        val queue = ServiceCommandCoalescer()
        val tick = queue.tryAcquire("TICK")
        val stop = queue.tryAcquire("STOP")
        val tickAfterStop = queue.tryAcquire("TICK")
        assertNotNull(tick)
        assertNotNull(stop)
        assertNotNull("TICK after STOP must be preserved", tickAfterStop)
    }
}
