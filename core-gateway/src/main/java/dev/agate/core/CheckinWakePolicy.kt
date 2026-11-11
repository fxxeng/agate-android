package dev.agate.core

import android.os.PowerManager

/** 每完成一个账号，取锁预算重新计算；静默等待期仍有硬上限。 */
internal object CheckinWakePolicy {
    const val IDLE_TIMEOUT_MS = 16 * 60 * 1000L

    @Volatile private var observedRevision = -1L

    @Synchronized
    fun reset() {
        observedRevision = -1L
    }

    @Synchronized
    fun observe(status: Gateway.Status, lock: PowerManager.WakeLock) {
        if (status.bridge < 4) {
            if (lock.isHeld) lock.release()
            error("当前内核不支持签到进展看门狗，请更新应用")
        }
        if (status.checkinRevision != observedRevision) {
            observedRevision = status.checkinRevision
            if (lock.isHeld) lock.release()
            lock.acquire(IDLE_TIMEOUT_MS)
        }
    }
}
