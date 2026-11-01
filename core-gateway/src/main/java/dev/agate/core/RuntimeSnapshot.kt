package dev.agate.core

/**
 * Immutable Kotlin-side view of the two-level native runtime.
 *
 * This is deliberately a pure state model: it does not call JNI, own Android objects,
 * serialize lifecycle commands, or publish to UI observers. RuntimeController integration
 * is a later batch. Native Gateway.status() remains the source of observed runtime facts.
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
    data class Failed(
        val stage: String?,
        val code: String?,
        val message: String,
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
    data object GatewayStopStarted : RuntimeStateEvent
    data object GatewayStopSucceeded : RuntimeStateEvent
    data class GatewayFailed(
        val stage: String?,
        val code: String?,
        val message: String,
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
                if (current.core is CoreRuntimeState.Initializing) {
                    current.copy(core = CoreRuntimeState.Failed(event.stage, event.code, event.message))
                } else current
            }
            RuntimeStateEvent.GatewayStartStarted -> {
                if (current.gateway is GatewayRuntimeState.Stopped || current.gateway is GatewayRuntimeState.Failed) {
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
            is RuntimeStateEvent.GatewayFailed -> {
                if (current.gateway is GatewayRuntimeState.Starting || current.gateway is GatewayRuntimeState.Stopping) {
                    current.copy(gateway = GatewayRuntimeState.Failed(event.stage, event.code, event.message))
                } else current
            }
            is RuntimeStateEvent.SchedulerObserved -> current.copy(schedulerEnabled = event.enabled)
            is RuntimeStateEvent.NativeStatusObserved -> current.copy(
                core = if (event.coreReady) CoreRuntimeState.Ready else CoreRuntimeState.NotInitialized,
                gateway = if (event.gatewayRunning) {
                    GatewayRuntimeState.Running(event.port, event.bindLan, event.version)
                } else {
                    GatewayRuntimeState.Stopped
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
