package dev.agate.core

/**
 * Immutable Kotlin-side view of the two-level native runtime.
 *
 * This is a pure state model: it does not call JNI, own Android objects, or publish to UI
 * observers. RuntimeController owns the process-local snapshot and applies events on its
 * serialized lifecycle worker. Native Gateway.status() remains the source of observed facts.
 */
data class RuntimeSnapshot(
    val core: CoreRuntimeState = CoreRuntimeState.NotInitialized,
    val gateway: GatewayRuntimeState = GatewayRuntimeState.Stopped,
    /** null means the native scheduler state has not been observed yet. */
    val schedulerEnabled: Boolean? = null,
    /** Monotonically increases when the represented state changes. */
    val revision: Long = 0,
    val updatedAtMillis: Long = 0,
)

sealed interface CoreRuntimeState {
    data object NotInitialized : CoreRuntimeState
    data object Initializing : CoreRuntimeState
    data object Ready : CoreRuntimeState
    data class Failed(
        val stage: String?,
        val code: String?,
        val message: String,
    ) : CoreRuntimeState
}

sealed interface GatewayRuntimeState {
    data object Stopped : GatewayRuntimeState
    data object Starting : GatewayRuntimeState
    data class Running(
        val port: Int,
        val bindLan: Boolean,
        val version: String,
    ) : GatewayRuntimeState
    data object Stopping : GatewayRuntimeState
    /**
     * 停止已发出但端口释放未确认（Rust stop_gateway released=false：shutdown
     * 已发、5s 释放探测超时）。与 Stopped 的区分是 R47/T3.4 的三态收口：
     * 此态下 UI/通知不得假装「已停止」，权威 NativeStatusObserved 探测会把
     * 它归位；新的 START 意图允许从本态直接迁移（快速 STOP→START 校准链）。
     */
    data object StopTimeout : GatewayRuntimeState
    data class Failed(
        val stage: String?,
        val code: String?,
        val message: String,
        /** bind 失败分类（Rust ConflictKind；occupied/reserved/other）。旧事件无此字段时为 null。 */
        val kind: String? = null,
    ) : GatewayRuntimeState
}

/** Events are outcomes/observations, not commands and not JNI wrappers. */
sealed interface RuntimeStateEvent {
    data object CoreInitializationStarted : RuntimeStateEvent
    data object CoreInitializationSucceeded : RuntimeStateEvent
    data class CoreInitializationFailed(
        val stage: String?,
        val code: String?,
        val message: String,
    ) : RuntimeStateEvent

    data object GatewayStartStarted : RuntimeStateEvent
    data class GatewayStartSucceeded(
        val port: Int,
        val bindLan: Boolean,
        val version: String,
    ) : RuntimeStateEvent
    /** START 时旧 listener 仍在释放：这是过渡态，不是启动失败。 */
    data object GatewayStoppingObserved : RuntimeStateEvent
    data object GatewayStopStarted : RuntimeStateEvent
    data object GatewayStopSucceeded : RuntimeStateEvent
    /** released=false 的停止结果：停止已发出，端口释放未在 5s 内确认（R47/T3.4）。 */
    data object GatewayStopTimedOut : RuntimeStateEvent
    data class GatewayFailed(
        val stage: String?,
        val code: String?,
        val message: String,
        /** bind 失败分类；仅 startWith 从 StartResult 带入，其他路径默认 null。 */
        val kind: String? = null,
    ) : RuntimeStateEvent

    data class SchedulerObserved(val enabled: Boolean) : RuntimeStateEvent

    /**
     * Authoritative native snapshot, used only at a serialized reconciliation boundary
     * (for example after a lifecycle JNI call or during explicit process recovery).
     * Do not feed arbitrary polling results while a lifecycle command is in flight.
     */
    data class NativeStatusObserved(
        val coreReady: Boolean,
        val gatewayRunning: Boolean,
        val port: Int,
        val bindLan: Boolean,
        val version: String,
        val schedulerEnabled: Boolean,
        /** 停止已发出、listener 尚未确认释放（R50/T1）；与 gatewayRunning 互斥。 */
        val stopping: Boolean = false,
    ) : RuntimeStateEvent
}

/**
 * Deterministic reducer for JVM tests and future RuntimeController integration.
 * Invalid/out-of-order completion events are ignored; command serialization and stale
 * asynchronous result protection remain the responsibility of RuntimeController.
 */
object RuntimeSnapshotReducer {
    fun reduce(
        current: RuntimeSnapshot,
        event: RuntimeStateEvent,
        nowMillis: Long,
    ): RuntimeSnapshot {
        val next = when (event) {
            RuntimeStateEvent.CoreInitializationStarted -> {
                if (current.core is CoreRuntimeState.NotInitialized || current.core is CoreRuntimeState.Failed) {
                    current.copy(core = CoreRuntimeState.Initializing)
                } else current
            }
            RuntimeStateEvent.CoreInitializationSucceeded -> {
                if (current.core is CoreRuntimeState.Initializing) {
                    current.copy(core = CoreRuntimeState.Ready)
                } else current
            }
            is RuntimeStateEvent.CoreInitializationFailed -> {
                // NativeStatusObserved may have reconciled a failed bootstrap to
                // NotInitialized immediately before this command's failure detail.
                if (current.core is CoreRuntimeState.Initializing ||
                    current.core is CoreRuntimeState.NotInitialized
                ) {
                    current.copy(core = CoreRuntimeState.Failed(event.stage, event.code, event.message))
                } else current
            }
            RuntimeStateEvent.GatewayStartStarted -> {
                // A prior STOP may not yet have been migrated to RuntimeController.
                // A fresh START must therefore be able to replace a stale Running
                // snapshot with Starting before its Native result is applied.
                // StopTimeout 同理（R47/T3.4）：停止已发出但释放未确认，新的
                // START 是权威命令，必须能取代它 —— 否则快速 STOP→START 会
                // 卡死在未确认态，比状态假象更糟。
                if (current.gateway is GatewayRuntimeState.Stopped ||
                    current.gateway is GatewayRuntimeState.Running ||
                    current.gateway is GatewayRuntimeState.Failed ||
                    current.gateway is GatewayRuntimeState.StopTimeout
                ) {
                    current.copy(gateway = GatewayRuntimeState.Starting)
                } else current
            }
            is RuntimeStateEvent.GatewayStartSucceeded -> {
                if (current.gateway is GatewayRuntimeState.Starting) {
                    current.copy(
                        gateway = GatewayRuntimeState.Running(event.port, event.bindLan, event.version),
                    )
                } else current
            }
            RuntimeStateEvent.GatewayStoppingObserved -> {
                if (current.gateway is GatewayRuntimeState.Starting) {
                    current.copy(gateway = GatewayRuntimeState.Stopping)
                } else current
            }
            RuntimeStateEvent.GatewayStopStarted -> {
                if (current.gateway is GatewayRuntimeState.Running || current.gateway is GatewayRuntimeState.Failed) {
                    current.copy(gateway = GatewayRuntimeState.Stopping)
                } else current
            }
            RuntimeStateEvent.GatewayStopSucceeded -> {
                if (current.gateway is GatewayRuntimeState.Stopping) {
                    current.copy(gateway = GatewayRuntimeState.Stopped)
                } else current
            }
            RuntimeStateEvent.GatewayStopTimedOut -> {
                if (current.gateway is GatewayRuntimeState.Stopping) {
                    current.copy(gateway = GatewayRuntimeState.StopTimeout)
                } else current
            }
            is RuntimeStateEvent.GatewayFailed -> {
                if (current.gateway is GatewayRuntimeState.Starting || current.gateway is GatewayRuntimeState.Stopping) {
                    current.copy(gateway = GatewayRuntimeState.Failed(event.stage, event.code, event.message, event.kind))
                } else current
            }
            is RuntimeStateEvent.SchedulerObserved -> current.copy(schedulerEnabled = event.enabled)
            is RuntimeStateEvent.NativeStatusObserved -> current.copy(
                core = if (event.coreReady) CoreRuntimeState.Ready else CoreRuntimeState.NotInitialized,
                gateway = when {
                    event.gatewayRunning ->
                        GatewayRuntimeState.Running(event.port, event.bindLan, event.version)
                    // R50/T1：stopping 是独立观测，不能折算成 Stopped —— 否则
                    // 停止复核会把「还在收尾」当成「已停」，提前释放保活锁。
                    event.stopping -> GatewayRuntimeState.Stopping
                    else -> GatewayRuntimeState.Stopped
                },
                schedulerEnabled = event.schedulerEnabled,
            )
        }

        if (next == current) return current
        return next.copy(
            revision = current.revision + 1,
            updatedAtMillis = nowMillis,
        )
    }
}
