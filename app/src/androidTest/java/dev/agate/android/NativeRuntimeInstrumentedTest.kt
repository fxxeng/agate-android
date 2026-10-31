package dev.agate.android

import android.Manifest
import android.content.Context
import android.content.Intent
import android.os.SystemClock
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.agate.core.AgateForegroundService
import dev.agate.core.Gateway
import dev.agate.core.GatewayPrefs
import dev.agate.core.RuntimeController
import java.io.File
import java.net.ServerSocket
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * 真正运行在 Android Runtime 中的 Native Core / Gateway JNI 验证。
 * 测试数据写入 app 私有 filesDir，不读取或修改用户正式数据。
 */
@RunWith(AndroidJUnit4::class)
class NativeRuntimeInstrumentedTest {

    private fun coreDataDir(context: Context): File =
        File(context.filesDir, "agate").apply { mkdirs() }

    private fun ensureCore(context: Context) {
        val result = RuntimeController.runSerialized {
            Gateway.coreInit(coreDataDir(context).absolutePath, GatewayPrefs.port(context), false)
        }
        assertTrue("coreInit failed: $result", result.optBoolean("ok"))
        val status = Gateway.status()
        assertTrue("Core should be ready: $status", status.core)
        assertFalse("Scheduler must remain disabled in instrumentation", status.scheduler)
    }

    @Test
    fun nativeLibraryCoreRouterAndGatewayLifecycleWork() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        // 仪器测试必须关闭后台签到调度，避免测试期间触发真实 Provider 任务。
        GatewayPrefs.setSchedulerOn(context, false)
        val dataDir = coreDataDir(context)
        ensureCore(context)
        assertTrue("nativeVersion must not be blank", Gateway.version().isNotBlank())

        // 重复初始化必须幂等，不应重建 Core 或启动监听端口。
        repeat(3) { ensureCore(context) }
        assertFalse("coreInit must not implicitly start Gateway", Gateway.status().running)

        // 覆盖 JNI GlobalRef 注册/撤销路径，不依赖异步日志时序。
        Gateway.setLogListener(Gateway.LogListener { _, _, _ -> })
        Gateway.setLogListener(null)

        // Router::oneshot 直调应在 Gateway 未监听端口时仍可用。
        val direct = JSONObject(Gateway.apiCall("GET", "/health", null, ""))
        assertTrue("apiCall failed: $direct", direct.optBoolean("ok"))
        assertEquals("unexpected /health status: $direct", 200, direct.optInt("status"))
        assertTrue("/health body must be JSON", runCatching { JSONObject(direct.optString("body")) }.isSuccess)

        val port = ServerSocket(0).use { it.localPort }
        try {
            val started = RuntimeController.runSerialized {
                Gateway.start(dataDir.absolutePath, port = port, bindLan = false)
            }
            assertTrue("Gateway.start failed: $started", started.ok)
            assertTrue("Gateway should report running after start", Gateway.status().running)

            val stopped = RuntimeController.runSerialized { Gateway.stop() }
            assertTrue("Gateway.stop failed: $stopped", stopped.optBoolean("ok"))
            assertTrue("Gateway should be stopped", await(8_000) { !Gateway.status().running })
        } finally {
            // 即使断言失败，也尽量清理进程级 listener，避免污染后续测试。
            runCatching { RuntimeController.runSerialized { Gateway.stop() } }
        }
    }

    @Test
    fun foregroundServicePreservesStartAfterStopOrderingBarrier() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        // Android 12+ 对后台启动 FGS 有限制。先把目标 Activity 带到前台，
        // 并授予通知权限，避免测试依赖 Emulator 的初始前台状态或权限弹窗。
        runCatching {
            instrumentation.uiAutomation.grantRuntimePermission(
                context.packageName,
                Manifest.permission.POST_NOTIFICATIONS,
            )
        }
        val launchIntent = context.packageManager.getLaunchIntentForPackage(context.packageName)
            ?: error("Agate launcher Activity not found")
        instrumentation.startActivitySync(launchIntent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        SystemClock.sleep(300)

        val oldPort = GatewayPrefs.port(context)
        val oldBindLan = GatewayPrefs.bindLan(context)
        val testPort = ServerSocket(0).use { it.localPort }
        try {
            GatewayPrefs.setPort(context, testPort)
            GatewayPrefs.setBindLan(context, false)
            GatewayPrefs.setSchedulerOn(context, false)
            ensureCore(context)

            startForegroundService(context)
            assertTrue("first START did not start Gateway", await(15_000) { Gateway.status().running })
            assertEquals(testPort, Gateway.status().port)

            // 不等待旧 STOP 完成就提交新 START，验证旧 startId 的 stopSelfResult
            // 不会移除新启动意图；命令队列必须按 STOP → START 顺序执行。
            AgateForegroundService.stop(context)
            startForegroundService(context)

            assertTrue("START after STOP was lost", await(20_000) {
                val status = Gateway.status()
                status.running && status.port == testPort
            })

            // 留出主线程处理旧 stopSelfResult(startId) 的窗口，再确认新 Gateway 仍运行。
            SystemClock.sleep(500)
            assertTrue("old STOP incorrectly killed the new START", Gateway.status().running)
        } finally {
            runCatching { AgateForegroundService.stop(context) }
            await(10_000) { runCatching { !Gateway.status().running }.getOrDefault(true) }
            runCatching {
                context.stopService(Intent(context, AgateForegroundService::class.java))
            }
            GatewayPrefs.setPort(context, oldPort)
            GatewayPrefs.setBindLan(context, oldBindLan)
            // 测试安装运行在一次性 Emulator；保持调度关闭，防止后台签到在报告收集期间启动。
        }
    }

    private fun startForegroundService(context: Context) {
        val intent = Intent(context, AgateForegroundService::class.java)
            .setAction(AgateForegroundService.ACTION_START)
        context.startForegroundService(intent)
    }

    private fun await(timeoutMs: Long, predicate: () -> Boolean): Boolean {
        val deadline = SystemClock.elapsedRealtime() + timeoutMs
        while (SystemClock.elapsedRealtime() < deadline) {
            if (runCatching(predicate).getOrDefault(false)) return true
            SystemClock.sleep(100)
        }
        return runCatching(predicate).getOrDefault(false)
    }
}
