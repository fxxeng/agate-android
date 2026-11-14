package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/**
 * QuWork 权益（客户端数据层）：月度赠送 + 订单号 / 邀请码兑换。
 *
 * ── 端点事实（对着内核 `api::quwork_benefits` 核过）──────────────
 * | 动作 | 端点 |
 * |---|---|
 * | 月度赠送（只读） | `GET /api/accounts/{id}/quwork/benefits` |
 * | 订单号兑换 | `POST /api/accounts/{id}/quwork/redeem` body `{orderId}` |
 * | 邀请码激活 | `POST /api/accounts/{id}/quwork/redeem` body `{inviteCode}` |
 *
 * ── 三条界面不许自己算的事 ───────────────────────────────────
 * 1. **月度赠送是只读的**：上游没有「领取月度赠送」的写端点（实测），
 *    界面只展示，不给按钮 —— 给了按钮就是骗点击。
 * 2. **兑换成功只认内核的 `ok`**（内核只认上游 `success`）：HTTP 200 不是判据
 *    （上游失败也回 200，只是 `success:false` 且有 `message`）。
 * 3. **积分是回读的当前值**（`pointsAfter`），不是「本次到账多少」：上游回执里
 *    没有增量，界面只能写「当前积分」，写「到账 N」就是把推导当事实。
 */
object QuworkOps {

    class OpResult(
        val ok: Boolean,
        val message: String = "",
        val data: JSONObject? = null,
    )

    /** 一条月度赠送（字段名对着上游 `gift_status` / `expected_gift_date` 那套）。 */
    data class Gift(
        val period: String,
        val status: String,
        /** 应到账时刻（毫秒，0 表示上游没给） */
        val expectedAt: Long,
        /** 实际到账时刻（毫秒，0 表示上游没给） */
        val actualAt: Long,
        /** 其余标量字段（界面原样展示，上游加字段这里自动跟上） */
        val extra: List<Pair<String, String>>,
    )

    data class Benefits(
        val items: List<Gift>,
        /** 上游读失败：界面写「暂不可用」，不写「没有赠送」 */
        val unavailable: Boolean,
        val error: String?,
    )

    fun configure(apiKey: String) {
        CoreTransport.configure(apiKey)
    }

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
                    OpResult(data?.optBoolean("ok") == true, data?.optString("message").orEmpty(), data)
                } else {
                    val error = obj.optJSONObject("error")?.optString("message").orEmpty()
                        .ifBlank { obj.optString("error") }
                    OpResult(false, error.ifBlank { "操作失败（HTTP ${reply.status}）" }, data)
                }
            }
        }
    }

    /** 月度赠送。读失败也返回 Benefits（`unavailable = true`），由界面如实写。 */
    suspend fun benefits(accountId: String): Benefits? {
        val data = getData("/api/accounts/$accountId/quwork/benefits") ?: return null
        val arr = data.optJSONArray("items") ?: JSONArray()
        val items = (0 until arr.length()).mapNotNull { index ->
            arr.optJSONObject(index)?.let { row ->
                val extra = mutableListOf<Pair<String, String>>()
                row.optJSONArray("extra")?.let { list ->
                    for (i in 0 until list.length()) {
                        val pair = list.optJSONObject(i) ?: continue
                        val key = pair.optString("key")
                        if (key.isBlank()) continue
                        extra += key to pair.optString("value")
                    }
                }
                Gift(
                    period = row.optString("period"),
                    status = row.optString("status"),
                    expectedAt = row.optLong("expectedAt"),
                    actualAt = row.optLong("actualAt"),
                    extra = extra,
                )
            }
        }
        return Benefits(
            items = items,
            unavailable = !data.optBoolean("ok", true),
            error = data.optString("error").ifBlank { null },
        )
    }

    /**
     * 兑换一次。`orderId` 与 `inviteCode` **只能给一个**（内核也这么判：
     * 两个都填就是 400 —— 不替用户猜该兑哪个）。
     */
    suspend fun redeem(accountId: String, orderId: String? = null, inviteCode: String? = null): OpResult {
        val body = JSONObject()
        val order = orderId?.trim().orEmpty()
        val invite = inviteCode?.trim().orEmpty()
        if (order.isNotEmpty()) body.put("orderId", order)
        if (invite.isNotEmpty()) body.put("inviteCode", invite)
        if (body.length() == 0) return OpResult(false, "请填写订单号（16 位数字）或邀请码")
        return post("/api/accounts/$accountId/quwork/redeem", body)
    }
}
