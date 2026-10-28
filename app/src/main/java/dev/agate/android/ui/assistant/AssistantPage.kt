package dev.agate.android.ui.assistant

import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.AssistantOps
import dev.agate.android.data.GatewayRepository
import dev.agate.android.ui.components.AgateFormInput
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import dev.agate.core.Gateway
import dev.agate.core.GatewayPrefs
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/**
 * AI 运维助手 —— 对齐 v44 的 assistant 页。
 *
 * ── 为什么这一页要求「网关运行中」─────────────────────────────
 * 对话端点是 SSE，而管理面通用的 JNI 通道是**全量缓冲**
 * （`jni_bridge::api_call` 用 `to_bytes` 收完整个 body 才返回，还挂 120s 超时），
 * 逐字渲染无从谈起。所以这一页走真实端口 + OkHttp SSE。
 *
 * 代价是 Core 态（网关未启动）下端口不监听 —— 那是内核的既定设计
 * （`jni_bridge` 模块头：「不监听任何端口」）。这一页的处理是**明说并引导**，
 * 而不是让用户对着一个永远转圈的错误猜。
 *
 * ── 工具调用为什么要「确认卡」─────────────────────────────────
 * 内核默认系统提示词里写着「调用改动类工具之前，先用一句话说清将要改动什么；
 * 确认卡由界面渲染，不要跳过它直接执行」。所以只读工具（列模型 / 查余额）
 * 直接跑，**改动类**（启停、映射、能力位、账号顺序）一律先弹确认卡。
 * 这是把「模型想改你的配置」和「你的配置真的被改了」之间留一道人。
 */
@Composable
fun AssistantPage(
    onOpenDrawer: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    var config by remember { mutableStateOf<AssistantOps.Config?>(null) }
    var gatewayRunning by remember { mutableStateOf(false) }
    var input by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var showSettings by remember { mutableStateOf(false) }
    /** 正在流式拼装的助手正文（流结束后并入 items）。 */
    var streaming by remember { mutableStateOf("") }
    /** 等待用户确认的工具调用（改动类）。 */
    var pending by remember { mutableStateOf<PendingTool?>(null) }

    val items = remember { mutableStateListOf<ChatItem>() }
    /** 真正发给模型的 OpenAI 消息历史（含 tool_calls / tool 结果）。 */
    val history = remember { mutableStateListOf<JSONObject>() }
    var handle by remember { mutableStateOf<AssistantOps.StreamHandle?>(null) }

    val port = remember { GatewayPrefs.port(ctx) }
    val apiKey = remember { GatewayPrefs.apiKey(ctx) }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    DisposableEffect(Unit) {
        GatewayRepository.configure(apiKey)
        onDispose { handle?.cancel() }
    }

    LaunchedEffect(Unit) {
        gatewayRunning = withContext(Dispatchers.IO) {
            runCatching { Gateway.status().running }.getOrDefault(false)
        }
        val r = AssistantOps.config()
        if (r.ok) config = r.data else error = r.error
    }

    /** 一次流式回合：发请求，收集正文与工具调用。 */
    suspend fun streamOnce(toolPayload: JSONArray): TurnResult {
        val messages = JSONArray()
        history.forEach { messages.put(it) }
        val done = CompletableDeferred<TurnResult>()
        val buffer = StringBuilder()
        handle = AssistantOps.chat(
            port = port,
            apiKey = apiKey,
            messages = messages,
            tools = toolPayload,
            model = config?.model,
            onDelta = { delta ->
                buffer.append(delta)
                streaming = buffer.toString()
            },
            // 思维链这一段**不并入正文**：混进去会让回答变成「先自言自语再作答」，
            // 且会随历史写回上游。单独渲染（折叠块）属于 M4 的结构化展示，
            // 这里先只保证不污染 content。
            onReasoning = { },
            onToolCalls = { calls ->
                if (!done.isCompleted) {
                    done.complete(TurnResult(buffer.toString(), calls, null))
                }
            },
            onDone = {
                if (!done.isCompleted) done.complete(TurnResult(buffer.toString(), emptyList(), null))
            },
            onError = { message ->
                if (!done.isCompleted) done.complete(TurnResult(buffer.toString(), emptyList(), message))
            },
        )
        return done.await()
    }

    /**
     * 跑一轮完整对话（可能含多轮工具调用）。
     *
     * 循环上限 6 轮：模型偶尔会陷进「一直调工具」，没有上限就会无限消耗额度。
     * 到上限时明确告诉用户，而不是静默停下。
     */
    suspend fun runTurn() {
        var rounds = 0
        while (rounds < 6) {
            rounds++
            val enabled = config?.enabledToolIds() ?: emptyList()
            val result = streamOnce(AssistantOps.toolPayload(enabled))
            streaming = ""
            if (result.error != null) {
                error = result.error
                return
            }
            if (result.content.isNotBlank()) {
                items.add(ChatItem.Assistant(result.content))
            }
            if (result.calls.isEmpty()) return

            // 助手这一轮「说了什么 + 要调什么」要整条入历史，工具结果才能对得上 id
            val assistantMsg = JSONObject().put("role", "assistant").put(
                "content",
                result.content.ifBlank { JSONObject.NULL },
            )
            val callsJson = JSONArray()
            result.calls.forEach { call ->
                callsJson.put(
                    JSONObject()
                        .put("id", call.id)
                        .put("type", "function")
                        .put(
                            "function",
                            JSONObject().put("name", call.name).put("arguments", call.arguments),
                        ),
                )
            }
            assistantMsg.put("tool_calls", callsJson)
            history.add(assistantMsg)

            for (call in result.calls) {
                val label = AssistantOps.TOOLS.firstOrNull { it.id == call.name }?.label ?: call.name
                val args = runCatching { JSONObject(call.arguments) }.getOrNull() ?: JSONObject()
                val index = items.size
                items.add(ChatItem.Tool(label, args.toString(), null, running = true))

                val mutating = call.name in AssistantOps.MUTATING_TOOLS
                if (mutating) {
                    // 改动类：挂起等用户点确认 / 取消
                    val decision = CompletableDeferred<Boolean>()
                    pending = PendingTool(label, call.name, args, decision)
                    val approved = decision.await()
                    pending = null
                    if (!approved) {
                        val refused = """{"ok":false,"error":"用户取消了这次操作"}"""
                        items[index] = ChatItem.Tool(label, args.toString(), refused, running = false)
                        history.add(
                            JSONObject()
                                .put("role", "tool")
                                .put("tool_call_id", call.id)
                                .put("content", refused),
                        )
                        continue
                    }
                }

                val output = AssistantOps.execute(call.name, args)
                items[index] = ChatItem.Tool(label, args.toString(), output, running = false)
                history.add(
                    JSONObject()
                        .put("role", "tool")
                        .put("tool_call_id", call.id)
                        .put("content", output),
                )
            }
        }
        error = "工具调用超过 6 轮，已停止（避免无限消耗额度）"
    }

    fun send(text: String) {
        val content = text.trim()
        if (content.isEmpty() || busy) return
        if (config?.model.isNullOrBlank()) {
            toast("请先在设置里选对话模型")
            showSettings = true
            return
        }
        error = null
        input = ""
        items.add(ChatItem.User(content))
        history.add(JSONObject().put("role", "user").put("content", content))
        busy = true
        scope.launch {
            try {
                runTurn()
            } finally {
                busy = false
                streaming = ""
            }
        }
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg).imePadding()) {
        AgatePageHeader(title = "AI 运维助手", onOpenDrawer = onOpenDrawer) {
            Text(
                "设置",
                color = C.textSub,
                fontSize = 13.sp,
                modifier = Modifier
                    .clickable { showSettings = true }
                    .padding(horizontal = 8.dp, vertical = 5.dp),
            )
        }

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))

            if (!gatewayRunning) {
                NoticeCard(
                    title = "网关未运行",
                    body = "助手走网关的流式接口，需要网关处于运行状态。请先到「网关控制」启动网关，再回到这一页。",
                )
                Spacer(Modifier.height(10.dp))
            }
            if (config?.model.isNullOrBlank()) {
                NoticeCard(
                    title = "还没选对话模型",
                    body = "助手需要一个模型来驱动。点右上角「设置」选一个已启用的模型。",
                )
                Spacer(Modifier.height(10.dp))
            }
            error?.let {
                NoticeCard(title = "出错了", body = it, danger = true)
                Spacer(Modifier.height(10.dp))
            }

            if (items.isEmpty() && streaming.isEmpty()) {
                QuickChips(onPick = { send(it) })
            }

            items.forEach { item -> ChatBubble(item) }

            if (streaming.isNotEmpty()) {
                ChatBubble(ChatItem.Assistant(streaming, streaming = true))
            }

            pending?.let { p ->
                ConfirmCard(
                    tool = p,
                    onConfirm = { p.decision.complete(true) },
                    onCancel = { p.decision.complete(false) },
                )
            }

            Spacer(Modifier.height(16.dp))
        }

        // ── 输入区 ──
        Row(
            Modifier
                .fillMaxWidth()
                .background(C.cardBg)
                .padding(horizontal = 12.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(Modifier.weight(1f)) {
                AgateFormInput(
                    value = input,
                    onValueChange = { input = it },
                    hint = if (busy) "正在回复…" else "问点什么，或让它改配置",
                    singleLine = false,
                    minLines = 1,
                )
            }
            Spacer(Modifier.width(8.dp))
            val canSend = !busy && input.isNotBlank() && gatewayRunning
            Text(
                if (busy) "停止" else "发送",
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                color = if (canSend || busy) C.accent else C.textDim,
                modifier = Modifier
                    .border(
                        1.dp,
                        if (canSend || busy) C.accent.copy(alpha = 0.4f) else C.border,
                        RoundedCornerShape(9.dp),
                    )
                    .clickable(enabled = canSend || busy) {
                        if (busy) {
                            handle?.cancel()
                            busy = false
                        } else {
                            send(input)
                        }
                    }
                    .padding(horizontal = 14.dp, vertical = 8.dp),
            )
        }
    }

    if (showSettings) {
        AssistantSettingsSheet(
            config = config,
            onDismiss = { showSettings = false },
            onSaved = { saved ->
                config = saved
                showSettings = false
                toast("已保存")
            },
        )
    }
}

// ══════════════════════════════════════════════════════════════
//  数据
// ══════════════════════════════════════════════════════════════

private class TurnResult(
    val content: String,
    val calls: List<AssistantOps.ToolCall>,
    val error: String?,
)

private class PendingTool(
    val label: String,
    val name: String,
    val args: JSONObject,
    val decision: CompletableDeferred<Boolean>,
)

/** 对话流里的一项。 */
private sealed class ChatItem {
    class User(val text: String) : ChatItem()
    class Assistant(val text: String, val streaming: Boolean = false) : ChatItem()
    class Tool(
        val label: String,
        val args: String,
        val result: String?,
        val running: Boolean,
    ) : ChatItem()
}

// ══════════════════════════════════════════════════════════════
//  组件
// ══════════════════════════════════════════════════════════════

@Composable
private fun NoticeCard(title: String, body: String, danger: Boolean = false) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(14.dp))
            .border(
                1.dp,
                if (danger) C.red.copy(alpha = 0.35f) else C.border,
                RoundedCornerShape(14.dp),
            )
            .padding(12.dp),
    ) {
        Text(
            title,
            fontSize = 12.5.sp,
            fontWeight = FontWeight.SemiBold,
            color = if (danger) C.red else C.textMain,
        )
        Spacer(Modifier.height(4.dp))
        Text(body, fontSize = 11.5.sp, color = C.textSub, lineHeight = 16.sp)
    }
}

@Composable
private fun QuickChips(onPick: (String) -> Unit) {
    val C = agateColors()
    val prompts = listOf(
        "现在有哪些模型是启用的？",
        "哪些账号的额度快到期了？",
        "帮我看看有没有模型没配映射",
        "测一下最常用的那个模型通不通",
    )
    Column(Modifier.fillMaxWidth()) {
        Text("试试这些", fontSize = 11.sp, color = C.textDim)
        Spacer(Modifier.height(6.dp))
        prompts.forEach { text ->
            Text(
                text,
                fontSize = 12.5.sp,
                color = C.textMain,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(bottom = 6.dp)
                    .background(C.insetBg, RoundedCornerShape(10.dp))
                    .border(1.dp, C.border, RoundedCornerShape(10.dp))
                    .clickable { onPick(text) }
                    .padding(horizontal = 12.dp, vertical = 9.dp),
            )
        }
    }
}

@Composable
private fun ChatBubble(item: ChatItem) {
    val C = agateColors()
    when (item) {
        is ChatItem.User -> Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
            Text(
                item.text,
                fontSize = 13.sp,
                color = C.onAccent,
                modifier = Modifier
                    .widthIn()
                    .clip(RoundedCornerShape(12.dp))
                    .background(C.accent)
                    .padding(horizontal = 12.dp, vertical = 8.dp),
            )
        }

        is ChatItem.Assistant -> {
            Spacer(Modifier.height(8.dp))
            Text(
                item.text + if (item.streaming) "▌" else "",
                fontSize = 13.sp,
                color = C.textMain,
                lineHeight = 19.sp,
            )
            Spacer(Modifier.height(8.dp))
        }

        is ChatItem.Tool -> {
            Spacer(Modifier.height(6.dp))
            Column(
                Modifier
                    .fillMaxWidth()
                    .background(C.insetBg, RoundedCornerShape(10.dp))
                    .border(1.dp, C.border, RoundedCornerShape(10.dp))
                    .padding(10.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        if (item.running) "调用中" else "已调用",
                        fontSize = 10.sp,
                        color = if (item.running) C.amber else C.textDim,
                    )
                    Spacer(Modifier.width(6.dp))
                    Text(item.label, fontSize = 12.sp, fontWeight = FontWeight.Medium, color = C.textMain)
                }
                Spacer(Modifier.height(3.dp))
                Text(
                    item.args,
                    fontSize = 10.5.sp,
                    color = C.textDim,
                    style = AgateType.Num,
                    lineHeight = 14.sp,
                )
                item.result?.let { result ->
                    Spacer(Modifier.height(4.dp))
                    val ok = runCatching { JSONObject(result).optBoolean("ok", true) }.getOrDefault(true)
                    Text(
                        result,
                        fontSize = 10.5.sp,
                        color = if (ok) C.textSub else C.red,
                        style = AgateType.Num,
                        lineHeight = 14.sp,
                        maxLines = 8,
                    )
                }
            }
            Spacer(Modifier.height(6.dp))
        }
    }
}

@Composable
private fun ConfirmCard(tool: PendingTool, onConfirm: () -> Unit, onCancel: () -> Unit) {
    val C = agateColors()
    Spacer(Modifier.height(8.dp))
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(14.dp))
            .border(1.dp, C.amber.copy(alpha = 0.45f), RoundedCornerShape(14.dp))
            .padding(12.dp),
    ) {
        Text("需要你确认", fontSize = 12.5.sp, fontWeight = FontWeight.SemiBold, color = C.amber)
        Spacer(Modifier.height(4.dp))
        Text("助手想执行：${tool.label}", fontSize = 12.sp, color = C.textMain)
        Spacer(Modifier.height(4.dp))
        Text(
            tool.args.toString(2),
            fontSize = 10.5.sp,
            color = C.textDim,
            style = AgateType.Num,
            lineHeight = 14.sp,
            maxLines = 10,
        )
        Spacer(Modifier.height(10.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                "取消",
                fontSize = 12.5.sp,
                color = C.textSub,
                modifier = Modifier
                    .border(1.dp, C.border, RoundedCornerShape(8.dp))
                    .clickable(onClick = onCancel)
                    .padding(horizontal = 14.dp, vertical = 7.dp),
            )
            Text(
                "执行",
                fontSize = 12.5.sp,
                fontWeight = FontWeight.SemiBold,
                color = C.accent,
                modifier = Modifier
                    .border(1.dp, C.accent.copy(alpha = 0.4f), RoundedCornerShape(8.dp))
                    .clickable(onClick = onConfirm)
                    .padding(horizontal = 14.dp, vertical = 7.dp),
            )
        }
    }
}

/** 让气泡有最大宽度上限（纯文本长行不许铺满整屏）。 */
private fun Modifier.widthIn(): Modifier = this.fillMaxWidth(0.86f)

// ══════════════════════════════════════════════════════════════
//  设置弹层
// ══════════════════════════════════════════════════════════════

/**
 * 助手设置：对话模型 / 工具勾选 / 系统提示词。
 *
 * ── 工具列表为什么是「前端定义 + 后端展开」两段 ───────────────
 * 名称与说明写在前端（[AssistantOps.TOOLS]），而「这一项现在勾没勾」由后端
 * 展开后给（`AssistantConfig::status_value` 把缺省按默认集补成明确 true/false）。
 * 前端因此**不维护第二份默认集** —— 否则以后改默认值，两处必然对不上。
 */
@Composable
private fun AssistantSettingsSheet(
    config: AssistantOps.Config?,
    onDismiss: () -> Unit,
    onSaved: (AssistantOps.Config) -> Unit,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    var model by remember { mutableStateOf(config?.model) }
    var promptEnabled by remember { mutableStateOf(config?.systemPromptEnabled ?: true) }
    var prompt by remember { mutableStateOf(config?.systemPrompt.orEmpty()) }
    // 只记「显式改过」的项，未动的沿用 config 的判定（= 后端展开后的值）
    val picked = remember { mutableStateMapOf<String, Boolean>() }
    var models by remember { mutableStateOf<List<String>>(emptyList()) }
    var loading by remember { mutableStateOf(true) }
    var saveError by remember { mutableStateOf<String?>(null) }
    var saving by remember { mutableStateOf(false) }

    LaunchedEffect(Unit) {
        models = AssistantOps.availableModels()
        loading = false
    }

    fun isOn(id: String): Boolean = picked[id] ?: (config?.isEnabled(id) ?: (id in AssistantOps.DEFAULT_ENABLED))

    Box(
        Modifier
            .fillMaxSize()
            .background(Color(0x99000000))
            .clickable(onClick = onDismiss),
        contentAlignment = Alignment.BottomCenter,
    ) {
        Column(
            Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(topStart = 18.dp, topEnd = 18.dp))
                .background(C.cardBg)
                .clickable(enabled = false, onClick = {})
                .padding(16.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "助手设置",
                    fontSize = 15.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = C.textMain,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    "✕",
                    fontSize = 14.sp,
                    color = C.textDim,
                    modifier = Modifier.clickable(onClick = onDismiss).padding(6.dp),
                )
            }

            Spacer(Modifier.height(10.dp))
            Column(Modifier.heightIn(max = 460.dp).verticalScroll(rememberScrollState())) {

                // ── 对话模型 ──
                Text("对话模型", fontSize = 11.sp, color = C.textDim)
                Spacer(Modifier.height(6.dp))
                if (loading) {
                    Text("读取模型清单…", fontSize = 12.sp, color = C.textDim)
                } else if (models.isEmpty()) {
                    Text(
                        "没有可用的模型。先去「模型」页启用至少一个模型。",
                        fontSize = 11.5.sp,
                        color = C.amber,
                        lineHeight = 16.sp,
                    )
                } else {
                    Column {
                        models.take(30).forEach { id ->
                            val active = id == model
                            Row(
                                Modifier
                                    .fillMaxWidth()
                                    .padding(bottom = 4.dp)
                                    .clip(RoundedCornerShape(8.dp))
                                    .background(if (active) C.accent.copy(alpha = 0.12f) else Color.Transparent)
                                    .clickable { model = id }
                                    .padding(horizontal = 10.dp, vertical = 7.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Text(
                                    id,
                                    fontSize = 12.sp,
                                    color = if (active) C.accent else C.textMain,
                                    style = AgateType.Num,
                                    maxLines = 1,
                                    modifier = Modifier.weight(1f),
                                )
                                if (active) Text("✓", fontSize = 12.sp, color = C.accent)
                            }
                        }
                    }
                }

                Spacer(Modifier.height(14.dp))

                // ── 工具 ──
                Text("可用工具", fontSize = 11.sp, color = C.textDim)
                Spacer(Modifier.height(6.dp))
                AssistantOps.TOOLS.forEach { tool ->
                    val on = isOn(tool.id)
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .padding(bottom = 4.dp)
                            .clip(RoundedCornerShape(8.dp))
                            .clickable { picked[tool.id] = !on }
                            .padding(horizontal = 8.dp, vertical = 6.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Box(
                            Modifier
                                .width(18.dp)
                                .height(18.dp)
                                .background(
                                    if (on) C.accent else C.insetBg,
                                    RoundedCornerShape(5.dp),
                                )
                                .border(
                                    1.dp,
                                    if (on) C.accent else C.border,
                                    RoundedCornerShape(5.dp),
                                ),
                            contentAlignment = Alignment.Center,
                        ) {
                            if (on) Text("✓", fontSize = 11.sp, color = C.onAccent)
                        }
                        Spacer(Modifier.width(8.dp))
                        Column(Modifier.weight(1f)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(tool.label, fontSize = 12.5.sp, color = C.textMain)
                                if (tool.id in AssistantOps.MUTATING_TOOLS) {
                                    Spacer(Modifier.width(5.dp))
                                    Text("改动类", fontSize = 9.5.sp, color = C.amber)
                                }
                            }
                            Text(tool.description, fontSize = 10.5.sp, color = C.textDim, lineHeight = 14.sp)
                        }
                    }
                }
                Spacer(Modifier.height(2.dp))
                Text(
                    "一只工具都不勾时不会注入系统提示词（助手退化成纯聊天），这是内核的口径。",
                    fontSize = 10.5.sp,
                    color = C.textDim,
                    lineHeight = 14.sp,
                )

                Spacer(Modifier.height(14.dp))

                // ── 系统提示词 ──
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("注入系统提示词", fontSize = 11.sp, color = C.textDim, modifier = Modifier.weight(1f))
                    Text(
                        if (promptEnabled) "开" else "关",
                        fontSize = 12.sp,
                        color = if (promptEnabled) C.accent else C.textSub,
                        modifier = Modifier
                            .border(1.dp, C.border, RoundedCornerShape(7.dp))
                            .clickable { promptEnabled = !promptEnabled }
                            .padding(horizontal = 10.dp, vertical = 4.dp),
                    )
                }
                Spacer(Modifier.height(6.dp))
                AgateFormInput(
                    value = prompt,
                    onValueChange = { prompt = it },
                    hint = "留空 = 使用内置默认提示词",
                    singleLine = false,
                    minLines = 3,
                )
            }

            saveError?.let {
                Spacer(Modifier.height(8.dp))
                Text(it, fontSize = 11.5.sp, color = C.red, lineHeight = 15.sp)
            }

            Spacer(Modifier.height(12.dp))
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "取消",
                    fontSize = 13.sp,
                    color = C.textSub,
                    modifier = Modifier
                        .clickable(onClick = onDismiss)
                        .padding(horizontal = 12.dp, vertical = 7.dp),
                )
                Spacer(Modifier.weight(1f))
                Text(
                    if (saving) "保存中…" else "保存",
                    fontSize = 13.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = if (saving) C.textDim else C.accent,
                    modifier = Modifier
                        .border(
                            1.dp,
                            if (saving) C.border else C.accent.copy(alpha = 0.4f),
                            RoundedCornerShape(8.dp),
                        )
                        .clickable(enabled = !saving) {
                            saving = true
                            saveError = null
                            scope.launch {
                                // 只把**显式改过**的项合进原表，其余沿用后端展开值 ——
                                // 整表回发会把「没碰过的项」也钉死成当前默认，
                                // 以后内核改默认集，这些用户就跟不上了。
                                val merged = LinkedHashMap<String, Boolean>()
                                AssistantOps.TOOLS.forEach { merged[it.id] = isOn(it.id) }
                                val r = AssistantOps.saveConfig(
                                    model = model,
                                    systemPromptEnabled = promptEnabled,
                                    systemPrompt = prompt,
                                    tools = merged,
                                )
                                saving = false
                                if (r.ok && r.data != null) {
                                    onSaved(r.data)
                                } else {
                                    saveError = r.error ?: "保存失败"
                                }
                            }
                        }
                        .padding(horizontal = 14.dp, vertical = 8.dp),
                )
            }
        }
    }
}
