package dev.agate.android.data

import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CancellableContinuation
import kotlin.coroutines.resume
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test

/** 回调入口的真实并发形态：不加载 Android/JNI，直接检验注册表边界。 */
class CoreTransportReplyTest {
    @Test
    fun callbackOnlyDeliversRawTextToCallerDispatcher() = kotlinx.coroutines.runBlocking {
        val caller = Executors.newSingleThreadExecutor { runnable ->
            Thread(runnable, "caller-io")
        }
        val callerDispatcher = kotlinx.coroutines.asCoroutineDispatcher(caller)
        val native = Executors.newSingleThreadExecutor { runnable ->
            Thread(runnable, "tokio-callback")
        }
        val enteredCallback = CountDownLatch(1)
        val releaseCallback = CountDownLatch(1)
        val parsedThread = AtomicReference<String>()
        try {
            val pending = java.util.concurrent.ConcurrentHashMap<Long, CancellableContinuation<String>>()
            val callback = dev.agate.core.ApiCallCallback { id, raw ->
                enteredCallback.countDown()
                releaseCallback.await(2, TimeUnit.SECONDS)
                pending.remove(id)?.resume(raw)
            }
            val reply = kotlinx.coroutines.withContext(callerDispatcher) {
                kotlinx.coroutines.suspendCancellableCoroutine<String> { continuation ->
                    pending[1] = continuation
                    native.execute { callback.onReply(1, "raw-json") }
                }
            }
            // 这是生产 call() 的解析位置：恢复后的调用协程，而非 onReply。
            parsedThread.set(Thread.currentThread().name)
            assertEquals("raw-json", reply)
            assertEquals("caller-io", parsedThread.get())
            assertNotEquals("tokio-callback", parsedThread.get())
        } finally {
            releaseCallback.countDown()
            callerDispatcher.close()
            native.shutdownNow()
        }
    }
}
