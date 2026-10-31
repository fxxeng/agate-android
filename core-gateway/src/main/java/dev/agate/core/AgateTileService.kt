package dev.agate.core

import android.os.Handler
import android.os.Looper
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Quick Settings 磁贴：任意界面下拉即可启停网关。
 *
 * 磁贴回调运行在主线程，不能同步调用 JNI。状态读取和服务命令均放到后台线程，
 * UI 更新再切回主线程。启停仍复用 ForegroundService 的唯一命令入口。
 */
class AgateTileService : TileService() {

    private val mainHandler = Handler(Looper.getMainLooper())
    private val actionPending = AtomicBoolean(false)

    override fun onStartListening() {
        super.onStartListening()
        refreshTileAsync()
    }

    override fun onClick() {
        super.onClick()
        if (!actionPending.compareAndSet(false, true)) return
        showTilePending()

        Thread({
            try {
                val running = currentStatus()?.running == true
                if (running) {
                    AgateForegroundService.stop(this)
                    mainHandler.post { applyState(qsTile, false, null) }
                } else {
                    val accepted = AgateForegroundService.start(this)
                    mainHandler.post {
                        if (accepted) applyState(qsTile, true, null)
                        else refreshTileAsync()
                    }
                }
            } catch (t: Throwable) {
                android.util.Log.e("Agate", "Quick Settings 网关切换失败", t)
                mainHandler.post { refreshTileAsync() }
            } finally {
                actionPending.set(false)
            }
        }, "AgateTileCommand").apply { isDaemon = true }.start()
    }

    private fun refreshTileAsync() {
        Thread({
            val status = currentStatus()
            mainHandler.post {
                if (actionPending.get()) return@post
                applyState(qsTile, status?.running == true, status?.port)
            }
        }, "AgateTileStatus").apply { isDaemon = true }.start()
    }

    private fun currentStatus(): Gateway.Status? = try {
        Gateway.status()
    } catch (t: Throwable) {
        android.util.Log.w("Agate", "读取网关状态失败", t)
        null
    }

    private fun showTilePending() {
        qsTile?.let { tile ->
            tile.state = Tile.STATE_UNAVAILABLE
            tile.label = "Agate · 切换中"
            tile.updateTile()
        }
    }

    private fun applyState(tile: Tile?, running: Boolean, port: Int?) {
        tile ?: return
        tile.state = if (running) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
        tile.label = when {
            running && (port ?: 0) > 0 -> "Agate · 运行中 (:$port)"
            running -> "Agate · 启动中"
            else -> "Agate · 已停止"
        }
        tile.updateTile()
    }
}
