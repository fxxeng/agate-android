package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/**
 * 出网代理池数据层 —— 「出网代理池」页。
 *
 * 内核路由（`agate-server/src/server/api/proxies.rs` 模块头）：
 * - `GET  /api/proxies/pool`             池列表（**进来时自动同步一次 Clash**）
 * - `POST /api/proxies/pool`             新建（只收手动条目）
 * - `POST /api/proxies/pool/update`      编辑（body 带 id；镜像条目不可改）
 * - `POST /api/proxies/pool/remove`      删除（body 带 id）
 * - `POST /api/proxies/pool/enable`      启用 / 禁用（body 带 id + enabled）
 * - `POST /api/proxies/pool/test`        测试并把结果写回条目
 * - `POST /api/proxies/pool/sync-clash`  手动同步 Clash 出口
 * - `GET/POST /api/proxies/settings`     节点端口范围 + 测试网址
 *
 * 全部写接口都回**最新全量列表**（与模型管理页同一约定），前端就地替换即可。
 *
 * ── 三种来源的能力边界（决定了界面能给哪些按钮）─────────────
 *   · `manual`：本页建的，可编辑 / 删除 / 启停；
 *   · `clash`：由「同步 Clash Verge」从 Clash 配置里导出，**不能改也不能删**
 *     （改它要去 Clash Verge 本身），只能在本页「本地禁用」；
 *   · `subscription`：订阅商节点，同理只能本地禁用。
 * 内核对手工新建这两种来源是**硬拒绝**（见 `normalize_item`）：镜像记录不属于
 * 任何同步集合，下次同步就被摘掉，等于白填一次。
 */
object ProxyOps {

    /** 来源（与内核 `source` 字段逐字一致）。 */
    enum class Source(val value: String, val label: String) {
        MANUAL("manual", "手动"),
        CLASH("clash", "Clash"),
        SUBSCRIPTION("subscription", "订阅"),
        ;

        companion object {
            fun of(raw: String): Source =
                entries.firstOrNull { it.value.equals(raw, ignoreCase = true) } ?: MANUAL
        }
    }

    /**
     * 一次测试结果（内核写在条目上的 `lastTest`）。
     *
     * 编辑配置后旧结果**不会被清掉**（内核有意保留，见 `normalize_item` 的注释），
     * 所以界面要带上 `at` 让用户自己判断这条结论是不是还对应当前配置。
     */
    class LastTest(
        val success: Boolean,
        val ip: String,
        val durationMs: Long,
        val at: Long,
        val error: String?,
    )

    /** 出口解析结果（内核对条目的解读；配置不全时为 null 并给 `resolveError`）。 */
    class Resolved(val protocol: String, val host: String, val port: Long, val label: String)

    /** 一条池条目。 */
    class Entry(o: JSONObject) {
        val id: String = o.optString("id")
        val name: String = o.optString("name").ifBlank { "未命名" }
        val source: Source = Source.of(o.optString("source"))

        /** 有效启用（来源侧启用 ∧ 非本地禁用）——开关显示的就是它。 */
        val enabled: Boolean = o.optBoolean("enabled", true)

        /**
         * 本地禁用位。只对镜像条目（clash / subscription）有意义：
         * 界面据此区分「来源侧关着」与「本页关着」两种 off。
         */
        val localDisabled: Boolean = o.optBoolean("localDisabled", false)

        val protocol: String = o.optString("protocol")
        val host: String = o.optString("host")
        val port: Long = o.optLong("port", 0L)
        val username: String = o.optString("username")
        val password: String = o.optString("password")

        /** clash 来源的监听器 uid（界面上标注来源用）。 */
        val listenerUid: String = o.optString("listenerUid")
        /** subscription 来源：所属订阅与节点名。 */
        val node: String = o.optString("node")

        /** 被哪些账号引用（`usedBy`，值是账号展示名或 id）。 */
        val usedBy: List<String> = o.optJSONArray("usedBy")?.let { arr ->
            (0 until arr.length()).mapNotNull { arr.optString(it).takeIf(String::isNotBlank) }
        } ?: emptyList()

        /**
         * 孤儿：订阅来源但所属订阅已不存在（内核比对活着的订阅清单得出）。
         *
         * 与 `resolveError` 不是一回事：孤儿一定解析不出来，但解析不出来的原因
         * 还可能是「配置不完整」。界面按「孤儿 → 红、解析失败 → 红、其余按启用」
         * 的分层给状态点。
         */
        val orphan: Boolean = o.optBoolean("orphan", false)

        val resolved: Resolved? = o.optJSONObject("resolved")?.let {
            Resolved(
                protocol = it.optString("protocol"),
                host = it.optString("host"),
                port = it.optLong("port", 0L),
                label = it.optString("label"),
            )
        }
        val resolveError: String? = o.optString("resolveError").takeIf { it.isNotBlank() }

        val lastTest: LastTest? = o.optJSONObject("lastTest")?.let {
            LastTest(
                success = it.optBoolean("success", false),
                ip = it.optString("ip"),
                durationMs = it.optLong("durationMs", 0L),
                at = it.optLong("at", 0L),
                error = (it.opt("error") as? String)?.takeIf { e -> e.isNotBlank() },
            )
        }

        /** 能否在本页编辑 / 删除（只有手动条目可以）。 */
        val editable: Boolean get() = source == Source.MANUAL

        /**
         * 出口的可读描述。
         *
         * 有 `resolved` 就用内核解析出来的那份（那是**实际会用的**地址，
         * 对 clash 来源它甚至不是条目里存的字段）；没有就退回条目自身的
         * 协议 / 主机 / 端口，再不行给空串由界面显示占位。
         */
        fun endpoint(): String {
            resolved?.let { r ->
                val via = if (r.label.isNotBlank() && r.label != name) " · ${r.label}" else ""
                return "${r.protocol}://${r.host}:${r.port}$via"
            }
            val head = listOf(protocol, host).filter { it.isNotBlank() }.joinToString(" · ")
            return if (head.isBlank()) "" else if (port > 0) "$head:$port" else head
        }
    }

    /** 池快照。 */
    class Pool(val items: List<Entry>) {
        val enabledCount: Int get() = items.count { it.enabled }
        /** 有问题的条目：孤儿，或配置解析不出来。 */
        val brokenCount: Int get() = items.count { it.orphan || it.resolveError != null }
        val referencedCount: Int get() = items.count { it.usedBy.isNotEmpty() }
    }

    /**
     * 代理设置。
     *
     * `testUrl` 空串表示「未自定义」（用默认）；内核原样返回而不是解析成生效值，
     * 是为了让「用户配没配过」与「当前生效的是什么」两个信息都在（见
     * `proxy_settings::get`）。
     */
    class Settings(
        val listenerPortBase: Long,
        val listenerPortEnd: Long,
        val testUrl: String,
        val defaultPortBase: Long,
        val defaultPortEnd: Long,
        val defaultTestUrl: String,
    )

    suspend fun pool(): OpResult<Pool> {
        val data = get("/api/proxies/pool") ?: return OpResult(false, null, "无法连接内核")
        return OpResult(true, parsePool(data), null)
    }

    /**
     * 同步 Clash Verge。
     *
     * 响应比列表多 `changes`（本次变更数）与 `syncError`（Clash 不可用时的原因）。
     * 列表接口自己也会同步一次，所以这个按钮的价值是**给用户一个反馈**：
     * 自动同步那次不该弹任何提示（用户没点任何东西）。
     */
    suspend fun syncClash(): OpResult<SyncReport> {
        val reply = post("/api/proxies/pool/sync-clash", JSONObject())
        if (!reply.ok) return OpResult(false, null, reply.error)
        val data = reply.data ?: return OpResult(false, null, "响应为空")
        return OpResult(
            true,
            SyncReport(
                pool = parsePool(data),
                changes = data.optInt("changes", 0),
                error = data.optString("syncError").takeIf { it.isNotBlank() },
            ),
            null,
        )
    }

    class SyncReport(val pool: Pool, val changes: Int, val error: String?)

    /** 新建手动条目。字段名与内核 `normalize_item` 逐字对齐。 */
    suspend fun create(
        name: String,
        protocol: String,
        host: String,
        port: Long,
        username: String,
        password: String,
    ): OpResult<Pool> {
        val body = JSONObject()
            .put("name", name)
            .put("protocol", protocol)
            .put("host", host)
            .put("port", port)
            .put("username", username)
            .put("password", password)
        return write("/api/proxies/pool", body)
    }

    /** 编辑手动条目。整份表单提交（没有「空 = 保留」的隐式行为）。 */
    suspend fun update(
        id: String,
        name: String,
        protocol: String,
        host: String,
        port: Long,
        username: String,
        password: String,
    ): OpResult<Pool> {
        val body = JSONObject()
            .put("id", id)
            .put("name", name)
            .put("protocol", protocol)
            .put("host", host)
            .put("port", port)
            .put("username", username)
            .put("password", password)
        return write("/api/proxies/pool/update", body)
    }

    suspend fun remove(id: String): OpResult<Pool> =
        write("/api/proxies/pool/remove", JSONObject().put("id", id))

    suspend fun setEnabled(id: String, enabled: Boolean): OpResult<Pool> =
        write("/api/proxies/pool/enable", JSONObject().put("id", id).put("enabled", enabled))

    /** 测试一条；结果由内核写回条目，响应里直接带最新 `items`。 */
    suspend fun test(id: String): OpResult<TestResult> {
        val reply = post("/api/proxies/pool/test", JSONObject().put("id", id))
        if (!reply.ok) return OpResult(false, null, reply.error)
        val data = reply.data ?: return OpResult(false, null, "响应为空")
        return OpResult(
            true,
            TestResult(
                success = data.optBoolean("success", false),
                ip = data.optString("ip"),
                durationMs = data.optLong("durationMs", 0L),
                error = (data.opt("error") as? String)?.takeIf { it.isNotBlank() },
                // 写回失败时内核仍返回结论，只多一个 saveError ——
                // 「测试跑完了」和「没记住这次结果」是两件事，不能混成一次失败
                saveError = data.optString("saveError").takeIf { it.isNotBlank() },
                // 测试响应里同时也带回了最新 items，直接就地刷新列表
                pool = parsePool(data),
            ),
            null,
        )
    }

    class TestResult(
        val success: Boolean,
        val ip: String,
        val durationMs: Long,
        val error: String?,
        val saveError: String?,
        val pool: Pool?,
    )

    suspend fun settings(): OpResult<Settings> {
        val data = get("/api/proxies/settings") ?: return OpResult(false, null, "无法连接内核")
        return OpResult(true, parseSettings(data), null)
    }

    suspend fun saveSettings(base: Long, end: Long, testUrl: String): OpResult<Settings> {
        val reply = post(
            "/api/proxies/settings",
            JSONObject()
                .put("listenerPortBase", base)
                .put("listenerPortEnd", end)
                .put("testUrl", testUrl),
        )
        if (!reply.ok) return OpResult(false, null, reply.error)
        val data = reply.data ?: return OpResult(false, null, "响应为空")
        return OpResult(true, parseSettings(data), null)
    }

    fun parsePool(data: JSONObject): Pool {
        val arr = data.optJSONArray("items") ?: JSONArray()
        return Pool(
            (0 until arr.length()).mapNotNull { arr.optJSONObject(it)?.let(::Entry) },
        )
    }

    private fun parseSettings(data: JSONObject): Settings {
        val defaults = data.optJSONObject("defaults")
        return Settings(
            listenerPortBase = data.optLong("listenerPortBase", 0L),
            listenerPortEnd = data.optLong("listenerPortEnd", 0L),
            testUrl = data.optString("testUrl"),
            defaultPortBase = defaults?.optLong("listenerPortBase", 0L) ?: 0L,
            defaultPortEnd = defaults?.optLong("listenerPortEnd", 0L) ?: 0L,
            defaultTestUrl = defaults?.optString("testUrl").orEmpty(),
        )
    }

    private suspend fun write(path: String, body: JSONObject): OpResult<Pool> {
        val reply = post(path, body)
        if (!reply.ok) return OpResult(false, null, reply.error)
        val data = reply.data ?: return OpResult(false, null, "响应为空")
        return OpResult(true, parsePool(data), null)
    }

    /** 统一的 POST：管理 API 一律 `{success, data}`，错误在 `error`。 */
    private suspend fun post(path: String, body: JSONObject): OpResult<JSONObject> =
        withContext(Dispatchers.IO) {
            val r = CoreTransport.call("POST", path, body.toString())
            if (r !is CoreTransport.Reply.Ok) {
                return@withContext OpResult(false, null, (r as? CoreTransport.Reply.Down)?.message ?: "链路错误")
            }
            val json = runCatching { JSONObject(r.text) }.getOrNull()
            val ok = r.status in 200..299
            if (!ok) {
                return@withContext OpResult(
                    false,
                    null,
                    json?.optString("error").orEmpty().ifBlank { "操作失败" },
                )
            }
            OpResult(true, json?.optJSONObject("data") ?: json, null)
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
