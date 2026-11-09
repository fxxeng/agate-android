package dev.agate.core

/**
 * 探测端口的唯一裁决（R49/T4）。
 *
 * 网关控制页、健康自检这类「往本机网关发 HTTP」的地方，端口只能有两个来源：
 *
 *  - **运行中的实际监听端口**（`RuntimeController.snapshot()` 的
 *    `GatewayRuntimeState.Running.port`，来自 Native status）；
 *  - **配置端口**（[GatewayPrefs.port]，设置页可改，默认 8787）。
 *
 * 运行中优先 —— 用户改端口后尚未重启网关时，配置值与实际监听值不同，探测
 * 打到配置端口只会得到「连接被拒绝」。没有运行态时才退回配置端口。
 *
 * 纯函数、无 Android 依赖：页面只负责把两个值读出来，判定与回归测试都在这里。
 */
object GatewayPortChoice {

    /** 默认端口（GatewayPrefs 的缺省值；此处只作为契约文档，不复制到调用方）。 */
    const val DEFAULT_PORT: Int = 8787

    fun probePort(configuredPort: Int, runningPort: Int?): Int = runningPort ?: configuredPort
}
