package dev.agate.core

import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong

/**
 * 有界日志投递队列（R50/T7）。
 *
 * Native 日志回调发生在 JNI 线程；Kotlin 侧固定用单个 AgateLog HandlerThread
 * 消费。消费速度慢于生产（大量请求日志）时，无界 `Handler.post` 会把消息堆在
 * 主内存里。这里在入队前计数，超过容量直接丢弃并记账 —— 内存有上限，且丢弃
 * 本身不阻塞调用方（日志不该反压业务线程）。
 *
 * @param capacity 允许排队的最大条数（正在执行的那条也计入）
 * @param post 实际投递函数；返回 false 表示投递失败（如 Looper 已退出）
 */
internal class BoundedLogQueue(
    private val capacity: Int,
    private val post: (Runnable) -> Boolean,
) {
    private val depth = AtomicInteger(0)
    private val dropped = AtomicLong(0)

    fun submit(task: () -> Unit): Boolean {
        if (depth.incrementAndGet() > capacity) {
            depth.decrementAndGet()
            dropped.incrementAndGet()
            return false
        }
        val accepted = post {
            try {
                task()
            } finally {
                depth.decrementAndGet()
            }
        }
        if (!accepted) depth.decrementAndGet()
        return accepted
    }

    /** 累计因容量上限丢弃的条数（不含 post 失败）。 */
    fun droppedCount(): Long = dropped.get()
}
