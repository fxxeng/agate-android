package dev.agate.core

import android.service.quicksettings.Tile
import android.service.quicksettings.TileService

/**
 * Quick Settings 磁贴：任意界面下拉即可启停网关。
 *
 * 启停复用 [AgateForegroundService] 的 START/STOP 链路（启动 already_running
 * 幂等、用户主动停止不复活），磁贴自身不触碰 JNI 启停；状态探测读
 * Gateway.status()（本地 JNI 调用，失败一律按未运行处理）。
 */
class AgateTileService : TileService() {

    override fun onStartListening() {
        // 面板每次可见都校准为真实状态（覆盖 onClick 的乐观显示）
        val st = currentStatus()
        val tile = qsTile ?: return
        applyState(tile, st?.running == true, st?.port)
    }

    override fun onClick() {
        val tile = qsTile ?: return
        val st = currentStatus()
        val running = st?.running == true
        Thread {
            if (running) {
                AgateForegroundService.stop(this)
            } else {
                AgateForegroundService.start(this)
            }
            // start/stop 都是发 Intent 异步生效，先按目标态乐观显示，
            // 下一次 onStartListening 用真实状态校正
            applyState(tile, !running, st?.port)
        }.start()
    }

    private fun currentStatus(): Gateway.Status? = try {
        Gateway.status()
    } catch (_: Throwable) {
        null
    }

    private fun applyState(tile: Tile, running: Boolean, port: Int?) {
        tile.state = if (running) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
        tile.label = if (running && (port ?: 0) > 0) "Agate · 运行中 (:$port)" else "Agate · 已停止"
        tile.updateTile()
    }
}
