package dev.agate.android.ui.settings

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
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.MaintenanceOps
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.AgateSubPageHeader
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateColors
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import dev.agate.android.ui.tasks.beijingStamp
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * 维护记录：凭证刷新 / 模型目录更新的逐次执行台账。
 *
 * ── task_state 与这里的区别 ──────────────────────────────────
 * 定时任务页只显示「最近一次排期快照」；这里是「每次执行的真实结果」——
 * 逐条账号的成功/失败、耗时、错误信息。自动排期与手动跑共用同一执行入口，
 * 所以手动跑一条会出现在这里，和定时任务自动触发的没有区别。
 *
 * ── 终态靠回读，不靠 man run 的返回值 ────────────────────────
 * 手动跑/重试返回的只是 running 起始详情；真正的结果要隔一会 detail(id) 回读。
 * 所以点完动作后起一个短轮询，拿到非 running 就停。
 */
@Composable
fun MaintenancePage(onBack: () -> Unit) {
    val C = agateColors()
    val scope = rememberCoroutineScope()

    var overview by remember { mutableStateOf<MaintenanceOps.Overview?>(null) }
    var filter by remember { mutableStateOf(0) } // 0 全部 1 成功 2 失败
    var error by remember { mutableStateOf<String?>(null) }
    var detail by remember { mutableStateOf<MaintenanceOps.Detail?>(null) }
    var busy by remember { mutableStateOf(false) }

    val statusFilter = when (filter) { 1 -> "success"; 2 -> "failed"; else -> null }

    fun reload() {
        scope.launch {
            val r = MaintenanceOps.overview(status = statusFilter, limit = 30)
            when (r) {
                is MaintenanceOps.Outcome.Ok -> { overview = r.value; error = null }
                is MaintenanceOps.Outcome.Err -> error = r.message
            }
        }
    }

    LaunchedEffect(Unit) { reload() }
    LaunchedEffect(filter) { reload() }

    // 详情未定稿时短轮询（手动跑/重试后 detail 是 running）
    LaunchedEffect(detail?.run?.id) {
        val id = detail?.run?.id ?: return@LaunchedEffect
        while (detail?.run?.id == id && detail?.run?.isRunning == true) {
            delay(1_500L)
            when (val r = MaintenanceOps.detail(id)) {
                is MaintenanceOps.Outcome.Ok -> detail = r.value
                is MaintenanceOps.Outcome.Err -> break
            }
        }
        reload()
    }

    fun runTask(taskId: String) {
        busy = true
        scope.launch {
            when (val r = MaintenanceOps.run(taskId)) {
                is MaintenanceOps.Outcome.Ok -> { detail = r.value; error = null }
                is MaintenanceOps.Outcome.Err -> error = r.message
            }
            busy = false
        }
    }

    fun retry(runId: String, itemId: String?) {
        busy = true
        scope.launch {
            when (val r = MaintenanceOps.retry(runId, itemId)) {
                is MaintenanceOps.Outcome.Ok -> { detail = r.value; error = null }
                is MaintenanceOps.Outcome.Err -> error = r.message
            }
            busy = false
        }
    }

    Column(Modifier.fillMaxSize().background(C.bg)) {
        AgateSubPageHeader("维护记录", onBack = onBack) {
            overview?.let { AgateBadge("${it.records.total} 次", BadgeTone.BLUE) }
        }
        Column(
            Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))
            Text(
                "自动排期与手动跑都进这本台账。失败项可单条重试，重试会生成一条新记录并关联原记录。",
                style = AgateType.Caption, color = C.textSub,
            )
            Spacer(Modifier.height(10.dp))

            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                AgateBadge("手动跑", BadgeTone.GRAY)
            }
            Spacer(Modifier.height(8.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                MaintBtn("刷新凭证", busy, C) { runTask(MaintenanceOps.TASK_CREDENTIAL) }
                MaintBtn("刷新模型目录", busy, C) { runTask(MaintenanceOps.TASK_MODEL) }
            }
            Spacer(Modifier.height(14.dp))

            if (error != null) {
                MaintBanner(error!!, C.red, C.redBg)
                Spacer(Modifier.height(10.dp))
            }

            Text("执行历史", style = AgateType.SectionTitle, color = C.textMain, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(8.dp))
            AgateSegmented(listOf("全部", "成功", "失败"), filter, onSelect = { filter = it })
            Spacer(Modifier.height(10.dp))

            val ov = overview
            if (ov == null) {
                MaintCard { Text("读取中…", style = AgateType.BodyS, color = C.textDim) }
            } else if (ov.records.items.isEmpty()) {
                MaintCard { Text("还没有执行记录。", style = AgateType.BodyS, color = C.textDim) }
            } else {
                MaintCard {
                    ov.records.items.forEachIndexed { index, run ->
                        if (index > 0) MaintDivider(C)
                        RunRow(run, C) { detail = null; scope.launch {
                            when (val r = MaintenanceOps.detail(run.id)) {
                                is MaintenanceOps.Outcome.Ok -> detail = r.value
                                is MaintenanceOps.Outcome.Err -> error = r.message
                            }
                        } }
                    }
                    if (ov.records.total > ov.records.items.size) {
                        Spacer(Modifier.height(6.dp))
                        Text("仅显示最近 ${ov.records.items.size} / ${ov.records.total} 条 · 保留 ${ov.retentionDays} 天",
                            style = AgateType.Caption, color = C.textDim)
                    }
                }
            }
            Spacer(Modifier.height(20.dp))
        }
    }

    detail?.let { d ->
        DetailSheet(
            detail = d,
            busy = busy,
            C = C,
            onDismiss = { detail = null },
            onRetryAll = { retry(d.run.id, null) },
            onRetryItem = { itemId -> retry(d.run.id, itemId) },
        )
    }
}

@Composable
private fun RunRow(run: MaintenanceOps.Run, C: AgateColors, onClick: () -> Unit) {
    val (tone, text) = when (run.status) {
        "success" -> BadgeTone.GREEN to "成功"
        "failed" -> BadgeTone.RED to "失败"
        "running" -> BadgeTone.AMBER to "执行中"
        "interrupted" -> BadgeTone.AMBER to "中断"
        else -> BadgeTone.GRAY to run.status
    }
    Row(
        Modifier.fillMaxWidth().clickable(onClick = onClick).padding(vertical = 9.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(taskLabel(run.taskId), color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.Medium)
            Text(
                buildString {
                    append(beijingStamp(run.startedAt))
                    append(" · ")
                    append(if (run.source == "manual") "手动" else "自动")
                    run.durationMs?.let { append(" · ${it}ms") }
                },
                style = AgateType.Caption, color = C.textDim,
            )
            if (run.refreshed + run.skipped + run.failed > 0) {
                Text("刷新 ${run.refreshed} · 跳过 ${run.skipped} · 失败 ${run.failed}",
                    style = AgateType.Caption, color = C.textSub)
            }
        }
        AgateBadge(text, tone)
        Text("›", fontSize = 20.sp, color = C.textDim, modifier = Modifier.padding(start = 6.dp))
    }
}

private fun taskLabel(id: String) = when (id) {
    MaintenanceOps.TASK_CREDENTIAL -> "凭证自动维护"
    MaintenanceOps.TASK_MODEL -> "模型目录刷新"
    else -> id
}

@Composable
private fun DetailSheet(
    detail: MaintenanceOps.Detail,
    busy: Boolean,
    C: AgateColors,
    onDismiss: () -> Unit,
    onRetryAll: () -> Unit,
    onRetryItem: (String) -> Unit,
) {
    androidx.compose.ui.window.Dialog(onDismissRequest = onDismiss) {
        Column(
            Modifier.fillMaxWidth().background(C.cardBg, RoundedCornerShape(20.dp))
                .border(1.dp, C.border, RoundedCornerShape(20.dp)).padding(18.dp)
                .verticalScroll(rememberScrollState()),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("执行详情", style = AgateType.SectionTitle, color = C.textMain, fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.weight(1f))
                Text("✕", color = C.textDim, modifier = Modifier.clickable(onClick = onDismiss))
            }
            Spacer(Modifier.height(8.dp))
            Text(
                "${taskLabel(detail.run.taskId)} · ${beijingStamp(detail.run.startedAt)} · " +
                    "刷新 ${detail.run.refreshed} / 跳过 ${detail.run.skipped} / 失败 ${detail.run.failed}",
                style = AgateType.Caption, color = C.textSub,
            )
            detail.run.error?.let {
                Spacer(Modifier.height(6.dp))
                MaintBanner(it, C.red, C.redBg)
            }
            Spacer(Modifier.height(12.dp))

            if (detail.results.isEmpty()) {
                Text("没有逐条结果。", style = AgateType.BodyS, color = C.textDim)
            } else {
                detail.results.forEachIndexed { index, item ->
                    if (index > 0) MaintDivider(C)
                    Row(Modifier.fillMaxWidth().padding(vertical = 7.dp), verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text(item.accountName ?: item.providerLabel ?: item.provider,
                                color = C.textMain, fontSize = 13.sp)
                            item.message?.let { Text(it, style = AgateType.Caption, color = C.textDim) }
                        }
                        val tone = when (item.status) {
                            "success" -> BadgeTone.GREEN
                            "failed" -> BadgeTone.RED
                            "skipped" -> BadgeTone.GRAY
                            else -> BadgeTone.AMBER
                        }
                        AgateBadge(item.status, tone)
                        if (item.status == "failed") {
                            Spacer(Modifier.width(6.dp))
                            Text("重试", fontSize = 12.sp, color = C.accent,
                                modifier = Modifier.clickable(enabled = !busy) { onRetryItem(item.id) })
                        }
                    }
                }
            }
            Spacer(Modifier.height(14.dp))
            if (detail.run.failed > 0) {
                MaintBtn("重试全部失败项", busy, C) { onRetryAll() }
                Spacer(Modifier.height(8.dp))
            }
        }
    }
}

@Composable
private fun MaintCard(content: @Composable ColumnScope.() -> Unit) {
    val C = agateColors()
    Column(Modifier.fillMaxWidth().agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
        .border(1.dp, C.border, RoundedCornerShape(20.dp)).padding(horizontal = 14.dp, vertical = 10.dp),
        content = content)
}

@Composable
private fun MaintDivider(C: AgateColors) = Box(Modifier.fillMaxWidth().height(1.dp).background(C.border.copy(alpha = 0.55f)))

@Composable
private fun MaintBanner(text: String, fg: Color, bg: Color) {
    Row(Modifier.fillMaxWidth().background(bg, RoundedCornerShape(10.dp)).padding(horizontal = 12.dp, vertical = 8.dp)) {
        Text(text, style = AgateType.Caption, color = fg)
    }
}

@Composable
private fun MaintBtn(text: String, busy: Boolean, C: AgateColors, onClick: () -> Unit) {
    Box(Modifier.background(if (busy) C.btnBg.copy(alpha = 0.5f) else C.accent, RoundedCornerShape(10.dp))
        .then(if (busy) Modifier else Modifier.clickable(onClick = onClick))
        .padding(horizontal = 14.dp, vertical = 9.dp), contentAlignment = Alignment.Center) {
        Text(text, style = AgateType.BodyS, fontWeight = FontWeight.SemiBold, color = if (busy) C.textDim else C.onAccent)
    }
}
