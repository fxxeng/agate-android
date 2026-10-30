package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import org.json.JSONArray
import org.json.JSONObject
import java.util.concurrent.TimeUnit

/**
 * 自定义 / 中转提供商（内核 `api/custom_providers.rs`）：
 * - GET  /api/custom-providers                    → {providers:[...]}
 * - POST /api/custom-providers                     → 新建（同时创建该家第一个账号）
 * - POST /api/custom-providers/update              {id, name?, protocol?, baseUrl?, enabled?}
 * - POST /api/custom-providers/remove              {id}  级联删名下账号
 * - POST /api/custom-providers/fetch-models        {providerId} 服务端代理拉模型
 * - POST /api/custom-providers/models              {providerId, models, mappings}
 *
 * 创建体字段（core::custom_providers::create 校验）：name 必填、protocol 归一、
 * baseUrl 合法；首个账号的凭证键随家（OpenAI 兼容用 apiKey）。
 */
object CustomProviderOps {



    @Volatile private var apiKey: String = ""

    fun configure(apiKey: String) {
        this.apiKey = apiKey
        CoreTransport.configure(apiKey)
    }

    class OpResult(val ok: Boolean, val data: JSONObject?, val error: String?)

    /**
     * `GET /api/custom-providers` 里 `providers[]` 的一条。
     *
     * 界面需要它的原因只有一个：**自定义提供商的展示名不在内置注册表里**。
     * 账号列表、模型页、请求日志拿到的都是 `custom-xxxx` 这种 id，
     * 不拿这份记录就只能把 id 原样显示（上一版就是这样）。
     * 上游 3.0.8 把同一件事做在 `wbProviders.labelOf` 的查找链里 ——
     * 一处合并、处处生效，本仓照这个口径。
     */
    class Record(
        val id: String,
        val name: String,
        val protocol: Protocol,
        val baseUrl: String,
        val enabled: Boolean,
        /** 余额查询方案（空串 = 不查余额；取值见 [balanceSchemes]）。 */
        val balanceScheme: String,
        /** 客户端形态伪装（空串 = 关；目前只有 `opencode` 一种）。 */
        val clientEmulation: String,
        val urlSuffix: String,
        /** 额外静态头（只用于展示「有几个」；编辑走元数据继承，不在界面改）。 */
        val headerCount: Int,
        /** 已登记的模型条数（含被关掉的）。「拉取模型」弹层用它显示「已登记 N 个」。 */
        val modelCount: Int,
    ) {
        /**
         * 展示用的名字：[name] 为空时回落到 id。
         *
         * ── 为什么要有这个回落 ────────────────────────────────────
         * [name] **保留原值**（不回落），是为了让「没取到名字」与「名字就叫
         * id」在数据层可区分 —— 而界面上总要显示点什么，空串不行。
         *
         * 回落放在这里（数据层的**唯一**出口）而不是散在各个界面里：这样
         * 「名字取不到时显示什么」只有一种说法。
         *
         * 注意：更彻底的兜底（账号的备注名）在 [ProviderPage] 那一层做 ——
         * 那里才拿得到账号列表；这里只能退回 id。
         */
        val displayName: String get() = name.ifBlank { id }

        companion object {
            fun of(o: JSONObject?): Record? {
                val obj = o ?: return null
                val id = obj.optString("id")
                if (id.isBlank()) return null
                return Record(
                    id = id,
                    // ⚠️ 名称**保留原值**，不在这里回落 id（2026-10-30 修）。
                    //
                    // 旧写法 `ifBlank { id }` 让「没取到名字」与「名字就叫 id」
                    // 这两件事在数据层无法区分 —— 而它们在界面上的后果完全不同：
                    // 前者是**故障**（该显示 id 之外的、真正的名字，或明确提示
                    // 取不到），后者是**事实**（用户真的把家命名为 custom-xxx）。
                    //
                    // 回落改由展示层做（`ProviderPage` 的 `labelOf`）：那里能
                    // 拿到别的来源做兜底（账号的展示名），数据层拿不到。
                    name = obj.optString("name").trim(),
                    protocol = Protocol.of(obj.optString("protocol")),
                    baseUrl = obj.optString("baseUrl"),
                    enabled = obj.optBoolean("enabled", true),
                    balanceScheme = obj.optString("balanceScheme"),
                    clientEmulation = obj.optString("clientEmulation"),
                    urlSuffix = obj.optString("urlSuffix"),
                    headerCount = obj.optJSONObject("headers")?.length() ?: 0,
                    modelCount = obj.optJSONArray("models")?.length() ?: 0,
                )
            }

            /** 解析 `{providers:[…]}` 信封；结构不对的条目跳过（不猜）。 */
            fun listOf(data: JSONObject?): List<Record> {
                val arr = data?.optJSONArray("providers") ?: return emptyList()
                val out = ArrayList<Record>(arr.length())
                for (i in 0 until arr.length()) {
                    of(arr.optJSONObject(i))?.let { out += it }
                }
                return out
            }
        }
    }

    /** 是不是自定义提供商的 id（与内核 `custom_providers::ID_PREFIX` 一致）。 */
    fun isCustomProviderId(id: String?): Boolean = id != null && id.startsWith("custom-")

    /**
     * 协议取值 —— **必须与内核 `custom_providers::PROTOCOLS` 逐字一致**
     * （`chat_completions` / `responses` / `anthropic`）。
     *
     * ⚠️ 这里曾经是 `OPENAI("openai", …)`，而内核只认 `chat_completions`：
     * 两个表单的默认选项都是 OpenAI 兼容，于是**默认路径必然 400**
     * （「协议必须是以下之一: chat_completions / responses / anthropic」）。
     * 用户看到的就是「中转 / 自定义创建失败」，也就是「这两个功能没实现」。
     * 上游 3.0.8 的 `PROTOCOL_OPTIONS` 用的也是 `chat_completions`。
     */
    enum class Protocol(val wire: String, val label: String) {
        OPENAI("chat_completions", "OpenAI - Chat Completions"),
        RESPONSES("responses", "OpenAI - Responses"),
        ANTHROPIC("anthropic", "Anthropic - Messages"),
        ;

        companion object {
            fun of(wire: String?): Protocol =
                entries.firstOrNull { it.wire == wire } ?: OPENAI
        }
    }

    suspend fun list(): JSONObject? = get("/api/custom-providers")

    /**
     * 自定义家列表的**带状态**读法：区分「确实没有自定义家」与「这次没拉到」。
     *
     * ── 为什么必须区分 ──────────────────────────────────────────
     * 旧接口 `list()` 失败时返回 `null`，而 `Record.listOf(null)` 给的是**空表**
     * —— 两种情况在界面上**一模一样**：名字表是空的 → 每个自定义家都拿不到名字
     * → 展示名回落成 `custom-xxxx` → 图标也按名反查不到 → 两字母色块。
     *
     * 而「没拉到」是**临时故障**（内核还没就绪 / 令牌不对 / 请求超时），
     * 「确实没有」是**事实**。把前者当成后者，用户看到的就是「我建的家全变成
     * 一串乱码了」，而界面上没有任何一处说明发生过什么。
     *
     * 现在把失败**如实带出来**（`ok=false` + 原因），让调用方能重试、能提示，
     * 也能在拿不到名字时**不显示 id 冒充名字**。
     */
    suspend fun listResult(): ListResult {
        val reply = CoreTransport.get("/api/custom-providers")
        if (reply !is CoreTransport.Reply.Ok) {
            val reason = (reply as? CoreTransport.Reply.Down)?.message ?: "链路错误"
            return ListResult(ok = false, records = emptyList(), error = reason)
        }
        if (reply.status < 200 || reply.status >= 300) {
            return ListResult(ok = false, records = emptyList(), error = "HTTP ${reply.status}")
        }
        val obj = try {
            JSONObject(reply.text)
        } catch (_: Exception) {
            return ListResult(ok = false, records = emptyList(), error = "内核响应异常")
        }
        if (!obj.optBoolean("success")) {
            val reason = obj.optString("error").ifBlank { "读取失败" }
            return ListResult(ok = false, records = emptyList(), error = reason)
        }
        val data = obj.optJSONObject("data")
            ?: return ListResult(ok = false, records = emptyList(), error = "响应格式异常")
        return ListResult(ok = true, records = Record.listOf(data), error = null)
    }

    /**
     * [listResult] 的结果。
     *
     * `ok=true` 且 `records` 为空 = **确实没有**自定义家（事实）；
     * `ok=false` = 这次没拉到（临时故障，`error` 是原因）。
     */
    data class ListResult(
        val ok: Boolean,
        val records: List<Record>,
        /** `ok=false` 时的原因（用户可读）；`ok=true` 时为 null。 */
        val error: String?,
    )

    /**
     * 新建自定义提供商 + 首个账号（一次事务，失败内核回滚）。
     *
     * ── 为什么带这么多可选参数 ──────────────────────────────────
     * 上游 3.0.8 的预置 API 卡片在创建时会把「这一家是谁」的派生事实一起写进
     * 提供商记录：`quirks`（urlSuffix / headers / anthropicToolType）、
     * `clientEmulation`（OpenCode 免费档的客户端形态伪装）、`balanceScheme`
     * （余额查询走哪条链路）。这些都不是用户在表单里选的，而是点哪张卡就带哪份
     * —— 手动新建时全为空，与上游「预置只是预填模板」的口径一致。
     *
     * `balanceQuery` 是该方案要补的凭证（NewAPI 的个人令牌、商汤的账号密码…），
     * 由 [PresetApiRegistry] 声明字段、表单收集后原样提交，**只带非空键**。
     */
    suspend fun create(
        name: String,
        baseUrl: String,
        apiKeyValue: String,
        protocol: Protocol,
        noAuth: Boolean = false,
        balanceScheme: String? = null,
        urlSuffix: String? = null,
        headers: Map<String, String>? = null,
        anthropicToolType: String? = null,
        clientEmulation: String? = null,
        balanceQuery: Map<String, String>? = null,
        accountName: String? = null,
    ): OpResult {
        val body = JSONObject()
            .put("name", name.trim())
            .put("baseUrl", baseUrl.trim())
            .put("protocol", protocol.wire)
        if (noAuth) {
            // 与 apiKey 互斥：勾了「无需鉴权」就不带 key（内核也按这个口径归一）
            body.put("noAuth", true)
        } else {
            body.put("apiKey", apiKeyValue.trim().removePrefix("Bearer ").trim())
        }
        balanceScheme?.takeIf { it.isNotBlank() }?.let { body.put("balanceScheme", it) }
        urlSuffix?.takeIf { it.isNotBlank() }?.let { body.put("urlSuffix", it) }
        headers?.takeIf { it.isNotEmpty() }?.let { map ->
            body.put("headers", JSONObject().apply { map.forEach { (k, v) -> put(k, v) } })
        }
        anthropicToolType?.takeIf { it.isNotBlank() }?.let { body.put("anthropicToolType", it) }
        clientEmulation?.takeIf { it.isNotBlank() }?.let { body.put("clientEmulation", it) }
        balanceQuery?.takeIf { it.isNotEmpty() }?.let { map ->
            body.put("balanceQuery", JSONObject().apply { map.forEach { (k, v) -> put(k, v) } })
        }
        return post("/api/custom-providers", body)
    }

    /**
     * 往**已有**自定义提供商再加一个账号（`POST /api/accounts` 的 custom 分支）。
     *
     * 与 [create] 的区别只在「提供商已经存在」：不新建记录，只追加账号。上游
     * 3.0.8 把这件事做成预置卡片的第二种语义（同一张卡再点一次 = 这家接着加
     * 一把 key），因此这条路径必须存在 —— 否则用户想加第二把 key 只能重建一家。
     */
    suspend fun addAccount(
        providerId: String,
        apiKeyValue: String,
        noAuth: Boolean = false,
        accountName: String? = null,
        balanceQuery: Map<String, String>? = null,
    ): OpResult {
        val body = JSONObject().put("provider", providerId)
        if (noAuth) body.put("noAuth", true)
        else body.put("apiKey", apiKeyValue.trim().removePrefix("Bearer ").trim())
        accountName?.takeIf { it.isNotBlank() }?.let { body.put("name", it) }
        balanceQuery?.takeIf { it.isNotEmpty() }?.let { map ->
            body.put("balanceQuery", JSONObject().apply { map.forEach { (k, v) -> put(k, v) } })
        }
        return post("/api/accounts", body)
    }

    /**
     * 余额直查方案（内核 `BALANCE_SCHEMES` 全集）+ 展示名。
     *
     * ⚠️ 取值必须逐字对上 [custom_providers::BALANCE_SCHEMES]：这里曾经把
     * NewAPI 写成 `newapi`，而内核是 `new_api` —— 于是「中转 → 类型选 NewAPI」
     * 提交时被 `validate_balance_scheme` 400 拒掉（「balanceScheme 只支持 …」）。
     * 与协议那个 bug 是同一类错：**前端猜的拼写与内核契约不一致**。
     */
    val balanceSchemes: List<Pair<String, String>> = listOf(
        "sub2api" to "Sub2API",
        "new_api" to "NewAPI",
        "deepseek" to "DeepSeek",
        "opencode" to "OpenCode Go",
        "sensenova" to "商汤日日新",
        "zhipu" to "智谱",
        "zhipu_zai" to "智谱 Z.AI",
    )

    /**
     * 更新提供商（字段缺省不动）。
     *
     * `clientEmulation` 用 `clearClientEmulation` 表达「显式清空」—— 三态与
     * 内核 update 的 patch 语义一致：不带 = 不改，带空串 = 清掉这个开关。
     * 上游 3.0.8 的编辑段就是这四件事（名称 / 协议 / Base URL / 伪装开关），
     * `custom-provider-ui.js` 的 update 也只提交这几项。
     */
    suspend fun update(
        id: String,
        name: String? = null,
        baseUrl: String? = null,
        protocol: Protocol? = null,
        enabled: Boolean? = null,
        clientEmulation: String? = null,
        clearClientEmulation: Boolean = false,
    ): OpResult {
        val body = JSONObject().put("id", id)
        name?.let { body.put("name", it) }
        baseUrl?.let { body.put("baseUrl", it) }
        protocol?.let { body.put("protocol", it.wire) }
        enabled?.let { body.put("enabled", it) }
        if (clearClientEmulation) body.put("clientEmulation", "")
        else clientEmulation?.takeIf { it.isNotBlank() }?.let { body.put("clientEmulation", it) }
        return post("/api/custom-providers/update", body)
    }

    suspend fun remove(id: String): OpResult =
        post("/api/custom-providers/remove", JSONObject().put("id", id))

    suspend fun fetchModels(providerId: String): OpResult =
        post("/api/custom-providers/fetch-models", JSONObject().put("providerId", providerId))

    /**
     * `POST /api/custom-providers/models` —— 整表替换这家的模型登记（含映射）。
     *
     * 内核是**整表替换**语义（`custom_providers::set_models`），所以调用方要先
     * 把「既有条目 + 新拉到的」合成一份再交上来；只交增量会把用户的启停状态
     * 和手填的能力位一起抹掉。
     */
    suspend fun saveModels(
        providerId: String,
        models: JSONArray,
        mappings: JSONArray = JSONArray(),
    ): OpResult = post(
        "/api/custom-providers/models",
        JSONObject()
            .put("providerId", providerId)
            .put("models", models)
            .put("mappings", mappings),
    )

    /**
     * 读出一家已登记的模型与映射（原样 JSON）。
     *
     * 给「拉取模型」用：它要拿既有条目做合并基线。返回 `null` = 这家不存在或
     * 列表拉不下来 —— 调用方按「没有基线」处理即可（不当成错误：
     * 此时用新拉到的清单覆盖也不会丢用户数据，因为本来就没有）。
     */
    suspend fun registeredModels(providerId: String): Pair<JSONArray, JSONArray>? {
        val arr = get("/api/custom-providers")?.optJSONArray("providers") ?: return null
        for (i in 0 until arr.length()) {
            val o = arr.optJSONObject(i) ?: continue
            if (o.optString("id") != providerId) continue
            return (o.optJSONArray("models") ?: JSONArray()) to
                (o.optJSONArray("mappings") ?: JSONArray())
        }
        return null
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