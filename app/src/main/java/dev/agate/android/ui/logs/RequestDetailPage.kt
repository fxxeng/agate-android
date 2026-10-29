package dev.agate.android.ui.logs

import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.ChatTraceOps
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.StatsOps
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import dev.agate.core.GatewayPrefs
import kotlinx.coroutines.launch

/**
 * 请求详情页（调用记录的下钻）—— 对齐 v44 `sub-reqdetail`。
 *
 * 三段：
 *   ① 概览：下游模型 → 上游模型、来源客户端、总用时
 *   ② 耗时拆解：首响前（TTFB）/ 内容下发 / 合计，外加生成速度与用量
 *   ③ 尝试明细：每次上游尝试各一行（失败时**默认展开**）
 *   ④ 报文原文：调试模式采集的请求 / 响应正文（折叠）
 *
 * ── 为什么「耗时拆解」只有三段而不是五段 ─────────────────────
 * V44 原稿画了五阶段（排队 / 连接 / 等待响应 / 响应中 / 完成）。内核落库的是
 * 「开始时刻 + 总用时 + 首响时刻」，**没有排队与连接各自的耗时** —— 五段里有
 * 两段我们拿不到数。与其编两个看起来像模像样的 0ms，不如只画能画准的三段：
 * 首响前 = firstResponseMs，内容下发 = durationMs − 首响，合计 = durationMs。
 * 顺带说明：这个拆法正是上游自己算「生成速度」用的口径
 * （输出 token ÷ 流式段用时，流式段 = 总用时 − 首响）。
 *
 * 在途请求另有 `phase` + `phaseElapsedMs`（内核给的当前阶段与已耗时），
 * 那种情况直接显示阶段名与已用时，不参与上面的拆解。
 */
@Composable
fun RequestDetailPage(
    requestId: String,
    onBack: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val clipboard = LocalClipboardManager.current

    var entry by remember { mutableStateOf<StatsOps.Entry?>(null) }
    var raw by remember { mutableStateOf<StatsOps.RawBody?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var rawError by remember { mutableStateOf<String?>(null) }
    var rawLoading by remember { mutableStateOf(false) }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    DisposableEffect(Unit) {
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
        onDispose { }
    }

    LaunchedEffect(requestId) {
        val p = StatsOps.page(limit = 200)
        if (p.ok && p.data != null) {
            entry = p.data.entries.firstOrNull { it.id == requestId }
            if (entry == null) error = "这条请求已不在列表中（可能已被清理）"
        } else {
            error = p.error ?: "读取失败"
        }
    }

    fun loadRaw() {
        scope.launch {
            rawLoading = true
            val r = StatsOps.raw(requestId)
            rawLoading = false
            if (r.ok) {
                raw = r.data
                rawError = null
            } else {
                // 失败也要记下来：AI 轨迹卡靠 rawError 决定「不渲染」还是
                // 「显示原因」，两种都不该静默
                rawError = r.error ?: "读取报文失败"
            }
        }
    }

    // 进页面就拉一次：AI 轨迹要它才能解析。原先只在用户点开「报文原文」时
    // 才拉，那是因为只有那一张卡需要；现在轨迹也要，晚拉等于轨迹永远不出现。
    LaunchedEffect(requestId) { loadRaw() }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "请求详情", onOpenDrawer = {}, onBack = onBack) {
            entry?.let { e ->
                when {
                    e.running -> AgateBadge(phaseLabel(e.phase), tone = BadgeTone.AMBER)
                    e.ok -> AgateBadge("成功 ${e.status}", tone = BadgeTone.GREEN)
                    else -> AgateBadge(
                        if (e.status > 0) "失败 ${e.status}" else "失败",
                        tone = BadgeTone.RED,
                    )
                }
            }
        }

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))

            if (error != null) {
                Box(
                    Modifier
                        .fillMaxWidth()
                        .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
                        .padding(20.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(error!!, fontSize = 13.sp, color = C.red)
                }
            } else if (entry == null) {
                Box(
                    Modifier
                        .fillMaxWidth()
                        .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
                        .padding(vertical = 28.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Text("正在加载请求详情…", fontSize = 13.sp, color = C.textDim)
                }
            } else {
                val e = entry!!
                OverviewCard(entry = e)
                Spacer(Modifier.height(10.dp))
                TimingCard(entry = e)
                Spacer(Modifier.height(10.dp))
                AttemptsCard(entry = e)

                // ── AI 轨迹：与「报文原文」共用同一份 raw ──
                // 两者都靠 StatsOps.raw(id) 那一次请求，所以只在这里解析一次、
                // 传下去。让两个卡片各发一次请求既浪费又会出现「一个加载完、
                // 另一个还在转」的错位。
                val trace = remember(raw) {
                    raw?.let {
                        ChatTraceOps.parse(it.requestBody, it.responseBody, it.truncated)
                    }
                }
                // 只有真的有东西可看时才占位；trace 为 null（还没拉 raw）时
                // 不显示 —— 拉取动作由「报文原文」那张卡的点击触发
                trace?.takeIf { !it.isEmpty }?.let { t ->
                    Spacer(Modifier.height(10.dp))
                    TraceCard(trace = t)
                }

                Spacer(Modifier.height(10.dp))
                RawCard(
                    raw = raw,
                    rawError = rawError,
                    loading = rawLoading,
                    onLoad = { loadRaw() },
                    onCopy = { text ->
                        clipboard.setText(AnnotatedString(text))
                        toast("已复制")
                    },
                )
            }

            Spacer(Modifier.height(20.dp))
        }
    }
}

// ══════════════════════════════════════════════════════════════
//  卡片
// ══════════════════════════════════════════════════════════════

/** 卡片外壳（统一圆角 16 + 边框 + inset 底）。 */
@Composable
private fun CardShell(
    title: String,
    subtitle: String? = null,
    trailing: (@Composable () -> Unit)? = null,
    content: @Composable ColumnScope.() -> Unit,
) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
            .border(1.dp, C.border, RoundedCornerShape(16.dp))
            .padding(12.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(title, fontSize = 13.5.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
            if (subtitle != null) {
                Spacer(Modifier.width(8.dp))
                Text(subtitle, fontSize = 10.5.sp, color = C.textDim, modifier = Modifier.weight(1f))
            } else {
                Spacer(Modifier.weight(1f))
            }
            if (trailing != null) trailing()
        }
        Spacer(Modifier.height(6.dp))
        content()
    }
}

/** ① 概览：模型映射关系 + 来源 + 总用时。 */
@Composable
private fun OverviewCard(entry: StatsOps.Entry) {
    val C = agateColors()
    CardShell(title = entry.model, subtitle = fmtClock(entry.ts)) {
        val target = entry.upstreamModel.ifBlank { entry.model }
        val asked = entry.clientModel.ifBlank { entry.model }
        Text(
            text = if (asked == target) "转发模型 $target" else "下游 $asked → 上游 $target",
            fontSize = 12.sp,
            color = C.textSub,
        )
        entry.error?.let {
            Spacer(Modifier.height(4.dp))
            Text(it, fontSize = 11.5.sp, color = C.red, lineHeight = 16.sp)
        }
    }
}

/** ② 耗时拆解：首响前 / 内容下发 / 合计 + 生成速度 + 用量。 */
@Composable
private fun TimingCard(entry: StatsOps.Entry) {
    val C = agateColors()
    CardShell(title = "耗时拆解") {
        if (entry.running) {
            Text(
                text = "进行中 · " + phaseLabel(entry.phase) +
                    (if (entry.phaseElapsedMs >= 0) " 已用时 " + fmtMs(entry.phaseElapsedMs) else ""),
                fontSize = 12.sp,
                color = C.amber,
            )
        } else {
            val first = entry.firstResponseMs.takeIf { it >= 0 }
            TimingRow("首响前（含思考）", first?.let { fmtMs(it) } ?: "—")
            TimingRow(
                "内容下发",
                if (first != null) fmtMs((entry.durationMs - first).coerceAtLeast(0)) else "—",
            )
            TimingRow("合计", fmtMs(entry.durationMs))
            if (entry.completionTokens > 0 && first != null) {
                val streamMs = (entry.durationMs - first).coerceAtLeast(1)
                val speed = entry.completionTokens * 1000.0 / streamMs
                Text(
                    text = "生成速度 ${speed.toInt()} tok/s · 用量 ${fmtCount(entry.totalTokens)} tok" +
                        if (entry.cacheReadTokens > 0) " · 缓存读取 ${fmtCount(entry.cacheReadTokens)}" else "",
                    fontSize = 11.sp,
                    color = C.textDim,
                )
            }
        }
    }
}

@Composable
private fun TimingRow(label: String, value: String) {
    val C = agateColors()
    Row(Modifier.fillMaxWidth().padding(vertical = 3.dp)) {
        Text(label, fontSize = 12.sp, color = C.textSub, modifier = Modifier.weight(1f))
        Text(value, fontSize = 12.sp, color = C.textMain, style = AgateType.Num)
    }
}

/** ③ 尝试明细：失败自动展开。 */
@Composable
private fun AttemptsCard(entry: StatsOps.Entry) {
    val C = agateColors()
    // 失败时用户要的就是原因，不该再点一下 —— 默认展开；成功则收起
    var open by remember { mutableStateOf(!entry.ok) }
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
        .border(1.dp, C.border, RoundedCornerShape(16.dp)),
    ) {
        Row(
            Modifier
                .fillMaxWidth()
                .clickable { open = !open }
                .padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("尝试明细", fontSize = 13.5.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
            Spacer(Modifier.weight(1f))
            Text("共 ${entry.attempts} 次", fontSize = 10.5.sp, color = C.textDim)
            Spacer(Modifier.width(6.dp))
            Text(if (open) "⌃" else "⌄", fontSize = 13.sp, color = C.textDim)
        }
        if (open) {
            Box(Modifier.fillMaxWidth().height(1.dp).background(C.border))
            Column(Modifier.fillMaxWidth().padding(12.dp)) {
                if (entry.attemptDetails.isEmpty()) {
                    Text("请求在任何账号接手之前就失败了（没有尝试记录）", fontSize = 12.sp, color = C.textDim)
                } else {
                    entry.attemptDetails.forEach { attempt ->
                        Row(
                            Modifier
                                .fillMaxWidth()
                                .padding(vertical = 6.dp),
                            verticalAlignment = Alignment.Top,
                        ) {
                            Text(
                                "${attempt.index}.",
                                fontSize = 11.5.sp,
                                color = C.textDim,
                                modifier = Modifier.width(20.dp),
                                style = AgateType.Num,
                            )
                            Column(Modifier.weight(1f)) {
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    Text(
                                        text = attempt.account.ifBlank { "—" },
                                        fontSize = 12.5.sp,
                                        color = if (attempt.ok) C.green else C.textMain,
                                        modifier = Modifier.weight(1f, fill = false),
                                    )
                                    // 这一轮的状态码：内核一直记着（`AttemptDetail.status`，
                                    // -1 表示没走到有状态码那一步），但界面此前只画了账号名 ——
                                    // 「同一账号为什么换了下一轮」这件事只有状态码能回答
                                    // （429 退避 / 5xx / 401）。
                                    if (attempt.status > 0) {
                                        Spacer(Modifier.width(6.dp))
                                        Text(
                                            attempt.status.toString(),
                                            fontSize = 10.5.sp,
                                            color = if (attempt.ok) C.green else C.red,
                                            style = AgateType.Num,
                                        )
                                    }
                                    if (attempt.retryCount > 0) {
                                        Spacer(Modifier.width(6.dp))
                                        Text(
                                            "内部重试 ${attempt.retryCount} 次",
                                            fontSize = 10.5.sp,
                                            color = C.textDim,
                                        )
                                    }
                                }
                                // notice（如「代理回退直连」）比 error 更该先讲：
                                // 它说明这一轮换了通路，不是纯粹的失败。
                                val note = attempt.notice ?: attempt.error
                                if (!note.isNullOrBlank()) {
                                    Text(note, fontSize = 11.sp, color = C.textDim, lineHeight = 15.sp)
                                }
                            }
                        }
                        if (attempt !== entry.attemptDetails.last()) {
                            Box(
                                Modifier
                                    .fillMaxWidth()
                                    .height(1.dp)
                                    .background(C.border.copy(alpha = 0.5f)),
                            )
                        }
                    }
                }
            }
        }
    }
}

/**
 * AI 轨迹：把一次请求里「模型想了什么 / 调了什么工具 / 工具回了什么」摊开。
 *
 * 对齐上游 3.0.8 请求详情弹窗的三段折叠（思考过程 / 调用工具 / 工具结果）。
 * 数据来源是 `StatsOps.raw` 的同一次响应 —— 这里不自己发请求。
 *
 * **整卡只在有内容时渲染**（调用点已经用 `!it.isEmpty` 挡过）。这不是省事，
 * 而是刻意：未开调试模式时两侧都不采，画一个空折叠条会让人以为「数据丢了」，
 * 而实际是「本来就没采」。
 */
@Composable
private fun TraceCard(trace: ChatTraceOps.Trace) {
    val C = agateColors()
    // 默认全折叠：这三段动辄几百行，一进来就铺开会把下面所有卡片顶出屏幕
    var openReasoning by remember { mutableStateOf(false) }
    var openCalls by remember { mutableStateOf(false) }
    var openResults by remember { mutableStateOf(false) }

    val summary = buildList {
        if (trace.reasoning.isNotBlank()) add("思考 ${trace.reasoning.length} 字")
        if (trace.toolCalls.isNotEmpty()) add("${trace.toolCalls.size} 次工具调用")
        if (trace.toolResults.isNotEmpty()) add("${trace.toolResults.size} 条工具结果")
    }.joinToString(" · ")

    CardShell(
        title = "AI 轨迹",
        subtitle = summary.ifBlank { null },
        trailing = if (trace.truncated) {
            { Text("可能不完整", fontSize = 10.5.sp, color = C.amber) }
        } else {
            null
        },
    ) {
        if (trace.reasoning.isNotBlank()) {
            TraceSection(
                title = "思考过程",
                open = openReasoning,
                onToggle = { openReasoning = !openReasoning },
                body = trace.reasoning,
                tone = C.blue,
            )
        }
        if (trace.toolCalls.isNotEmpty()) {
            if (trace.reasoning.isNotBlank()) Spacer(Modifier.height(8.dp))
            TraceSection(
                title = "调用工具",
                open = openCalls,
                onToggle = { openCalls = !openCalls },
                body = trace.toolCalls.joinToString("\n\n") { call ->
                    // 工具名单独一行、参数缩进，比把两者挤在一行好读
                    "${call.name.ifBlank { "（未命名）" }}(${call.id})\n" +
                        ChatTraceOps.prettyJson(call.arguments)
                },
                tone = C.accent,
            )
        }
        if (trace.toolResults.isNotEmpty()) {
            if (trace.reasoning.isNotBlank() || trace.toolCalls.isNotEmpty()) {
                Spacer(Modifier.height(8.dp))
            }
            TraceSection(
                title = "工具结果",
                open = openResults,
                onToggle = { openResults = !openResults },
                body = trace.toolResults.joinToString("\n\n") { r ->
                    "${r.name.ifBlank { r.callId }.ifBlank { "（未知工具）" }}\n${r.content}"
                },
                tone = C.green,
            )
        }
    }
}

/** 一段可折叠的轨迹内容。折叠头始终一行，展开后正文限高并内部滚动。 */
@Composable
private fun TraceSection(
    title: String,
    open: Boolean,
    onToggle: () -> Unit,
    body: String,
    tone: androidx.compose.ui.graphics.Color,
) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .background(C.insetBg, RoundedCornerShape(10.dp))
            .border(1.dp, C.border, RoundedCornerShape(10.dp)),
    ) {
        Row(
            Modifier
                .fillMaxWidth()
                .clickable { onToggle() }
                .padding(horizontal = 10.dp, vertical = 9.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            // 小色点当分类标记：三段各自的色调不同，扫一眼能分辨
            Box(Modifier.size(6.dp).background(tone, RoundedCornerShape(3.dp)))
            Spacer(Modifier.width(8.dp))
            Text(title, fontSize = 12.5.sp, fontWeight = FontWeight.Medium, color = C.textMain)
            Spacer(Modifier.weight(1f))
            Text(if (open) "⌃" else "⌄", fontSize = 13.sp, color = C.textDim)
        }
        if (open) {
            Box(Modifier.fillMaxWidth().height(1.dp).background(C.border))
            Box(
                Modifier
                    .fillMaxWidth()
                    .heightIn(min = 60.dp, max = 260.dp)
                    .verticalScroll(rememberScrollState())
                    .padding(10.dp),
            ) {
                Text(
                    text = body.ifBlank { "（空）" },
                    fontSize = 11.sp,
                    color = C.textSub,
                    lineHeight = 15.sp,
                    style = AgateType.Num,
                )
            }
        }
    }
}

/** ④ 报文原文：按需拉取，折叠展示。 */
@Composable
private fun RawCard(
    raw: StatsOps.RawBody?,
    rawError: String?,
    loading: Boolean,
    onLoad: () -> Unit,
    onCopy: (String) -> Unit,
) {
    val C = agateColors()
    var open by remember { mutableStateOf(false) }
    var section by remember { mutableStateOf(0) }   // 0 请求 / 1 响应

    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
        .border(1.dp, C.border, RoundedCornerShape(16.dp)),
    ) {
        Row(
            Modifier
                .fillMaxWidth()
                .clickable {
                    if (!open && raw == null) onLoad()
                    open = !open
                }
                .padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("报文原文", fontSize = 13.5.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
            Spacer(Modifier.weight(1f))
            Text(
                when {
                    loading -> "读取中…"
                    raw == null && rawError != null -> "未采集"
                    raw?.truncated == true -> "正文可能已被截断"
                    else -> "凭据已替换为 [redacted]"
                },
                fontSize = 10.5.sp,
                color = C.textDim,
            )
            Spacer(Modifier.width(6.dp))
            Text(if (open) "⌃" else "⌄", fontSize = 13.sp, color = C.textDim)
        }

        if (open) {
            Box(Modifier.fillMaxWidth().height(1.dp).background(C.border))
            Column(Modifier.fillMaxWidth().padding(12.dp)) {
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    FilterChip(text = "请求", active = section == 0, onClick = { section = 0 })
                    FilterChip(text = "响应", active = section == 1, onClick = { section = 1 })
                    Spacer(Modifier.weight(1f))
                    if (raw != null) {
                        Text(
                            "复制",
                            fontSize = 11.5.sp,
                            color = C.accent,
                            modifier = Modifier
                                .clickable { onCopy(if (section == 0) raw!!.requestBody else raw!!.responseBody) }
                                .padding(horizontal = 6.dp, vertical = 4.dp),
                        )
                    }
                }
                Spacer(Modifier.height(8.dp))
                val text = when {
                    raw == null -> rawError ?: if (loading) "正在加载…" else "没有报文"
                    section == 0 -> raw!!.requestBody
                    else -> raw!!.responseBody
                }
                Box(
                    Modifier
                        .fillMaxWidth()
                        .heightIn(min = 90.dp, max = 260.dp)
                        .background(C.insetBg, RoundedCornerShape(10.dp))
                        .border(1.dp, C.border, RoundedCornerShape(10.dp))
                        .verticalScroll(rememberScrollState())
                        .padding(10.dp),
                ) {
                    Text(
                        text = text.ifBlank { "无内容" },
                        fontSize = 11.sp,
                        color = C.textSub,
                        lineHeight = 15.sp,
                        style = AgateType.Num,
                    )
                }
                // 这里不再补一句「未开启调试模式」—— `raw == null` 时上面的正文框
                // 已经显示 `rawError`，而那串文案本身就是
                // 「这条请求没有保存报文（未开启调试模式时新请求不采集）」。
                // 旧版把提示写成 `rawError == null` 才渲染，而 loadRaw 失败时
                // rawError 必不为 null —— 那段是永远走不到的死代码。
            }
        }
    }
}
