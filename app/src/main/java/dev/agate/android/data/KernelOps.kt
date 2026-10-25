package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.OkHttpClient
import okhttp3.Request
import java.util.concurrent.TimeUnit

/**
 * 内核存活探测。
 *
 * 为什么需要：账号登录（`/api/session/login/…`）、落账号（`/api/accounts`）全部由
 * **内核进程**执行 —— 客户端没有登录实现。网关没起来时这些调用只会得到
 * 「无法连接内核」。这里用公开的 `/health`（免鉴权，见 http.rs 的路由注释）
 * 做一次轻量探活，界面据此给出「启动网关」入口，而不是抛一句错误。
 */
object KernelOps {

    private const val PORT = 8787

    private val client by lazy {
        OkHttpClient.Builder()
            .connectTimeout(1500, TimeUnit.MILLISECONDS)
            .readTimeout(1500, TimeUnit.MILLISECONDS)
            .build()
    }

    suspend fun isUp(): Boolean = withContext(Dispatchers.IO) {
        try {
            val req = Request.Builder().url("http://127.0.0.1:$PORT/health").get().build()
            client.newCall(req).execute().use { it.isSuccessful }
        } catch (_: Exception) {
            false
        }
    }
}