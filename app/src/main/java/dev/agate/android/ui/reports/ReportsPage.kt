package dev.agate.android.ui.reports

import android.app.DatePickerDialog
import android.widget.Toast
import androidx.compose.foundation.Canvas
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
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.ReportOps
import dev.agate.android.ui.components.AgateHeatmap
import dev.agate.android.ui.components.AgateHeatmapLegend
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.AgateStatRow
import dev.agate.android.ui.components.StatCell
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import dev.agate.core.GatewayPrefs
import kotlinx.coroutines.launch
import java.util.Calendar

/**
 * 数据报表页 —— 对齐 v44 `sub-reports`。
 *
 * 自上而下：时间档位 → 概览 → 消耗趋势（按日）→ 近 24 小时双轴 →
 * 近 365 天热力图 → 用量占比（提供商 / 模型 / 账号）→ 三张排行。
 *
 * ── 时间档位为什么不是原稿的六档 ─────────────────────────────
 * v44 原稿写的是「10分 / 1时 / 24时 / 7日 / 30日 / 本月」，但内核
 * `/api/stats/summary` 的 `RANGES` 是 `today / 7 / 30 / month / all / custom`
 * —— 它按**天**聚合，根本没有分钟级的桶（`GET /api/stats/summary` 的注释与
 * `request_stats::report::normalize_range` 都只认这六个）。
 * 原稿是按 Node 版画的，这里以内核为准：**不做一个查不到数的时间档**。
 *
 * ── 为什么一次只在最外层拉一次 ───────────────────────────────
 * 九个板块用的是同一份聚合结果（见 `ReportOps.Summary` 的说明），拆开请求会让
 * 「概览的请求量」与「三张排行的合计」来自不同时刻 —— 而这两个数在内核里是
 * 恒等的（同源同区间），分两次拉就会让页面上出现对不上的数字。
 *
 * ── 命中率为什么自己画而不复用 AgateDualAxisChart ─────────────
 * 那个组件的右轴读数会 `coerceIn(0f, 1f)`。而缓存命中率**合法地会超过 100%**
 * （上游把 cacheRead 与 promptTokens 分开报时，命中率就是 会大于 1），
 * 内核 `safe_rate` 也明确写着「不把结果夹到 1.0，那是拿数据真实性换观感」。
 * 复用会把真实比值压平，所以这里按真实最大值定右轴。
 */
@Composable
fun ReportsPage(
    onOpenDrawer: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val clipboard = LocalClipboardManager.current

    var range by remember { mutableStateOf(ReportOps.Range.WEEK) }
    // 自定义区间的起止日（`yyyy-MM-dd`，本地自然日）。只在该档位生效。
    var startDate by remember { mutableStateOf<String?>(null) }
    var endDate by remember { mutableStateOf<String?>(null) }
    var data by remember { mutableStateOf<ReportOps.Summary?>(null) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    fun reload() {
        // 自定义档在两端日期都选齐之前**不发请求**：内核 `stats_summary` 对
        // `range=custom` 缺 `startDate`/`endDate` 是硬 400（见
        // `parse_day_param` 的调用处），切到该档的瞬间必然缺参数。
        // 与其先弹一次错、再等用户选完才恢复，不如保持上一次的结果不动。
        if (range.isCustom && (startDate == null || endDate == null)) {
            return
        }
        loading = true
        scope.launch {
            val r = ReportOps.summary(range = range, startDate = startDate, endDate = endDate)
            loading = false
            if (r.ok && r.data != null) {
                data = r.data
                error = null
            } else {
                error = r.error ?: "读取失败"
            }
        }
    }

    DisposableEffect(Unit) {
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
        onDispose { }
    }
    LaunchedEffect(range, startDate, endDate) { reload() }

    fun pickDate(current: String?, onPicked: (String) -> Unit) {
        val cal = Calendar.getInstance()
        current?.split("-")?.takeIf { it.size == 3 }?.let { parts ->
            parts.getOrNull(0)?.toIntOrNull()?.let { cal.set(Calendar.YEAR, it) }
            parts.getOrNull(1)?.toIntOrNull()?.let { cal.set(Calendar.MONTH, it - 1) }
            parts.getOrNull(2)?.toIntOrNull()?.let { cal.set(Calendar.DAY_OF_MONTH, it) }
        }
        DatePickerDialog(
            ctx,
            { _, year, month, day ->
                onPicked(String.format("%04d-%02d-%02d", year, month + 1, day))
            },
            cal.get(Calendar.YEAR),
            cal.get(Calendar.MONTH),
            cal.get(Calendar.DAY_OF_MONTH),
        ).show()
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "数据报表", onOpenDrawer = onOpenDrawer) {
            Text(
                "复制 CSV",
                color = C.accent,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clickable(enabled = data != null) {
                        val csv = data?.let { toCsv(it) }
                        if (csv.isNullOrBlank()) {
                            toast("还没有数据可导出")
                        } else {
                            clipboard.setText(AnnotatedString(csv))
                            toast("已复制 CSV 到剪贴板")
                        }
                    }
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

            // ── 时间档位（六档，横向可滑）──
            Row(
                Modifier
                    .fillMaxWidth()
                    .horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                ReportOps.Range.entries.forEach { item ->
                    val active = item == range
                    Text(
                        item.label,
                        fontSize = 12.sp,
                        color = if (active) C.accent else C.textSub,
                        modifier = Modifier
                            .background(
                                if (active) C.accent.copy(alpha = 0.12f) else C.insetBg,
                                RoundedCornerShape(9.dp),
                            )
                            .border(
                                1.dp,
                                if (active) C.accent.copy(alpha = 0.35f) else C.border,
                                RoundedCornerShape(9.dp),
                            )
                            .clickable { range = item }
                            .padding(horizontal = 11.dp, vertical = 6.dp),
                    )
                }
            }

            // 自定义区间：两端各一个日期按钮。仅在「自定义」档下出现 ——
            // 其他档位传了内核也不读（见 `summarize` 的 `custom` 参数）。
            if (range.isCustom) {
                Spacer(Modifier.height(8.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    DateChip(
                        label = "开始",
                        value = startDate ?: "选择",
                        onClick = { pickDate(startDate) { startDate = it } },
                    )
                    Spacer(Modifier.width(8.dp))
                    Text("—", fontSize = 12.sp, color = C.textDim)
                    Spacer(Modifier.width(8.dp))
                    DateChip(
                        label = "结束",
                        value = endDate ?: "选择",
                        onClick = { pickDate(endDate) { endDate = it } },
                    )
                }
            }

            Spacer(Modifier.height(10.dp))

            val s = data
            if (range.isCustom && (startDate == null || endDate == null)) {
                // 状态与档位必须一致：此时上面选中的是「自定义」，若继续展示上一档
                // 的数，页面就在暗示"自定义的结果和近 7 天一样"。
                Box(
                    Modifier
                        .fillMaxWidth()
                        .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
                        .border(1.dp, C.border, RoundedCornerShape(16.dp))
                        .padding(20.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Text("选择起止日期后开始统计", fontSize = 13.sp, color = C.textDim)
                }
            } else if (error != null) {
                ErrorBox(error!!)
            } else if (s == null) {
                Box(
                    Modifier
                        .fillMaxWidth()
                        .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
                        .padding(vertical = 28.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(if (loading) "正在统计…" else "暂无数据", fontSize = 13.sp, color = C.textDim)
                }
            } else {
                SectionGap()
                OverviewCard(s)
                SectionGap()
                TrendCard(s)
                SectionGap()
                CacheTrendCard(s)
                SectionGap()
                HeatmapCard(s)
                SectionGap()
                ShareCard(s)
                SectionGap()
                RankCard("Top 账号", "按请求量", s.accounts.map {
                    RankLine(it.label, it.requests, it.requests.toDouble() / s.overview.requests.coerceAtLeast(1))
                })
                SectionGap()
                RankCard("提供商排名", "按 Token", s.providers.map {
                    RankLine(it.label, it.tokens, it.tokens.toDouble() / s.overview.tokens.coerceAtLeast(1))
                })
                SectionGap()
                RankCard("Top 模型", "按 Token", s.models.map {
                    RankLine(it.label, it.tokens, it.tokens.toDouble() / s.overview.tokens.coerceAtLeast(1))
                })
            }

            Spacer(Modifier.height(20.dp))
        }
    }
}

@Composable
private fun SectionGap() = Spacer(Modifier.height(10.dp))

@Composable
private fun ErrorBox(message: String) {
    val C = agateColors()
    Box(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
            .border(1.dp, C.border, RoundedCornerShape(16.dp))
            .padding(20.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(message, fontSize = 13.sp, color = C.red)
    }
}

@Composable
private fun DateChip(label: String, value: String, onClick: () -> Unit) {
    val C = agateColors()
    Row(
        Modifier
            .background(C.insetBg, RoundedCornerShape(9.dp))
            .border(1.dp, C.border, RoundedCornerShape(9.dp))
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text("$label ", fontSize = 10.5.sp, color = C.textDim)
        Text(value, fontSize = 12.sp, color = C.textMain, style = AgateType.Num)
    }
}

/** 卡片外壳：统一圆角 16 + 边框 + 标题行。 */
@Composable
private fun ReportCard(
    title: String,
    subtitle: String? = null,
    trailing: (@Composable () -> Unit)? = null,
    content: @Composable () -> Unit,
) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
            .border(1.dp, C.border, RoundedCornerShape(16.dp))
            .padding(14.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(title, fontSize = 13.5.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
                if (subtitle != null) {
                    Text(subtitle, fontSize = 10.5.sp, color = C.textDim)
                }
            }
            trailing?.invoke()
        }
        Spacer(Modifier.height(10.dp))
        content()
    }
}

// ══════════════════════════════════════════════════════════════
//  概览
// ══════════════════════════════════════════════════════════════

@Composable
private fun OverviewCard(s: ReportOps.Summary) {
    val C = agateColors()
    val o = s.overview
    Column {
        AgateStatRow(
            cells = listOf(
                StatCell("请求", fmtCount(o.requests)),
                StatCell("成功率", fmtPct(o.successRate), if (o.requests > 0) C.green else null),
                StatCell("Token", fmtCount(o.tokens)),
                StatCell("连续", "${o.streak} 天", if (o.streak > 0) C.amber else null),
            ),
        )
        Spacer(Modifier.height(8.dp))
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text(
                "活跃 ${o.activeDays} 天 · 失败 ${fmtCount(o.failures)}",
                fontSize = 11.sp,
                color = C.textDim,
                modifier = Modifier.weight(1f),
            )
            s.topModel?.let { top ->
                Text(
                    "冠军 ${top.labelOrModel()} ${fmtPct(top.percentage)}",
                    fontSize = 11.sp,
                    color = C.accent,
                    maxLines = 1,
                )
            }
        }
    }
}

private fun ReportOps.TopModel.labelOrModel(): String =
    model.ifBlank { "未知" }

// ══════════════════════════════════════════════════════════════
//  消耗趋势（按日）
// ══════════════════════════════════════════════════════════════

@Composable
private fun TrendCard(s: ReportOps.Summary) {
    val C = agateColors()
    val days = s.trend
    ReportCard(
        title = "消耗趋势",
        subtitle = "按日 · Token · ${s.startDate} → ${s.endDate}",
    ) {
        if (days.isEmpty()) {
            EmptyHint("这一档没有可画的数据")
            return@ReportCard
        }
        val max = (days.maxOfOrNull { it.tokens } ?: 0L).coerceAtLeast(1L)
        Canvas(Modifier.fillMaxWidth().height(96.dp)) {
            val n = days.size
            // 柱子多于 40 根时留 1px 缝；否则按 55% 宽度居中，视觉更像柱状图
            val slot = size.width / n
            val barW = if (n > 40) (slot - 1f).coerceAtLeast(1f) else slot * 0.55f
            days.forEachIndexed { index, day ->
                val ratio = day.tokens.toFloat() / max
                val h = (size.height - 2f) * ratio
                val left = slot * index + (slot - barW) / 2f
                drawRect(
                    color = C.accent.copy(alpha = if (day.tokens > 0) 0.75f else 0.15f),
                    topLeft = Offset(left, size.height - h),
                    size = Size(barW, h.coerceAtLeast(if (day.tokens > 0) 1f else 0f)),
                )
            }
        }
        Spacer(Modifier.height(6.dp))
        Row(Modifier.fillMaxWidth()) {
            Text(days.first().date, fontSize = 10.sp, color = C.textDim, modifier = Modifier.weight(1f))
            Text(days.last().date, fontSize = 10.sp, color = C.textDim)
        }
        // 悬停不可用的移动端：把峰值与合计直接写出来，比"能点才看得到"更有用
        val total = days.sumOf { it.tokens }
        Spacer(Modifier.height(4.dp))
        Text(
            "峰值 ${fmtCount(max)} · 合计 ${fmtCount(total)}",
            fontSize = 11.sp,
            color = C.textDim,
        )
    }
}

// ══════════════════════════════════════════════════════════════
//  近 24 小时双轴（Token × 缓存命中率）
// ══════════════════════════════════════════════════════════════

@Composable
private fun CacheTrendCard(s: ReportOps.Summary) {
    val C = agateColors()
    val hours = s.cacheTrend
    val label = s.cacheRates["last24h"]
    ReportCard(
        title = "近 24 小时",
        subtitle = "左轴 Token · 右轴缓存命中率",
        trailing = label?.let {
            {
                Text(
                    "命中 ${fmtPct(it.rate)}",
                    fontSize = 11.sp,
                    color = C.green,
                    style = AgateType.Num,
                )
            }
        },
    ) {
        if (hours.isEmpty()) {
            EmptyHint("近 24 小时没有请求")
            return@ReportCard
        }
        val maxTokens = (hours.maxOfOrNull { it.totalTokens } ?: 0L).coerceAtLeast(1L)
        // 右轴按**真实**最大值定（命中率可超 100%，见本文件头部的说明）
        val maxRate = (hours.maxOfOrNull { it.rate } ?: 0.0).coerceAtLeast(0.01)
        Canvas(Modifier.fillMaxWidth().height(92.dp)) {
            val n = hours.size
            val slot = size.width / n
            val barW = (slot * 0.55f).coerceAtLeast(1f)
            hours.forEachIndexed { index, hour ->
                val h = (size.height - 2f) * (hour.totalTokens.toFloat() / maxTokens)
                drawRect(
                    color = C.blue.copy(alpha = 0.55f),
                    topLeft = Offset(slot * index + (slot - barW) / 2f, size.height - h),
                    size = Size(barW, h.coerceAtLeast(0f)),
                )
            }
            // 命中率折线（右轴）
            val points = hours.mapIndexed { index, hour ->
                val x = slot * index + slot / 2f
                val ratio = (hour.rate / maxRate).toFloat().coerceIn(0f, 1f)
                Offset(x, size.height - size.height * ratio)
            }
            points.zipWithNext().forEach { (a, b) ->
                drawLine(Color(0xFF22C55E), a, b, strokeWidth = 1.6f)
            }
            points.forEach { drawCircle(Color(0xFF22C55E), radius = 1.6f, center = it) }
        }
        Spacer(Modifier.height(6.dp))
        Row(Modifier.fillMaxWidth()) {
            Text(hours.first().hour, fontSize = 10.sp, color = C.textDim, modifier = Modifier.weight(1f))
            Text(hours.last().hour, fontSize = 10.sp, color = C.textDim)
        }
        Spacer(Modifier.height(4.dp))
        Text(
            "命中率 = 缓存读 Token ÷ 输入 Token，可超 100%，如实展示不夹取",
            fontSize = 10.5.sp,
            color = C.textDim,
            lineHeight = 14.sp,
        )
    }
}

// ══════════════════════════════════════════════════════════════
//  365 天热力图
// ══════════════════════════════════════════════════════════════

@Composable
private fun HeatmapCard(s: ReportOps.Summary) {
    val C = agateColors()
    val cells = s.heatmap
    val active = cells.count { it.requests > 0 }
    ReportCard(
        title = "近 365 天 · 活跃热力图",
        trailing = {
            Text("$active / ${cells.size} 天活跃", fontSize = 10.5.sp, color = C.textDim)
        },
    ) {
        if (cells.isEmpty()) {
            EmptyHint("没有热力图数据")
            return@ReportCard
        }
        val max = (cells.maxOfOrNull { it.requests } ?: 0L).coerceAtLeast(1L)
        // 组件按「每列 7 格（一周）」排布，365 天 ≈ 53 列；不足的尾巴补 0。
        AgateHeatmap(
            values = cells.map { it.requests.toFloat() / max },
            weeks = 53,
        )
        Spacer(Modifier.height(6.dp))
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            AgateHeatmapLegend()
            Spacer(Modifier.weight(1f))
            Text(
                "峰值 ${fmtCount(max)} 次/天",
                fontSize = 10.sp,
                color = C.textDim,
            )
        }
        Spacer(Modifier.height(4.dp))
        Text(
            "方格为本地自然日，无请求为空槽 · 档位按窗口峰值自动定 · 不受上方时间范围影响",
            fontSize = 10.5.sp,
            color = C.textDim,
            lineHeight = 14.sp,
        )
    }
}

// ══════════════════════════════════════════════════════════════
//  用量占比（三维）
// ══════════════════════════════════════════════════════════════

@Composable
private fun ShareCard(s: ReportOps.Summary) {
    var tab by remember { mutableIntStateOf(0) }
    val rows: List<RankLine> = when (tab) {
        1 -> s.models.map {
            RankLine(it.label, it.tokens, it.tokens.toDouble() / s.overview.tokens.coerceAtLeast(1))
        }
        2 -> s.accounts.map {
            RankLine(it.label, it.tokens, it.tokens.toDouble() / s.overview.tokens.coerceAtLeast(1))
        }
        else -> s.providers.map {
            RankLine(it.label, it.tokens, it.tokens.toDouble() / s.overview.tokens.coerceAtLeast(1))
        }
    }
    ReportCard(title = "用量占比 · 三维", subtitle = "按 Token 份额") {
        AgateSegmented(
            options = listOf("提供商", "模型", "账号"),
            selected = tab,
            onSelect = { tab = it },
        )
        Spacer(Modifier.height(10.dp))
        if (rows.isEmpty()) {
            EmptyHint("这一维没有用量")
        } else {
            rows.take(8).forEachIndexed { index, row ->
                ShareBar(label = row.label, value = row.value, ratio = row.ratio, rank = index)
                if (index != rows.take(8).lastIndex) Spacer(Modifier.height(8.dp))
            }
        }
    }
}

@Composable
private fun ShareBar(label: String, value: Long, ratio: Double, rank: Int) {
    val C = agateColors()
    val color = when (rank) {
        0 -> C.red
        1 -> C.amber
        2 -> C.blue
        else -> C.textDim
    }
    Column(Modifier.fillMaxWidth()) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                label,
                fontSize = 11.5.sp,
                color = C.textMain,
                maxLines = 1,
                modifier = Modifier.weight(1f),
            )
            Text(
                "${fmtPct(ratio)} · ${fmtCount(value)}",
                fontSize = 10.5.sp,
                color = C.textDim,
                style = AgateType.Num,
            )
        }
        Spacer(Modifier.height(3.dp))
        Box(
            Modifier
                .fillMaxWidth()
                .height(4.dp)
                .background(C.insetBg, RoundedCornerShape(2.dp)),
        ) {
            Box(
                Modifier
                    .fillMaxWidth(ratio.coerceIn(0.0, 1.0).toFloat())
                    .height(4.dp)
                    .background(color, RoundedCornerShape(2.dp)),
            )
        }
    }
}

// ══════════════════════════════════════════════════════════════
//  排行
// ══════════════════════════════════════════════════════════════

private class RankLine(val label: String, val value: Long, val ratio: Double)

@Composable
private fun RankCard(title: String, subtitle: String, rows: List<RankLine>) {
    val C = agateColors()
    ReportCard(title = title, subtitle = subtitle) {
        if (rows.isEmpty()) {
            EmptyHint("这一档没有数据")
            return@ReportCard
        }
        rows.take(6).forEachIndexed { index, row ->
            Row(
                Modifier.fillMaxWidth().padding(vertical = 3.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    "${index + 1}",
                    fontSize = 11.sp,
                    color = if (index < 3) C.accent else C.textDim,
                    style = AgateType.Num,
                    modifier = Modifier.width(18.dp),
                )
                Text(
                    row.label,
                    fontSize = 12.sp,
                    color = C.textMain,
                    maxLines = 1,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    fmtCount(row.value),
                    fontSize = 12.sp,
                    color = C.textMain,
                    style = AgateType.Num,
                )
                Spacer(Modifier.width(8.dp))
                Text(
                    fmtPct(row.ratio),
                    fontSize = 10.5.sp,
                    color = C.textDim,
                    style = AgateType.Num,
                    modifier = Modifier.width(38.dp),
                )
            }
        }
    }
}

@Composable
private fun EmptyHint(text: String) {
    val C = agateColors()
    Text(text, fontSize = 11.5.sp, color = C.textDim)
}

// ══════════════════════════════════════════════════════════════
//  格式化
// ══════════════════════════════════════════════════════════════

/** 计数缩写：`1234` → `1.2k`，`1284500` → `1.28M`。 */
internal fun fmtCount(value: Long): String = when {
    value >= 100_000_000L -> String.format("%.2f亿", value / 100_000_000.0)
    value >= 1_000_000L -> String.format("%.2fM", value / 1_000_000.0)
    value >= 10_000L -> String.format("%.1f万", value / 10_000.0)
    value >= 1_000L -> String.format("%.1fk", value / 1_000.0)
    else -> value.toString()
}

/** 比率 → 百分比。入参是 0..1 的比值（内核 `rate` / `percentage` 都是这个口径）。 */
internal fun fmtPct(ratio: Double): String {
    if (!ratio.isFinite()) return "0%"
    val pct = ratio * 100.0
    return if (pct >= 100.0) String.format("%.0f%%", pct) else String.format("%.1f%%", pct)
}

/**
 * 生成 CSV（概览 + 排行 + 逐日趋势）。
 *
 * ── 为什么在前端拼而不是让内核加个导出端点 ───────────────────
 * 报表要导出的就是**当前页面上这份**聚合结果（含当前时间档与自定义区间）。
 * 让内核再算一遍会引入"导出的与看到的不是同一次查询"的可能，而这份数据
 * 已经在手里了 —— 直接拼最忠实。
 *
 * 字段一律加引号并转义内部引号：模型名与账号名里出现逗号是常事
 * （`gpt-4o, preview` 这类），不加引号会把列数撑乱。
 */
private fun toCsv(s: ReportOps.Summary): String {
    fun cell(value: Any?): String = "\"" + value.toString().replace("\"", "\"\"") + "\""
    val sb = StringBuilder()
    sb.appendLine("section,key,requests,tokens,ratio")
    sb.appendLine(
        listOf(
            cell("overview"),
            cell("${s.startDate}~${s.endDate}"),
            cell(s.overview.requests),
            cell(s.overview.tokens),
            cell(fmtPct(s.overview.successRate)),
        ).joinToString(","),
    )
    s.providers.forEach {
        sb.appendLine(
            listOf(
                cell("provider"), cell(it.label), cell(it.requests), cell(it.tokens),
                cell(fmtPct(it.tokens.toDouble() / s.overview.tokens.coerceAtLeast(1))),
            ).joinToString(","),
        )
    }
    s.models.forEach {
        sb.appendLine(
            listOf(
                cell("model"), cell(it.label), cell(it.requests), cell(it.tokens),
                cell(fmtPct(it.tokens.toDouble() / s.overview.tokens.coerceAtLeast(1))),
            ).joinToString(","),
        )
    }
    s.accounts.forEach {
        sb.appendLine(
            listOf(
                cell("account"), cell(it.label), cell(it.requests), cell(it.tokens),
                cell(fmtPct(it.tokens.toDouble() / s.overview.tokens.coerceAtLeast(1))),
            ).joinToString(","),
        )
    }
    s.trend.forEach {
        sb.appendLine(
            listOf(cell("day"), cell(it.date), cell(it.requests), cell(it.tokens), cell("")).joinToString(","),
        )
    }
    return sb.toString()
}
