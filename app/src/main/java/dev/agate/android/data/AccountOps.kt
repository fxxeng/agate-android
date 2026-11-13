package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import org.json.JSONObject
import java.util.concurrent.TimeUnit

/**
 * 发码被风控拦下时的挑战信息（内核 `tabbit::login::challenge_of`）。
 *
 * [verifyUrl] 已由内核限定为 https 的 `*.meituan.com` 域（`is_meituan_https`），
 * 客户端**不再判第二次** —— 地址校验只有一处，界面只负责把它打开。
 * [requestCode] 只用于排障展示，重发靠的是同一轮的 [AccountOps.smsSend] ticket。
 */
data class SmsCaptcha(val message: String, val verifyUrl: String, val requestCode: String)

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

    /**
     * provider → 短信端点前缀（**唯一一份**：smsSend / smsVerify 共用）。
     *
     * ── 为什么必须是表 ──────────────────────────────────────────
     * 原来这两个方法各写一套 `if (providerId == "loomy")`，加第三家时只要漏改
     * 一处，就会出现「发码打到 A 家、登录打到 B 家」这种最难排查的错 ——
     * 而界面因为文案取自卡片 label，还会显示「已添加」。
     *
     * 表里没有的家落 AutoClaw 那一对端点：这是历史口径（老客户端不带
     * `provider`，而那些用户本来就在用 AutoClaw 国内版）。
     *
     * Tabbit 的国际站与国内站共用一对端点：拒绝的判定在内核
     * （上游对国际站灰度 403），客户端不做第二份判断。
     */
    private fun smsEndpoint(providerId: String): String = when (providerId) {
        "loomy" -> "/api/session/login/loomy/sms"
        "tabbit", "tabbit-intl" -> "/api/session/login/tabbit/sms"
        else -> "/api/session/login/sms"
    }

    /**
     * 发送验证码。loomy 返回的 msgid 由调用方保存、verify 时回传。
     *
     * [ticket] 是**上一轮**发码的中间态（Tabbit 那里就是 uuid）。过完滑块重发时
     * 必须原样带回：换了 uuid 上游就找不到刚通过的风控结论，滑块等于白过。
     * 内核 `login_tabbit_sms_send` 走 `ticket_of` 统一认这个字段，这里不需要
     * 再按 provider 分叉（三家都忽略多余字段）。
     */
    suspend fun smsSend(providerId: String, phone: String, ticket: String? = null): OpResult {
        val body = JSONObject().put("phone", phone).put("provider", providerId)
        if (!ticket.isNullOrBlank()) body.put("ticket", ticket)
        return post(smsEndpoint(providerId) + "/send", body)
    }

    /**
     * 从发码响应里读风控挑战：只有 `status:captcha` 且给了能打开的验证页才算。
     *
     * 字段一律走 `jsonTextOrNull`：真机 libcore 的 `optString` 会把 JSON null
     * 读成字面量 `"null"`，拿它当文案会显示 "null"、当地址会打开一个坏链接；
     * 单测用的 org.json 返回 default，测不出这个差异（路由队列踩过同一个坑）。
     * 返回 null = 这是一次**正常**发码，界面照旧走验证码流程。
     */
    fun smsCaptcha(data: JSONObject?): SmsCaptcha? {
        if (data == null || jsonTextOrNull(data, "status") != "captcha") return null
        val captcha = data.optJSONObject("captcha") ?: return null
        val verifyUrl = jsonTextOrNull(captcha, "verifyUrl") ?: return null
        val message = jsonTextOrNull(captcha, "message") ?: "需要进行人机验证"
        val requestCode = jsonTextOrNull(captcha, "requestCode") ?: ""
        return SmsCaptcha(message, verifyUrl, requestCode)
    }

    /** 用手机号 + 验证码登录并直接落成账号 → {account, list}。 */
    /**
     * 用手机号 + 验证码登录并直接落成账号 → {account, list}。
     *
     * [ticket] 是**发码响应里的统一中间态字段**（内核三家的原名分别是
     * `deviceId` / `msgid` / `uuid`，API 层统一补出一个 `ticket`，
     * 见内核 `api::session::with_ticket`）。客户端只认这一个名字 ——
     * 各认各的就得在这里再分一次叉。
     */
    suspend fun smsVerify(
        providerId: String,
        phone: String,
        code: String,
        ticket: String?,
        name: String?,
    ): OpResult {
        val body = JSONObject()
            .put("phone", phone)
            .put("code", code)
            .put("provider", providerId)
        if (!name.isNullOrBlank()) body.put("name", name)
        if (!ticket.isNullOrBlank()) body.put("ticket", ticket)
        return post(smsEndpoint(providerId) + "/verify", body)
    }

    /**
     * 粘贴凭证添加（统一入口 POST /api/accounts，按家分派凭证字段）：
     * - kuku：cookie 整串（BDUSS/STOKEN…），上游校验并补全昵称
     * - commandcode：apiKey（user_ 前缀）
     * - antigravity：refreshToken（email 可选）
     * - chatglm：accessToken（refreshToken 可选）；**两者都空 = 访客**
     *
     * ── chatglm 这条为什么以前完全用不了 ────────────────────────
     * 注册表早就把 chatglm 的通道写成「粘贴凭证」，但这里没有对应分支，
     * 于是任何输入都落到末尾的 `else` → 「该提供商不支持粘贴凭证添加」。
     * 内核那边**本来就是好的**（`api::accounts` 的 Chatglm 分支认
     * accessToken / refreshToken / guest，访客还能现换一份凭证），断的只是
     * 客户端这一跳。
     *
     * ── 访客为什么显式带 `guest` ────────────────────────────────
     * 内核的缺省规则是「没有令牌就当访客」，但**显式传 `guest: true`** 才能让
     * 后续维护者一眼看出这次是有意的免登录添加；也避免将来内核把缺省值改成
     * false 时这里静默失效（那会让「添加访客」变成 400「请提供访问令牌」）。
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
            // 豆包工作：粘贴 Cookie 整串（内核 `credentials::normalize_paste`
            // 负责解成 Cookie 对并校验登录态；这里原样送出去，不在客户端
            // 做半套解析 —— 两处各解析一次迟早会分叉）。
            "doubaowork" -> body.put("cookie", primary.trim())
            // WPS 灵犀：整段 Cookie 或裸 wps_sid 都放进 `text`，由内核
            // `lingxi::credentials::normalize_paste` 判三形态（客户端不做半套解析）。
            "lingxi" -> body.put("text", primary.trim())
            "commandcode" -> body.put("apiKey", primary.trim())
            // DeepSeek 网页版：粘贴 userToken（不是 Cookie，也不是 sk- 平台 Key
            // —— 那种是另一套接口）。内核 `login::verify_token` 会先做一次
            // 归一化（剥引号 / `userToken:` 整行 / localStorage JSON 的 .value）
            // 再 best-effort 探活，所以这里原样送出即可。
            "deepseek-web" -> body.put("accessToken", primary.trim())
            "antigravity" -> {
                body.put("refreshToken", primary.trim())
                if (!secondary.isNullOrBlank()) body.put("email", secondary.trim())
            }
            "chatglm" -> {
                // 用户可能整段粘了 `chatglm_token=xxx;`（内核也会剥一次，
                // 两边都剥是为了让「回显给用户的字段」与「真正发出去的值」
                // 在日志里长得一样）
                val token = primary.trim()
                    .removePrefix("chatglm_token=")
                    .trimEnd(';')
                    .trim()
                if (token.isEmpty()) {
                    body.put("guest", true)
                } else {
                    body.put("accessToken", token)
                    if (!secondary.isNullOrBlank()) body.put("refreshToken", secondary.trim())
                }
            }
            // ModelScope（魔搭）两家：只有一枚 `ms-` 令牌（既调推理又读身份 /
            // 魔粒 / 签到），所以没有第二个字段可填 —— 表单里的 secondary 对
            // 这两家是空的（`pasteSecondaryHint` 未声明）。
            //
            // 内核添加时会**出网探活一次**（顺便就是当日签到），拿上游的
            // HavanaId 做账号 id；令牌无效或**站点点错**（国内令牌打国际站）
            // 都会在这一步 400，所以错误文案要原样带回给用户。
            "modelscope", "modelscope-intl" -> body.put("accessToken", primary.trim())
            // LobsterAI（有道龙虾）：**没有粘贴通道** —— 内核对该家的粘贴入口
            // 始终回 400（续期要的 uuid / keyfrom 只能从换码拿到，见注册表里
            // 它自己那条注释）。走到这里说明界面漏了拦截，如实转成一句可读的话，
            // 而不是把空体发出去换一个内核的原始 400。
            "lobsterai" -> return OpResult(
                false,
                null,
                "LobsterAI 只能通过网页登录添加（点卡片里的登录按钮）",
            )
            // AtomCode（AtomGit）：accessToken 必填、refreshToken 可选（有它才能
            // 自动续期）。内核 `api::accounts` 的 Atomcode 分支把这份 JSON 交给
            // `atomcode::normalize_paste` 归一（驼峰 / 下划线两种写法都认），
            // 缺 accessToken 时回 400 并说明该粘什么。
            "atomcode" -> {
                body.put("accessToken", primary.trim())
                if (!secondary.isNullOrBlank()) body.put("refreshToken", secondary.trim())
            }
            // ── 第 4 批四家 ────────────────────────────────────────────
            //
            // Tabbit 两家：**故意塞 `accessToken` 而不是 `cookie`**。
            // 内核 `tabbit::credentials::normalize_payload` 只在「这段文本看着像
            // Cookie 串」时（`looks_like_cookie_paste`：含 `token=` / `cookie:` /
            // ` -b `）才拆包回填；塞进 `cookie` 字段的**裸 JWT** 两条规则都不命中，
            // 最后 `from_fields` 读到的是一个没有 `token=` 的串 → 400「缺少登录令牌」。
            // 塞进 `accessToken` 则三种粘法都成立：
            //   Cookie 串 / cURL 整段 → 按内容拆出 token + cookie（覆盖原文）
            //   裸 JWT                → 原样留着，由 `from_fields` 合成 Cookie
            // 客户端因此**不需要**自己判断用户粘的是哪种 —— 判据只在内核一份。
            "tabbit", "tabbit-intl" -> body.put("accessToken", primary.trim())
            // Kiro：一段 textarea 收「裸 API key（ksk_…）」与「整段
            // kiro-auth-token.json」两种，`kiro::credentials::normalize_paste`
            // 先搬令牌、再解字符串里的 JSON，两种都认。
            "kiro" -> body.put("text", primary.trim())
            // AStudio：同上。`astudio::credentials::normalize_paste` 认裸
            // `modelBearerToken`、带 `Bearer ` 外壳的整行、整段 `astron-session.json`、
            // 以及动态登录的 `{inputs:{…}}` 包装 —— 按内容判，所以这里只递原文。
            "astudio" -> body.put("text", primary.trim())
            // QuWork：primary 是整段凭证（JSON / 纯 llmToken / yaml 行），
            // secondary 是可选的登录凭证 accessToken。内核按内容判，不在这里解析。
            "quwork" -> {
                body.put("text", primary.trim())
                if (!secondary.isNullOrBlank()) body.put("accessToken", secondary.trim())
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
     * `POST /api/accounts/{id}/rate-limits/clear` —— 清除账号的限流标记。
     *
     * [model] 非空只清那一个模型，null / 空 = 全清。语义只是「让本机立刻
     * 重新尝试」：标记是本机从上游 429 推断的冷却期，清掉后下一次请求若
     * 仍被限流会再次被标记 —— 内核注释明确这个动作安全、无需二次确认。
     */
    suspend fun clearRateLimits(id: String, model: String? = null): OpResult {
        val body = JSONObject()
        if (!model.isNullOrBlank()) body.put("model", model)
        return post(
            "/api/accounts/" + java.net.URLEncoder.encode(id, "UTF-8") + "/rate-limits/clear",
            body,
        )
    }

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
