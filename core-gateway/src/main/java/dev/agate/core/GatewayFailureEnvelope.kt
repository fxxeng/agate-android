package dev.agate.core

/** Rust 文案由 Kotlin 统一透传；停止过渡不得被归类成普通启动失败。 */
object GatewayFailureEnvelope {
    fun display(result: Gateway.StartResult?): String = when {
        result == null -> "内核启动异常，请检查 Native 内核库是否匹配。"
        result.error == "stopping" -> "上一个网关正在停止，请稍后重试。"
        !result.error.isNullOrBlank() -> result.error
        else -> GatewayStartFailure.guidance(result.stage, result.kind, result.port, result.error)
    }

    fun health(status: Gateway.Status?): String = when {
        status == null -> "unknown"
        status.stopping -> "stopping"
        status.running -> "running"
        else -> "stopped"
    }
}
