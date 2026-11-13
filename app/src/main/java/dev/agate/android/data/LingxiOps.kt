package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/**
 * WPS 灵犀的任务中心 / 智点明细 / 通道设置（客户端数据层）。
 *
 * ── 端点事实（逐条对着内核 `api::lingxi_api` 核过）──────────────
 * | 动作 | 端点 |
 * |---|---|
 * | 任务看板（只读） | `GET /api/accounts/{id}/lingxi/tasks` |
 * | 领取某任务 | `POST /api/accounts/{id}/lingxi/tasks/claim` body `{taskKey}` |
 * | 代跑某任务 | `POST /api/accounts/{id}/lingxi/tasks/run` body `{taskKey}` |
 * | 智点明细 | `GET /api/accounts/{id}/lingxi/ledger?type=&incomeCursor=&expenseCursor=&limit=` |
 * | 通道设置 | `GET/POST /api/lingxi/settings` |
 *
 * ── 三条必须由内核裁决、界面不许自己算的事 ───────────────────
 * 1. **「已领取」的判据是任务状态**，不是 HTTP 200：上游对重复领取回 200
 *    假成功（方案 L11）。内核在领取后回读 `status == "claimed"` 才把
 *    `claimed` 置真，界面只显示 `message` 与 `claimed`。
 * 2. **任务能不能代跑**由内核给 `auto`，界面不按任务名猜。
 * 3. **明细读不出来**时内核仍返回 200，但带 `upstreamStatus` 与 `error` ——
 *    界面写「暂不可用」，不拿空表冒充「没有流水」。
 *
 * ── 通道设置为什么要单独一条端点 ─────────────────────────────
 * 它只影响灵犀一家的转发形态（SSE / cowork WS），切换后**下一条请求**即生效
 * （内核 `is_stateful()` 每请求现读）。两条通道各自拿到什么由内核在
 * `options[].detail` 里给，界面不自己编文案。
 */
object LingxiOps {

    class OpResult(
        val ok: Boolean,
        /** 内核给的人话结论（领取 / 代跑后用它的 `message`）。 */
        val message: String = "",
        val data: JSONObject? = null,
    )

    data class Task(
        val key: String,
        val title: String,
        val status: String,
        val claimable: Boolean,
        val claimed: Boolean,
        val reward: Double?,
        val auto: Boolean,
        val hint: String,
    )

    data class Board(
        val tasks: List<Task>,
        val incomplete: Int,
        val claimable: Int,
        val claimed: Int,
        val pendingCredit: Double,
        val claimedCredit: Double,
        val checkedInToday: Boolean,
        /** 上游读失败时为真：界面写「暂不可用」而不是「没有任务」。 */
        val unavailable: Boolean,
        val error: String?,
    )

    data class Txn(
        val id: String,
        val kind: String,
        val title: String,
        val source: String,
        val amount: Double,
        val time: String,
        val expireTime: String,
    )

    data class TxnPage(
        val items: List<Txn>,
        val incomeCursor: Long,
        val expenseCursor: Long,
        val hasMore: Boolean,
        val unavailable: Boolean,
        val error: String?,
    )

    data class ChannelOption(val id: String, val label: String, val detail: String)

    fun configure(apiKey: String) {
        CoreTransport.configure(apiKey)
    }

    // ── 读 ──────────────────────────────────────────────────────

    private suspend fun getData(path: String): JSONObject? = withContext(Dispatchers.IO) {
        val reply = CoreTransport.get(path) as? CoreTransport.Reply.Ok ?: return@withContext null
        if (reply.status < 200 || reply.status >= 300) return@withContext null
        val obj = runCatching { JSONObject(reply.text) }.getOrNull() ?: return@withContext null
        if (!obj.optBoolean("success")) null else obj.optJSONObject("data")
    }

    private suspend fun post(path: String, body: JSONObject): OpResult = withContext(Dispatchers.IO) {
        when (val reply = CoreTransport.post(path, body.toString())) {
            is CoreTransport.Reply.Down -> OpResult(false, reply.message)
            is CoreTransport.Reply.Ok -> {
                val obj = runCatching { JSONObject(reply.text) }.getOrNull()
                    ?: return@withContext OpResult(false, "内核响应异常")
                val data = obj.optJSONObject("data")
                if (obj.optBoolean("success")) {
                    OpResult(true, data?.optString("message").orEmpty(), data)
                } else {
                    val error = obj.optJSONObject("error")?.optString("message").orEmpty()
                        .ifBlank { obj.optString("error") }
                    OpResult(false, error.ifBlank { "操作失败（HTTP ${reply.status}）" }, data)
                }
            }
        }
    }

    /** 任务看板。读失败也返回 Board（`unavailable = true`），由界面写「暂不可用」。 */
    suspend fun board(accountId: String): Board? {
        val data = getData("/api/accounts/$accountId/lingxi/tasks") ?: return null
        val tasksArr = data.optJSONArray("tasks") ?: JSONArray()
        val tasks = (0 until tasksArr.length()).mapNotNull { i ->
            tasksArr.optJSONObject(i)?.let { row ->
                Task(
                    key = row.optString("key"),
                    title = row.optString("title").ifBlank { row.optString("key") },
                    status = row.optString("status"),
                    claimable = row.optBoolean("claimable"),
                    claimed = row.optBoolean("claimed"),
                    reward = if (row.has("reward") && !row.isNull("reward")) row.optDouble("reward") else null,
                    auto = row.optBoolean("auto"),
                    hint = row.optString("hint"),
                )
            }
        }
        val summary = data.optJSONObject("summary")
        return Board(
            tasks = tasks,
            incomplete = summary?.optInt("incomplete") ?: 0,
            claimable = summary?.optInt("claimable") ?: 0,
            claimed = summary?.optInt("claimed") ?: 0,
            pendingCredit = summary?.optDouble("pendingCredit") ?: 0.0,
            claimedCredit = summary?.optDouble("claimedCredit") ?: 0.0,
            checkedInToday = data.optBoolean("checkedInToday"),
            unavailable = data.optInt("upstreamStatus", 200) != 200 && tasks.isEmpty(),
            error = data.optString("error").ifBlank { null },
        )
    }

    /** 领取一个任务（签到也走这条）。`message` 是内核回读任务状态后的人话结论。 */
    suspend fun claim(accountId: String, taskKey: String): OpResult =
        post("/api/accounts/$accountId/lingxi/tasks/claim", JSONObject().put("taskKey", taskKey))

    /** 代跑一次性任务（专家 / 客户端 / 群聊）。慢：几十秒到两分钟。 */
    suspend fun run(accountId: String, taskKey: String): OpResult =
        post("/api/accounts/$accountId/lingxi/tasks/run", JSONObject().put("taskKey", taskKey))

    suspend fun ledger(
        accountId: String,
        type: String = "all",
        incomeCursor: Long = 0,
        expenseCursor: Long = 0,
        limit: Int = 20,
    ): TxnPage? {
        val data = getData(
            "/api/accounts/$accountId/lingxi/ledger" +
                "?type=$type&incomeCursor=$incomeCursor&expenseCursor=$expenseCursor&limit=$limit"
        ) ?: return null
        val arr = data.optJSONArray("items") ?: JSONArray()
        val items = (0 until arr.length()).mapNotNull { i ->
            arr.optJSONObject(i)?.let { row ->
                Txn(
                    id = row.optString("id"),
                    kind = row.optString("kind"),
                    title = row.optString("title"),
                    source = row.optString("source"),
                    amount = row.optDouble("amount"),
                    time = row.optString("time"),
                    expireTime = row.optString("expireTime"),
                )
            }
        }
        return TxnPage(
            items = items,
            incomeCursor = data.optLong("incomeCursor"),
            expenseCursor = data.optLong("expenseCursor"),
            hasMore = data.optBoolean("hasMore"),
            unavailable = data.optInt("upstreamStatus", 200) != 200 && items.isEmpty(),
            error = data.optString("error").ifBlank { null },
        )
    }

    // ── 通道设置 ────────────────────────────────────────────────

    suspend fun channel(): Pair<String, List<ChannelOption>>? {
        val data = getData("/api/lingxi/settings") ?: return null
        val arr = data.optJSONArray("options") ?: JSONArray()
        val options = (0 until arr.length()).mapNotNull { i ->
            arr.optJSONObject(i)?.let { row ->
                ChannelOption(
                    id = row.optString("id"),
                    label = row.optString("label"),
                    detail = row.optString("detail"),
                )
            }
        }
        return data.optString("channel").ifBlank { "sse" } to options
    }

    suspend fun saveChannel(channel: String): Boolean =
        post("/api/lingxi/settings", JSONObject().put("channel", channel)).ok
}
