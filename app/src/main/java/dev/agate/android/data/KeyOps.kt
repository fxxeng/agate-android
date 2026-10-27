package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

/**
 * 网关 API Key 管理（访问密钥页）。
 *
 * 内核路由（agate-server/src/server/http.rs:533）：
 * - `GET    /api/keys`            → 列表
 * - `POST   /api/keys`            `{name?, key?, allowedProviders?, allowedModels?}`
 * - `PATCH  /api/keys/{id}`       改启用 / 白名单
 * - `DELETE /api/keys/{id}`       吊销
 *
 * ── 这份 Key 是干嘛的（决定了这一页为什么重要）──────────────
 * `/v1` 系列路由的闸门只认「启用的 Key 列表 + 环境变量里那把壳层 Key」。没有可用
 * Key 时，外部客户端（Cherry Studio / Cline / 脚本）调 `/v1/chat/completions`
 * 会直接拿到 `401 Unauthorized: invalid or missing API key` —— 这是「账号都
 * 登好了却调不通」最常见的一处断点。所以这一页要能：
 *   ① 看到壳层自动生成的那把 Key（[dev.agate.core.GatewayPrefs.apiKey]，前缀 `agate-`）；
 *   ② 一键复制 Base URL + Key（发给外部客户端用）；
 *   ③ 自建更多 Key（可限制可用提供商 / 模型）。
 */
object KeyOps {

    @Volatile private var apiKey: String = ""

    fun configure(key: String) {
        apiKey = key
    }

    /** 一条 Key 记录（内核 `api_keys` 的展示面）。 */
    data class KeyRow(
        val id: String,
        val name: String,
        val keyTail: String,
        val enabled: Boolean,
        val allowedProviders: List<String>,
        val allowedModels: List<String>,
        /** 是否为壳层注入的那把（走环境变量，不在 SQLite 列表里）。 */
        val fromEnv: Boolean = false,
    ) {
        val restricted: Boolean
            get() = allowedProviders.isNotEmpty() || allowedModels.isNotEmpty()
    }

    suspend fun list(): Result = call("GET", "/api/keys", null)

    suspend fun create(name: String, key: String? = null): Result =
        call(
            "POST",
            "/api/keys",
            JSONObject().put("name", name).apply {
                key?.takeIf { it.isNotBlank() }?.let { put("key", it) }
            }.toString(),
        )

    suspend fun setEnabled(id: String, enabled: Boolean): Result =
        call("PATCH", "/api/keys/$id", JSONObject().put("enabled", enabled).toString())

    suspend fun remove(id: String): Result = call("DELETE", "/api/keys/$id", null)

    /**
     * 解出列表。
     *
     * ── 为什么要额外补一行「本机默认」────────────────────────
     * 壳层那把 Key 走**环境变量**注入（`AGATE_PROXY_API_KEY`），压根不在
     * SQLite 的 Key 列表里，但闸门认它。如果界面只显示列表，用户会以为
     * 「列表空 = 我没有可用 Key」，而实际手上那把就能用 —— 这正是很多
     * 「明明配了却 401」的来源。所以把它单独补在最上面，并标明来历。
     */
    fun parseRows(data: JSONObject?, envKey: String): List<KeyRow> {
        val out = ArrayList<KeyRow>()
        if (envKey.isNotBlank()) {
            out += KeyRow(
                id = ENV_ID,
                name = "本机默认（自动生成）",
                keyTail = "…" + envKey.takeLast(4),
                enabled = true,
                allowedProviders = emptyList(),
                allowedModels = emptyList(),
                fromEnv = true,
            )
        }
        if (data == null) return out
        val arr = data.optJSONArray("keys")
            ?: data.optJSONArray("items")
            ?: return out
        for (i in 0 until arr.length()) {
            val o = arr.optJSONObject(i) ?: continue
            val id = o.optString("id")
            if (id.isBlank()) continue
            fun strings(key: String): List<String> {
                val a = o.optJSONArray(key) ?: return emptyList()
                return (0 until a.length()).mapNotNull { a.optString(it).takeIf(String::isNotBlank) }
            }
            out += KeyRow(
                id = id,
                name = o.optString("name").ifBlank { "未命名" },
                keyTail = o.optString("keyTail").ifBlank {
                    o.optString("tail").ifBlank { "…" }
                },
                enabled = o.optBoolean("enabled", true),
                allowedProviders = strings("allowedProviders"),
                allowedModels = strings("allowedModels"),
            )
        }
        return out
    }

    /** 壳层那把的哨兵 id（不是真的 Key 记录，不能 PATCH / DELETE）。 */
    const val ENV_ID = "__env__"

    private suspend fun call(method: String, path: String, body: String?): Result =
        withContext(Dispatchers.IO) {
            val r = CoreTransport.call(method, path, body)
            if (r !is CoreTransport.Reply.Ok) {
                return@withContext Result(false, null, (r as? CoreTransport.Reply.Down)?.message ?: "链路错误")
            }
            val json = runCatching { JSONObject(r.text) }.getOrNull()
            val ok = r.status in 200..299
            Result(
                ok = ok,
                data = json?.optJSONObject("data"),
                error = if (ok) null else json?.optJSONObject("error")?.optString("message"),
            )
        }

    class Result(val ok: Boolean, val data: JSONObject?, val error: String?)
}
