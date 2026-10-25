package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONObject
import java.util.concurrent.TimeUnit

/**
 * 模型操作（M3-5 模型页），与 AccountOps 同款约定：suspend + IO、不持状态；
 * apiKey 与 GatewayRepository 同一处 configure。
 *
 * 内核路由（agate-server/src/server/http.rs）：
 * - POST /api/models/refresh  {accounts?, providers?} 强制刷新各家清单
 * - POST /api/models/state    {id, provider?, enabled} 启停单条
 * - POST /api/models/mappings {alias, target, provider?, reasoning?} 新增/改映射
 * - POST /api/models/custom   {provider, id} 登记自定义模型
 * - POST /api/models/capabilities {provider, id, capabilities} 覆盖能力位
 *
 * 本轮模型页只用到 refresh；其余接口先备好，供后续「能力位 / 映射 / 自定义」子页使用。
 */
object ModelOps {

    private const val PORT = 8787
    private val JSON_MEDIA = "application/json; charset=utf-8".toMediaType()

    private val client by lazy {
        OkHttpClient.Builder()
            .connectTimeout(8, TimeUnit.SECONDS)
            // 刷新要去上游各家拉目录，放宽到 25s
            .readTimeout(25, TimeUnit.SECONDS)
            .build()
    }

    @Volatile private var apiKey: String = ""

    fun configure(apiKey: String) {
        this.apiKey = apiKey
    }

    class OpResult(val ok: Boolean, val data: JSONObject?, val error: String?)

    /** 强制刷新模型清单（不带 accounts/providers = 刷新所有已实现家）。 */
    suspend fun refresh(accounts: List<String>? = null, providers: List<String>? = null): OpResult {
        val body = JSONObject()
        accounts?.let { body.put("accounts", org.json.JSONArray(it)) }
        providers?.let { body.put("providers", org.json.JSONArray(it)) }
        return post("/api/models/refresh", body)
    }

    /** 启停某家的一条模型（provider 可选；缺省为旧版全局语义）。 */
    suspend fun setState(id: String, enabled: Boolean, provider: String? = null): OpResult {
        val body = JSONObject().put("id", id).put("enabled", enabled)
        if (!provider.isNullOrBlank()) body.put("provider", provider)
        return post("/api/models/state", body)
    }

    private suspend fun post(path: String, body: JSONObject): OpResult =
        withContext(Dispatchers.IO) {
            val builder = Request.Builder()
                .url("http://127.0.0.1:$PORT$path")
                .post(body.toString().toRequestBody(JSON_MEDIA))
            if (apiKey.isNotBlank()) builder.header("Authorization", "Bearer $apiKey")
            try {
                client.newCall(builder.build()).execute().use { resp ->
                    val text = resp.body?.string() ?: return@withContext OpResult(false, null, "空响应")
                    val obj = JSONObject(text)
                    if (obj.optBoolean("success")) {
                        OpResult(true, obj.optJSONObject("data"), null)
                    } else {
                        OpResult(false, obj.optJSONObject("data"), obj.optString("error").ifBlank { "操作失败" })
                    }
                }
            } catch (e: Exception) {
                OpResult(false, null, e.message ?: "网络错误")
            }
        }
}