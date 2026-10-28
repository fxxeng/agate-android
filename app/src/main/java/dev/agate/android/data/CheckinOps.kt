package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/**
 * 签到中心的**动作**层（签到中心 / 定时签到 两页的写操作与按需查询）。
 *
 * ── 端点事实（逐条对着内核核过，见 docs/任务中心-上游3.0.8签到中心深挖.md）──
 * | 动作 | 端点 |
 * |---|---|
 * | 单账号签到 | `POST /api/accounts/checkin` body `{id}` —— **不是** `/{id}/checkin` |
 * | 批量签到 | `POST /api/auto-checkin/run`（范围由定时条目配置决定） |
 * | 国际版三档 | `POST /api/checkin-activity` body `{id, mode}`，mode = keepalive/claim/full |
 * | 一次收藏 | `GET /api/accounts/{id}/onboarding?refresh=1`，`POST …/onboarding/claim` |
 * | 套餐探测/领取 | `POST …/zcode-claim/preview`，`POST …/zcode-claim` |
 * | 成长任务 | `GET …/growth`，`POST …/growth/task` / `…/growth/school` / `…/growth/travel` |
 * | 条目 CRUD | `POST /api/checkin-schedules`（按 id upsert）、`POST …/{id}/delete`、`POST …/{id}/run` |
 * | 验证码池 | `GET /api/zcode/captcha`、`GET/PUT /api/zcode/captcha-pool` |
 *
 * ── 两个容易踩的坑 ──────────────────────────────────────────
 * 1. **国际版不要按 `edition` 字段判要不要调 `/checkin-activity`** ——
 *    Qoder 国际版也带 `edition:"intl"`，把它派去 WorkBuddy 日活接口会稳定
 *    400「该账号不是 WorkBuddy 国际版账号」。判据是**分组 id**（页面按
 *    `group.id == "workbuddy-intl"` 给三颗按钮），不是账号的 edition。
 * 2. **`/zcode-claim` 的失败也是 HTTP 200**：`failure` 字段才是业务结论
 *    （already_claimed / quota_exhausted / captcha…）。按 HTTP 状态判会把
 *    「今天已经领过」当成错误弹一堆红字。
 */
object CheckinOps {

    class OpResult(
        val ok: Boolean,
        val data: JSONObject? = null,
        val error: String? = null,
    )

    fun configure(apiKey: String) {
        CoreTransport.configure(apiKey)
    }

    // ── 读：一律走内核直调，`{success, data}` 信封 ──────────────

    /** GET 一条管理端点，返回 `data` 对象；失败（链路 / 非 2xx / success=false）给 null。 */
    suspend fun getData(path: String): JSONObject? = withContext(Dispatchers.IO) {
        val reply = CoreTransport.get(path) as? CoreTransport.Reply.Ok ?: return@withContext null
        if (reply.status < 200 || reply.status >= 300) return@withContext null
        val obj = runCatching { JSONObject(reply.text) }.getOrNull() ?: return@withContext null
        if (!obj.optBoolean("success")) null else obj.optJSONObject("data")
    }

    /** POST 一条管理端点（含 DELETE 以外的一切写动作）。 */
    suspend fun post(path: String, body: JSONObject): OpResult = withContext(Dispatchers.IO) {
        val reply = CoreTransport.post(path, body.toString())
        when (reply) {
            is CoreTransport.Reply.Down -> OpResult(false, null, reply.message)
            is CoreTransport.Reply.Ok -> {
                val obj = runCatching { JSONObject(reply.text) }.getOrNull()
                    ?: return@withContext OpResult(false, null, "内核响应异常")
                if (obj.optBoolean("success")) {
                    OpResult(true, obj.optJSONObject("data"))
                } else {
                    // 非管理信封（OpenAI 风格的 `{error:{message}}`）也读得出来
                    val message = obj.optString("error").ifBlank {
                        obj.optJSONObject("error")?.optString("message").orEmpty()
                    }
                    OpResult(false, obj.optJSONObject("data"), message.ifBlank { "操作失败（HTTP ${reply.status}）" })
                }
            }
        }
    }

    // ── 签到 ────────────────────────────────────────────────────

    /** 单账号签到（id 在 body 里）。 */
    suspend fun signAccount(id: String): OpResult =
        post("/api/accounts/checkin", JSONObject().put("id", id))

    /** 批量签到：范围由定时条目配置决定，id 不传 = 走批量路径。 */
    suspend fun signAll(): OpResult = post("/api/auto-checkin/run", JSONObject())

    /**
     * WorkBuddy 国际版日活三档。
     * - `keepalive` 只维持活跃、不领取；
     * - `claim` 只探测活动并领奖励；
     * - `full` 两步都做。
     *
     * 一颗按钮混做两件事，用户看不出「积分为什么没涨」—— 所以是三颗。
     */
    suspend fun runActivity(id: String, mode: String): OpResult =
        post("/api/checkin-activity", JSONObject().put("id", id).put("mode", mode))

    // ── 一次性福利 / 套餐 / 成长任务 ────────────────────────────

    /** 新手任务状态。`refresh = true` 是唯一「重新问上游」的入口（覆盖结算记忆）。 */
    suspend fun onboardingStatus(id: String, refresh: Boolean): JSONObject? =
        getData("/api/accounts/$id/onboarding" + if (refresh) "?refresh=1" else "")

    suspend fun onboardingClaim(id: String): OpResult =
        post("/api/accounts/$id/onboarding/claim", JSONObject())

    /** ZCode 套餐探测（只读、不要验证码）。 */
    suspend fun planPreview(id: String): OpResult =
        post("/api/accounts/$id/zcode-claim/preview", JSONObject())

    /**
     * ZCode 套餐领取。`planId` 空 = 领优先级最高的那份（内核自己挑）。
     * 验证码参数留空：本版不做过码（见设计预览 §⑥），空值内核放行，
     * 上游真要码时会回 `captcha` 失败，界面如实显示原因。
     */
    suspend fun planClaim(id: String, planId: String = ""): OpResult =
        post("/api/accounts/$id/zcode-claim", JSONObject().put("planId", planId))

    suspend fun growthStatus(id: String): JSONObject? = getData("/api/accounts/$id/growth")

    /** 执行单个成长任务（内核只暴露单任务端点，见 api::growth 的模块头）。 */
    suspend fun growthTask(id: String, code: String, mode: String = "full"): OpResult =
        post("/api/accounts/$id/growth/task", JSONObject().put("code", code).put("mode", mode))

    suspend fun growthSchool(id: String, lottery: Boolean): OpResult =
        post("/api/accounts/$id/growth/school", JSONObject().put("lottery", lottery))

    suspend fun growthTravel(id: String): OpResult =
        post("/api/accounts/$id/growth/travel", JSONObject())

    // ── 定时条目 CRUD ───────────────────────────────────────────

    /**
     * 保存一条条目（按 id upsert；id 为空 = 新增，后端分配）。
     * `providers` 空数组 = 全部提供商（后端的「不限范围」口径）。
     */
    suspend fun saveSchedule(
        id: String?,
        label: String,
        enabled: Boolean,
        time: String,
        providers: List<String>,
    ): OpResult {
        val body = JSONObject()
            .put("label", label)
            .put("enabled", enabled)
            .put("time", time)
            .put("providers", JSONArray(providers))
        if (!id.isNullOrBlank()) body.put("id", id)
        return post("/api/checkin-schedules", body)
    }

    suspend fun deleteSchedule(id: String): OpResult =
        post("/api/checkin-schedules/$id/delete", JSONObject())

    suspend fun runSchedule(id: String): OpResult =
        post("/api/checkin-schedules/$id/run", JSONObject())

    /** 总开关（老接口 `/api/auto-checkin`，与条目 CRUD 是两组）。 */
    suspend fun setAutoEnabled(enabled: Boolean): OpResult =
        post("/api/auto-checkin", JSONObject().put("enabled", enabled))

    /** 保活链（空清单保存 = 恢复缺省链）。 */
    suspend fun saveKeepalive(models: List<String>): OpResult =
        post("/api/checkin-keepalive", JSONObject().put("models", JSONArray(models)))

    // ── 验证码池 ────────────────────────────────────────────────

    /**
     * 池子概况。字段见 `api::zcode_captcha::stats_json`：
     * `ready / required / target / startPlanAccounts / needsTokens / pollMs /
     * maxPerRound / minted / consumed / rejected / stale / ttlMs / lastSyncAt`。
     *
     * `needsTokens` 是「界面此刻该不该铸造」的唯一判据（有账号走这条路 +
     * 库存不足目标 + 上游确实要令牌）。本版**不铸造**，只把它显示出来 ——
     * 铸造由后台守卫在桌面侧做，手机上白铸是烧阿里云风控配额。
     */
    suspend fun captchaStats(): JSONObject? = getData("/api/zcode/captcha")

    /** 补货节奏（`target` / `pollMs` / `maxPerRound`；允许部分字段）。 */
    suspend fun captchaPool(): JSONObject? = getData("/api/zcode/captcha-pool")

    suspend fun saveCaptchaPool(target: Int, pollMs: Int, maxPerRound: Int): OpResult =
        post(
            "/api/zcode/captcha-pool",
            JSONObject()
                .put("zcodeCaptchaPoolTarget", target)
                .put("zcodeCaptchaPoolPollMs", pollMs)
                .put("zcodeCaptchaPoolMaxPerRound", maxPerRound),
        )
}
