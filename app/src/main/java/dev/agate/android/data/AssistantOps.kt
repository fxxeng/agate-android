package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import okhttp3.sse.EventSource
import okhttp3.sse.EventSourceListener
import okhttp3.sse.EventSources
import org.json.JSONArray
import org.json.JSONObject
import java.util.concurrent.TimeUnit
import kotlin.coroutines.resume

/**
 * AI 运维助手数据层 —— 「AI 运维助手」页。
 *
 * 内核（`agate-server/src/server/api/assistant.rs` 模块头）：
 * - `GET  /api/assistant/config`  读配置
 * - `PUT  /api/assistant/config`  整块保存
 * - `POST /api/assistant/chat`    **SSE 流**（OpenAI 格式 delta，含 tool_calls）
 *
 * ── 为什么对话必须走端口，而不像其它管理接口走 JNI ─────────────
 * 管理面接口默认经 `CoreTransport`（JNI → 内核内存 Router），但那条通道是
 * **全量缓冲**：`jni_bridge::api_call` 用 `body::to_bytes(...)` 收完整个响应体
 * 才返回，还挂 120 秒超时。对 SSE 来说等于「等流结束才给第一帧」，逐字渲染
 * 无从谈起；长回答还会直接撞超时。
 *
 * 所以对话这一条改走真实 HTTP：网关运行时端口已监听，`OkHttp + EventSource`
 * 能逐帧拿（`GatewayRepository` 的 `/api/events` 就是这条通道）。
 *
 * **代价（已在 UI 上明说）**：助手只在**网关运行中**可用。Core 态
 * （`jni_bridge` 模块头：「不监听任何端口」）下端口不存在，这一页会引导用户
 * 先去启动网关，而不是给一个永远转圈的错误。
 *
 * ── 工具为什么只有 8 个（上游是 9 个）─────────────────────────
 * 上游有 `set_provider_order`（调整提供商顺位）。本内核**刻意下线了**
 * `providerRoute`：账号已经是「全局一条队列」，先用哪一家由**账号优先级**决定
 * （见 `api/config_api.rs` 那行注释）。复活它等于把已经收敛掉的第二个排序维度
 * 又装回去，会让「顺序到底谁说了算」重新变成两处。所以这一条不做，其语义由
 * `set_account_order` 承担 —— 内核里确实只剩这一个顺序。
 */
object AssistantOps {

    private val JSON_MEDIA = "application/json; charset=utf-8".toMediaType()

    /** 工具 id（与内核 `ALL_TOOL_IDS` 对得上；`set_provider_order` 已剔除）。 */
    const val TOOL_LIST_MODELS = "list_models"
    const val TOOL_LIST_BALANCE = "list_account_balance"
    const val TOOL_SET_MODEL_ENABLED = "set_model_enabled"
    const val TOOL_SET_MODEL_MAPPING = "set_model_mapping"
    const val TOOL_SET_MODEL_CAPABILITY = "set_model_capability"
    const val TOOL_TEST_MODEL = "test_model"
    const val TOOL_SET_ACCOUNT_ORDER = "set_account_order"
    const val TOOL_SET_ACCOUNT_ENABLED = "set_account_enabled"

    /** 改动类工具：执行前要界面弹确认卡（只读的不弹）。 */
    val MUTATING_TOOLS = setOf(
        TOOL_SET_MODEL_ENABLED,
        TOOL_SET_MODEL_MAPPING,
        TOOL_SET_MODEL_CAPABILITY,
        TOOL_SET_ACCOUNT_ORDER,
        TOOL_SET_ACCOUNT_ENABLED,
    )

    /** 存量安装的默认勾选（与内核 `DEFAULT_ENABLED_TOOLS` 同口径，去掉已下线的那个）。 */
    val DEFAULT_ENABLED = setOf(
        TOOL_LIST_MODELS,
        TOOL_LIST_BALANCE,
        TOOL_SET_MODEL_ENABLED,
        TOOL_SET_MODEL_MAPPING,
        TOOL_SET_MODEL_CAPABILITY,
        TOOL_TEST_MODEL,
    )

    /** 一只工具的界面文案（名称 / 说明）。 */
    class ToolMeta(val id: String, val label: String, val description: String)

    /** 全部工具（顺序即设置页的展示顺序）。 */
    val TOOLS: List<ToolMeta> = listOf(
        ToolMeta(TOOL_LIST_MODELS, "获取模型列表", "读出当前模型清单（含启停、提供商、映射）"),
        ToolMeta(TOOL_LIST_BALANCE, "查询账号余额", "读账号的余额 / 额度包 / 到期时间"),
        ToolMeta(TOOL_SET_MODEL_ENABLED, "启用禁用模型", "打开或关闭某个模型"),
        ToolMeta(TOOL_SET_MODEL_MAPPING, "设置模型映射", "把一个对外名指向另一个上游模型"),
        ToolMeta(TOOL_SET_MODEL_CAPABILITY, "修改模型能力", "改上下文长度、最大输出等能力位"),
        ToolMeta(TOOL_TEST_MODEL, "发起模型测试", "对某个模型打一次真实请求并回结果"),
        ToolMeta(TOOL_SET_ACCOUNT_ORDER, "调整账号优先级", "在全局账号队列里上移 / 下移一个账号"),
        ToolMeta(TOOL_SET_ACCOUNT_ENABLED, "启用禁用账号", "打开或关闭某个账号"),
    )

    // ── 配置 ──────────────────────────────────────────────────

    /**
     * 助手配置。
     *
     * `tools` 是**展开后**的表（每个 id 都有明确 true/false）——内核
     * `status_value` 出站时就展开了，前端不必再维护一份默认集（那样两处迟早对不上）。
     */
    class Config(
        val model: String?,
        val systemPromptEnabled: Boolean,
        val systemPrompt: String,
        val tools: Map<String, Boolean>,
    ) {
        fun isEnabled(id: String): Boolean = tools[id] ?: (id in DEFAULT_ENABLED)

        /** 勾了的工具 id（顺序按 [TOOLS]，保持稳定）。 */
        fun enabledToolIds(): List<String> = TOOLS.map { it.id }.filter { isEnabled(it) }
    }

    suspend fun config(): OpResult<Config> {
        val reply = CoreTransport.get("/api/assistant/config")
        if (reply !is CoreTransport.Reply.Ok) {
            return OpResult(false, null, downText(reply))
        }
        val body = runCatching { JSONObject(reply.text) }.getOrNull()
            ?: return OpResult(false, null, "响应不是合法 JSON")
        if (!body.optBoolean("success")) {
            return OpResult(false, null, body.optString("error").ifBlank { "读取配置失败" })
        }
        val data = body.optJSONObject("data")?.optJSONObject("config")
            ?: return OpResult(false, null, "配置形状异常")
        return OpResult(true, parseConfig(data), null)
    }

    fun parseConfig(o: JSONObject): Config {
        val tools = LinkedHashMap<String, Boolean>()
        o.optJSONObject("tools")?.let { table ->
            val keys = table.keys()
            while (keys.hasNext()) {
                val id = keys.next()
                tools[id] = table.optBoolean(id, false)
            }
        }
        return Config(
            model = o.optString("model").takeIf { it.isNotBlank() },
            systemPromptEnabled = o.optBoolean("systemPromptEnabled", true),
            systemPrompt = o.optString("systemPrompt"),
            tools = tools,
        )
    }

    /**
     * 保存配置。`PUT` 是**整块覆盖**（内核 `parse_config` 逐键读，缺键按默认）。
     *
     * `model` 传 null 表示清空（内核把 null 当「还没选」）。
     */
    suspend fun saveConfig(
        model: String?,
        systemPromptEnabled: Boolean,
        systemPrompt: String,
        tools: Map<String, Boolean>,
    ): OpResult<Config> = withContext(Dispatchers.IO) {
        val toolTable = JSONObject()
        tools.forEach { (id, on) -> toolTable.put(id, on) }
        val block = JSONObject()
            .put("model", model ?: JSONObject.NULL)
            .put("systemPromptEnabled", systemPromptEnabled)
            .put("systemPrompt", systemPrompt)
            .put("tools", toolTable)
        val payload = JSONObject().put("config", block)
        val reply = CoreTransport.call("PUT", "/api/assistant/config", payload.toString())
        if (reply !is CoreTransport.Reply.Ok) {
            return@withContext OpResult(false, null, downText(reply))
        }
        val body = runCatching { JSONObject(reply.text) }.getOrNull()
            ?: return@withContext OpResult(false, null, "响应不是合法 JSON")
        if (!body.optBoolean("success")) {
            return@withContext OpResult(
                false,
                null,
                body.optString("error").ifBlank { "保存失败" },
            )
        }
        val data = body.optJSONObject("data")?.optJSONObject("config")
            ?: return@withContext OpResult(false, null, "响应形状异常")
        OpResult(true, parseConfig(data), null)
    }

    // ── 工具定义（OpenAI tools 数组）──────────────────────────

    /**
     * 可选的对话模型 —— **对外名**清单。
     *
     * 读 `/api/models/list-view` 的 `ids`：那是「客户端所见」的名字（同名跨家
     * 合并后的对外名），与助手的 `model` 语义完全一致（内核 `model_from` 注释：
     * 这里的 model 是 `/v1/models` 的 `data[].id`，映射名也算）。
     * 不用 `/api/models/manage`：那边是内部规则视图，会给出一堆客户端根本
     * 请求不到的名字。
     */
    suspend fun availableModels(): List<String> = withContext(Dispatchers.IO) {
        val reply = CoreTransport.get("/api/models/list-view")
        if (reply !is CoreTransport.Reply.Ok) return@withContext emptyList()
        val body = runCatching { JSONObject(reply.text) }.getOrNull() ?: return@withContext emptyList()
        if (!body.optBoolean("success")) return@withContext emptyList()
        val data = body.optJSONObject("data") ?: body
        val arr = data.optJSONArray("ids") ?: return@withContext emptyList()
        (0 until arr.length()).mapNotNull { arr.optString(it).takeIf(String::isNotBlank) }
    }

    /**
     * 按勾选过滤后的 tools 数组。
     *
     * 定义写在前端是内核的既定分工（见 `provider_identity` 之外的
     * `api/assistant.rs` 模块头：「工具定义与执行都在前端」，后端不认识任何一个
     * 工具，加工具不必动 Rust）。
     */
    fun toolPayload(enabled: List<String>): JSONArray {
        val arr = JSONArray()
        enabled.forEach { id -> toolDefinition(id)?.let { arr.put(it) } }
        return arr
    }

    private fun obj(spec: String): JSONObject = JSONObject(spec)

    private fun toolDefinition(id: String): JSONObject? {
        val (description, parameters) = when (id) {
            TOOL_LIST_MODELS -> "读出当前模型清单：模型名、提供商、是否启用、映射关系。只读。" to
                obj("""{"type":"object","properties":{},"required":[]}""")

            TOOL_LIST_BALANCE -> "查询账号余额与额度包（含到期时间）。只读。" to
                obj("""{"type":"object","properties":{"account":{"type":"string","description":"账号名；不传则查全部"}},"required":[]}""")

            TOOL_SET_MODEL_ENABLED -> "启用或禁用一个模型。改动类，执行前会请用户确认。" to
                obj(
                    """{"type":"object","properties":{
                        "model":{"type":"string","description":"模型对外名"},
                        "provider":{"type":"string","description":"提供商 id；不传则改该模型在全部家的启用状态"},
                        "enabled":{"type":"boolean","description":"true 启用 / false 禁用"}
                    },"required":["model","enabled"]}""",
                )

            TOOL_SET_MODEL_MAPPING -> "把对外名映射到某个上游模型。改动类，执行前会请用户确认。" to
                obj(
                    """{"type":"object","properties":{
                        "alias":{"type":"string","description":"对外名（客户端请求用的名字）"},
                        "target":{"type":"string","description":"实际上游模型名"},
                        "provider":{"type":"string","description":"限定只在该提供商生效；不传表示全部"}
                    },"required":["alias","target"]}""",
                )

            TOOL_SET_MODEL_CAPABILITY -> "修改模型的能力位（上下文长度 / 最大输出等）。改动类，执行前会请用户确认。" to
                obj(
                    """{"type":"object","properties":{
                        "provider":{"type":"string","description":"提供商 id"},
                        "model":{"type":"string","description":"模型名"},
                        "key":{"type":"string","description":"能力键，如 contextWindow / maxOutputTokens"},
                        "value":{"type":"integer","description":"新的数值"}
                    },"required":["provider","model","key","value"]}""",
                )

            TOOL_TEST_MODEL -> "对某个模型打一次真实请求，返回成功与否、耗时与错误摘要。会产生真实消耗。" to
                obj(
                    """{"type":"object","properties":{
                        "provider":{"type":"string","description":"提供商 id"},
                        "model":{"type":"string","description":"模型名"},
                        "account_id":{"type":"string","description":"只测这个账号；不传则按队列"}
                    },"required":["provider","model"]}""",
                )

            TOOL_SET_ACCOUNT_ORDER -> "在全局账号队列里上移 / 下移一个账号。改动类，执行前会请用户确认。" to
                obj(
                    """{"type":"object","properties":{
                        "account":{"type":"string","description":"账号名或 id"},
                        "direction":{"type":"string","enum":["up","down"],"description":"移动方向"}
                    },"required":["account","direction"]}""",
                )

            TOOL_SET_ACCOUNT_ENABLED -> "启用或禁用一个账号。改动类，执行前会请用户确认。" to
                obj(
                    """{"type":"object","properties":{
                        "account":{"type":"string","description":"账号名或 id"},
                        "enabled":{"type":"boolean","description":"true 启用 / false 禁用"}
                    },"required":["account","enabled"]}""",
                )

            else -> return null
        }
        return JSONObject().put("type", "function").put(
            "function",
            JSONObject()
                .put("name", id)
                .put("description", description)
                .put("parameters", parameters),
        )
    }

    /**
     * 执行一只工具，返回给模型看的**文本结果**。
     *
     * 返回值直接作为 `role: "tool"` 的 content：所以要么是紧凑 JSON、要么是一句
     * 可读的中文结论。错误也走同一条路返回（而不是抛异常）——模型需要看到失败
     * 原因才能自己决定换参数重试还是向用户解释。
     */
    suspend fun execute(name: String, args: JSONObject): String = withContext(Dispatchers.IO) {
        runCatching { runTool(name, args) }
            .getOrElse { error -> """{"ok":false,"error":"${esc(error.message ?: "工具执行异常")}"}""" }
    }

    private suspend fun runTool(name: String, args: JSONObject): String {
        when (name) {
            TOOL_LIST_MODELS -> {
                val r = ModelOps.fetchListView()
                if (!r.ok) return fail(r.error)
                val data = r.data ?: return fail("没有数据")
                return ok(data)
            }

            TOOL_LIST_BALANCE -> {
                val account = args.optString("account").trim().ifEmpty { null }
                val data = AccountOps.usage(account)
                    ?: return fail("读取余额失败（内核未就绪或接口不可用）")
                return ok(data)
            }

            TOOL_SET_MODEL_ENABLED -> {
                val model = args.optString("model").trim()
                if (model.isEmpty()) return fail("缺少 model")
                val provider = args.optString("provider").trim().ifEmpty { null }
                val enabled = args.optBoolean("enabled", true)
                val r = ModelOps.setState(model, enabled, provider)
                if (!r.ok) return fail("设置失败")
                return """{"ok":true,"model":"${esc(model)}","enabled":$enabled}"""
            }

            TOOL_SET_MODEL_MAPPING -> {
                val alias = args.optString("alias").trim()
                val target = args.optString("target").trim()
                if (alias.isEmpty() || target.isEmpty()) return fail("alias 与 target 都不能为空")
                val provider = args.optString("provider").trim().ifEmpty { null }
                val r = ModelOps.setMapping(alias, target, provider)
                if (!r.ok) return fail("设置映射失败")
                return """{"ok":true,"alias":"${esc(alias)}","target":"${esc(target)}"}"""
            }

            TOOL_SET_MODEL_CAPABILITY -> {
                val provider = args.optString("provider").trim()
                val model = args.optString("model").trim()
                val key = args.optString("key").trim()
                if (provider.isEmpty() || model.isEmpty() || key.isEmpty()) {
                    return fail("provider / model / key 都不能为空")
                }
                if (!args.has("value")) return fail("缺少 value")
                val patch = JSONObject().put(key, args.get("value"))
                val r = ModelOps.setCapabilities(provider, model, patch)
                if (!r.ok) return fail("修改能力位失败")
                return """{"ok":true,"provider":"${esc(provider)}","model":"${esc(model)}","key":"${esc(key)}"}"""
            }

            TOOL_TEST_MODEL -> {
                val provider = args.optString("provider").trim()
                val model = args.optString("model").trim()
                if (provider.isEmpty() || model.isEmpty()) return fail("provider / model 都不能为空")
                val body = JSONObject().put("provider", provider).put("model", model)
                args.optString("account_id").trim().ifEmpty { null }?.let { body.put("account_id", it) }
                val reply = CoreTransport.post("/api/models/test", body.toString())
                if (reply !is CoreTransport.Reply.Ok) return fail(downText(reply))
                val parsed = runCatching { JSONObject(reply.text) }.getOrNull() ?: return fail("测试响应无法解析")
                if (reply.status !in 200..299 || !parsed.optBoolean("success")) {
                    return fail(parsed.optString("error").orEmpty().ifBlank { "测试失败" })
                }
                return ok(parsed.optJSONObject("data") ?: parsed)
            }

            TOOL_SET_ACCOUNT_ORDER -> {
                val target = args.optString("account").trim()
                if (target.isEmpty()) return fail("缺少 account")
                val up = args.optString("direction").trim().lowercase() != "down"
                val id = resolveAccountId(target) ?: return fail("找不到账号：$target")
                val r = AccountOps.move(id, up)
                if (!r.ok) return fail("调整顺序失败")
                return """{"ok":true,"account":"${esc(target)}","direction":"${if (up) "up" else "down"}"}"""
            }

            TOOL_SET_ACCOUNT_ENABLED -> {
                val target = args.optString("account").trim()
                if (target.isEmpty()) return fail("缺少 account")
                val id = resolveAccountId(target) ?: return fail("找不到账号：$target")
                val enabled = args.optBoolean("enabled", true)
                val r = AccountOps.update(id, JSONObject().put("enabled", enabled))
                if (!r.ok) return fail("设置失败")
                return """{"ok":true,"account":"${esc(target)}","enabled":$enabled}"""
            }

            else -> return fail("未知工具：$name")
        }
    }

    /**
     * 账号名 → 账号 id。模型给的是展示名（它看不到内部 id），而账号接口要 id。
     *
     * 用 `GET /api/accounts`（列表，含 `accounts[]` 与每项的 id / name / enabled）；
     * **不能用 `AccountOps.connections()`** —— 那只返回 `{counts:{id:count}}`，
     * 没有任何名字，拿它反查必然一条都对不上。
     *
     * 同名多个账号时取第一个：重名本身是配置问题，模型从 `list_account_balance`
     * 能看到确切的名字，重名时该由用户先在界面里区分开。
     */
    private suspend fun resolveAccountId(nameOrId: String): String? {
        val reply = CoreTransport.get("/api/accounts")
        if (reply !is CoreTransport.Reply.Ok) return null
        val body = runCatching { JSONObject(reply.text) }.getOrNull() ?: return null
        val data = body.optJSONObject("data") ?: body
        val accounts = data.optJSONArray("accounts") ?: return null
        var firstByName: String? = null
        for (i in 0 until accounts.length()) {
            val o = accounts.optJSONObject(i) ?: continue
            val id = o.optString("id")
            if (id.isBlank()) continue
            if (id.equals(nameOrId, ignoreCase = true)) return id
            val label = o.optString("name").ifBlank { o.optString("label") }
            if (firstByName == null && label.equals(nameOrId, ignoreCase = true)) firstByName = id
        }
        return firstByName
    }

    // ── 对话（端口 SSE）─────────────────────────────────────

    /**
     * 一轮对话。返回句柄用于中断。
     *
     * **所有回调都在 OkHttp 的线程上**（不是主线程）。写 Compose 状态是安全的
     * ——快照系统本身线程安全——但涉及 `mutableStateListOf` 这类结构性修改时，
     * 调用方仍需自己切回主线程。
     */
    class StreamHandle internal constructor(private val source: EventSource?) {
        fun cancel() {
            runCatching { source?.cancel() }
        }
    }

    /** 一次工具调用（流式拼装完整后才是可以执行的形态）。 */
    class ToolCall(val id: String, val name: String, val arguments: String)

    /**
     * 发起对话。
     *
     * @param port 网关监听端口（Core 态下端口不监听，调用方应先检查网关是否在跑）
     * @param apiKey 局域网模式下的网关 Key（本机模式可空）
     * @param messages 历史消息（含本轮用户输入），OpenAI 格式
     * @param tools 已按勾选过滤的 tools 数组
     * @param onDelta 正文增量（`choices[0].delta.content`）
     * @param onReasoning 思维链增量（`choices[0].delta.reasoning_content`）。
     *   必须与正文分开：推理模型会两路同时吐，混在一起会让回答变成
     *   「先自言自语再作答」的一坨，而且写回历史后上游会把它当成自己说过的话。
     * @param onToolCalls 一轮结束时若带工具调用，这里给完整列表
     * @param onDone 流正常结束
     * @param onError 失败（网络 / 上游 / 协议解析）
     */
    fun chat(
        port: Int,
        apiKey: String,
        messages: JSONArray,
        tools: JSONArray,
        model: String?,
        onDelta: (String) -> Unit,
        onReasoning: (String) -> Unit = {},
        onToolCalls: (List<ToolCall>) -> Unit,
        onDone: () -> Unit,
        onError: (String) -> Unit,
    ): StreamHandle {
        val payload = JSONObject().put("messages", messages)
        // 显式传模型：配置里可能还没选，此时内核会 400 并给可读消息 ——
        // 比前端悄悄用默认模型更诚实（用户以为在用 A，其实跑的是 B）。
        if (!model.isNullOrBlank()) payload.put("model", model)
        if (tools.length() > 0) payload.put("tools", tools)

        val builder = Request.Builder()
            .url("http://127.0.0.1:$port/api/assistant/chat")
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .post(payload.toString().toRequestBody(JSON_MEDIA))
        if (apiKey.isNotBlank()) builder.header("Authorization", "Bearer $apiKey")

        // 读超时给足：模型的思考时间可能很长，而这里等的是**帧**不是整段，
        // 只要上游还在吐字节就不会触发。60s 是「上游卡死」的判定线。
        val client = OkHttpClient.Builder()
            .connectTimeout(5, TimeUnit.SECONDS)
            .readTimeout(60, TimeUnit.SECONDS)
            .retryOnConnectionFailure(false)
            .build()

        var source: EventSource? = null
        // 工具调用是**分片**到达的：同一 index 的多帧要按 argument 片段拼接，
        // 直到流结束才是完整 JSON。这里按 index 累积，结束时统一回调。
        val calls = LinkedHashMap<Int, Triple<StringBuilder, String, StringBuilder>>()
        var finished = false

        source = EventSources.createFactory(client).newEventSource(
            builder.build(),
            object : EventSourceListener() {
                override fun onEvent(eventSource: EventSource, id: String?, type: String?, data: String) {
                    val text = data.trim()
                    if (text.isEmpty()) return
                    if (text == "[DONE]") {
                        if (!finished) {
                            finished = true
                            emitToolCalls(calls, onToolCalls)
                            onDone()
                        }
                        eventSource.cancel()
                        return
                    }
                    val obj = runCatching { JSONObject(text) }.getOrNull() ?: return
                    // 上游或内核的错误信封可能直接塞进 SSE 帧里
                    obj.optJSONObject("error")?.let { err ->
                        val message = err.optString("message").ifBlank { "上游返回错误" }
                        if (!finished) {
                            finished = true
                            onError(message)
                        }
                        return
                    }
                    val choices = obj.optJSONArray("choices") ?: return
                    val first = choices.optJSONObject(0) ?: return
                    val delta = first.optJSONObject("delta") ?: first.optJSONObject("message") ?: return

                    // 两路分开：思维链不进正文，否则会污染回答与后续历史
                    delta.optString("reasoning_content").takeIf { it.isNotEmpty() }?.let { onReasoning(it) }
                    delta.optString("content").takeIf { it.isNotEmpty() }?.let { onDelta(it) }

                    delta.optJSONArray("tool_calls")?.let { arr ->
                        for (i in 0 until arr.length()) {
                            val call = arr.optJSONObject(i) ?: continue
                            val index = call.optInt("index", i)
                            val slot = calls.getOrPut(index) {
                                Triple(StringBuilder(), "", StringBuilder())
                            }
                            // id 与 name 只在首帧出现，后续帧是空
                            val idPart = call.optString("id")
                            if (idPart.isNotEmpty() && slot.first.isEmpty()) slot.first.append(idPart)
                            val fn = call.optJSONObject("function") ?: continue
                            val namePart = fn.optString("name")
                            if (namePart.isNotEmpty()) {
                                // name 可能分片，简单情况下一次到齐
                                if (slot.second.isEmpty()) {
                                    calls[index] = Triple(slot.first, namePart, slot.third)
                                }
                            }
                            fn.optString("arguments").takeIf { it.isNotEmpty() }?.let { slot.third.append(it) }
                        }
                    }
                }

                override fun onClosed(eventSource: EventSource) {
                    if (!finished) {
                        finished = true
                        emitToolCalls(calls, onToolCalls)
                        onDone()
                    }
                }

                override fun onFailure(eventSource: EventSource, t: Throwable?, response: Response?) {
                    if (finished) return
                    finished = true
                    val code = response?.code
                    val message = when {
                        code == 401 -> "网关鉴权失败（401）：请在「访问密钥」里核对 Key"
                        code == 404 -> "网关未运行或该接口不存在（404）"
                        code != null -> "网关返回 $code"
                        else -> t?.message ?: "连接中断"
                    }
                    onError(message)
                }
            },
        )
        return StreamHandle(source)
    }

    private fun emitToolCalls(
        calls: LinkedHashMap<Int, Triple<StringBuilder, String, StringBuilder>>,
        sink: (List<ToolCall>) -> Unit,
    ) {
        if (calls.isEmpty()) return
        val out = calls.entries.sortedBy { it.key }.mapNotNull { (index, slot) ->
            val arguments = slot.third.toString()
            if (slot.second.isBlank() && arguments.isBlank()) return@mapNotNull null
            ToolCall(
                id = slot.first.toString().ifBlank { "call_$index" },
                name = slot.second,
                arguments = arguments.ifBlank { "{}" },
            )
        }
        if (out.isNotEmpty()) sink(out)
    }

    private fun ok(data: JSONObject): String = """{"ok":true,"data":$data}"""

    private fun fail(message: String?): String =
        """{"ok":false,"error":"${esc(message.orEmpty().ifBlank { "未知错误" })}"}"""

    /** JSON 字符串转义（结果要拼进 JSON 文本里给模型读）。 */
    private fun esc(text: String): String = text
        .replace("\\", "\\\\")
        .replace("\"", "\\\"")
        .replace("\n", "\\n")
        .replace("\r", "\\r")
        .replace("\t", "\\t")

    private fun downText(reply: CoreTransport.Reply): String =
        (reply as? CoreTransport.Reply.Down)?.message ?: "链路错误"

    class OpResult<T>(val ok: Boolean, val data: T?, val error: String?)
}
