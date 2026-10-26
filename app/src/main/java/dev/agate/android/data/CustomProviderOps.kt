package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import org.json.JSONObject
import java.util.concurrent.TimeUnit

/**
 * 自定义 / 中转提供商（内核 `api/custom_providers.rs`）：
 * - GET  /api/custom-providers                    → {providers:[...]}
 * - POST /api/custom-providers                     → 新建（同时创建该家第一个账号）
 * - POST /api/custom-providers/update              {id, name?, protocol?, baseUrl?, enabled?}
 * - POST /api/custom-providers/remove              {id}  级联删名下账号
 * - POST /api/custom-providers/fetch-models        {providerId} 服务端代理拉模型
 * - POST /api/custom-providers/models              {providerId, models, mappings}
 *
 * 创建体字段（core::custom_providers::create 校验）：name 必填、protocol 归一、
 * baseUrl 合法；首个账号的凭证键随家（OpenAI 兼容用 apiKey）。
 */
object CustomProviderOps {



    @Volatile private var apiKey: String = ""

    fun configure(apiKey: String) {
        this.apiKey = apiKey
        CoreTransport.configure(apiKey)
    }

    class OpResult(val ok: Boolean, val data: JSONObject?, val error: String?)

    /** 协议取值（内核 normalize_protocol 认识的三种）。 */
    enum class Protocol(val wire: String, val label: String) {
        OPENAI("openai", "OpenAI 兼容"),
        ANTHROPIC("anthropic", "Anthropic"),
        RESPONSES("responses", "Responses"),
    }

    suspend fun list(): JSONObject? = get("/api/custom-providers")

    /** 新建自定义提供商 + 首个账号（一次事务，失败内核回滚）. */
    suspend fun create(
        name: String,
        baseUrl: String,
        apiKeyValue: String,
        protocol: Protocol,
        noAuth: Boolean = false,
        balanceScheme: String? = null,
    ): OpResult {
        val body = JSONObject()
            .put("name", name.trim())
            .put("baseUrl", baseUrl.trim())
            .put("protocol", protocol.wire)
        if (noAuth) {
            body.put("noAuth", true)
        } else {
            body.put("apiKey", apiKeyValue.trim().removePrefix("Bearer ").trim())
        }
        if (!balanceScheme.isNullOrBlank()) body.put("balanceScheme", balanceScheme)
        return post("/api/custom-providers", body)
    }

    /** 余额直查方案（内核 BALANCE_SCHEMES 全集）+ 展示名。 */
    val balanceSchemes: List<Pair<String, String>> = listOf(
        "newapi" to "NewAPI",
        "sub2api" to "Sub2API",
        "deepseek" to "DeepSeek",
        "opencode" to "OpenCode",
        "sensenova" to "SenseNova",
        "zhipu" to "智谱",
        "zhipu_zai" to "智谱 Z.AI",
    )

    suspend fun update(id: String, name: String? = null, baseUrl: String? = null, enabled: Boolean? = null): OpResult {
        val body = JSONObject().put("id", id)
        name?.let { body.put("name", it) }
        baseUrl?.let { body.put("baseUrl", it) }
        enabled?.let { body.put("enabled", it) }
        return post("/api/custom-providers/update", body)
    }

    suspend fun remove(id: String): OpResult =
        post("/api/custom-providers/remove", JSONObject().put("id", id))

    suspend fun fetchModels(providerId: String): OpResult =
        post("/api/custom-providers/fetch-models", JSONObject().put("providerId", providerId))

    // P2：经 CoreTransport 内核直调（不经端口，Core 态可用）
    private suspend fun get(path: String): JSONObject? {
        val r = CoreTransport.get(path)
        val ok = r as? CoreTransport.Reply.Ok ?: return null
        if (ok.status < 200 || ok.status >= 300) return null
        val obj = try {
            JSONObject(ok.text)
        } catch (_: Exception) {
            return null
        }
        return if (obj.optBoolean("success")) obj.optJSONObject("data") else null
    }

    // P2：经 CoreTransport 内核直调（错误信封随 status 一并透传，不拦 4xx/5xx）
    private suspend fun post(path: String, body: JSONObject): OpResult {
        val r = CoreTransport.post(path, body.toString())
        if (r !is CoreTransport.Reply.Ok) {
            return OpResult(false, null, (r as? CoreTransport.Reply.Down)?.message ?: "链路错误")
        }
        val obj = try {
            JSONObject(r.text)
        } catch (_: Exception) {
            return OpResult(false, null, "内核响应异常")
        }
        return if (obj.optBoolean("success")) OpResult(true, obj.optJSONObject("data"), null)
        else OpResult(false, obj.optJSONObject("data"), obj.optString("error").ifBlank { "操作失败" })
    }
}