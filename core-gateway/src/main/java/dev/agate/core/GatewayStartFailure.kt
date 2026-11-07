package dev.agate.core

/**
 * 网关启动失败的用户指引出口（R44 T1：端口冲突用户闭环）。
 *
 * 分层契约：Rust `port_conflict.rs` 按 bind 错误码分好类（[Gateway.StartResult.kind] =
 * occupied / reserved / other），并在 [Gateway.StartResult.error] 里给出**已含处置建议**
 * 的完整文案。本对象是 Kotlin 侧的统一出口：通知栏与界面失败卡片都从这里取文案，
 * 而文案本身原样透传 Rust —— 遵守「界面不自己拼文案」的既定原则，避免两个出口
 * 各说各话。kind 作为结构化锚点供测试与未来 UI 动作（如「跳设置换端口」）使用。
 */
object GatewayStartFailure {
    const val STAGE_BIND = "bind"

    /** 与 Rust `ConflictKind::as_str` 一一对应（kebab-case）。 */
    const val KIND_OCCUPIED = "occupied"
    const val KIND_RESERVED = "reserved"
    const val KIND_OTHER = "other"

    /**
     * 启动失败的一句话指引。
     *
     * @param stage native 回传的失败阶段（"bind" = 端口监听失败）
     * @param kind  bind 失败分类；旧版内核未回传时为 null
     * @param port  请求监听的端口（kind 为 null 的兜底文案里用到）
     * @param error native 给出的完整说明；非空时原样透传
     */
    fun guidance(stage: String?, kind: String?, port: Int, error: String?): String {
        if (!error.isNullOrBlank()) {
            return error
        }
        return when (stage) {
            STAGE_BIND -> "端口 $port 无法监听，请改用其它端口（设置页）。"
            else -> "内核启动失败。"
        }
    }
}
