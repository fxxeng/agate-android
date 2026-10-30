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
 * - 跳过回环与**虚拟网卡**：`rmnet`（蜂窝数据）、`dummy`、`p2p`（直连），
 *   以及 **tun / tap / ppp / ccmni** —— 最后一组是 VPN 与虚拟网卡的高频命名
 *   （Clash / v2rayNG / Tailscale / WireGuard 建的都是 tun0；蜂窝 PPP 会是
 *   ppp0），它们的私网段 IP（10.x / 172.16.x）对同网段设备完全不可达 ——
 *   不滤掉的话，用户挂着 VPN 时复制到的地址根本连不上（2026-10-30 修）；
 * - 多个候选时按**接口身份**排优先级：`wlan*`（Wi-Fi，最常见）>
 *   `ap* / swlan*`（**热点**——电脑连的是手机开的热点，这时候热点口才是
 *   对的那一个）> `eth*`（USB 网络共享 / 以太网）> 其余第一个可用的。
 *
 * 失败一律返回 null，由调用方决定「不显示这一行」还是显示提示 —— 这里不抛。
 */
object LanAddress {

    fun ipv4(): String? = runCatching {
        // 候选按接口身份分档：档位越小越可信，取**最高档**里的第一个私网地址。
        // 不能「遇到 wlan 就立即返回」—— 那样没问题，但「热点口 ap0 优先于
        // 别的杂项」这类次级优先就没有位置放了；分档统一表达。
        var best: Pair<Int, String>? = null
        for (nic in NetworkInterface.getNetworkInterfaces()) {
            if (!nic.isUp || nic.isLoopback) continue
            val name = nic.name.orEmpty().lowercase()
            if (isVirtual(name)) continue
            val rank = rankOf(name) ?: continue
            for (addr in nic.inetAddresses) {
                val v4 = addr as? Inet4Address ?: continue
                if (v4.isLoopbackAddress) continue
                val ip = v4.hostAddress ?: continue
                if (!isPrivate(ip)) continue
                val current = best
                if (current == null || rank < current.first) best = rank to ip
            }
        }
        best?.second
    }.getOrNull()

    /**
     * 虚拟网卡判定（按名称前缀）。**宁可错杀的名单要短**：名单里出现的都是
     * 「对同网段设备必然不可达 / 不是共享入口」的口 —— 漏一个 VPN 口的代价
     * （用户复制到一个连不上的地址）远大于多滤一个真口的代价（多一个候选，
     * 优先级照样能把它排到后面）。
     */
    private fun isVirtual(name: String): Boolean = name.startsWith("rmnet") ||
        name.startsWith("dummy") ||
        name.startsWith("p2p") ||
        // VPN 与隧道：Clash / v2rayNG / Tailscale / WireGuard / OpenVPN 全是 tun，
        // 老式 VPN 与部分蜂窝 PPP 是 tap / ppp，ccmni 是 MTK 蜂窝
        name.startsWith("tun") ||
        name.startsWith("tap") ||
        name.startsWith("ppp") ||
        name.startsWith("ccmni")

    /**
     * 接口身份 → 优先档位（越小越优先）。不认识的口给**最后档**而不是丢弃：
     * 各厂商的口名没有统一标准，误丢一个真口比多留一个候选更难排查 ——
     * 前面的虚拟网卡过滤已经把「必然不可达」的口挡掉了，剩下的都值得参与
     * 排序，只是排在认识的身份后面。
     */
    private fun rankOf(name: String): Int = when {
        name.startsWith("wlan") -> 0            // Wi-Fi
        name.startsWith("ap") || name.startsWith("swlan") -> 1   // 热点
        name.startsWith("eth") || name.startsWith("rndis") ||
            name.startsWith("usb") -> 2         // USB 网络共享 / 以太网
        else -> 9                               // 未知口：最后兜底
    }

    private fun isPrivate(ip: String): Boolean {
        val parts = ip.split('.').mapNotNull { it.toIntOrNull() }
        if (parts.size != 4) return false
        val (a, b) = parts[0] to parts[1]
        return a == 10 || (a == 172 && b in 16..31) || (a == 192 && b == 168)
    }
}
