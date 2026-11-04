package dev.agate.core

import org.json.JSONObject

/**
 * Structured result returned by the Native log-callback registration endpoint.
 * A failed update must not replace the last callback that Native still owns.
 */
data class LogCallbackUpdateResult(
    val ok: Boolean,
    val cleared: Boolean = false,
    val error: String? = null,
)

/**
 * Keeps the Kotlin strong reference in sync with the callback successfully
 * installed in Native. The native operation runs before [current] is changed,
 * so a rejected registration/clear leaves the previous callback reachable.
 */
internal class LogListenerRegistry<S : Any> {
    @Volatile
    private var current: S? = null

    @Synchronized
    fun update(
        candidate: S?,
        nativeUpdate: (S?) -> LogCallbackUpdateResult,
    ): LogCallbackUpdateResult {
        val result = nativeUpdate(candidate)
        if (result.ok) current = candidate
        return result
    }

    @Synchronized
    fun current(): S? = current
}


internal fun parseLogCallbackUpdateResponse(raw: String): LogCallbackUpdateResult {
    val response = JSONObject(raw)
    require(response.has("ok")) {
        "Native log callback response is missing 'ok'"
    }
    return LogCallbackUpdateResult(
        ok = response.getBoolean("ok"),
        cleared = response.optBoolean("cleared", false),
        error = response.optString("error").takeIf { it.isNotEmpty() },
    )
}
