package dev.agate.android.ui.tasks

import android.widget.Toast
import androidx.compose.foundation.background
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
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.BeijingDay
import dev.agate.android.data.CheckinOps
import dev.agate.android.data.CheckinRecords
import dev.agate.android.data.PresetRegistry
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateFormInput
import dev.agate.android.ui.components.AgateSubPageHeader
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch

/**
 * 签到记录 —— 对齐上游 3.0.8 `RecordsTab`（`checkin-records-tab.tsx`）。
 *
 * ── 这一页不是「签到台账」──────────────────────────────────
 * 上游已主动淘汰「一条一轮 + 失败明细前 5 条」的轮次台账（`checkin-page.tsx`
 * 文件头写着「原『最近签到记录』时间线已迁出，记录长成了明细」）。
 * 轮次台账回答不了「浣熊-C 上周三到底失败在哪」，明细能。
 * 轮次视角仍然存在，但降为签到中心总览卡上的一个读数。
 *
 * ── 三件事必须讲清 ─────────────────────────────────────────
 * 1. **时区**：日期是北京时间（后端 `date` 字段就是这个口径）。界面**不重算**
 *    `checkedInToday` 那种布尔，只格式化时刻 —— 跟手机时区走会让海外设备
 *    看到与内核落账不同的一天。
 * 2. **结果判据只在内核一处**：`success / already / pending / failed` 由后端
 *    落账时判，界面只按 outcome 上色，不自己再判一遍。
 * 3. **清空护栏**：带筛选 = 只删命中的；不带筛选必须显式 `all=1`。
 *    两条提示语分开说 —— 混成一句会让人以为「清空全部」也包括筛选内的。
 */
@Composable
fun CheckinRecordsPage(
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    var page by remember { mutableStateOf<CheckinRecords.Page?>(null) }
    var range by remember { mutableStateOf(CheckinRecords.Range.ALL) }
    var outcome by remember { mutableStateOf<String?>(null) }
    var provider by remember { mutableStateOf<String?>(null) }
    var keyword by remember { mutableStateOf("") }
    var offset by remember { mutableIntStateOf(0) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var showClear by remember { mutableStateOf(false) }
    var showOutcomeMenu by remember { mutableStateOf(false) }
    var showProviderMenu by remember { mutableStateOf(false) }

    val limit = 50

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()
    fun toastLong(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_LONG).show()

    fun reload(resetOffset: Boolean = false) {
        if (loading) return
        if (resetOffset) offset = 0
        loading = true
        scope.launch {
            val p = CheckinRecords.query(
                outcome = outcome,
                provider = provider,
                keyword = keyword.trim().ifBlank { null },
                range = range,
                offset = offset,
                limit = limit,
            )
            loading = false
            if (p != null) {
                page = p
                error = null
            } else {
                error = "读取失败（内核未就绪或端点不可用）"
            }
        }
    }

    DisposableEffect(Unit) {
        val key = GatewayPrefs.apiKey(ctx)
        CheckinOps.configure(key)
        onDispose { }
    }

    // 进页面与筛选变化都重查；从第一页开始（offset 归零）
    LaunchedEffect(range, outcome, provider) { reload(resetOffset = true) }

    val records = page?.records ?: emptyList()
    val total = page?.total ?: 0
    val hasFilter = outcome != null || provider != null || keyword.isNotBlank() || range != CheckinRecords.Range.ALL

    fun clear(mode: ClearMode) {
        scope.launch {
            val r = when (mode) {
                ClearMode.MATCHING -> CheckinRecords.clearMatching(
                    outcome = outcome,
                    provider = provider,
                    keyword = keyword.trim().ifBlank { null },
                    range = range,
                )
                ClearMode.ALL -> CheckinRecords.clearAll()
            }
            if (r.ok) {
                val removed = r.data?.optLong("removed", 0) ?: 0
                // 两条提示语分开说：一条讲「删了几个」，一条讲「删的是哪一批」
                toast("已删除 $removed 条")
                if (mode == ClearMode.ALL) toastLong("已清空全部签到记录")
                else toastLong("只删了当前筛选命中的记录，其余保留")
                reload(resetOffset = true)
            } else {
                toast(r.error ?: "删除失败")
            }
        }
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgateSubPageHeader("签到记录", onBack = onBack) {
            Text(
                "清空",
                color = C.red,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clickable { showClear = true }
                    .padding(horizontal = 10.dp, vertical = 6.dp),
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
                options = CheckinRecords.Range.entries.map { it.label },
                selected = CheckinRecords.Range.entries.indexOf(range),
                onSelect = { range = CheckinRecords.Range.entries[it] },
            )
            Spacer(Modifier.height(9.dp))

            // ── 筛选 ──
            Row(
                modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                FilterPill(
                    text = "结果：" + (outcome?.let { CheckinRecords.Outcome.of(it)?.label ?: it } ?: "全部"),
                    active = outcome != null,
                ) { showOutcomeMenu = true }
                FilterPill(
                    text = "提供商：" + (provider?.let { PresetRegistry.labelOf(it) } ?: "全部"),
                    active = provider != null,
                ) { showProviderMenu = true }
                if (hasFilter) {
                    FilterPill(text = "清除 ✕", active = true, danger = true) {
                        outcome = null
                        provider = null
                        keyword = ""
                        range = CheckinRecords.Range.ALL
                    }
                }
            }
            Spacer(Modifier.height(8.dp))
            AgateFormInput(
                value = keyword,
                onValueChange = { keyword = it },
                hint = "搜账号 / 收益 / 错误",
            )
            if (keyword.isNotBlank()) {
                Spacer(Modifier.height(6.dp))
                TaskBtn("按关键词查询", outlined = true) { reload(resetOffset = true) }
            }

            Spacer(Modifier.height(8.dp))
            Text(
                "${page?.windowText(offset) ?: "0"} / $total 条",
                style = AgateType.Caption,
                color = C.textDim,
            )
            Spacer(Modifier.height(8.dp))

            when {
                error != null -> TaskCard {
                    Text(error!!, color = C.red, fontSize = 13.sp)
                }

                records.isEmpty() && !loading -> TaskCard {
                    Text(
                        if (hasFilter) "没有符合筛选条件的记录" else "还没有签到记录",
                        style = AgateType.BodyS,
                        color = C.textDim,
                    )
                    Spacer(Modifier.height(4.dp))
                    TaskHint("记录在每次签到落账时写入（定时 / 启动补签 / 批量 / 单账号 / 活动领取五种来源）。")
                }

                else -> TaskCard {
                    records.forEachIndexed { index, entry ->
                        if (index > 0) TaskDivider()
                        RecordRow(entry)
                    }
                }
            }

            // ── 分页 ──
            if (total > limit) {
                Spacer(Modifier.height(10.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        "第 ${offset / limit + 1} / ${(total + limit - 1) / limit} 页",
                        style = AgateType.Caption,
                        color = C.textDim,
                        modifier = Modifier.weight(1f),
                    )
                    TaskBtn("‹ 上一页", enabled = offset > 0 && !loading) {
                        offset = (offset - limit).coerceAtLeast(0)
                        reload()
                    }
                    Spacer(Modifier.width(6.dp))
                    TaskBtn("下一页 ›", enabled = offset + limit < total && !loading) {
                        offset += limit
                        reload()
                    }
                }
            }

            Spacer(Modifier.height(10.dp))
            TaskFold(
                "时区：日期是北京时间（上游自然日口径）。跟手机时区走会让海外部署下「今日已签」错位。" +
                    "结果判据只在内核一处 —— 界面按 outcome 上色，不自己再判一遍。" +
                    "清空：带筛选只删命中的；不带筛选必须显式 all=1（后端拦 400）。"
            )
            TaskGap(24)
        }
    }

    if (showOutcomeMenu) {
        ChoiceDialog(
            title = "结果",
            options = listOf<Pair<String?, String>>(null to "全部结果") +
                CheckinRecords.Outcome.entries.map { it.value to it.label },
            current = outcome,
            onPick = {
                outcome = it
                showOutcomeMenu = false
            },
            onDismiss = { showOutcomeMenu = false },
        )
    }

    if (showProviderMenu) {
        ChoiceDialog(
            title = "提供商",
            options = listOf<Pair<String?, String>>(null to "全部提供商") +
                records.map { it.provider }.distinct().filter { it.isNotBlank() }
                    .map { it to PresetRegistry.labelOf(it) },
            current = provider,
            onPick = {
                provider = it
                showProviderMenu = false
            },
            onDismiss = { showProviderMenu = false },
        )
    }

    if (showClear) {
        AlertDialog(
            onDismissRequest = { showClear = false },
            title = { Text("清空签到记录", color = C.textMain, fontSize = 16.sp) },
            text = {
                Text(
                    if (hasFilter) "只删除**当前筛选命中**的记录（$total 条），其余保留。"
                    else "将删除**全部**签到记录。这个操作不可撤销。",
                    color = C.textSub,
                    fontSize = 13.sp,
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    showClear = false
                    clear(if (hasFilter) ClearMode.MATCHING else ClearMode.ALL)
                }) {
                    Text(if (hasFilter) "删除命中项" else "全部清空", color = C.red)
                }
            },
            dismissButton = {
                TextButton(onClick = { showClear = false }) {
                    Text("取消", color = C.textSub)
                }
            },
            containerColor = C.cardBg,
        )
    }
}

private enum class ClearMode { MATCHING, ALL }

/** 一行明细：时间 / 账号 / 结果 / 说明（+ 来源与保活角标）。 */
@Composable
private fun RecordRow(entry: CheckinRecords.Entry) {
    val C = agateColors()
    val outcome = entry.outcomeEnum
    val tone = when (outcome) {
        CheckinRecords.Outcome.SUCCESS -> C.green
        CheckinRecords.Outcome.ALREADY -> C.blue
        CheckinRecords.Outcome.PENDING -> C.amber
        CheckinRecords.Outcome.FAILED -> C.red
        null -> C.textDim
    }
    Row(
        Modifier.fillMaxWidth().padding(vertical = 8.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Column(Modifier.width(48.dp)) {
            Text(
                if (entry.ts > 0) beijingClock(entry.ts) else "—",
                style = AgateType.Num.copy(fontSize = 12.sp),
                color = C.textMain,
            )
            Text(
                if (entry.date.isNotBlank()) entry.date.takeLast(5) else beijingMonthDay(entry.ts),
                style = AgateType.Caption,
                color = C.textDim,
            )
        }
        Spacer(Modifier.width(8.dp))
        Column(Modifier.weight(1f)) {
            Text(
                entry.accountName.ifBlank { entry.accountId },
                color = C.textMain,
                fontSize = 13.sp,
                fontWeight = FontWeight.Medium,
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    PresetRegistry.labelOf(entry.provider),
                    style = AgateType.Caption,
                    color = C.textDim,
                )
                Text(" · ", style = AgateType.Caption, color = C.textDim)
                Text(
                    CheckinRecords.sourceLabel(entry.source),
                    style = AgateType.Caption,
                    color = C.textDim,
                )
                if (entry.keptActive) {
                    Text(" · 已保活", style = AgateType.Caption, color = C.blue)
                }
            }
            if (entry.detail.isNotBlank()) {
                Text(
                    entry.detail,
                    style = AgateType.Caption,
                    color = if (outcome == CheckinRecords.Outcome.FAILED) C.red else C.textSub,
                    lineHeight = 15.sp,
                    modifier = Modifier.padding(top = 2.dp),
                )
            }
        }
        Spacer(Modifier.width(6.dp))
        TaskTag(outcome?.label ?: entry.outcome.ifBlank { "—" }, tone)
    }
}

@Composable
private fun FilterPill(
    text: String,
    active: Boolean,
    danger: Boolean = false,
    onClick: () -> Unit,
) {
    val C = agateColors()
    val color = when {
        danger -> C.red
        active -> C.accent
        else -> C.textSub
    }
    Text(
        text,
        color = color,
        fontSize = 11.sp,
        fontWeight = if (active) FontWeight.SemiBold else FontWeight.Normal,
        modifier = Modifier
            .background(
                if (active) color.copy(alpha = 0.12f) else C.insetBg,
                RoundedCornerShape(999.dp),
            )
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 5.dp),
    )
}

@Composable
private fun ChoiceDialog(
    title: String,
    options: List<Pair<String?, String>>,
    current: String?,
    onPick: (String?) -> Unit,
    onDismiss: () -> Unit,
) {
    val C = agateColors()
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title, color = C.textMain, fontSize = 16.sp) },
        text = {
            Column(
                Modifier
                    .fillMaxWidth()
                    .verticalScroll(rememberScrollState()),
            ) {
                options.forEach { (value, label) ->
                    val picked = value == current
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .clickable { onPick(value) }
                            .padding(vertical = 9.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            label,
                            color = if (picked) C.accent else C.textMain,
                            fontSize = 14.sp,
                            fontWeight = if (picked) FontWeight.SemiBold else FontWeight.Normal,
                            modifier = Modifier.weight(1f),
                        )
                        if (picked) AgateBadge("当前", BadgeTone.BLUE)
                    }
                }
            }
        },
        confirmButton = {
            TextButton(onClick = onDismiss) { Text("关闭", color = C.textSub) }
        },
        containerColor = C.cardBg,
    )
}
