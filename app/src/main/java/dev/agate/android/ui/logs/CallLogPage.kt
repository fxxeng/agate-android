package dev.agate.android.ui.logs

import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.StatsOps
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.AgateStatRow
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.StatCell
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateShape
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * 调用记录页（原 M3 骨架，本次补实）—— 对齐 v44 `sub-calllog`。
 *
 * 两个 Tab：
 * - **转发请求**：每次转发一行，状态按阶段显示，进行中的可就地终止；
 * - **调用报文**：调试模式采集的出站原文（`GET /api/debug/traffic`），
 *   点行展开看请求 / 响应正文。
 *
 * ── 统计条为什么是一行 ─────────────────────────────────────
 * 见 [AgateStatRow] 的说明：四个读数只配一行，不再各占一张卡。
 *
 * ── 停留本页自动刷新 ───────────────────────────────────────
 * 日志页的价值在「此刻正在发生什么」，所以进页面后按需轮询；退到后台
 * 就没必要了 —— 用 `DisposableEffect` 把定时器绑在页面生命周期上，
 * 离开即停（与控制台页同一取向）。
 */
@Composable
fun CallLogPage(
    onOpenDrawer: () -> Unit = {},
    onOpenDetail: (String) -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val snap by GatewayRepository.snapshot.collectAsState()

    var tab by remember { mutableIntStateOf(0) }
    var page by remember { mutableStateOf<StatsOps.Page?>(null) }
    var filters by remember { mutableStateOf<StatsOps.Filters?>(null) }
    var status by remember { mutableStateOf(StatsOps.StatusFilter.ALL) }
    var provider by remember { mutableStateOf<String?>(null) }
    var model by remember { mutableStateOf<String?>(null) }
    var sensitiveOnly by remember { mutableStateOf(false) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var showClear by remember { mutableStateOf(false) }
    var showProvider by remember { mutableStateOf(false) }
    var showModel by remember { mutableStateOf(false) }
    var showStatus by remember { mutableStateOf(false) }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    /** 长文案提示（清理口径说明这类需要读完的句子）。 */
    fun toastLong(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_LONG).show()

    fun reload() {
        if (loading) return
        loading = true
        scope.launch {
            val p = StatsOps.page(status = status, provider = provider, model = model)
            val f = StatsOps.filters()
            loading = false
            if (p.ok && p.data != null) {
                page = p.data
                error = null
            } else {
                error = p.error ?: "读取失败"
            }
            if (f.ok && f.data != null) filters = f.data
        }
    }

    DisposableEffect(Unit) {
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
        onDispose { }
    }
    LaunchedEffect(Unit) { reload() }

    // 事件驱动：一条请求收尾（内核 `request_done`）就立刻重拉，而不是等下一轮
    // 轮询（空闲时是 15s）。轮询保留为兜底 —— 事件丢了也不会一直陈旧。
    // 收尾很密时（流式并发）由走廊里的 reload 自身节流（在途去重）。
    LaunchedEffect(Unit) {
        GatewayRepository.bus.collect { kind ->
            if (kind == "request_done") reload()
        }
    }

    // 停留本页自动刷新：进行中有请求时 3s 一轮，否则 15s —— 界面不动时省电，
    // 有在途请求时才跟得紧。离开页面（DisposableEffect onDispose）即停。
    LaunchedEffect(status, provider, model) {
        while (true) {
            delay(if ((page?.running ?: 0L) > 0L) 3_000L else 15_000L)
            reload()
        }
    }

    val entries = page?.entries ?: emptyList()
    val shown = if (sensitiveOnly) entries.filter { it.sensitiveHits.isNotEmpty() } else entries
    val activeFilters = (if (provider != null) 1 else 0) +
        (if (model != null) 1 else 0) +
        (if (status != StatsOps.StatusFilter.ALL) 1 else 0) +
        (if (sensitiveOnly) 1 else 0)

    fun resetFilters() {
        provider = null
        model = null
        status = StatsOps.StatusFilter.ALL
        sensitiveOnly = false
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "调用记录", onOpenDrawer = onOpenDrawer) {
            Text(
                if (loading) "刷新中…" else "刷新",
                color = C.accent,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clickable(enabled = !loading) { reload() }
                    .padding(horizontal = 8.dp, vertical = 5.dp),
            )
            Text(
                "清理…",
                color = C.textSub,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clickable { showClear = true }
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

            AgateSegmented(
                options = listOf("转发请求", "调用报文"),
                selected = tab,
                onSelect = { tab = it },
            )

            Spacer(Modifier.height(10.dp))

            // ── 统计条（一行四读数）──
            val overviewRequests = snap.requestsToday
            val overviewSuccess = snap.successfulToday
            AgateStatRow(
                cells = listOf(
                    StatCell("今日", overviewRequests.toString()),
                    StatCell("成功", overviewSuccess.toString(), C.green),
                    StatCell("失败", (overviewRequests - overviewSuccess).coerceAtLeast(0).toString(),
                        if (overviewRequests > overviewSuccess) C.red else null),
                    StatCell("进行中", page?.running?.toString() ?: "—",
                        if ((page?.running ?: 0L) > 0L) C.amber else null),
                ),
            )

            Spacer(Modifier.height(10.dp))

            // ── 筛选 chips ──
            Column(
                Modifier
                    .fillMaxWidth()
                    .background(C.cardBg, RoundedCornerShape(14.dp))
                    .border(1.dp, C.border, RoundedCornerShape(14.dp))
                    .padding(10.dp),
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    FilterChip(
                        text = "提供商：" + (provider?.let { p -> filters?.providers?.firstOrNull { it.id == p }?.label ?: p } ?: "全部"),
                        active = provider != null,
                        onClick = { showProvider = true },
                    )
                    FilterChip(
                        text = "模型：" + (model ?: "全部"),
                        active = model != null,
                        onClick = { showModel = true },
                    )
                    FilterChip(
                        text = "状态：" + statusLabel(status),
                        active = status != StatsOps.StatusFilter.ALL,
                        onClick = { showStatus = true },
                    )
                    FilterChip(
                        text = "敏",
                        active = sensitiveOnly,
                        tone = if (sensitiveOnly) BadgeTone.AMBER else BadgeTone.GRAY,
                        onClick = { sensitiveOnly = !sensitiveOnly },
                    )
                    if (activeFilters > 0) {
                        FilterChip(
                            text = "清除 ✕",
                            active = true,
                            tone = BadgeTone.RED,
                            onClick = { resetFilters() },
                        )
                    }
                }
                if (activeFilters > 0) {
                    Spacer(Modifier.height(6.dp))
                    Text(
                        "命中 ${shown.size} / ${entries.size} 条",
                        fontSize = 11.sp,
                        color = C.textDim,
                    )
                }
            }

            Spacer(Modifier.height(10.dp))

            if (error != null) {
                Box(
                    Modifier
                        .fillMaxWidth()
                        .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
                        .padding(20.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(error!!, fontSize = 13.sp, color = C.red)
                }
            } else if (entries.isEmpty() && !loading) {
                Box(
                    Modifier
                        .fillMaxWidth()
                        .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
                        .padding(vertical = 28.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Text("暂无请求日志，网关还没有转发过请求", fontSize = 13.sp, color = C.textDim)
                }
            } else {
                Column(
                    Modifier
                        .fillMaxWidth()
                        .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
                        .padding(12.dp),
                ) {
                    Text(
                        "转发请求 · 共 ${page?.matched ?: 0} 条",
                        fontSize = 12.sp,
                        color = C.textDim,
                    )
                    Spacer(Modifier.height(4.dp))
                    shown.forEach { entry ->
                        RequestRow(entry = entry, onClick = {
                            if (entry.hasRaw) onOpenDetail(entry.id)
                        }, onTerminate = {
                            scope.launch {
                                val r = StatsOps.terminate(entry.id)
                                if (r.ok) {
                                    toast("已受理终止：上游连接已断开，列表稍后刷新")
                                    reload()
                                } else {
                                    toast(r.error ?: "终止失败")
                                }
                            }
                        })
                    }
                    if (sensitiveOnly && shown.isEmpty()) {
                        Spacer(Modifier.height(8.dp))
                        Text("没有符合筛选条件的请求", fontSize = 12.sp, color = C.textDim)
                    }
                }
            }

            Spacer(Modifier.height(12.dp))

            // ── 日志清理（折叠组）──
            ClearGroup(
                busy = loading,
                expanded = showClear,
                onExpandedChange = { showClear = it },
                onClear = { mode ->
                    scope.launch {
                        val r = StatsOps.clear(mode = mode, status = status, provider = provider, model = model)
                        if (r.ok) {
                            val n = r.data?.deleted ?: 0L
                            toast(if (mode == "raw") "已抹掉 $n 条报文原文" else "已删除 $n 条")
                            // 带筛选删除时日报不会重算，内核会附一口径说明；这条
                            // 不讲清楚用户会以为报表漏了自己的删除操作。
                            r.data?.note?.let { toastLong(it) }
                            reload()
                        } else {
                            toast(r.error ?: "清理失败")
                        }
                    }
                },
                onCompact = {
                    scope.launch {
                        val r = StatsOps.compact()
                        toast(if (r.ok) "压缩已在后台开始，完成后自动提示" else (r.error ?: "压缩失败"))
                    }
                },
            )

            Spacer(Modifier.height(10.dp))
            Text(
                "模型调用明细在这里查看 · App 自身运行事件见「应用日志」",
                fontSize = 11.sp,
                color = C.textDim,
            )
            Spacer(Modifier.height(20.dp))
        }
    }

    if (showProvider) {
        FilterSheet(
            title = "按提供商筛选",
            options = listOf(null to "全部") +
                (filters?.providers?.map { it.id to it.label } ?: emptyList()),
            selected = provider,
            onPick = { provider = it },
            onDismiss = { showProvider = false },
        )
    }

    if (showModel) {
        FilterSheet(
            title = "按模型筛选",
            options = listOf(null to "全部") +
                (filters?.models?.map { it to it } ?: emptyList()),
            selected = model,
            onPick = { model = it },
            onDismiss = { showModel = false },
        )
    }

    if (showStatus) {
        FilterSheet(
            title = "按请求结果筛选",
            options = listOf(
                null to "全部",
                StatsOps.StatusFilter.OK.value to "成功",
                StatsOps.StatusFilter.ERROR.value to "失败",
                StatsOps.StatusFilter.RUNNING.value to "进行中",
            ),
            selected = status.value,
            onPick = { v -> status = StatsOps.StatusFilter.entries.firstOrNull { it.value == v } ?: StatsOps.StatusFilter.ALL },
            onDismiss = { showStatus = false },
        )
    }
}

// ══════════════════════════════════════════════════════════════
//  行 / 筛选 / 清理
// ══════════════════════════════════════════════════════════════

/** 状态筛选的展示文案。 */
internal fun statusLabel(filter: StatsOps.StatusFilter): String = when (filter) {
    StatsOps.StatusFilter.ALL -> "全部"
    StatsOps.StatusFilter.OK -> "成功"
    StatsOps.StatusFilter.ERROR -> "失败"
    StatsOps.StatusFilter.RUNNING -> "进行中"
}

/** 转发阶段 → 中文（内核 `LogPhase::as_str()` 的字面量）。 */
internal fun phaseLabel(phase: String): String = when (phase) {
    "queued" -> "排队中"
    "connecting" -> "连接中"
    "waiting" -> "等待响应"
    "streaming" -> "响应中"
    "retrying" -> "重试中"
    else -> "进行中"
}

/** 一条转发请求。进行中的行右侧给「终止」而不是箭头。 */
@Composable
private fun RequestRow(
    entry: StatsOps.Entry,
    onClick: () -> Unit,
    onTerminate: () -> Unit,
) {
    val C = agateColors()
    Row(
        Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(vertical = 9.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    entry.model,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = C.textMain,
                    maxLines = 1,
                    overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
                Spacer(Modifier.width(6.dp))
                if (entry.running) {
                    AgateBadge(phaseLabel(entry.phase), tone = BadgeTone.AMBER)
                } else if (entry.ok) {
                    AgateBadge("成功 ${entry.status}", tone = BadgeTone.GREEN)
                } else {
                    AgateBadge(
                        if (entry.status > 0) "失败 ${entry.status}" else "失败",
                        tone = BadgeTone.RED,
                    )
                }
                if (entry.sensitiveHits.isNotEmpty()) {
                    Spacer(Modifier.width(4.dp))
                    Text("敏", fontSize = 10.sp, color = C.amber)
                }
            }
            Spacer(Modifier.height(2.dp))
            Text(
                entry.subLine(),
                fontSize = 10.sp,
                color = C.textDim,
                maxLines = 1,
                overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
            )
        }
        Spacer(Modifier.width(8.dp))
        if (entry.running) {
            Text(
                "终止",
                fontSize = 11.sp,
                color = C.red,
                modifier = Modifier
                    .border(1.dp, C.red.copy(alpha = 0.35f), RoundedCornerShape(7.dp))
                    .clickable(onClick = onTerminate)
                    .padding(horizontal = 9.dp, vertical = 4.dp),
            )
        } else {
            Text("›", fontSize = 14.sp, color = C.textDim)
        }
    }
}

/** 副行：提供商·账号 · 用时（/ 首响）· 重试次数。 */
private fun StatsOps.Entry.subLine(): String {
    val parts = ArrayList<String>()
    val who = listOf(providerLabel, accountName).filter { it.isNotBlank() }.joinToString("·")
    if (who.isNotBlank()) parts += who
    val duration = if (running) {
        if (phaseElapsedMs >= 0) "已用时 ${fmtMs(phaseElapsedMs)}" else "进行中"
    } else {
        val first = firstResponseMs.takeIf { it >= 0 }
        if (first != null) "${fmtMs(durationMs)} / 首响 ${fmtMs(first)}" else fmtMs(durationMs)
    }
    parts += duration
    if (attempts > 1) parts += "↻ ${attempts - 1} 次"
    if (isTest) parts += "测试"
    return parts.joinToString(" · ")
}

/** 毫秒 → 可读时长。 */
internal fun fmtMs(value: Long): String = when {
    value < 0 -> "—"
    value < 1000 -> "${value}ms"
    value < 60_000 -> String.format(java.util.Locale.US, "%.2fs", value / 1000.0)
    else -> {
        val minutes = value / 60_000
        val seconds = (value % 60_000) / 1000
        "${minutes}分${seconds}秒"
    }
}

/** 数字千分位（token 计数用）。 */
internal fun fmtCount(value: Long): String = String.format(java.util.Locale.US, "%,d", value)

/** 时间 → HH:mm:ss */
internal fun fmtClock(ts: Long): String {
    if (ts <= 0) return "--:--:--"
    val format = java.text.SimpleDateFormat("HH:mm:ss", java.util.Locale.US)
    return format.format(java.util.Date(ts))
}
