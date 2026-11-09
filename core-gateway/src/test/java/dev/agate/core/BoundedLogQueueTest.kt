package dev.agate.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * R50/T7：日志投递必须有界，消费慢时丢弃并记账，不能无界堆内存。
 */
class BoundedLogQueueTest {

    @Test
    fun overflowIsDroppedAndCounted() {
        val pending = mutableListOf<Runnable>()
        val queue = BoundedLogQueue(capacity = 1) { runnable ->
            pending += runnable
            true
        }

        assertTrue(queue.submit { })
        assertFalse("容量用尽后必须丢弃", queue.submit { })
        assertEquals(1L, queue.droppedCount())

        // 消费一条后深度释放，下一次重新可投递。
        pending.removeAt(0).run()
        assertTrue(queue.submit { })
    }

    @Test
    fun taskRunsAndReleasesDepth() {
        val pending = mutableListOf<Runnable>()
        val queue = BoundedLogQueue(capacity = 1) { runnable ->
            pending += runnable
            true
        }
        var ran = false
        assertTrue(queue.submit { ran = true })
        pending.single().run()
        assertTrue(ran)
        assertTrue("执行完应释放深度", queue.submit { })
    }

    @Test
    fun rejectedPostDoesNotLeakDepthOrCountAsDrop() {
        val queue = BoundedLogQueue(capacity = 1) { false }
        assertFalse(queue.submit { })
        assertEquals(0L, queue.droppedCount())
        // post 失败已回滚深度，仍可再次尝试（例如 Handler 尚未就绪）。
        assertFalse(queue.submit { })
        assertEquals(0L, queue.droppedCount())
    }
}
