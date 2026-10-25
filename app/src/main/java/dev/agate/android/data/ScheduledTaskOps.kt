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
 * 定时任务（内核 `api/scheduled_tasks.rs`）：
 * - `GET  /api/scheduled-tasks`      → `{tasks:[{id,label,enabled,interval,min,max,nextRunAt,…}]}`
 * - `POST /api/scheduled-tasks/{id}` → `{enabled?, interval?}`（间隔单位由任务的 `unit` 决定）
 *
 * 目前用到的只有 `credential_maintenance`（凭证自动维护 = 自动续期）。
 */
object ScheduledTaskOps {

    const val TASK_CREDENTIAL_MAINTENANCE = "credential_maintenance"

    private const val PORT = 8787
    private val JSON_MEDIA = "application/json; charset=utf-8".toMediaType()

    private val client by lazy {
        OkHttpClient.Builder()
            .connectTimeout(5, TimeUnit.SECONDS)
            .readTimeout(10, TimeUnit.SECONDS)
            .build()
    }

    @Volatile private var apiKey: String = ""

    fun configure(apiKey: String) {
        this.apiKey = apiKey
    }

    class OpResult(val ok: Boolean, val error: String?)

    suspend fun list(): JSONArray? = withContext(Dispatchers.IO) {
        val builder = Request.Builder().url("http://127.0.0.1:$PORT/api/scheduled-tasks").get()
        if (apiKey.isNotBlank()) builder.header("Authorization", "Bearer $apiKey")
        try {
            client.newCall(builder.build()).execute().use { resp ->
                if (!resp.isSuccessful) return@withContext null
                val obj = JSONObject(resp.body?.string() ?: return@withContext null)
                if (obj.optBoolean("success")) obj.optJSONObject("data")?.optJSONArray("tasks") else null
            }
        } catch (_: Exception) {
            null
        }
    }

    suspend fun configure(id: String, enabled: Boolean? = null, interval: Int? = null): OpResult =
        withContext(Dispatchers.IO) {
            val body = JSONObject()
            enabled?.let { body.put("enabled", it) }
            interval?.let { body.put("interval", it) }
            val builder = Request.Builder()
                .url("http://127.0.0.1:$PORT/api/scheduled-tasks/$id")
                .post(body.toString().toRequestBody(JSON_MEDIA))
            if (apiKey.isNotBlank()) builder.header("Authorization", "Bearer $apiKey")
            try {
                client.newCall(builder.build()).execute().use { resp ->
                    val text = resp.body?.string() ?: ""
                    val obj = runCatching { JSONObject(text) }.getOrNull()
                    if (resp.isSuccessful && obj?.optBoolean("success") == true) OpResult(true, null)
                    else OpResult(false, obj?.optString("error")?.ifBlank { "保存失败" } ?: "保存失败")
                }
            } catch (e: Exception) {
                OpResult(false, e.message ?: "网络错误")
            }
        }
}
