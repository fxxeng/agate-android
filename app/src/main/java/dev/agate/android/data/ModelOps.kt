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
 * - GET  /api/models/manage                     → {models, mappings, reasoningLevels}（管理视图）
 * - POST /api/models/refresh  {accounts?, providers?} 强制刷新各家清单
 * - POST /api/models/state    {id, provider?, enabled} 启停单条
 * - POST /api/models/mappings {alias, target, provider?, reasoning?, enabled?} 新增/改映射
 * - POST /api/models/mappings/remove {alias, target, provider?} 删除映射
 * - POST /api/models/custom   {provider, id} 登记自定义模型
 * - POST /api/models/custom/remove {provider, id} 移除登记
 * - POST /api/models/capabilities {provider, id, capabilities} 覆盖能力位
 *
 * ★ 三态协议（reasoning 与 enabled 共用，model_manage.rs 模块头）：
 *   请求体里键「缺失」= 不动已有值；reasoning 给空串/null = 显式清成「不覆盖」；
 *   reasoning 给字符串 = 设为该档（表外自定义值也放行，由内核只做长度约束）；
 *   enabled 只认布尔（缺失 = 不动）。
 *
 * ★ capabilities 是 patch 语义：只提交改动键；值 null = 恢复上游值（清除覆盖）；
 *   token 键要 1~1 亿正整数、开关键要布尔，仅内置提供商可用（自定义提供商
 *   的能力走整表保存接口，本端点会 400）。
 *
 * 所有写接口都返回最新 manage 视图（就地重绘，不必再拉一次）。
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

    /** 映射行（manage 视图 mappings[] 一条）。 */
    class MappingRow(
        val alias: String,
        val target: String,
        val provider: String?,
        val enabled: Boolean,
        val reasoning: String?,
        val isDefault: Boolean,
        val dangling: Boolean,
        val carried: Boolean,
    ) {
        companion object {
            fun from(o: JSONObject): MappingRow = MappingRow(
                alias = o.optString("alias"),
                target = o.optString("target"),
                // provider 可空（空串 = 全局语义：由所有承载 target 的家接收）
                provider = o.optString("provider").ifBlank { null },
                enabled = o.optBoolean("enabled", true),
                reasoning = o.optString("reasoning").ifBlank { null },
                isDefault = o.optBoolean("isDefault", false),
                dangling = o.optBoolean("dangling", false),
                carried = o.optBoolean("carried", true),
            )
        }
    }

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

    /** 拉管理视图（GET，无请求体）。 */
    suspend fun fetchManage(): OpResult = get("/api/models/manage")

    /**
     * 新增映射（同 alias+target+provider 三元组 = 命中已有条目改它）。
     * reasoning 传 null 且 reasoningExplicit=false = 不动；explicit=true 且 null/空 = 清成不覆盖。
     */
    suspend fun setMapping(
        alias: String,
        target: String,
        provider: String? = null,
        reasoning: String? = null,
        reasoningExplicit: Boolean = false,
        enabled: Boolean? = null,
    ): OpResult {
        val body = JSONObject().put("alias", alias).put("target", target)
        if (!provider.isNullOrBlank()) body.put("provider", provider)
        if (reasoningExplicit) body.put("reasoning", reasoning ?: "")
        if (enabled != null) body.put("enabled", enabled)
        return post("/api/models/mappings", body)
    }

    suspend fun removeMapping(alias: String, target: String, provider: String? = null): OpResult {
        val body = JSONObject().put("alias", alias).put("target", target)
        if (!provider.isNullOrBlank()) body.put("provider", provider)
        return post("/api/models/mappings/remove", body)
    }

    /** 登记一个上游目录里没有的自定义模型。 */
    suspend fun addCustomModel(provider: String, id: String): OpResult =
        post("/api/models/custom", JSONObject().put("provider", provider).put("id", id))

    suspend fun removeCustomModel(provider: String, id: String): OpResult =
        post("/api/models/custom/remove", JSONObject().put("provider", provider).put("id", id))

    /** 覆盖能力位（patch：只提交改动键；值 null = 恢复上游；仅内置提供商）。 */
    suspend fun setCapabilities(provider: String, id: String, patch: JSONObject): OpResult =
        post(
            "/api/models/capabilities",
            JSONObject().put("provider", provider).put("id", id).put("capabilities", patch),
        )

    private suspend fun get(path: String): OpResult =
        withContext(Dispatchers.IO) {
            val builder = Request.Builder().url("http://127.0.0.1:$PORT$path")
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
