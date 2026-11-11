package dev.agate.android.auth

import java.net.URI
import java.net.URLDecoder

/** WebView 顶层导航转发白名单；内核仍是 state 校验的最终安全边界。 */
object AuthCallbackPolicy {
    fun shouldForward(providerId: String, expectedState: String, rawUrl: String): Boolean {
        if (providerId != "raccoon" || expectedState.isEmpty()) return false
        val uri = try {
            URI(rawUrl)
        } catch (_: Exception) {
            return false
        }
        if (uri.scheme != "office-raccoon" || uri.host != "auth" || uri.path != "/callback") {
            return false
        }
        if (uri.rawUserInfo != null || uri.port != -1 || uri.rawFragment != null) return false
        val states = uri.rawQuery?.split('&').orEmpty().mapNotNull { pair ->
            val split = pair.indexOf('=')
            val rawKey = if (split >= 0) pair.substring(0, split) else pair
            if (decode(rawKey) != "state") return@mapNotNull null
            decode(if (split >= 0) pair.substring(split + 1) else "")
        }
        return states.size == 1 && states.single() == expectedState
    }

    private fun decode(raw: String): String? = try {
        URLDecoder.decode(raw, "UTF-8")
    } catch (_: Exception) {
        null
    }
}
