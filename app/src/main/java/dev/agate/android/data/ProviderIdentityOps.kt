package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

/**
 * 「本家专有设置」的读写：ZCode 客户端版本号、Cline 伪装头。
 *
 * ── 为什么这两项要能在界面上改 ──────────────────────────────
 * 它们都进**出站请求头**，而上游按「请求长得像不像官方客户端」决定放不放行：
 *   · ZCode：`User-Agent: ZCode/{version}` + `X-ZCode-App-Version`。上游抬高
 *     版本门槛后，旧版本会被拒 —— 桌面/安卓形态改不了进程环境变量，所以必须有
 *     一个界面入口（否则只能等发版）。
 *   · Cline：`X-CLIENT-TYPE` 是免费池的硬门槛，其余头（版本/平台）让请求更
 *     贴近官方 CLI；上游收紧时同样需要能自己调。
 *
 * 两条接口都在 protected 组，走 [CoreTransport] 直调（与 ModelOps 同款）。
 */
object ProviderIdentityOps {

    /** ZCode 版本号：`overridden` 为界面填的值（空 = 未覆盖），`default` 是内置默认。 */
    data class ZcodeIdentity(val overridden: String, val default: String) {
        /** 实际生效值（界面显示「当前生效 = X」）。 */
        val effective: String get() = overridden.ifBlank { default }
    }

    /** Cline 伪装头：`overrides` 是用户改过的键，`defaults` 是代码内置，`effective` 是合并结果。 */
    data class ClineHeaders(
        val overrides: Map<String, String>,
        val defaults: Map<String, String>,
        val effective: Map<String, String>,
    )

    suspend fun getZcode(): ZcodeIdentity? {
        val data = get("/api/zcode/identity") ?: return null
        return ZcodeIdentity(
            overridden = data.optString("appVersion"),
            default = data.optString("default"),
        )
    }

    suspend fun setZcode(version: String): Result =
        put("/api/zcode/identity", JSONObject().put("appVersion", version))

    suspend fun getCline(): ClineHeaders? {
        val data = get("/api/cline/headers") ?: return null
        fun read(key: String): Map<String, String> {
            val o = data.optJSONObject(key) ?: return emptyMap()
            val out = LinkedHashMap<String, String>()
            o.keys().forEach { k -> out[k] = o.optString(k) }
            return out
        }
        return ClineHeaders(read("overrides"), read("defaults"), read("effective"))
    }

    /** 整体替换覆盖表（与内核语义一致：删掉某键 = 回到默认）。 */
    suspend fun setCline(overrides: Map<String, String>): Result {
        val o = JSONObject()
        overrides.forEach { (k, v) -> o.put(k, v) }
        return put("/api/cline/headers", JSONObject().put("overrides", o))
    }

    private suspend fun get(path: String): JSONObject? = withContext(Dispatchers.IO) {
        val r = CoreTransport.get(path)
        if (r !is CoreTransport.Reply.Ok) return@withContext null
        runCatching { JSONObject(r.text) }.getOrNull()?.optJSONObject("data")
    }

    private suspend fun put(path: String, body: JSONObject): Result = withContext(Dispatchers.IO) {
        val r = CoreTransport.call("PUT", path, body.toString())
        if (r !is CoreTransport.Reply.Ok) {
            return@withContext Result(false, (r as? CoreTransport.Reply.Down)?.message ?: "链路错误")
        }
        val json = runCatching { JSONObject(r.text) }.getOrNull()
        Result(
            ok = r.status in 200..299,
            error = if (r.status in 200..299) null
            else json?.optJSONObject("error")?.optString("message"),
        )
    }

    class Result(val ok: Boolean, val error: String?)
}
