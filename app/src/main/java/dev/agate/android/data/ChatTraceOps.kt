package dev.agate.android.data

import org.json.JSONObject

/**
 * 从一条请求的原始报文里，还原「模型这一轮到底做了什么」。
 *
 * 对应上游 3.0.8 请求详情弹窗里的三段折叠：**思考过程** / **调用工具** / **工具结果**。
 *
 * ── 数据来自哪里，为什么不是新接口 ─────────────────────────────
 * `GET /api/stats/requests/raw?id=` 已经同时给出两侧正文
 * （`{id, requestBody, responseBody, truncated}`）：
 * - `responseBody` 是**客户端可见的字节**。流式请求由 `RecordingStream` 里的
 *   `RawCapture` 逐帧抄一份，在 `settle()` 处定稿（`api/pipeline.rs`）——
 *   所以这里拿到的就是完整的 SSE 文本，不是「流式不采」。
 * - `requestBody` 是发出去的请求体，里面带 `messages`、`tools`、模型名。
 * 所以这是**纯客户端解析**，内核一行都不用改。
 *
 * ── 为什么必须能「没数据就不渲染」───────────────────────────────
 * 未开调试模式时两侧都不采（同一个开关，构造时定格），`raw` 接口给 404。
 * 解析结果因此用 [Trace.isEmpty] 表示「这段没东西可显示」，由界面决定不渲染，
 * 而不是画一个空的折叠条让用户以为丢数据了。
 */
object ChatTraceOps {

    /** 一段可折叠的内容。 */
    class Section(val title: String, val text: String)

    /**
     * 一条请求的解析结果。
     *
     * [reasoning] / [toolCalls] / [toolResults] 三者互不依赖：有的模型只吐思维链，
     * 有的只调工具，有的什么都没有（普通问答）。
     */
    class Trace(
        val reasoning: String,
        val toolCalls: List<ToolCall>,
        val toolResults: List<ToolResult>,
        /** 上游是否把响应截断了（`truncated`），截断时解析结果可能不完整。 */
        val truncated: Boolean,
    ) {
        val isEmpty: Boolean
            get() = reasoning.isBlank() && toolCalls.isEmpty() && toolResults.isEmpty()
    }

    /** 模型请求的一次工具调用（参数已拼成完整 JSON 文本）。 */
    class ToolCall(val id: String, val name: String, val arguments: String)

    /** 工具执行结果（下一轮请求里 `role=tool` 的那条消息）。 */
    class ToolResult(val callId: String, val name: String, val content: String)

    // ── 入口 ──────────────────────────────────────────────────

    /**
     * 解析一条请求。
     *
     * @param requestBody 发出去的请求体（可能是空串 —— 未开调试模式）
     * @param responseBody 收到的响应体，流式时是完整 SSE 文本
     * @param truncated 上游标记的截断
     */
    fun parse(requestBody: String, responseBody: String, truncated: Boolean): Trace {
        val reasoning = StringBuilder()
        val calls = LinkedHashMap<Int, Pair<StringBuilder, Pair<String, StringBuilder>>>()
        val results = mutableListOf<ToolResult>()

        // ── 响应侧：思维链 + 工具调用分片 ──
        forEachDelta(responseBody) { delta ->
            delta.optString("reasoning_content").takeIf { it.isNotEmpty() }?.let { reasoning.append(it) }
            delta.optJSONArray("tool_calls")?.let { arr ->
                for (i in 0 until arr.length()) {
                    val call = arr.optJSONObject(i) ?: continue
                    val index = call.optInt("index", i)
                    val slot = calls.getOrPut(index) {
                        StringBuilder() to ("" to StringBuilder())
                    }
                    val idPart = call.optString("id")
                    if (idPart.isNotEmpty() && slot.first.isEmpty()) slot.first.append(idPart)
                    val fn = call.optJSONObject("function") ?: continue
                    val namePart = fn.optString("name")
                    // name 正常一次到齐；真遇到分片就拼接（便宜且不会错）
                    if (namePart.isNotEmpty()) {
                        val old = slot.second.first
                        if (old.isEmpty()) {
                            calls[index] = slot.first to (namePart to slot.second.second)
                        } else if (!old.contains(namePart)) {
                            calls[index] = slot.first to ((old + namePart) to slot.second.second)
                        }
                    }
                    fn.optString("arguments").takeIf { it.isNotEmpty() }?.let { slot.second.second.append(it) }
                }
            }
        }

        // ── 请求侧：`role=tool` 的历史消息就是「工具结果」──
        // 同一请求体里，`tool` 消息出现在「模型已经调过一次」之后，
        // 所以它是上一轮的结果；本轮新调用的结果要等下一次请求才有。
        // 用 `tool_call_id` 反查工具名：名字只在上一条 assistant 的 tool_calls 里。
        runCatching { JSONObject(requestBody) }.getOrNull()
            ?.optJSONArray("messages")
            ?.let { messages ->
                // 先收齐所有 assistant 的 tool_calls：id → name
                val idToName = HashMap<String, String>()
                for (i in 0 until messages.length()) {
                    val msg = messages.optJSONObject(i) ?: continue
                    msg.optJSONArray("tool_calls")?.let { arr ->
                        for (j in 0 until arr.length()) {
                            val call = arr.optJSONObject(j) ?: continue
                            val id = call.optString("id")
                            val name = call.optJSONObject("function")?.optString("name").orEmpty()
                            if (id.isNotEmpty() && name.isNotEmpty()) idToName[id] = name
                        }
                    }
                }
                for (i in 0 until messages.length()) {
                    val msg = messages.optJSONObject(i) ?: continue
                    if (msg.optString("role") != "tool") continue
                    val callId = msg.optString("tool_call_id")
                    results.add(
                        ToolResult(
                            callId = callId,
                            name = idToName[callId].orEmpty(),
                            content = msg.optString("content"),
                        )
                    )
                }
            }

        val toolCalls = calls.entries.sortedBy { it.key }.mapNotNull { (index, slot) ->
            val name = slot.second.first
            val args = slot.second.second.toString()
            if (name.isBlank() && args.isBlank()) return@mapNotNull null
            ToolCall(
                id = slot.first.toString().ifBlank { "call_$index" },
                name = name,
                arguments = args.ifBlank { "{}" },
            )
        }

        return Trace(reasoning.toString(), toolCalls, results, truncated)
    }

    /**
     * 遍历响应体里的每个 `choices[0].delta`。
     *
     * 两种形态都要认：
     * - **SSE 文本**（`data: {...}` 逐行）—— 流式请求的常态；
     * - **整段 JSON**（`{choices:[{message:{...}}]}`）—— 非流式，或上游把流聚合了。
     *
     * 解析失败一律静默跳过：这里面对的是**可能被截断的**历史数据，
     * 为一行坏数据让整个详情页报错不值得。
     */
    private fun forEachDelta(body: String, sink: (JSONObject) -> Unit) {
        val text = body.trim()
        if (text.isEmpty()) return

        // 整段 JSON：首字符是 `{` 且不是 SSE 的 `data:` 行
        if (text.startsWith("{")) {
            val whole = runCatching { JSONObject(text) }.getOrNull()
            if (whole != null) {
                val choices = whole.optJSONArray("choices")
                val first = choices?.optJSONObject(0)
                first?.optJSONObject("delta")?.let(sink)
                    ?: first?.optJSONObject("message")?.let(sink)
                return
            }
            // 落到 SSE 分支（有些上游的 `data:` 行里嵌套 JSON，但整体仍以 { 开头时
            // 上面已经处理；这里走到说明整段解析失败，继续按行尝试）
        }

        for (line in text.lineSequence()) {
            val trimmed = line.trim()
            if (!trimmed.startsWith("data:")) continue
            val payload = trimmed.removePrefix("data:").trim()
            if (payload.isEmpty() || payload == "[DONE]") continue
            val obj = runCatching { JSONObject(payload) }.getOrNull() ?: continue
            // 错误帧不是 delta，跳过（不把它当成内容显示）
            if (obj.has("error")) continue
            val first = obj.optJSONArray("choices")?.optJSONObject(0) ?: continue
            (first.optJSONObject("delta") ?: first.optJSONObject("message"))?.let(sink)
        }
    }

    /**
     * 把工具参数美化成可读文本。
     *
     * 参数常是一行紧凑 JSON（`{"model":"x","enabled":true}`），直接展示很难读；
     * 解析得动就给缩进，解析不动就原样返回 —— 半截 JSON（被截断）也不该消失。
     */
    fun prettyJson(raw: String): String {
        val text = raw.trim()
        if (text.isEmpty()) return "{}"
        val obj = runCatching { JSONObject(text) }.getOrNull() ?: return text
        return runCatching { obj.toString(2) }.getOrDefault(text)
    }
}
