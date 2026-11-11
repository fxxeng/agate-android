package dev.agate.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * R44 T1：网关启动失败指引出口的契约测试。
 *
 * 文案由 Rust `port_conflict.rs` 单一出口生成，Kotlin 只透传（「界面不拼文案」原则）；
 * 这里的断言用与 Rust 测试相同的关键词锚定同一口径，并锁定 kind 常量与
 * Rust `ConflictKind::as_str` 的契约不被改坏。
 */
class GatewayStartFailureTest {
    @Test
    fun nonBlankErrorIsPassedThroughVerbatim() {
        val nativeCopy = "端口 8787 已被其他应用占用。可结束占用该端口的程序，或改用其它端口（设置页）。"
        assertEquals(
            nativeCopy,
            GatewayStartFailure.guidance(
                stage = GatewayStartFailure.STAGE_BIND,
                kind = GatewayStartFailure.KIND_OCCUPIED,
                port = 8787,
                error = nativeCopy,
            ),
        )
    }

    @Test
    fun blankErrorFallsBackByStage() {
        assertEquals(
            "端口 8787 无法监听，请改用其它端口（设置页）。",
            GatewayStartFailure.guidance("bind", null, 8787, ""),
        )
        assertEquals(
            "内核启动失败。",
            GatewayStartFailure.guidance("config", null, 0, null),
        )
    }

    @Test
    fun stoppingIsTransitionGuidanceNotEnglishFailure() {
        val guidance = GatewayStartFailure.guidance(null, null, 8787, "stopping")
        assertEquals("上一个网关正在停止，请稍后重试。", guidance)
    }

    @Test
    fun kindConstantsMatchRustConflictKindAsStr() {
        assertEquals("occupied", GatewayStartFailure.KIND_OCCUPIED)
        assertEquals("reserved", GatewayStartFailure.KIND_RESERVED)
        assertEquals("other", GatewayStartFailure.KIND_OTHER)
        assertEquals("bind", GatewayStartFailure.STAGE_BIND)
    }

    @Test
    fun reservedSampleCopyNeverMentionsWindowsReservation() {
        // Rust 端同款断言的关键词镜像：reserved 文案不允许再出现桌面版保留段描述
        val reservedCopy =
            "端口 80 被系统拒绝（保留段或权限不足，例如 1024 以下特权端口），结束进程也无法解决。请改用其它端口（设置页）。"
        assertTrue(!reservedCopy.contains("Hyper-V") && !reservedCopy.contains("winnat"))
        assertEquals(reservedCopy, GatewayStartFailure.guidance("bind", GatewayStartFailure.KIND_RESERVED, 80, reservedCopy))
    }
}
