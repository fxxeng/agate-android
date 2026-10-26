package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
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



    @Volatile private var apiKey: String = ""

    fun configure(apiKey: String) {
        this.apiKey = apiKey
        CoreTransport.configure(apiKey)
    }

    class OpResult(val ok: Boolean, val error: String?)

    // P2：经 CoreTransport 内核直调（不经端口，Core 态可用）
    suspend fun list(): JSONArray? = withContext(Dispatchers.IO) {
        val r = CoreTransport.get("/api/scheduled-tasks")
        val ok = r as? CoreTransport.Reply.Ok ?: return@withContext null
        if (ok.status < 200 || ok.status >= 300) return@withContext null
        val obj = runCatching { JSONObject(ok.text) }.getOrNull() ?: return@withContext null
        if (obj.optBoolean("success")) obj.optJSONObject("data")?.optJSONArray("tasks") else null
    }

    suspend fun configure(id: String, enabled: Boolean? = null, interval: Int? = null): OpResult =
        withContext(Dispatchers.IO) {
            val body = JSONObject()
            enabled?.let { body.put("enabled", it) }
            interval?.let { body.put("interval", it) }
            val r = CoreTransport.post("/api/scheduled-tasks/$id", body.toString())
            if (r !is CoreTransport.Reply.Ok) {
                return@withContext OpResult(false, (r as? CoreTransport.Reply.Down)?.message ?: "链路错误")
            }
            val obj = runCatching { JSONObject(r.text) }.getOrNull()
            if (r.status in 200..299 && obj?.optBoolean("success") == true) OpResult(true, null)
            else OpResult(false, obj?.optString("error")?.ifBlank { "保存失败" } ?: "保存失败")
        }
}
