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
 * 账号添加（短信 / 粘贴凭证）与账号操作（启停 / 删除）（M3-4）。
 *
 * 与 LoginClient 同款约定：suspend + IO、不持状态；apiKey 与 GatewayRepository
 * 同一处 configure。响应统一 OpResult —— 内核管理面错误走 {success:false, error}，
 * 成功时 data 可能为 null（batch 只回 success）。
 *
 * 路由事实（内核 http.rs / api/session.rs）：
 * - AutoClaw 两地：POST /api/session/login/sms/send {phone, provider}；
 *   verify {phone, code, deviceId?, name?, provider} → {account, list}
 * - Loomy：POST /api/session/login/loomy/sms/send {phone}（响应含 msgid）；
 *   verify {phone, code, msgid, name?} → {account, list}
 *   两条链路分开挂是刻意的（签名/站点/错误码不同），不在内核合并，壳侧也不合并。
 * - 粘贴型三家走统一 POST /api/accounts（各家凭证字段见 addAccountFor）。
 * - 批量操作：POST /api/accounts/batch {ids, action: enable|disable|remove}。
 */
object AccountOps {

    private const val PORT = 8787
    private val JSON_MEDIA = "application/json; charset=utf-8".toMediaType()

    private val client by lazy {
        OkHttpClient.Builder()
            .connectTimeout(8, TimeUnit.SECONDS)
            // 粘贴型添加会去上游校验凭证（kuku enrich_identity 等），
            // 余额查询要逐账号打上游，统一放宽到 40s
            .readTimeout(40, TimeUnit.SECONDS)
            .build()
    }

    @Volatile private var apiKey: String = ""

    fun configure(apiKey: String) {
        this.apiKey = apiKey
    }

    class OpResult(val ok: Boolean, val data: JSONObject?, val error: String?)

    /** 发送验证码。loomy 返回的 msgid 由调用方保存、verify 时回传。 */
    suspend fun smsSend(providerId: String, phone: String): OpResult {
        val (path, body) = if (providerId == "loomy") {
            "/api/session/login/loomy/sms/send" to JSONObject().put("phone", phone)
        } else {
            "/api/session/login/sms/send" to
                JSONObject().put("phone", phone).put("provider", providerId)
        }
        return post(path, body)
    }

    /** 用手机号 + 验证码登录并直接落成账号 → {account, list}。 */
    suspend fun smsVerify(
        providerId: String,
        phone: String,
        code: String,
        msgid: String?,
        name: String?,
    ): OpResult {
        val body = JSONObject().put("phone", phone).put("code", code)
        if (!name.isNullOrBlank()) body.put("name", name)
        return if (providerId == "loomy") {
            if (!msgid.isNullOrBlank()) body.put("msgid", msgid)
            post("/api/session/login/loomy/sms/verify", body)
        } else {
            body.put("provider", providerId)
            post("/api/session/login/sms/verify", body)
        }
    }

    /**
     * 粘贴凭证添加（统一入口 POST /api/accounts，按家分派凭证字段）：
     * - kuku：cookie 整串（BDUSS/STOKEN…），上游校验并补全昵称
     * - commandcode：apiKey（user_ 前缀）
     * - antigravity：refreshToken（email 可选）
     */
    suspend fun addAccountFor(
        providerId: String,
        primary: String,
        secondary: String?,
        name: String?,
    ): OpResult {
        val body = JSONObject().put("provider", providerId)
        if (!name.isNullOrBlank()) body.put("name", name)
        when (providerId) {
            "kuku" -> body.put("cookie", primary.trim())
            "commandcode" -> body.put("apiKey", primary.trim())
            "antigravity" -> {
                body.put("refreshToken", primary.trim())
                if (!secondary.isNullOrBlank()) body.put("email", secondary.trim())
            }
            else -> return OpResult(false, null, "该提供商不支持粘贴凭证添加")
        }
        return post("/api/accounts", body)
    }

    /** 批量启停/删除。单账号操作传单元素列表（内核同一端点，无单条路由）。 */
    suspend fun batch(ids: List<String>, action: String): Boolean {
        val body = JSONObject()
            .put("ids", org.json.JSONArray(ids))
            .put("action", action)
        return post("/api/accounts/batch", body).ok
    }

    /** GET /api/accounts/usage[?id=] → {results:[{id,name,usage,error,code?}], skipped}。 */
    suspend fun usage(id: String? = null): JSONObject? {
        val q = if (id.isNullOrBlank()) "" else "?id=" + java.net.URLEncoder.encode(id, "UTF-8")
        return get("/api/accounts/usage$q")
    }

    /** GET /api/accounts/connections → {counts:{id:count}}。 */
    suspend fun connections(): JSONObject? = get("/api/accounts/connections")

    /** PATCH /api/accounts/{id}：改备注名 / enabled / maxConcurrent / proxy 等。 */
    suspend fun update(id: String, body: JSONObject): OpResult =
        patch("/api/accounts/" + java.net.URLEncoder.encode(id, "UTF-8"), body)

    /** POST /api/accounts/{id}/move {direction: up|down}：本家内调整出战顺序。 */
    suspend fun move(id: String, up: Boolean): OpResult =
        post("/api/accounts/" + java.net.URLEncoder.encode(id, "UTF-8") + "/move",
            JSONObject().put("direction", if (up) "up" else "down"))

    /** POST /api/accounts/refresh-expiring：并发刷新临期凭证。 */
    suspend fun refreshExpiring(): OpResult = post("/api/accounts/refresh-expiring", JSONObject())

    /**
     * 导入凭证文件（POST /api/accounts）：
     * - 文本 JSON（auth.json / providers.json）→ [text]（authFileContent）
     * - 二进制（KukuAI 的 Cookies SQLite）→ [b64]（authFileB64）
     * [fileName] 只用于错误文案。
     */
    suspend fun importCredential(
        provider: String,
        fileName: String?,
        text: String? = null,
        b64: String? = null,
        accountName: String? = null,
    ): OpResult {
        val body = JSONObject().put("provider", provider)
        if (!fileName.isNullOrBlank()) body.put("authFileName", fileName)
        if (text != null) body.put("authFileContent", text)
        if (b64 != null) body.put("authFileB64", b64)
        if (!accountName.isNullOrBlank()) body.put("name", accountName)
        return post("/api/accounts", body)
    }

    private suspend fun get(path: String): JSONObject? = withContext(Dispatchers.IO) {
        val builder = Request.Builder().url("http://127.0.0.1:$PORT$path").get()
        if (apiKey.isNotBlank()) builder.header("Authorization", "Bearer $apiKey")
        try {
            client.newCall(builder.build()).execute().use { resp ->
                if (!resp.isSuccessful) return@withContext null
                val text = resp.body?.string() ?: return@withContext null
                val obj = JSONObject(text)
                if (obj.optBoolean("success")) obj.optJSONObject("data") else null
            }
        } catch (_: Exception) {
            null
        }
    }

    private suspend fun patch(path: String, body: JSONObject): OpResult =
        withContext(Dispatchers.IO) {
            val builder = Request.Builder()
                .url("http://127.0.0.1:$PORT$path")
                .patch(body.toString().toRequestBody(JSON_MEDIA))
            if (apiKey.isNotBlank()) builder.header("Authorization", "Bearer $apiKey")
            try {
                client.newCall(builder.build()).execute().use { resp ->
                    val text = resp.body?.string() ?: return@withContext OpResult(false, null, "空响应")
                    val obj = JSONObject(text)
                    if (obj.optBoolean("success")) OpResult(true, obj.optJSONObject("data"), null)
                    else OpResult(false, obj.optJSONObject("data"), obj.optString("error").ifBlank { "操作失败" })
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
