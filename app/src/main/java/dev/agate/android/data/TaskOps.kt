package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
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



    @Volatile private var apiKey: String = ""

    fun configure(apiKey: String) {
        this.apiKey = apiKey
        CoreTransport.configure(apiKey)
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