package dev.agate.core

import java.util.concurrent.Callable
import java.util.concurrent.ExecutionException
import java.util.concurrent.Executors
import java.util.concurrent.ThreadFactory
import java.util.concurrent.atomic.AtomicReference

/**
 * Android 进程内 Native Core 生命周期操作的唯一串行入口。
 *
 * 仅串行化会改变 Core / Gateway 生命周期状态的控制面操作：coreInit、start、
 * stop、schedulerSet。长耗时业务请求（apiCall / checkinRunNow）和只读 status
 * 不在此队列中，避免一次签到或管理请求阻塞网关启停。
 *
 * 调用方不得在主线程直接执行 JNI；当前入口是同步的，调用方必须来自后台线程。
 * 同一控制线程内允许重入，避免组合操作自我等待造成死锁。
 */
object RuntimeController {
    private val worker = AtomicReference<Thread?>()
    private val executor = Executors.newSingleThreadExecutor(
        ThreadFactory { runnable ->
            Thread(runnable, "AgateNativeLifecycle").apply {
                isDaemon = true
                worker.set(this)
            }
        },
    )

    /** 在专用控制线程上执行一个生命周期变更，并把异常原样交还调用方。 */
    fun <T> runSerialized(operation: () -> T): T {
        if (Thread.currentThread() === worker.get()) return operation()

        val future = executor.submit(Callable(operation))
        // 生命周期变更一旦入队就不能因为调用线程被中断而“假取消”：
        // Future.get() 被中断并不会自动取消任务，若立即返回，start/stop 仍可能
        // 在调用方认为失败之后悄悄执行。这里等待已提交操作完成，再恢复中断标记，
        // 让调用方得到真实结果并由上层决定如何处理线程中断。
        var interrupted = false
        try {
            while (true) {
                try {
                    return future.get()
                } catch (_: InterruptedException) {
                    interrupted = true
                } catch (failed: ExecutionException) {
                    throw failed.cause ?: failed
                }
            }
        } finally {
            if (interrupted) Thread.currentThread().interrupt()
        }
    }

    /** 从任意线程按调用顺序入队；完成回调在控制线程执行，调用方自行切回 UI。 */
    fun <T> submitSerialized(
        operation: () -> T,
        onComplete: (Result<T>) -> Unit,
    ) {
        executor.execute {
            val result = runCatching(operation)
            runCatching { onComplete(result) }
        }
    }
}
