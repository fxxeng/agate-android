package dev.agate.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class GatewayIntentTrackerTest {
    @Test
    fun initialIdleStateCountsAsStoppedIntent() {
        val tracker = GatewayIntentTracker()

        // 从未 START：销毁兜底清理应放行（保持旧实例「系统杀服务时兜底停内核」语义）。
        assertTrue(tracker.mayStopNow())
        assertEquals(0L, tracker.snapshot().generation)
    }

    @Test
    fun stopIsAllowedOnlyWhileTheLastIntentIsAStop() {
        val tracker = GatewayIntentTracker()
        val stopped = tracker.requestStop()

        assertTrue(tracker.mayStopNow())
        assertEquals(false, stopped.shouldRun)

        // R11 实测场景：销毁/清理流程期间到达的新 START＝最后意图是启动，
        // 迟到的 stop 必须跳过，不得杀死新 START 拉起的网关。
        tracker.requestStart()
        assertFalse("a pending START must block the delayed stop", tracker.mayStopNow())

        tracker.requestStop()
        assertTrue(tracker.mayStopNow())
    }

    @Test
    fun everyAcceptedIntentAdvancesGenerationEvenWhenStateRepeats() {
        val tracker = GatewayIntentTracker()
        val firstStart = tracker.requestStart()
        val secondStart = tracker.requestStart()

        assertTrue(secondStart.generation > firstStart.generation)
        assertEquals(true, secondStart.shouldRun)
        assertTrue(tracker.snapshot().generation >= secondStart.generation)
    }
}
