package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

/**
 * 周期任务（任务中心 · 周期任务页）—— 内核 `api/scheduled_tasks.rs`。
 *
 * ```text
 *   GET   /api/scheduled-tasks         {credentialMaintenanceDisabledProviders[], tasks:[…]}
 *   PATCH /api/scheduled-tasks/{id}    {enabled?, interval?}（POST 是同义别名）
 *   POST  /api/scheduled-tasks/{id}/run  立即执行（仅 backend 任务可用）
 * ```
 *
 * ── 这一页与签到没有语义关系 ────────────────────────────────
 * 上游也是另一个一级页（`data-page="tasks"`）。六条里两类：
 * - `backend` 由网关循环执行，`canRun = true`，给「立即执行」；
 * - `frontend` 的执行者是**页面自己的定时器**，`canRun = false`。
 *   点了没有任何东西可跑，所以不给按钮 —— 一个点了没反应的按钮比没有更糟。
 *
 * ── `update_check` 在界面上隐藏 ─────────────────────────────
 * 它的配置入口收在「设置 → 更新设置」弹窗里。同一件事两个入口会让人以为是
 * 两件事。后端照常执行，只是本页不渲染它。
 *
 * ── 间隔单位由任务的 `unit` 决定 ────────────────────────────
 * `minutes`（凭证维护 / 模型刷新 / 版本检查）与 `seconds`（三条前端刷新）。
 * 界面显示与输入都必须跟着 `unit` 走，不能一律当分钟 —— 30 秒写成 30 分钟
 * 会让日志页半小时才刷一次。
 */
object PeriodicTaskOps {

    /** 界面隐藏的任务（入口在别处）。 */
    private val HIDDEN = setOf("update_check")

    class Row(o: JSONObject) {
        val id: String = o.optString("id")
        val label: String = o.optString("label")
        val description: String = o.optString("description")
        val unit: String = o.optString("unit").ifBlank { "minutes" }
        val runner: String = o.optString("runner").ifBlank { "backend" }
        val enabled: Boolean = o.optBoolean("enabled")
        val interval: Int = o.optInt("interval")
        val min: Int = o.optInt("min")
        val max: Int = o.optInt("max")
        val defaultInterval: Int = o.optInt("defaultInterval")
        val running: Boolean = o.optBoolean("running")
        val lastRunAt: Long = o.optLong("lastRunAt", 0L)
        val lastSuccessAt: Long = o.optLong("lastSuccessAt", 0L)
        val lastResult: String? = o.optString("lastResult").takeIf { it.isNotBlank() }
        val lastError: String? = o.optString("lastError").takeIf { it.isNotBlank() }
        val retryAt: Long = o.optLong("retryAt", 0L)
        val nextRunAt: Long = o.optLong("nextRunAt", 0L)
        val canRun: Boolean = o.optBoolean("canRun")

        val isBackend: Boolean get() = runner == "backend"

        /** 间隔的人话（`60 分钟` / `30 秒`）。 */
        val intervalText: String get() = "$interval " + unitLabel(unit)

        companion object {
            fun unitLabel(unit: String): String = if (unit == "seconds") "秒" else "分钟"
        }
    }

    class Snap(val tasks: List<Row>) {
        val visible: List<Row> get() = tasks.filter { it.id !in HIDDEN }
        val backend: List<Row> get() = visible.filter { it.isBackend }
        val frontend: List<Row> get() = visible.filter { !it.isBackend }
        val enabledCount: Int get() = visible.count { it.enabled }
    }

    suspend fun load(): Snap? = withContext(Dispatchers.IO) {
        val data = CheckinOps.getData("/api/scheduled-tasks") ?: return@withContext null
        val arr = data.optJSONArray("tasks") ?: return@withContext Snap(emptyList())
        Snap((0 until arr.length()).mapNotNull { i -> arr.optJSONObject(i)?.let(::Row) })
    }

    /** 改一条任务（只改传进来的字段）。 */
    suspend fun configure(id: String, enabled: Boolean? = null, interval: Int? = null): CheckinOps.OpResult {
        val body = JSONObject()
        enabled?.let { body.put("enabled", it) }
        interval?.let { body.put("interval", it) }
        return CheckinOps.post("/api/scheduled-tasks/$id", body)
    }

    suspend fun runNow(id: String): CheckinOps.OpResult =
        CheckinOps.post("/api/scheduled-tasks/$id/run", JSONObject())
}
