package dev.agate.android

import android.app.Application
import dev.agate.core.Gateway
import dev.agate.core.RuntimeController
import dev.agate.core.GatewayPrefs

/**
 * 进程入口（B 包 · 两级内核）：任何组件起来（MainActivity / FGS / 闹钟）
 * 都先过这里。Core 态随进程常驻 —— bootstrap 基础设施 + 后台调度（凭证
 * 维护 / 模型刷新 / auto_checkin，开关见 GatewayPrefs.schedulerOn），
 * 不监听端口；网关仍由用户手动启动（FGS ACTION_START）。
 *
 * 后台线程初始化，不阻塞主线程冷启动；失败仅 logcat（UI 轮询 status
 * 兜底，FGS tick 也会再兜底一次 coreInit）。
 */
class AgateApp : Application() {
    override fun onCreate() {
        super.onCreate()
        Thread {
            try {
                RuntimeController.runSerialized {
                    Gateway.coreInit(
                        filesDir.resolve("agate").absolutePath,
                        GatewayPrefs.port(this),
                        GatewayPrefs.schedulerOn(this),
                    )
                }
            } catch (t: Throwable) {
                android.util.Log.e("Agate", "Core 初始化异常（.so 加载失败？）", t)
            }
        }.start()
    }
}
