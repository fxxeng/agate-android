package dev.agate.android.data

import java.net.Inet4Address
import java.net.NetworkInterface

/**
 * 手机当前的局域网 IPv4（形如 `192.168.1.105`）；拿不到返回 null。
 *
 * ── 为什么不用 WifiManager ─────────────────────────────────
 * 那条路在 Android 10+ 需要定位权限（`getConnectionInfo` 返回的 IP 在关闭定位时
 * 是 `0.0.0.0`），而这里只想回答「本机的私网地址是几」—— 用
 * `NetworkInterface` 遍历不申请任何权限，Wi-Fi / 热点 / USB 共享网络都能覆盖，
 * 也不受「WLAN 已连接但定位关着」这一常见状态影响。
 *
 * 判定口径：
 * - 只要 IPv4、且是**私网**（10/8、172.16/12、192.168/16）—— 手机也可能拿到
 *   公网地址（少数运营商 CGNAT 之外的场景），那种不该拿去当局域网地址给人抄；
 * - 跳过回环与虚拟网卡（`rmnet` 蜂窝数据、`dummy`、`p2p` 直连），否则用户会
 *   复制到一个同网段设备根本连不上的地址；
 * - 有多个候选时优先 `wlan` 前缀（最常见），其次第一个可用的。
 *
 * 失败一律返回 null，由调用方决定「不显示这一行」还是显示提示 —— 这里不抛。
 */
object LanAddress {

    fun ipv4(): String? = runCatching {
        var fallback: String? = null
        for (nic in NetworkInterface.getNetworkInterfaces()) {
            if (!nic.isUp || nic.isLoopback) continue
            val name = nic.name.orEmpty()
            if (name.startsWith("rmnet") || name.startsWith("dummy") || name.startsWith("p2p")) continue
            for (addr in nic.inetAddresses) {
                val v4 = addr as? Inet4Address ?: continue
                if (v4.isLoopbackAddress) continue
                val ip = v4.hostAddress ?: continue
                if (!isPrivate(ip)) continue
                if (name.startsWith("wlan")) return@runCatching ip
                if (fallback == null) fallback = ip
            }
        }
        fallback
    }.getOrNull()

    private fun isPrivate(ip: String): Boolean {
        val parts = ip.split('.').mapNotNull { it.toIntOrNull() }
        if (parts.size != 4) return false
        val (a, b) = parts[0] to parts[1]
        return a == 10 || (a == 172 && b in 16..31) || (a == 192 && b == 168)
    }
}
