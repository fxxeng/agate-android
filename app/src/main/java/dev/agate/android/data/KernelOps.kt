package dev.agate.android.data

import android.content.Context
import dev.agate.core.GatewayPrefs
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

    private val client by lazy {
        OkHttpClient.Builder()
            .connectTimeout(1500, TimeUnit.MILLISECONDS)
            .readTimeout(1500, TimeUnit.MILLISECONDS)
            .build()
    }

    /**
     * 网关端口活性探测。
     *
     * ⚠️ 端口**取自设置**（`GatewayPrefs.port`），不能写死 8787：设置页允许改端口，
     * 写死会让改了端口的用户永远得到「网关未运行」——界面据此弹「先运行网关」，
     * 而他明明已经跑着，点了又「启动成功」，从头到尾没有一句真话。
     *
     * [port] 显式传入优先（调用方手里已有值时省一次 SharedPreferences 读）；
     * 传 0 表示「按设置取」。
     */
    suspend fun isUp(ctx: Context?, port: Int = 0): Boolean = withContext(Dispatchers.IO) {
        val actual = port.takeIf { it in 1..65535 }
            ?: ctx?.let { GatewayPrefs.port(it) }
            ?: 8787
        try {
            val req = Request.Builder().url("http://127.0.0.1:$actual/health").get().build()
            client.newCall(req).execute().use { it.isSuccessful }
        } catch (_: Exception) {
            false
        }
    }
}