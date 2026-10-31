package dev.agate.core

/**
 * 合并队列尾部相同的 Service 命令，但不跨越不同命令合并。
 *
 * 例如 START, START 合并为一个；START, STOP, START 必须保留三个命令，
 * 因为最后一次 START 是用户在 STOP 之后的新意图，不能被早先的 START 在途标记吞掉。
 * token 用于避免较早命令完成时清除较新命令的尾部状态。
 */
internal class ServiceCommandCoalescer {
    private var nextToken = 0L
    private var lastToken = 0L
    private var lastAction: String? = null

    @Synchronized
    fun tryAcquire(action: String): Long? {
        require(action.isNotBlank()) { "action must not be blank" }
        if (lastAction == action) return null
        val token = ++nextToken
        lastToken = token
        lastAction = action
        return token
    }

    @Synchronized
    fun complete(token: Long) {
        if (lastToken == token) {
            lastToken = 0L
            lastAction = null
        }
    }
}
