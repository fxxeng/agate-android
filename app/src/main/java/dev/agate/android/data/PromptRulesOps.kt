package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/**
 * 提示词规则数据层：`GET/PUT /api/prompt-rules`。
 *
 * ── 与旧「提示词」页是两套心智模型 ────────────────────────────
 * `/api/prompt` 是全局单一提示词；这里是「按 家/客户端/模型 条件换提示词」的规则表。
 * 两者共用同一份回落落点（promptMode/File/Text）。内核读分开、写合写：本 Ops
 * 的 [load] 只读规则视图，[save] 把规则与回落项一起提交（revision 覆盖四键）。
 * 规则**有序**，首条命中即止，所以 PUT 整份替换而非合并。
 *
 * 回落项读的是「用户原始输入」而非生效正文：[fallback] 的 text 是 inline 原文，
 * 不是内置默认展开后的文本 —— 直接把生效文本当用户输入回写会把内置提示词塞进编辑框。
 */
object PromptRulesOps {

    class OpResult<T>(val ok: Boolean, val data: T?, val error: String?)

    class Condition(
        val id: String,
        val field: String,
        val op: String,
        val values: List<String>,
    ) {
        fun toJson(): JSONObject = JSONObject()
            .put("id", id)
            .put("field", field)
            .put("op", op)
            .put("values", JSONArray(values))
    }

    /** 一条规则的可编辑部分（PUT 提交）。 */
    class Rule(
        val id: String,
        val name: String,
        val enabled: Boolean,
        val logic: String,
        val conditions: List<Condition>,
        val mode: String,
        val file: String,
        val text: String,
    ) {
        fun toJson(): JSONObject = JSONObject()
            .put("id", id)
            .put("name", name)
            .put("enabled", enabled)
            .put("logic", logic)
            .put("conditions", JSONArray(conditions.map { it.toJson() }))
            .put("mode", mode)
            .put("file", file)
            .put("text", text)
    }

    /** 规则视图条目：额外带编译后的只读字段（effectiveText/source/lines/fileError）。 */
    class RuleView(
        val rule: Rule,
        val effectiveText: String,
        val source: String,
        val lines: Int,
        val fileError: String,
    )

    /** 回落项（用户原始输入，与旧提示词页共用落点）。 */
    class Fallback(
        val mode: String,
        val file: String,
        val text: String,
    ) {
        fun toJson(): JSONObject = JSONObject()
            .put("mode", mode)
            .put("file", file)
            .put("text", text)
    }

    class Limits(val rules: Int, val conditions: Int, val values: Int)

    class View(
        val rules: List<RuleView>,
        val fallback: Fallback,
        val legacy: Int,
        val configured: Boolean,
        val revision: String,
        val error: String,
        val limits: Limits,
    )

    suspend fun load(): OpResult<View> {
        val reply = withContext(Dispatchers.IO) { CoreTransport.get("/api/prompt-rules") }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = dataOf(reply) ?: return OpResult(false, null, "响应没有数据")
        return OpResult(true, parseView(data), null)
    }

    /** 整份替换：规则 + 回落项一起提交，带 revision 防并发覆盖。 */
    suspend fun save(rules: List<Rule>, fallback: Fallback, revision: String): OpResult<View> {
        val body = JSONObject()
            .put("rules", JSONArray(rules.map { it.toJson() }))
            .put("fallback", fallback.toJson())
            .put("revision", revision)
        val reply = withContext(Dispatchers.IO) {
            CoreTransport.call("PUT", "/api/prompt-rules", body.toString())
        }
        val failure = failureOf(reply)
        if (failure != null) return OpResult(false, null, failure)
        val data = dataOf(reply) ?: return OpResult(false, null, "响应没有数据")
        return OpResult(true, parseView(data), null)
    }

    /** 纯解析入口（公开便于单测）：把内核视图 JSON 映射成 [View]。 */
    fun parseView(data: JSONObject): View {
        val rules = data.optJSONArray("rules")?.let { arr ->
            (0 until arr.length()).mapNotNull { i ->
                arr.optJSONObject(i)?.let { o ->
                    val conditions = o.optJSONArray("conditions")?.let { ca ->
                        (0 until ca.length()).mapNotNull { j ->
                            ca.optJSONObject(j)?.let { c ->
                                val values = c.optJSONArray("values")?.let { va ->
                                    (0 until va.length()).map { va.optString(it) }
                                } ?: emptyList()
                                Condition(
                                    c.optString("id"),
                                    c.optString("field").ifBlank { "provider" },
                                    c.optString("op").ifBlank { "is" },
                                    values,
                                )
                            }
                        }
                    } ?: emptyList()
                    val rule = Rule(
                        id = o.optString("id"),
                        name = o.optString("name"),
                        enabled = o.optBoolean("enabled", false),
                        logic = o.optString("logic").ifBlank { "all" },
                        conditions = conditions,
                        mode = o.optString("mode").ifBlank { "passthrough" },
                        file = o.optString("file"),
                        text = o.optString("text"),
                    )
                    RuleView(
                        rule = rule,
                        effectiveText = o.optString("effectiveText"),
                        source = o.optString("source"),
                        lines = o.optInt("lines", 0),
                        fileError = o.optString("fileError"),
                    )
                }
            }
        } ?: emptyList()
        val fb = data.optJSONObject("fallback") ?: JSONObject()
        val limits = data.optJSONObject("limits") ?: JSONObject()
        return View(
            rules = rules,
            fallback = Fallback(
                mode = fb.optString("mode").ifBlank { "passthrough" },
                file = fb.optString("file"),
                text = fb.optString("text"),
            ),
            legacy = data.optInt("legacy", 0),
            configured = data.optBoolean("configured", false),
            revision = data.optString("revision"),
            error = data.optString("error"),
            limits = Limits(
                rules = limits.optInt("rules", 200),
                conditions = limits.optInt("conditions", 32),
                values = limits.optInt("values", 100),
            ),
        )
    }

    private fun failureOf(reply: CoreTransport.Reply): String? {
        if (reply !is CoreTransport.Reply.Ok) {
            return (reply as? CoreTransport.Reply.Down)?.message ?: "链路错误"
        }
        val parsed = runCatching { JSONObject(reply.text) }.getOrNull()
        if (reply.status !in 200..299 || parsed?.optBoolean("success") == false) {
            return parsed?.optString("error").orEmpty()
                .ifBlank { "操作失败（HTTP ${reply.status}）" }
        }
        return null
    }

    private fun dataOf(reply: CoreTransport.Reply): JSONObject? {
        if (reply !is CoreTransport.Reply.Ok) return null
        val parsed = runCatching { JSONObject(reply.text) }.getOrNull() ?: return null
        return parsed.optJSONObject("data") ?: parsed
    }
}
