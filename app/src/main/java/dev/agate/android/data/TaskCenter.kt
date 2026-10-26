package dev.agate.android.data

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import okhttp3.OkHttpClient
import org.json.JSONObject
import java.util.concurrent.TimeUnit

/**
 * 任务中心数据源（M4 任务页）：定时签到状态 + 日活保活链。
 *
 * 只读、页面生命周期内轮询（与 GatewayRepository 快照同款约定）：
 * - GET /api/auto-checkin        → 总开关 / 计划条目 / 下次执行 / 是否在跑
 * - GET /api/checkin-keepalive   → 保活模型链 {models:[...]}
 *
 * 写操作在 TaskOps；写成功后页面调 [refreshOnce] 就地更新。
 *
 * v44 任务页的「福利中心 / 验证码池 / 待处理」在 Android 侧无对应内核端点
 * （属任务领取聚合），本层不臆造，页面按诚实占位呈现。
 */
object TaskCenter {

    data class ScheduleRow(
        val id: String,
        val displayLabel: String,
        val time: String,
        val enabled: Boolean,
        val providers: List<String>,
        val lastFiredToday: Boolean,
        val nextRunAt: Long?,
        val lastResult: String?,
    )

    data class TaskSnapshot(
        val fetchedAtMs: Long = 0,
        /** 总开关（master） */
        val enabled: Boolean = false,
        /** 是否有一轮签到正在执行 */
        val running: Boolean = false,
        /** 聚合「下次执行」时间戳（ms；无则 null） */
        val nextRunAt: Long? = null,
        val schedules: List<ScheduleRow> = emptyList(),
        /** 可选签到范围（id → label），CHECKIN_PROVIDERS 注册顺序 */
        val providerOptions: List<Pair<String, String>> = emptyList(),
        /** 日活保活模型链 */
        val keepalive: List<String> = emptyList(),
    )

    private const val INTERVAL_MS = 8_000L

    private val _snapshot = MutableStateFlow(TaskSnapshot())
    val snapshot: StateFlow<TaskSnapshot> = _snapshot

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error

    @Volatile private var apiKey: String = ""
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var pollJob: Job? = null


    fun configure(apiKey: String) {
        this.apiKey = apiKey
        CoreTransport.configure(apiKey)
    }

    fun startPolling() {
        if (pollJob?.isActive == true) return
        pollJob = scope.launch {
            while (isActive) {
                refreshOnce()
                delay(INTERVAL_MS)
            }
        }
    }

    fun stopPolling() {
        pollJob?.cancel()
        pollJob = null
    }

    fun refreshOnce() {
        scope.launch {
            val checkin = getJson("/api/auto-checkin")
            val keepalive = getJson("/api/checkin-keepalive")
            if (checkin == null && keepalive == null) {
                _error.value = "无法连接内核"
                return@launch
            }
            _error.value = null
            val prev = _snapshot.value
            _snapshot.value = TaskSnapshot(
                fetchedAtMs = System.currentTimeMillis(),
                enabled = checkin?.optBoolean("enabled") ?: prev.enabled,
                running = checkin?.optBoolean("running") ?: prev.running,
                nextRunAt = checkin?.optLong("nextRunAt", 0L)?.takeIf { it > 0 } ?: prev.nextRunAt,
                schedules = checkin?.let { parseSchedules(it) } ?: prev.schedules,
                providerOptions = checkin?.let { parseOptions(it) } ?: prev.providerOptions,
                keepalive = keepalive?.let { parseKeepalive(it) } ?: prev.keepalive,
            )
        }
    }

    private fun parseSchedules(obj: JSONObject): List<ScheduleRow> {
        val arr = obj.optJSONArray("schedules") ?: return emptyList()
        val out = ArrayList<ScheduleRow>(arr.length())
        for (i in 0 until arr.length()) {
            val s = arr.optJSONObject(i) ?: continue
            val id = s.optString("id")
            if (id.isBlank()) continue
            val providers = ArrayList<String>()
            s.optJSONArray("providers")?.let { ps ->
                for (j in 0 until ps.length()) ps.optString(j).takeIf { it.isNotBlank() }?.let { providers += it }
            }
            out += ScheduleRow(
                id = id,
                displayLabel = s.optString("displayLabel").ifBlank { s.optString("time") },
                time = s.optString("time"),
                enabled = s.optBoolean("enabled"),
                providers = providers,
                lastFiredToday = s.optBoolean("lastFiredToday"),
                nextRunAt = s.optLong("nextRunAt", 0L).takeIf { it > 0 },
                lastResult = s.opt("lastResult")?.let { if (it == JSONObject.NULL) null else it.toString() },
            )
        }
        return out
    }

    private fun parseOptions(obj: JSONObject): List<Pair<String, String>> {
        val arr = obj.optJSONArray("providerOptions") ?: return emptyList()
        val out = ArrayList<Pair<String, String>>(arr.length())
        for (i in 0 until arr.length()) {
            val o = arr.optJSONObject(i) ?: continue
            val id = o.optString("id")
            if (id.isNotBlank()) out += id to o.optString("label").ifBlank { id }
        }
        return out
    }

    private fun parseKeepalive(obj: JSONObject): List<String> {
        val arr = obj.optJSONArray("models") ?: return emptyList()
        val out = ArrayList<String>(arr.length())
        for (i in 0 until arr.length()) arr.optString(i).takeIf { it.isNotBlank() }?.let { out += it }
        return out
    }

    // P2：经 CoreTransport 内核直调（调用方均在 scope.launch 协程内，suspend 合法）
    private suspend fun getJson(path: String): JSONObject? {
        val r = CoreTransport.get(path)
        val ok = r as? CoreTransport.Reply.Ok ?: return null
        if (ok.status < 200 || ok.status >= 300) return null
        val obj = runCatching { JSONObject(ok.text) }.getOrNull() ?: return null
        if (!obj.optBoolean("success")) return null
        return obj.optJSONObject("data")
    }
}