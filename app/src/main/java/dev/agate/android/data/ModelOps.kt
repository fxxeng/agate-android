package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import org.json.JSONObject
import java.util.concurrent.TimeUnit

/**
 * 模型操作（M3-5 模型页），与 AccountOps 同款约定：suspend + IO、不持状态；
 * apiKey 与 GatewayRepository 同一处 configure。
 *
 * 内核路由（agate-server/src/server/http.rs）：
 * - GET  /api/models/manage                     → {models, mappings, reasoningLevels}（管理视图）
 * - POST /api/models/refresh  {accounts?, providers?} 强制刷新各家清单
 * - POST /api/models/state    {id, provider?, enabled} 启停单条
 * - POST /api/models/mappings {alias, target, provider?, reasoning?, enabled?} 新增/改映射
 * - POST /api/models/mappings/remove {alias, target, provider?} 删除映射
 * - POST /api/models/custom   {provider, id} 登记自定义模型
 * - POST /api/models/custom/remove {provider, id} 移除登记
 * - POST /api/models/capabilities {provider, id, capabilities} 覆盖能力位
 *
 * ★ 三态协议（reasoning 与 enabled 共用，model_manage.rs 模块头）：
 *   请求体里键「缺失」= 不动已有值；reasoning 给空串/null = 显式清成「不覆盖」；
 *   reasoning 给字符串 = 设为该档（表外自定义值也放行，由内核只做长度约束）；
 *   enabled 只认布尔（缺失 = 不动）。
 *
 * ★ capabilities 是 patch 语义：只提交改动键；值 null = 恢复上游值（清除覆盖）；
 *   token 键要 1~1 亿正整数、开关键要布尔，仅内置提供商可用（自定义提供商
 *   的能力走整表保存接口，本端点会 400）。
 *
 * 所有写接口都返回最新 manage 视图（就地重绘，不必再拉一次）。
 */
object ModelOps {



    @Volatile private var apiKey: String = ""

    fun configure(apiKey: String) {
        this.apiKey = apiKey
        CoreTransport.configure(apiKey)
    }

    class OpResult(val ok: Boolean, val data: JSONObject?, val error: String?)

    /**
     * `GET /api/models/manage` 的 `models[]` 一条 —— **含已禁用的条目**。
     *
     * 与 `/api/session` 的 `models` 是两回事：那边只给「已启用」（客户端真会
     * 收到的清单），没有 `enabled` 字段，也**没有已关掉的那些**。模型页要画
     * 启停开关、要把关掉的置灰留着，就必须读这一份。
     *
     * 能力位在这里是 `capabilities` 对象（键名 `supportsToolCall` 等），而
     * session 清单里是扁平的 `supports_tool_call` 等 —— 两套命名都要认，
     * 所以解析在 [toModelRow] 里统一成 [GatewayRepository.ModelRow]。
     */
    class ManageModelRow(
        val id: String,
        val provider: String,
        val providerLabel: String,
        val credits: String?,
        val enabled: Boolean,
        /** `null` = 上游没给 picker 名单，不做判断（见 `manage_view` 的 visibility）。 */
        val official: Boolean?,
        val maxInput: Int?,
        val maxOutput: Int?,
        val toolCall: Boolean,
        val images: Boolean,
        val reasoning: Boolean,
        val isDefault: Boolean,
    ) {
        fun toModelRow(): GatewayRepository.ModelRow = GatewayRepository.ModelRow(
            id = id,
            provider = provider,
            providerLabel = providerLabel,
            name = null,
            credits = credits,
            maxInput = maxInput,
            maxOutput = maxOutput,
            toolCall = toolCall,
            images = images,
            reasoning = reasoning,
            isDefault = isDefault,
            enabled = enabled,
            official = official,
        )

        companion object {
            /** 解析 manage 响应的 `models[]`；结构不对的条目跳过（不猜）。 */
            fun parse(data: JSONObject?): List<ManageModelRow> {
                val arr = data?.optJSONArray("models") ?: return emptyList()
                val out = ArrayList<ManageModelRow>(arr.length())
                for (i in 0 until arr.length()) {
                    val o = arr.optJSONObject(i) ?: continue
                    val id = o.optString("id")
                    if (id.isBlank()) continue
                    val caps = o.optJSONObject("capabilities")
                    val provider = o.optString("provider")
                    out += ManageModelRow(
                        id = id,
                        provider = provider,
                        providerLabel = o.optString("providerLabel").ifBlank { provider },
                        // credits 可能是 JSON null：`optString` 会把它读成字符串
                        // "null"（org.json 的 objectToString），那种值别当倍率显示。
                        credits = (o.opt("credits") as? String)?.trim()?.takeIf { it.isNotEmpty() },
                        enabled = o.optBoolean("enabled", true),
                        // visibility 缺失 = 没依据 → null（界面不标注），
                        // 不能当成 false 去标「桌面端未列」—— 那是谎报。
                        official = when (o.opt("visibility") as? String) {
                            "shown" -> true
                            "hidden" -> false
                            else -> null
                        },
                        maxInput = caps?.optInt("maxInputTokens", 0)?.takeIf { it > 0 },
                        maxOutput = caps?.optInt("maxOutputTokens", 0)?.takeIf { it > 0 },
                        toolCall = caps?.optBoolean("supportsToolCall") ?: false,
                        images = caps?.optBoolean("supportsImages") ?: false,
                        reasoning = caps?.optBoolean("supportsReasoning") ?: false,
                        isDefault = o.optBoolean("isDefault"),
                    )
                }
                return out
            }
        }
    }

    /** 映射行（manage 视图 mappings[] 一条）。 */
    class MappingRow(
        val alias: String,
        val target: String,
        val provider: String?,
        val enabled: Boolean,
        val reasoning: String?,
        val isDefault: Boolean,
        val dangling: Boolean,
        val carried: Boolean,
    ) {
        companion object {
            fun from(o: JSONObject): MappingRow = MappingRow(
                alias = o.optString("alias"),
                target = o.optString("target"),
                // provider 可空（空串 = 全局语义：由所有承载 target 的家接收）
                provider = o.optString("provider").ifBlank { null },
                enabled = o.optBoolean("enabled", true),
                reasoning = o.optString("reasoning").ifBlank { null },
                isDefault = o.optBoolean("isDefault", false),
                dangling = o.optBoolean("dangling", false),
                carried = o.optBoolean("carried", true),
            )
        }
    }

    /** 强制刷新模型清单（不带 accounts/providers = 刷新所有已实现家）。 */
    suspend fun refresh(accounts: List<String>? = null, providers: List<String>? = null): OpResult {
        val body = JSONObject()
        accounts?.let { body.put("accounts", org.json.JSONArray(it)) }
        providers?.let { body.put("providers", org.json.JSONArray(it)) }
        return post("/api/models/refresh", body)
    }

    /** 启停某家的一条模型（provider 可选；缺省为旧版全局语义）。 */
    suspend fun setState(id: String, enabled: Boolean, provider: String? = null): OpResult {
        val body = JSONObject().put("id", id).put("enabled", enabled)
        if (!provider.isNullOrBlank()) body.put("provider", provider)
        return post("/api/models/state", body)
    }

    /**
     * 批量启停（`POST /api/models/state/batch`）：一次改多条的同一个开关值。
     *
     * 内核在同一把锁内改完再写一次盘。逐条打 [setState] 的话，一家 48 个模型就是
     * 48 次「序列化整份规则 + 写盘」；中途失败还会留下改了一半的状态，而界面
     * 并不知道停在哪条。
     */
    suspend fun setStates(provider: String?, ids: List<String>, enabled: Boolean): OpResult {
        val body = JSONObject().put("enabled", enabled)
        body.put("ids", org.json.JSONArray(ids))
        provider?.takeIf { it.isNotBlank() }?.let { body.put("provider", it) }
        return post("/api/models/state/batch", body)
    }

    /**
     * 单模型测速（`POST /api/models/test`）—— **真实打一次上游**，会消耗额度。
     *
     * 内核那条接口走的是生产转发链路（`UpstreamService::forward`），并额外做了
     * 三件只对测试有意义的事（见 `api/model_test.rs` 模块头）：钉住这一家
     * （`provider`）、钉住这一个账号（`accountId`，不顺延换号）、跳过启停门禁
     * （关着的模型也能测）。因此结论与真实请求同源，不是另开一条内部通道。
     *
     * 响应**永远是 200**，失败结论在 `data.success=false` + `status`/`error` 里，
     * 所以这里不能只看 [OpResult.ok]。
     */
    suspend fun test(
        provider: String,
        model: String,
        accountId: String? = null,
        stream: Boolean = true,
    ): TestResult {
        val body = JSONObject()
            .put("provider", provider)
            .put("model", model)
            .put("stream", stream)
        accountId?.takeIf { it.isNotBlank() }?.let { body.put("account_id", it) }
        val r = post("/api/models/test", body)
        val d = r.data
        if (!r.ok || d == null) {
            return TestResult(false, null, r.error ?: "测试失败", null, null, null, null, null, null)
        }
        val success = d.optBoolean("success", false)
        return TestResult(
            ok = success,
            status = d.optInt("status", 0).takeIf { it > 0 },
            error = d.optString("error").takeIf { it.isNotBlank() },
            // `ttfb_ms` 可能是 JSON null（非流式 / 上游没给首字时间）
            ttfbMs = (d.opt("ttfb_ms") as? Number)?.toLong(),
            durationMs = (d.opt("duration_ms") as? Number)?.toLong(),
            completionTokens = d.optInt("completion_tokens", 0).takeIf { it > 0 },
            totalTokens = d.optInt("total_tokens", 0).takeIf { it > 0 },
            reply = d.optString("reply").takeIf { it.isNotBlank() },
            accountName = d.optString("account_name").takeIf { it.isNotBlank() },
            attempts = d.optInt("attempts", 0).takeIf { it > 0 },
        )
    }

    /**
     * 一次测速的结论。
     *
     * `ok=false` 时 [error] 一定有值（内核把上游的报文摘进去了）；`ok=true` 时
     * 看 `ttfbMs` / `durationMs` / `completionTokens` —— 吞吐（tok/s）不在这里算，
     * 界面自己按 `completionTokens / (durationMs - ttfbMs)` 算，因为「首字之后
     * 的生成速度」才是模型本身的速度，把首字等待算进去会让结论随网络抖动。
     */
    data class TestResult(
        val ok: Boolean,
        val status: Int?,
        val error: String?,
        val ttfbMs: Long?,
        val durationMs: Long?,
        val completionTokens: Int?,
        val totalTokens: Int?,
        val reply: String?,
        val accountName: String? = null,
        val attempts: Int? = null,
    )

    /** 拉管理视图（GET，无请求体）。 */
    suspend fun fetchManage(): OpResult = get("/api/models/manage")

    /**
     * 客户端所见清单的**状态与推演计数**（GET /api/models/list-view）。
     *
     * 响应：`{mode, pins, counts:{model,provider,hybrid}, ids:[...]}`。
     * 三种形态的项数一次给全 —— 界面切模式时数字即时，不必再往返一次。
     * `counts` 与 `/v1/models` 走内核同一个函数，所以「预览说 42」就等于
     * 「客户端真会收到 42」。
     */
    suspend fun fetchListView(): OpResult = get("/api/models/list-view")

    /**
     * 设置清单形态（POST /api/models/list-view）。
     *
     * 模式非法时内核回 400（不静默回落）—— 界面把用户的选择原样提交，
     * 静默改成别的值会让开关看起来「点了没反应」。
     */
    suspend fun setListView(mode: String, pins: List<String>): OpResult =
        post(
            "/api/models/list-view",
            JSONObject()
                .put("mode", mode)
                .put("pins", org.json.JSONArray(pins)),
        )

    /**
     * 新增映射（同 alias+target+provider 三元组 = 命中已有条目改它）。
     * reasoning 传 null 且 reasoningExplicit=false = 不动；explicit=true 且 null/空 = 清成不覆盖。
     */
    suspend fun setMapping(
        alias: String,
        target: String,
        provider: String? = null,
        reasoning: String? = null,
        reasoningExplicit: Boolean = false,
        enabled: Boolean? = null,
    ): OpResult {
        val body = JSONObject().put("alias", alias).put("target", target)
        if (!provider.isNullOrBlank()) body.put("provider", provider)
        if (reasoningExplicit) body.put("reasoning", reasoning ?: "")
        if (enabled != null) body.put("enabled", enabled)
        return post("/api/models/mappings", body)
    }

    suspend fun removeMapping(alias: String, target: String, provider: String? = null): OpResult {
        val body = JSONObject().put("alias", alias).put("target", target)
        if (!provider.isNullOrBlank()) body.put("provider", provider)
        return post("/api/models/mappings/remove", body)
    }

    /** 登记一个上游目录里没有的自定义模型。 */
    suspend fun addCustomModel(provider: String, id: String): OpResult =
        post("/api/models/custom", JSONObject().put("provider", provider).put("id", id))

    suspend fun removeCustomModel(provider: String, id: String): OpResult =
        post("/api/models/custom/remove", JSONObject().put("provider", provider).put("id", id))

    /** 覆盖能力位（patch：只提交改动键；值 null = 恢复上游；仅内置提供商）。 */
    suspend fun setCapabilities(provider: String, id: String, patch: JSONObject): OpResult =
        post(
            "/api/models/capabilities",
            JSONObject().put("provider", provider).put("id", id).put("capabilities", patch),
        )

    // P2：经 CoreTransport 内核直调（错误信封随 status 一并透传，不拦 4xx/5xx）
    private suspend fun get(path: String): OpResult {
        val r = CoreTransport.get(path)
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
