package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONArray
import org.json.JSONObject
import java.util.concurrent.TimeUnit

/**
 * 任务中心操作（M4 任务页），与 AccountOps 同款约定：suspend + IO、不持状态。
 *
 * 内核路由事实（agate-server/src/server/http.rs）：
 * - GET  /api/auto-checkin         → {enabled, time, providers, providerOptions[],
 *                                     schedules[], nextRunAt, running}
 * - POST /api/auto-checkin         {enabled?} 改总开关
 * - POST /api/auto-checkin/run     立即全量签到（无条目归属）
 * - POST /api/checkin-schedules/{id}/run  立即执行某条计划
 * - GET  /api/checkin-keepalive    → {models:[...]} 日活保活模型链
 * - POST /api/checkin-keepalive    {models:[...]} 保存保活链
 *
 * 读态（auto-checkin / keepalive）由 TaskCenter 在页面层拉，写操作收口到这里。
 */
object TaskOps {

    private const val PORT = 8787
    private val JSON_MEDIA = "application/json; charset=utf-8".toMediaType()

    private val client by lazy {
        OkHttpClient.Builder()
            .connectTimeout(8, TimeUnit.SECONDS)
            // 立即签到会逐个账号打上游，放宽到 60s
            .readTimeout(60, TimeUnit.SECONDS)
            .build()
    }

    @Volatile private var apiKey: String = ""

    fun configure(apiKey: String) {
        this.apiKey = apiKey
    }

    class OpResult(val ok: Boolean, val data: JSONObject?, val error: String?)

    /** 总开关：POST /api/auto-checkin {enabled}。 */
    suspend fun setEnabled(enabled: Boolean): OpResult =
        post("/api/auto-checkin", JSONObject().put("enabled", enabled))

    /** 立即全量签到：POST /api/auto-checkin/run。 */
    suspend fun runNow(): OpResult = post("/api/auto-checkin/run", JSONObject())

    /** 立即执行某条定时计划：POST /api/checkin-schedules/{id}/run。 */
    suspend fun runSchedule(id: String): OpResult =
        post("/api/checkin-schedules/$id/run", JSONObject())

    /** 保存日活保活模型链：POST /api/checkin-keepalive {models:[...]}。 */
    suspend fun saveKeepalive(models: List<String>): OpResult {
        val body = JSONObject().put("models", JSONArray(models))
        return post("/api/checkin-keepalive", body)
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