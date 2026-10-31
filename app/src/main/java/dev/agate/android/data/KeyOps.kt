package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
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
        CoreTransport.configure(key)
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

    /** 「可用提供商」的一格候选（`providers[]` 一条）。 */
    data class ProviderOption(val id: String, val label: String, val count: Int = 0)

    /**
     * 列表响应的候选表。
     *
     * ── 为什么必须由后端给，而不是前端自己凑 ─────────────────────
     * `modelsByProvider` 是「各家清单 + 映射别名」的合并结果，前端拿不到这层；
     * 而 `/api/session` 的 `models` 是跨家**去重**后的对外视图 —— 按家过滤它
     * 会漏掉被别家认领的同名模型。项目里也明确禁止维护第二份 provider 清单
     * （见 `account_store` 模块头）。所以这里只做搬运，不做加工。
     */
    class Catalog(
        val providers: List<ProviderOption>,
        val modelsByProvider: Map<String, List<String>>,
    ) {
        /**
         * 某家的对外模型清单；认不出这家就给空表。
         *
         * 键按小写兜底再查一次：`modelsByProvider` 的键是 provider id 原文，
         * 而勾选的 id 来自同一张响应所以通常完全一致 —— 兜底只为挡住「同一家
         * 在两张表里大小写不同」这种畸形数据，命中不了也不用 stats 撑场面。
         */
        fun modelsFor(providerId: String): List<String> {
            modelsByProvider[providerId]?.let { return it }
            val folded = modelsByProvider.entries.firstOrNull {
                it.key.equals(providerId, ignoreCase = true)
            }
            return folded?.value ?: emptyList()
        }
    }

    /** 解出随列表一起回来的两张候选表。 */
    fun parseCatalog(data: JSONObject?): Catalog {
        if (data == null) return Catalog(emptyList(), emptyMap())
        val providers = ArrayList<ProviderOption>()
        val arr = data.optJSONArray("providers")
        if (arr != null) {
            for (i in 0 until arr.length()) {
                val o = arr.optJSONObject(i) ?: continue
                val id = o.optString("id")
                if (id.isBlank()) continue
                providers += ProviderOption(
                    id = id,
                    label = o.optString("label").ifBlank { id },
                    count = o.optInt("count", 0),
                )
            }
        }
        val modelsByProvider = LinkedHashMap<String, List<String>>()
        data.optJSONObject("modelsByProvider")?.let { obj ->
            val keys = obj.keys()
            while (keys.hasNext()) {
                val id = keys.next()
                val a = obj.optJSONArray(id) ?: continue
                modelsByProvider[id] =
                    (0 until a.length()).mapNotNull { a.optString(it).takeIf(String::isNotBlank) }
            }
        }
        return Catalog(providers, modelsByProvider)
    }

    suspend fun list(): OpResult = call("GET", "/api/keys", null)

    suspend fun create(name: String, key: String? = null): OpResult =
        call(
            "POST",
            "/api/keys",
            JSONObject().put("name", name).apply {
                key?.takeIf { it.isNotBlank() }?.let { put("key", it) }
            }.toString(),
        )

    suspend fun setEnabled(id: String, enabled: Boolean): OpResult =
        call("PATCH", "/api/keys/$id", JSONObject().put("enabled", enabled).toString())

    /**
     * 改「可用范围」：可用提供商 + 可用模型。
     *
     * ── 为什么总是同时发两个键 ───────────────────────────────
     * 内核 `parse_allowlist` 的三态语义是「键缺失 = 不动」（是为了不让旧版前端
     * 顺手抹掉限制而设的），但这里是明确的整块覆盖编辑：用户把某半清空就是**要
     * 清空**。所以两个键一次都发，空数组表示「清成不限制」，而不是省略。
     */
    suspend fun setScope(id: String, providers: List<String>, models: List<String>): OpResult =
        call(
            "PATCH",
            "/api/keys/$id",
            JSONObject()
                .put("allowedProviders", jsonArray(providers))
                .put("allowedModels", jsonArray(models))
                .toString(),
        )

    suspend fun remove(id: String): OpResult = call("DELETE", "/api/keys/$id", null)

    /** String 列表转 JSONArray（org.json 没有直接从 List 构造的入口）。 */
    private fun jsonArray(items: List<String>): JSONArray =
        JSONArray().apply { items.forEach { put(it) } }

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

    private suspend fun call(method: String, path: String, body: String?): OpResult =
        withContext(Dispatchers.IO) {
            val r = CoreTransport.call(method, path, body)
            if (r !is CoreTransport.Reply.Ok) {
                return@withContext OpResult(false, null, (r as? CoreTransport.Reply.Down)?.message ?: "链路错误")
            }
            val json = runCatching { JSONObject(r.text) }.getOrNull()
            val ok = r.status in 200..299
            OpResult(
                ok = ok,
                data = json?.optJSONObject("data"),
                error = if (ok) null else json?.optJSONObject("error")?.optString("message"),
            )
        }

    class OpResult(val ok: Boolean, val data: JSONObject?, val error: String?)
}
