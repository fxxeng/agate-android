package dev.agate.android.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * [ChatTraceOps] 的解析测试。
 *
 * 这些用例刻意都用**真实上游可能吐出的形态**：分片 tool_calls、流式 SSE 帧、
 * 截断的半截 JSON。解析器的价值全在这些边界上 —— 正常一问一答不解析也不会错。
 */
class ChatTraceOpsTest {

    private fun sse(vararg deltas: String): String =
        deltas.joinToString("") { "data: $it\n\n" } + "data: [DONE]\n\n"

    @Test
    fun `流式帧里的思维链与正文分开收`() {
        val body = sse(
            """{"choices":[{"delta":{"reasoning_content":"先看余额"}}]}""",
            """{"choices":[{"delta":{"reasoning_content":"再决定"}}]}""",
            """{"choices":[{"delta":{"content":"余额是 10 元"}}]}""",
        )
        val trace = ChatTraceOps.parse("", body, false)
        // 思维链只从 reasoning_content 来，正文不该混进来
        assertEquals("先看余额再决定", trace.reasoning)
        assertTrue(trace.toolCalls.isEmpty())
    }

    @Test
    fun `只吐正文时不产生思维链段`() {
        val body = sse(
            """{"choices":[{"delta":{"content":"你好"}}]}""",
            """{"choices":[{"delta":{"content":"，有什么要改的"}}]}""",
        )
        val trace = ChatTraceOps.parse("", body, false)
        assertEquals("", trace.reasoning)
        assertTrue(trace.isEmpty)
    }

    @Test
    fun `工具调用按 index 拼装分片参数`() {
        val body = sse(
            """{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"set_model_enabled","arguments":"{\"mod"}}]}}]}""",
            """{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"el\":\"gpt-4\"}"}}]}}]}""",
        )
        val trace = ChatTraceOps.parse("", body, false)
        assertEquals(1, trace.toolCalls.size)
        val call = trace.toolCalls.first()
        assertEquals("call_a", call.id)
        assertEquals("set_model_enabled", call.name)
        // 两片拼成一个合法 JSON
        assertEquals("""{"model":"gpt-4"}""", call.arguments)
    }

    @Test
    fun `多只工具调用按 index 排序而不按到达顺序`() {
        val body = sse(
            """{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"b","arguments":"{}"}},{"index":0,"id":"call_a","function":{"name":"a","arguments":"{}"}}]}}]}""",
        )
        val trace = ChatTraceOps.parse("", body, false)
        assertEquals(listOf("a", "b"), trace.toolCalls.map { it.name })
    }

    @Test
    fun `工具结果从请求体的 role tool 消息里取`() {
        // 场景：上一轮调了 list_models，结果在下一次请求体的 tool 消息里
        val request = """
            {"messages":[
              {"role":"user","content":"看看有哪些模型"},
              {"role":"assistant","content":null,"tool_calls":[
                {"id":"call_x","type":"function","function":{"name":"list_models","arguments":"{}"}}
              ]},
              {"role":"tool","tool_call_id":"call_x","content":"{\"ok\":true}"}
            ]}
        """.trimIndent()
        val trace = ChatTraceOps.parse(request, "", false)
        assertEquals(1, trace.toolResults.size)
        val result = trace.toolResults.first()
        assertEquals("call_x", result.callId)
        // 名字要能从 assistant 那条的 tool_calls 反查出来
        assertEquals("list_models", result.name)
        assertEquals("""{"ok":true}""", result.content)
    }

    @Test
    fun `非流式整段 JSON 也能解析`() {
        val body = """{"choices":[{"message":{"content":"好","reasoning_content":"想了想"}}]}"""
        val trace = ChatTraceOps.parse("", body, false)
        assertEquals("想了想", trace.reasoning)
    }

    @Test
    fun `错误帧不会被当成内容`() {
        val body = sse("""{"error":{"message":"上游 429"}}""")
        val trace = ChatTraceOps.parse("", body, false)
        assertTrue(trace.isEmpty)
    }

    @Test
    fun `截断时如实传递标记`() {
        val body = sse("""{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"n","arguments":"{\"a\":"}}]}}]}""")
        val trace = ChatTraceOps.parse("", body, true)
        assertTrue(trace.truncated)
        // 半截参数不消失：原样给出，界面能看出「这里被截断了」
        assertEquals("""{"a":""", trace.toolCalls.first().arguments)
    }

    @Test
    fun `只有工具结果时也算有内容`() {
        // 界面靠 isEmpty 决定整卡渲不渲染。这一条是唯一「真有两段其一为空」
        // 的组合：请求体里有 tool 结果，但响应体是空的（比如上一次请求的报文
        // 已被清理，只剩这次请求体）。此时必须仍然渲染，否则用户看不到结果。
        val request = """
            {"messages":[
              {"role":"assistant","content":null,"tool_calls":[
                {"id":"call_y","type":"function","function":{"name":"list_models","arguments":"{}"}}
              ]},
              {"role":"tool","tool_call_id":"call_y","content":"{\"models\":[]}"}
            ]}
        """.trimIndent()
        val trace = ChatTraceOps.parse(request, "", false)
        assertFalse(trace.isEmpty)
        assertTrue(trace.reasoning.isBlank())
        assertTrue(trace.toolCalls.isEmpty())
        assertEquals(1, trace.toolResults.size)
    }

    @Test
    fun `空报文不抛异常且视为无内容`() {
        val trace = ChatTraceOps.parse("", "", false)
        assertTrue(trace.isEmpty)
        assertFalse(trace.truncated)
    }

    @Test
    fun `坏数据不炸整页`() {
        val trace = ChatTraceOps.parse("{坏掉的", "data: 也不是 JSON\n\n", false)
        assertTrue(trace.isEmpty)
    }

    @Test
    fun `工具参数美化能缩进合法 JSON`() {
        val pretty = ChatTraceOps.prettyJson("""{"model":"x","enabled":true}""")
        assertTrue(pretty.contains("\n"))
        assertTrue(pretty.contains("\"model\""))
    }

    @Test
    fun `工具参数美化对半截 JSON 原样返回`() {
        assertEquals("""{"a":""", ChatTraceOps.prettyJson("""{"a":"""))
    }
}
