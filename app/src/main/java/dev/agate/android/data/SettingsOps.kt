package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/**
 * 应用设置（「设置」页十组）的数据层。
 *
 * ── 这一层存在的理由 ─────────────────────────────────────────
 * 设置页涉及 10 组、几十个字段，每组的**合法范围**都由内核规定，而且彼此不同
 * （超时 1–3600 秒、重试次数 0–10、排队次数 0–10、状态码 100–599……）。范围写在
 * 界面上会散成一片魔法数字，改一处忘一处；写在这里就只有一个来源，界面直接用
 * [Field] 提供的 min/max 做校验与提示。
 *
 * 所有范围常量都逐字对齐 `agate-server/src/server/config/types.rs`。
 * **不要凭常识猜**：重试次数上限是 10 不是 5，间隔上限 300 秒不是 60。
 *
 * ── 写入语义 ─────────────────────────────────────────────────
 * 每组都是「部分更新」：不传的键 = 不动，响应回**生效后的全量值**（内核
 * `put_*` 的既定契约）。所以保存后直接用响应重绘，不必再 GET 一次。
 *
 * ── 一个容易踩的空值 ─────────────────────────────────────────
 * 若干接口用 `null` 表示「清除」（`apiKey: null` 删 Key、`promptText: null`
 * 回到文件/内置默认、`promptFile: null` 清路径）。这里用 [Opt] 三态区分
 * 「不动 / 设成某值 / 清空」—— 用普通可空参数表达不了这个区别，
 * `null` 只能表示其中一种。
 */
object SettingsOps {

    /**
     * 三态参数：不传（不动）/ 设为某值 / 显式清空。
     *
     * Kotlin 的可空参数只有两种状态，而内核这些接口有三种：键不出现 = 保持原值，
     * `null` = 清除，有值 = 设置。混用可空参数会把「想清空」变成「不动」——
     * 表现是点了「清除」但值还在。
     */
    sealed class Opt<out T> {
        /** 不出现这个键，保持原值。 */
        object Skip : Opt<Nothing>()

        /** 显式 `null`，清除。 */
        object Clear : Opt<Nothing>()

        /** 设为某值。 */
        data class Set<T>(val value: T) : Opt<T>()
    }

    /**
     * 一个整数字段的范围。界面据此做输入校验与提示文案。
     *
     * @param unit 单位（「秒」「次」），用于拼提示；空串表示无量纲。
     */
    class Field(
        val key: String,
        val label: String,
        val min: Long,
        val max: Long,
        val unit: String,
        val desc: String,
        val default: Long,
    ) {
        /** `1–3600 秒` 这样的范围文本。 */
        val rangeText: String get() = "$min–$max$unit"

        fun accepts(value: Long): Boolean = value in min..max
    }

    // ── 超时（对齐 types.rs:767–789）────────────────────────────
    val TIMEOUT_FIELDS = listOf(
        Field(
            "connectTimeoutSeconds", "连接超时", 1, 3600, " 秒",
            "建立上游 TCP/TLS 连接（含到代理那段）。给太短会在慢网络下大量误判失败。", 30,
        ),
        Field(
            "headersTimeoutSeconds", "等待响应", 1, 3600, " 秒",
            "请求发出到响应头到达。推理模型的思考时间也算在这一段里。", 300,
        ),
        Field(
            "streamIdleTimeoutSeconds", "流式空闲", 1, 3600, " 秒",
            "流式响应相邻两片数据之间的最长静默。收到新数据即重置，不是总时长。", 300,
        ),
        Field(
            "bodyTimeoutSeconds", "非流式响应", 1, 3600, " 秒",
            "非流式响应体读完的总预算。一次性计时，不重置。", 300,
        ),
    )

    // ── 重试（对齐 types.rs:642–690）───────────────────────────
    val RETRY_FIELDS = listOf(
        Field(
            "retryCount", "原地重发", 0, 10, " 次",
            "在同一个账号上重发几次。0 = 失败立即换号。", 3,
        ),
        Field(
            "retryCrossProviderCount", "换号次数", 0, 10, " 次",
            "失败后最多换几个账号再试（按账号计，不分家）。键名是旧措辞。", 5,
        ),
        Field(
            "retryIntervalSeconds", "重试间隔", 0, 300, " 秒",
            "每次重发前等多久。0 = 立即重发。", 5,
        ),
    )

    /** 「指定状态码直接换号」的合法区间。 */
    const val RETRY_CODE_MIN = 100
    const val RETRY_CODE_MAX = 599

    /** 这个列表最多几项（内核 `RETRY_MAX_NO_RETRY_CODES`）。 */
    const val RETRY_MAX_NO_RETRY_CODES = 50

    // ── 排队（对齐 types.rs:864–884）───────────────────────────
    val QUEUE_FIELDS = listOf(
        Field(
            "queueMaxWaits", "最多等几次", 0, 10, " 次",
            "有些上游繁忙时不报错、只回「N 秒后再来」。0 = 不等，排队即返回 503。", 2,
        ),
        Field(
            "queueWaitSeconds", "单次等待", 0, 120, " 秒",
            "每次等多久。0 = 按上游建议的时长自适应。", 0,
        ),
    )

    // ──────────────────────────────────────────────────────────
    //  1. 服务接入（API Key / 语言 / 默认模型）
    // ──────────────────────────────────────────────────────────

    /**
     * 网关配置（`GET /api/config`）。
     *
     * `apiKey` 是**掩码**（`前6...后4`），只用来显示「已经有了」；真正的值拿不到、
     * 也不该拿到。`apiKeySet` 才是判断依据 —— 掩码可能为空串而 Key 是设过的。
     */
    class Config(o: JSONObject) {
        val apiKeyMasked: String = o.optString("apiKey")
        val apiKeySet: Boolean = o.optBoolean("apiKeySet", false)
        val locale: String = o.optString("locale").ifBlank { "zh" }
        val defaultModel: String = o.optString("defaultModel")
        val sanitize: Boolean = o.optBoolean("sanitizeBlacklistFingerprints", true)
    }

    /** 内核要求的最小 Key 长度提示（`MIN_API_KEY_LENGTH`）。 */
    const val MIN_API_KEY_LENGTH = 8

    suspend fun config(): OpResult<Config> = readTyped("/api/config", ::Config)

    /**
     * 保存 API Key。`null` = 清除 Key（之后接口恢复免鉴权）。
     *
     * 内核先校验再落盘：非法直接 400 且 locale 的改动也不生效，所以这两项
     * 应当分开保存，别让一次失败把另一项也吞掉。
     */
    suspend fun saveApiKey(apiKey: String?): OpResult<Config> {
        if (apiKey != null && apiKey.length < MIN_API_KEY_LENGTH) {
            return OpResult(false, null, "API Key 至少 $MIN_API_KEY_LENGTH 个字符")
        }
        return writeTyped("/api/config", JSONObject().put("apiKey", apiKey ?: JSONObject.NULL), ::Config)
    }

    suspend fun saveLocale(locale: String): OpResult<Config> =
        writeTyped("/api/config", JSONObject().put("locale", locale), ::Config)

    // ──────────────────────────────────────────────────────────
    //  2. 流式与输出 / 排队与超时 / 重试
    // ──────────────────────────────────────────────────────────

    suspend fun timeouts(): OpResult<Map<String, Long>> = readValues("/api/timeouts", TIMEOUT_FIELDS)
    suspend fun saveTimeouts(values: Map<String, Long>): OpResult<Map<String, Long>> =
        writeValues("/api/timeouts", values, TIMEOUT_FIELDS)

    suspend fun retry(): OpResult<Map<String, Long>> = readValues("/api/retry", RETRY_FIELDS)
    suspend fun saveRetry(values: Map<String, Long>): OpResult<Map<String, Long>> =
        writeValues("/api/retry", values, RETRY_FIELDS)

    suspend fun queue(): OpResult<Map<String, Long>> = readValues("/api/queue", QUEUE_FIELDS)
    suspend fun saveQueue(values: Map<String, Long>): OpResult<Map<String, Long>> =
        writeValues("/api/queue", values, QUEUE_FIELDS)

    // ──────────────────────────────────────────────────────────
    //  2b. 流式与输出（保活换行 / 断流续写 / 调试报文）
    // ──────────────────────────────────────────────────────────

    /** 续写轮数的合法区间（`AUTO_CONTINUE_MIN_ROUNDS..=AUTO_CONTINUE_MAX_ROUNDS`）。 */
    const val AUTO_CONTINUE_MIN_ROUNDS = 1
    const val AUTO_CONTINUE_MAX_ROUNDS = 8
    const val AUTO_CONTINUE_DEFAULT_ROUNDS = 3

    /**
     * 流式与输出组的三个开关。
     *
     * 三条端点形状一致（GET 读 / PUT 写 / 响应就是新状态），但**默认值方向不同**：
     * `stripKeepalive` 与 `developerRoleCompat` 默认开，`autoContinue` 与 `debug`
     * 默认关 —— 后两个都会多消耗配额或落盘大量报文，默认关是刻意的。
     *
     * 是 `data class` 而不是普通 class：这三个 PUT **不回全量状态**（响应就是
     * 被改的那一个键），所以保存成功后界面只能自己用 `copy(...)` 把这份快照
     * 改对。不 copy 的后果是「开关保存成功、收起再展开又变回旧值」——
     * 用户看到的是开关自己弹回去，与保存失败的观感一模一样。
     */
    data class Stream(
        val stripKeepalive: Boolean,
        val autoContinue: Boolean,
        val autoContinueRounds: Int,
        val debug: Boolean,
        /** 已采集的调试报文条数（只在 debug 开着时有意义）。 */
        val debugRows: Long,
        /** 调试报文的保留上限（内核 `debug_traffic::MAX_ENTRIES`）。 */
        val debugLimit: Long,
    )

    suspend fun stream(): OpResult<Stream> = withContext(Dispatchers.IO) {
        // 三条独立端点，并发读一次拿全
        val keepalive = get("/api/keepalive-strip")
        val auto = get("/api/auto-continue")
        val debug = get("/api/debug")
        if (keepalive == null && auto == null && debug == null) {
            return@withContext OpResult(false, null, "无法连接内核")
        }
        OpResult(
            true,
            Stream(
                stripKeepalive = keepalive?.optBoolean("stripNewlineKeepalive", true) ?: true,
                autoContinue = auto?.optBoolean("autoContinue", false) ?: false,
                autoContinueRounds = auto?.optInt("autoContinueMaxRounds", AUTO_CONTINUE_DEFAULT_ROUNDS)
                    ?: AUTO_CONTINUE_DEFAULT_ROUNDS,
                debug = debug?.optBoolean("debugMode", false) ?: false,
                // `GET /api/debug` 给的是 `{debugMode, count, limit}` —— 条数与上限
                // 都是**内存计数器**（`debug_traffic`），与 `/api/storage` 里那个
                // 库行数快照是同名不同源。读不到就当 0，不让它拖垮整组。
                debugRows = debug?.optLong("count", 0L) ?: 0L,
                debugLimit = debug?.optLong("limit", 0L) ?: 0L,
            ),
            null,
        )
    }

    suspend fun saveStripKeepalive(enabled: Boolean): OpResult<Unit> = saveSwitch(
        "/api/keepalive-strip", JSONObject().put("stripNewlineKeepalive", enabled),
    )

    suspend fun saveDebug(enabled: Boolean): OpResult<Unit> = saveSwitch(
        "/api/debug", JSONObject().put("debugMode", enabled),
    )

    /**
     * 保存断流续写开关与轮数。
     *
     * 这条端点的 `null` 语义与 `/api/retry` **不同**：`put_retry` 把 `null` 当
     * 「这一项不改」，`put_auto_continue` 明确**不认 null**（见其 `patch_from`
     * 的注释：null 在这里既不是「关掉」也不是「回到未配」，只能说明参数写错了）。
     * 所以这里两个参数都是非空 —— 用不到的那项干脆不进 body。
     */
    suspend fun saveAutoContinue(
        enabled: Boolean? = null,
        rounds: Int? = null,
    ): OpResult<Unit> {
        if (rounds != null && rounds !in AUTO_CONTINUE_MIN_ROUNDS..AUTO_CONTINUE_MAX_ROUNDS) {
            return OpResult(
                false,
                null,
                "续写轮数必须在 $AUTO_CONTINUE_MIN_ROUNDS–$AUTO_CONTINUE_MAX_ROUNDS 之间（收到 $rounds）",
            )
        }
        if (enabled == null && rounds == null) return OpResult(false, null, "没有要保存的改动")
        val body = JSONObject()
        enabled?.let { body.put("autoContinue", it) }
        rounds?.let { body.put("autoContinueMaxRounds", it) }
        return saveSwitch("/api/auto-continue", body)
    }

    // ──────────────────────────────────────────────────────────
    //  2c. 数据保留（三档天数）
    // ──────────────────────────────────────────────────────────

    /** 保留天数的合法区间（`RETENTION_MIN_DAYS..=RETENTION_MAX_DAYS`，上限 99999 = 永久）。 */
    const val RETENTION_MIN_DAYS = 1L
    const val RETENTION_MAX_DAYS = 99999L

    val RETENTION_FIELDS = listOf(
        Field(
            "logRetentionDays", "日志保留", RETENTION_MIN_DAYS, RETENTION_MAX_DAYS, " 天",
            "与上游交互的事件流水。超出即滚动丢弃，改小会立刻裁剪。", 30,
        ),
        Field(
            "requestLogRetentionDays", "请求明细保留", RETENTION_MIN_DAYS, RETENTION_MAX_DAYS, " 天",
            "调用记录页的逐条明细（含 token 用量与耗时）。", 99999,
        ),
        Field(
            "dailyAggregateRetentionDays", "按日聚合保留", RETENTION_MIN_DAYS, RETENTION_MAX_DAYS, " 天",
            "报表里的长期趋势靠它；删了就只剩明细能重算。", 99999,
        ),
    )

    /**
     * 读三档保留天数。
     *
     * 与超时 / 重试不同，这条的 PUT **带副作用**：内核写完会立刻触发一次裁剪
     * （见 `put_retention` 的注释）。所以界面必须提示「改小 = 立即删数据」，
     * 不能把它当成一个普通的数字设置。
     */
    suspend fun retention(): OpResult<Map<String, Long>> =
        readValues("/api/retention", RETENTION_FIELDS)

    suspend fun saveRetention(values: Map<String, Long>): OpResult<Map<String, Long>> =
        writeValues("/api/retention", values, RETENTION_FIELDS)

    /**
     * 「指定状态码直接换号」的列表。
     *
     * 单独一个方法，因为它与 [RETRY_FIELDS] 的形状不同（整数数组 vs 整数），
     * 塞进同一个键值映射会让类型变成 Any —— 那是另一个坑。
     */
    suspend fun noRetryCodes(): OpResult<List<Int>> = readCodes()

    private suspend fun readCodes(): OpResult<List<Int>> {
        val data = get("/api/retry") ?: return OpResult(false, null, "无法连接内核")
        val arr = data.optJSONArray("noRetryStatusCodes") ?: JSONArray()
        val out = ArrayList<Int>(arr.length())
        for (i in 0 until arr.length()) {
            val code = arr.optInt(i, 0)
            if (code != 0) out.add(code)
        }
        return OpResult(true, out, null)
    }

    /**
     * 保存状态码列表。内核会**排序去重**，返回的是规整后的结果。
     *
     * 越界项在界面侧就该拦下（内核是整单 400，不会悄悄剔除）——
     * 用户填了一百个码，其中一个写成 700，整次保存都会失败。
     */
    suspend fun saveNoRetryCodes(codes: List<Int>): OpResult<List<Int>> {
        val bad = codes.firstOrNull { it < RETRY_CODE_MIN || it > RETRY_CODE_MAX }
        if (bad != null) {
            return OpResult(false, null, "状态码 $bad 不在 $RETRY_CODE_MIN–$RETRY_CODE_MAX 之间")
        }
        if (codes.size > RETRY_MAX_NO_RETRY_CODES) {
            return OpResult(false, null, "最多 $RETRY_MAX_NO_RETRY_CODES 个状态码")
        }
        val body = JSONObject().put("noRetryStatusCodes", JSONArray(codes))
        val reply = withContext(Dispatchers.IO) { CoreTransport.call("PUT", "/api/retry", body.toString()) }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        return readCodes()
    }

    // ──────────────────────────────────────────────────────────
    //  3. 安全防护（脱敏 / CORS / 开发者角色兼容）
    // ──────────────────────────────────────────────────────────

    /**
     * 安全防护三个开关。
     *
     * 默认值不是随手写的：
     * - `sanitize` **默认开** —— 关掉等于把客户端 system 模板原样发出去，
     *   那正是被上游误拦（400 code=11-128）的原因。
     * - `cors` **默认关** —— 关着时浏览器第三方页面调不到网关（预检会被 401）。
     * - `developerRoleCompat` **默认开** —— 部分上游不认 `developer` 角色会直接 400。
     */
    data class Guards(
        val sanitize: Boolean,
        val cors: Boolean,
        val developerRoleCompat: Boolean,
    )

    suspend fun guards(): OpResult<Guards> = withContext(Dispatchers.IO) {
        // 三条独立端点（内核没做聚合），并发读一次拿全
        val sanitize = get("/api/sanitize")
        val cors = get("/api/cors")
        val role = get("/api/developer-role-compat")
        if (sanitize == null && cors == null && role == null) {
            return@withContext OpResult(false, null, "无法连接内核")
        }
        OpResult(
            true,
            Guards(
                sanitize = sanitize?.optBoolean("sanitizeBlacklistFingerprints", true) ?: true,
                cors = cors?.optBoolean("corsEnabled", false) ?: false,
                developerRoleCompat = role?.optBoolean("developerRoleCompat", true) ?: true,
            ),
            null,
        )
    }

    suspend fun saveSanitize(enabled: Boolean): OpResult<Unit> = saveSwitch(
        "/api/sanitize", JSONObject().put("sanitizeBlacklistFingerprints", enabled),
    )

    suspend fun saveCors(enabled: Boolean): OpResult<Unit> = saveSwitch(
        "/api/cors", JSONObject().put("corsEnabled", enabled),
    )

    suspend fun saveDeveloperRoleCompat(enabled: Boolean): OpResult<Unit> = saveSwitch(
        "/api/developer-role-compat", JSONObject().put("developerRoleCompat", enabled),
    )

    // ── 零宽空格脱敏词表（/api/zwsp）────────────────────────────
    //
    // 这是**第二层**脱敏，与上面的 `sanitize`（内置规则集）并列、互不替代：
    // 内置规则集知道的是上游实测拦过的那几句固定模板，而这一层是用户自己
    // 维护的词 —— 命中的每一处都会被插入一个零宽空格（U+200B），词本身
    // 一个字不改。两层在核心里先后执行，开关也各自独立。

    /** 词表条数上限（与内核 `SANITIZE_ZWSP_MAX_WORDS` 逐字一致）。 */
    const val ZWSP_MAX_WORDS = 200

    /** 单个词条长度上限（与内核 `SANITIZE_ZWSP_MAX_WORD_CHARS` 逐字一致）。 */
    const val ZWSP_MAX_WORD_CHARS = 64

    /** 零宽空格的字符形态（只看不用 —— 用来在界面上识别「已经打断过」的词条）。 */
    const val ZWSP = "\u200B"

    /**
     * 读词表（`GET /api/zwsp`）。
     *
     * 内核回的是 `{sanitizeZwspWords: [...]}`；缺失或不是数组时按空表处理
     * （与内核读侧口径一致：没有词就没有这一层）。
     */
    suspend fun zwspWords(): OpResult<List<String>> = withContext(Dispatchers.IO) {
        // `get` 已经解包了信封（`{success,data}`），拿到的就是内核的响应体本体。
        // 返回 null 同时覆盖「链路断」与「内核回 success:false」两种 —— 此时
        // 报「无法连接内核」并不精确，但对这一页足够：它的两条出路都是重试。
        val data = get("/api/zwsp") ?: return@withContext OpResult(
            false, null, "无法读取词表（内核未响应或拒绝）",
        )
        val array = data.optJSONArray("sanitizeZwspWords")
        val words = buildList {
            if (array != null) {
                for (index in 0 until array.length()) {
                    val word = array.optString(index, "")
                    if (word.isNotEmpty()) add(word)
                }
            }
        }
        OpResult(true, words, null)
    }

    /**
     * 整份替换词表（`PUT /api/zwsp`）。
     *
     * 为什么是整份而不是逐词增删：界面上的编辑动作（加/删/改/排序）最终都落成
     * 「这份表现在长这样」。逐词接口要额外定义四种操作与各自的冲突语义，
     * 而词表规模只有几条到几十条 —— 整份 PUT 的代价可以忽略。与
     * `setClineHeaders`（整份覆盖表）同一取向。
     *
     * 空列表 = 清空，这一层随即不生效（内核侧 `None` 快路径，零成本）。
     */
    suspend fun saveZwspWords(words: List<String>): OpResult<Unit> = withContext(Dispatchers.IO) {
        val array = JSONArray()
        words.forEach { array.put(it) }
        val reply = CoreTransport.call(
            "PUT", "/api/zwsp", JSONObject().put("sanitizeZwspWords", array).toString(),
        )
        val failure = failureOf(reply)
        if (failure != null) OpResult(false, null, failure) else OpResult(true, Unit, null)
    }

    /**
     * 把一个词条规整成可以发给内核的形态，返回 `null` 表示这一条不能收。
     *
     * 判据与内核 `validated_words` 逐条对齐（**内核是最终裁决**，这里只是
     * 提前把明显的错误挡在界面上，避免用户点保存才看到 400）：
     * - 去首尾空白后非空 —— 空串会让内核的 `find("")` 在每个字节位命中；
     * - 不含零宽空格 —— 那说明它是「已经打断过」的形态，再加一个会越积越长；
     * - 不超过 [ZWSP_MAX_WORD_CHARS] 个字符。
     *
     * 返回 `reason` 版本由 [validateZwspWord] 提供（界面要显示原因）。
     */
    fun sanitizeZwspWord(raw: String): String? = validateZwspWord(raw).getOrNull()

    /** 同 [sanitizeZwspWord]，但带上拒绝原因（给界面提示用）。 */
    fun validateZwspWord(raw: String): Result<String> {
        val word = raw.trim()
        if (word.isEmpty()) return Result.failure(IllegalArgumentException("词条不能为空"))
        if (word.contains(ZWSP)) {
            return Result.failure(
                IllegalArgumentException("这个词里已经含零宽空格了，请填原始词"),
            )
        }
        if (word.length > ZWSP_MAX_WORD_CHARS) {
            return Result.failure(
                IllegalArgumentException("最多 $ZWSP_MAX_WORD_CHARS 个字符，现在是 ${word.length} 个"),
            )
        }
        return Result.success(word)
    }

    private suspend fun saveSwitch(path: String, body: JSONObject): OpResult<Unit> {
        val reply = withContext(Dispatchers.IO) { CoreTransport.call("PUT", path, body.toString()) }
        val failure = failureOf(reply)
        return if (failure != null) OpResult(false, null, failure) else OpResult(true, Unit, null)
    }

    // ──────────────────────────────────────────────────────────
    //  4. 提示词
    // ──────────────────────────────────────────────────────────

    /**
     * 系统提示词设置（`GET /api/prompt`）。
     *
     * `text` 是**生效正文**（passthrough 下为空串）。`source` 说明它从哪来：
     * `none` / `builtin` / `file` / `inline` —— 界面要显示这个，否则用户会以为
     * 自己编辑的生效了，其实文件里的覆盖了它。
     */
    class Prompt(o: JSONObject) {
        val mode: String = o.optString("promptMode").ifBlank { "passthrough" }
        val file: String = o.optString("promptFile")
        val text: String = o.optString("promptText")
        val source: String = o.optString("promptSource").ifBlank { "none" }
        val fileError: String = o.optString("promptFileError")
        val lines: Int = o.optInt("promptLines", 0)
        val degradeActive: Boolean = o.optBoolean("degradeActive", false)
        val degradeUntilText: String = o.optString("degradeUntilText")

        /** 按提供商的覆盖条数（稀疏表：没配过的家不出现）。 */
        val providerOverrides: Int =
            o.optJSONObject("promptProviders")?.length() ?: 0

        val modeLabel: String
            get() = when (mode) {
                "custom" -> "替换 system"
                "append" -> "追加到 system 后"
                else -> "透传客户端"
            }

        val sourceLabel: String
            get() = when (source) {
                "builtin" -> "内置默认"
                "file" -> "文件"
                "inline" -> "界面编辑"
                else -> "未使用"
            }

        /** `custom` / `append` 之外模式不读文件与正文，界面要把输入框禁掉。 */
        val usesOwnPrompt: Boolean get() = mode == "custom" || mode == "append"
    }

    /** 三种模式的合法值（内核只认这三个，其余 400）。 */
    val PROMPT_MODES = listOf("passthrough", "custom", "append")

    suspend fun prompt(): OpResult<Prompt> = readTyped("/api/prompt", ::Prompt)

    /**
     * 保存提示词。
     *
     * ── 清除为什么用空串而不是 `null` ──────────────────────────
     * 这条端点的 `null` 语义与文件头的 [Opt] 假设**相反**：
     * `put_prompt` 把「键缺失」与「值为 null」都当成**这一项不改**
     * （见其 `None | Some(Value::Null) => settings.file.clone()`）；
     * 真正的「清除」是**空串**（`Some(Value::String(t)) if t.trim().is_empty()`）。
     * 所以这里 `Opt.Clear` 必须发 `""`。
     *
     * 早先这处发的是 `JSONObject.NULL`，后果是点了「清除提示词文件」什么都不会
     * 发生 —— 而 `null` 恰恰是内核留给桥接层「只解除降级」用的（`bridge.rs`
     * 会带 `promptFile: null`，若把 null 当清除，点一下「立即解除」就把用户
     * 配好的路径抹了）。
     *
     * @param mode 三种之一；`Opt.Clear` 无意义（模式不能为 null）
     * @param file 路径，`Opt.Clear` = 清掉路径（回到内置默认）
     * @param text 界面正文，`Opt.Clear` = 清空（回到「文件 > 内置默认」）
     * @param clearDegrade `true` = 立即解除降级期（改好提示词后的出口）
     */
    suspend fun savePrompt(
        mode: Opt<String> = Opt.Skip,
        file: Opt<String> = Opt.Skip,
        text: Opt<String> = Opt.Skip,
        clearDegrade: Boolean = false,
    ): OpResult<Prompt> {
        val body = JSONObject()
        when (mode) {
            is Opt.Skip -> Unit
            is Opt.Clear -> return OpResult(false, null, "promptMode 不能为 null")
            is Opt.Set -> {
                if (mode.value !in PROMPT_MODES) {
                    return OpResult(false, null, "promptMode 只认 ${PROMPT_MODES.joinToString(" / ")}")
                }
                body.put("promptMode", mode.value)
            }
        }
        when (file) {
            is Opt.Skip -> Unit
            is Opt.Clear -> body.put("promptFile", "")
            is Opt.Set -> body.put("promptFile", file.value)
        }
        when (text) {
            is Opt.Skip -> Unit
            // 正文同理：空串 = 「这份不要了」，null = 不改
            is Opt.Clear -> body.put("promptText", "")
            is Opt.Set -> body.put("promptText", text.value)
        }
        if (clearDegrade) body.put("clearDegrade", true)
        if (body.length() == 0) return OpResult(false, null, "没有要保存的改动")
        return writeTyped("/api/prompt", body, ::Prompt)
    }

    // ──────────────────────────────────────────────────────────
    //  5. 数据管理（存储概况）
    // ──────────────────────────────────────────────────────────

    /**
     * 存储概况（`GET /api/storage`）。**单库语义** —— 改造前是「三类数据各自一个
     * 目录」，现在只有一个库文件，所以这里不再有「搬家」入口。
     *
     * 响应是**嵌套**的 `{configDir, database:{file, bytes, available, accounts,
     * logs, requests, dailyDays, debug}}`，不是扁平结构 —— 按扁平读会让所有数字
     * 静默变成 0。
     *
     * `available = false` 时各计数都回落到 0，而「真的没有数据」也是 0，
     * 两者在数字上无法区分 —— 所以界面必须看这个布尔来说「不可用」，
     * 而不是显示一排看着正常的 0。
     */
    class Storage(o: JSONObject) {
        private val db: JSONObject = o.optJSONObject("database") ?: JSONObject()

        /** 配置目录（用户想知道数据放哪时看这个）。 */
        val configDir: String = o.optString("configDir")

        /** 库文件的完整路径。 */
        val file: String = db.optString("file")
        val bytes: Long = db.optLong("bytes", 0L)
        val available: Boolean = db.optBoolean("available", false)

        val accounts: Long = db.optLong("accounts", 0L)
        val logs: Long = db.optLong("logs", 0L)
        val requests: Long = db.optLong("requests", 0L)
        val dailyDays: Long = db.optLong("dailyDays", 0L)
        val debugRows: Long = db.optLong("debug", 0L)

        /** 各表条数，按数量降序（界面画成一行行，多的在上）。 */
        val tables: List<Pair<String, Long>>
            get() = listOf(
                "请求明细" to requests,
                "事件日志" to logs,
                "账号" to accounts,
                "调试报文" to debugRows,
                "聚合天数" to dailyDays,
            ).filter { it.second > 0 }.sortedByDescending { it.second }
    }

    suspend fun storage(): OpResult<Storage> = readTyped("/api/storage", ::Storage)

    // ──────────────────────────────────────────────────────────
    //  6. 备份与恢复
    // ──────────────────────────────────────────────────────────

    /** 导出结果：文件大小与各表导出行数。 */
    class ExportResult(val bytes: Long, val tables: List<Pair<String, Long>>)

    /**
     * 整库备份 / 恢复。
     *
     * 内核的做法是**由后端直接读写用户给的路径**（不走 HTTP 传文件内容），
     * 所以界面只要一个路径输入框。`includeLogs = false` 时排除日志表，
     * 备份文件小很多 —— 只想保住账号与配置时用它。
     *
     * 返回 `{bytes, tables}`（`db/backup.rs:export_to_file`），**没有 filePath** ——
     * 路径是调用方传进去的，内核不回显。
     */
    suspend fun exportBackup(filePath: String, includeLogs: Boolean): OpResult<ExportResult> {
        if (filePath.isBlank()) return OpResult(false, null, "请填写备份文件路径")
        val body = JSONObject().put("filePath", filePath).put("includeLogs", includeLogs)
        val reply = withContext(Dispatchers.IO) { CoreTransport.call("POST", "/api/backup/export", body.toString()) }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = parseData(reply) ?: return OpResult(true, ExportResult(0, emptyList()), null)
        val tables = runCatching {
            data.optJSONObject("tables")?.let { t ->
                t.keys().asSequence()
                    .map { it to t.optLong(it, 0L) }
                    .sortedByDescending { it.second }
                    .toList()
            } ?: emptyList()
        }.getOrDefault(emptyList())
        return OpResult(true, ExportResult(data.optLong("bytes", 0L), tables), null)
    }

    /**
     * 从 SQL 文件恢复。
     *
     * **会整体覆盖主库** —— 界面必须先确认。恢复后当前进程的数据全部被替换，
     * 包括账号、Key、设置。
     */
    suspend fun importBackup(filePath: String): OpResult<String> {
        if (filePath.isBlank()) return OpResult(false, null, "请填写备份文件路径")
        val body = JSONObject().put("filePath", filePath)
        val reply = withContext(Dispatchers.IO) { CoreTransport.call("POST", "/api/backup/import", body.toString()) }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = parseData(reply)
        return OpResult(true, data?.optString("message").orEmpty().ifBlank { "恢复完成" }, null)
    }

    // ──────────────────────────────────────────────────────────
    //  内部
    // ──────────────────────────────────────────────────────────

    private suspend fun readValues(path: String, fields: List<Field>): OpResult<Map<String, Long>> {
        val data = get(path) ?: return OpResult(false, null, "无法连接内核")
        val out = LinkedHashMap<String, Long>()
        fields.forEach { f -> out[f.key] = data.optLong(f.key, f.default) }
        return OpResult(true, out, null)
    }

    private suspend fun writeValues(
        path: String,
        values: Map<String, Long>,
        fields: List<Field>,
    ): OpResult<Map<String, Long>> {
        // 先在本地按内核的范围校验一遍。内核是「有一项非法就整体不落盘」，
        // 所以这里必须逐项查完再发，别把半套设置送上去换来一个 400
        val byKey = fields.associateBy { it.key }
        values.forEach { (key, value) ->
            val f = byKey[key] ?: return OpResult(false, null, "未知字段：$key")
            if (!f.accepts(value)) {
                return OpResult(false, null, "${f.label} 必须在 ${f.rangeText} 之间（收到 $value）")
            }
        }
        val body = JSONObject()
        values.forEach { (key, value) -> body.put(key, value) }
        if (body.length() == 0) return OpResult(false, null, "没有要保存的改动")
        val reply = withContext(Dispatchers.IO) { CoreTransport.call("PUT", path, body.toString()) }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = parseData(reply) ?: return OpResult(false, null, "响应没有数据")
        val out = LinkedHashMap<String, Long>()
        fields.forEach { f -> out[f.key] = data.optLong(f.key, f.default) }
        return OpResult(true, out, null)
    }

    private suspend fun <T> readTyped(path: String, build: (JSONObject) -> T): OpResult<T> {
        val data = get(path) ?: return OpResult(false, null, "无法连接内核")
        return OpResult(true, build(data), null)
    }

    private suspend fun <T> writeTyped(
        path: String,
        body: JSONObject,
        build: (JSONObject) -> T,
    ): OpResult<T> {
        val reply = withContext(Dispatchers.IO) { CoreTransport.call("PUT", path, body.toString()) }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = parseData(reply) ?: return OpResult(false, null, "响应没有数据")
        return OpResult(true, build(data), null)
    }

    /** 统一失败文案：链路断 / 非 2xx / 业务信封报错，三种都说清。 */
    private fun failureOf(reply: CoreTransport.Reply): String? {
        if (reply !is CoreTransport.Reply.Ok) {
            return (reply as? CoreTransport.Reply.Down)?.message ?: "链路错误"
        }
        val parsed = runCatching { JSONObject(reply.text) }.getOrNull()
        if (reply.status !in 200..299 || parsed?.optBoolean("success") == false) {
            return parsed?.optString("error").orEmpty().ifBlank { "保存失败（HTTP ${reply.status}）" }
        }
        return null
    }

    private fun parseData(reply: CoreTransport.Reply): JSONObject? {
        val parsed = runCatching { JSONObject((reply as CoreTransport.Reply.Ok).text) }.getOrNull()
            ?: return null
        return parsed.optJSONObject("data") ?: parsed
    }

    private suspend fun get(path: String): JSONObject? {
        val reply = withContext(Dispatchers.IO) { CoreTransport.get(path) }
        if (reply !is CoreTransport.Reply.Ok) return null
        val body = runCatching { JSONObject(reply.text) }.getOrNull() ?: return null
        if (!body.optBoolean("success")) return null
        return body.optJSONObject("data") ?: body
    }

    class OpResult<T>(val ok: Boolean, val data: T?, val error: String?)
}
