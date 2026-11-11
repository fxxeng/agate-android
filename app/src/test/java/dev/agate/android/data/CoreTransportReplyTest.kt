package dev.agate.android.data

import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.asCoroutineDispatcher
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
        val callerDispatcher = caller.asCoroutineDispatcher()
        val native = Executors.newSingleThreadExecutor { runnable ->
            Thread(runnable, "tokio-callback")
        }
        val parsedThread = AtomicReference<String>()
        try {
            val pending = java.util.concurrent.ConcurrentHashMap<Long, CancellableContinuation<String>>()
            val callback = dev.agate.core.ApiCallCallback { id, raw ->
                pending.remove(id)?.resume(raw)
            }
            val reply = kotlinx.coroutines.withContext(callerDispatcher) {
                val raw = kotlinx.coroutines.suspendCancellableCoroutine<String> { continuation ->
                    pending[1] = continuation
                    native.execute { callback.onComplete(1, "raw-json") }
                }
                // 这是生产 call() 的解析位置：恢复后的调用协程，仍应在 caller 调度器上。
                parsedThread.set(Thread.currentThread().name)
                raw
            }
            assertEquals("raw-json", reply)
            assertEquals("caller-io", parsedThread.get().substringBefore(" @"))
            assertNotEquals("tokio-callback", parsedThread.get().substringBefore(" @"))
        } finally {
            callerDispatcher.close()
            native.shutdownNow()
        }
    }
}
