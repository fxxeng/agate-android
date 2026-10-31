package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
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



    @Volatile private var apiKey: String = ""

    fun configure(apiKey: String) {
        this.apiKey = apiKey
        CoreTransport.configure(apiKey)
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

    /**
     * GET /api/accounts/usage[?id=] → `{results:[{id,name,usage,error,code?}], skipped}`。
     *
     * ── 为什么要 [force] ────────────────────────────────────────
     * 这个接口会真的打到各家上游，官方对余额类接口有频次风控。页面原本「进入即查」，
     * 来回翻几页就是连串请求。现在默认走缓存（见 [UsageCache]）：
     * - `force = false`：新鲜缓存直接用；有过期缓存也先给旧的（不让上游背锅）；
     *   只有**从没查过**才真的发一次请求；
     * - `force = true`：用户显式点「查询余额」时才用，且受 20 秒硬节流保护。
     *
     * 返回 null 只表示「既没有缓存、请求也失败」。
     */
    suspend fun usage(id: String? = null, force: Boolean = false): JSONObject? {
        if (!force) {
            val cached = UsageCache.cached(id)
            if (cached != null && UsageCache.isFresh(id)) return cached
            // 有过期数据：先用旧的，不打扰上游（下次点按钮才刷）
            if (cached != null) return cached
        } else if (UsageCache.throttleRemaining(id) > 0) {
            // 连点保护：拿缓存顶上，避免又发一轮
            UsageCache.cached(id)?.let { return it }
        }
        val q = if (id.isNullOrBlank()) "" else "?id=" + java.net.URLEncoder.encode(id, "UTF-8")
        val body = get("/api/accounts/usage$q") ?: return UsageCache.cached(id)
        UsageCache.put(id, body)
        return body
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

    /**
     * `POST /api/accounts/reorder` —— 整队重排（拖拽用）。
     *
     * ── 为什么不用上面那条 `/move` 逐次相邻交换 ───────────────────
     * `/move` 一次只换相邻两行，而拖拽一次可能跨好几格 —— 连打 N 次要 N 次落盘，
     * 中途失败就留下改了一半的顺序。内核这条按给定队列把全局 `priority` 一次编成
     * 0..n-1（`store_order::reorder_accounts`），一次请求、一次落盘。
     *
     * ⚠️ `queue` 必须是**全部账号的一个排列**（内核校验），不能只交某一家 ——
     * 队列语义是「全局一条按优先级的队列」，分组只是展示口径。所以调用方要拿
     * `/api/session` 里的全量账号列表去拼。
     */
    suspend fun reorder(queue: List<String>): OpResult {
        val arr = org.json.JSONArray()
        queue.forEach { arr.put(it) }
        return post("/api/accounts/reorder", JSONObject().put("queue", arr))
    }

    /** POST /api/accounts/refresh {id}：刷新单个账号 token（不支持的家会明确报错）。 */
    suspend fun refreshAccount(id: String): OpResult =
        post("/api/accounts/refresh", JSONObject().put("id", id))

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

    // P2：经 CoreTransport 内核直调（不经端口，Core 态可用）
    private suspend fun get(path: String): JSONObject? {
        val r = CoreTransport.get(path)
        val ok = r as? CoreTransport.Reply.Ok ?: return null
        if (ok.status < 200 || ok.status >= 300) return null
        val obj = try {
            JSONObject(ok.text)
        } catch (_: Exception) {
            return null
        }
        return if (obj.optBoolean("success")) obj.optJSONObject("data") else null
    }

    // P2：经 CoreTransport 内核直调
    private suspend fun patch(path: String, body: JSONObject): OpResult {
        val r = CoreTransport.call("PATCH", path, body.toString())
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
