package dev.agate.android.ui.tasks

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
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
import dev.agate.android.data.CheckinOps
import dev.agate.android.data.CheckinRecovery
import dev.agate.android.data.PresetRegistry
import dev.agate.android.ui.components.AgateSubPageHeader
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import dev.agate.core.GatewayPrefs
import kotlinx.coroutines.launch

/** Uncertain check-in diagnostics. Positive reconciliation is read-only upstream; no retry/force-clear action exists. */
@Composable
fun CheckinRecoveryPage(onBack: () -> Unit, modifier: Modifier = Modifier) {
    val colors = agateColors()
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    var entries by remember { mutableStateOf<List<CheckinRecovery.Pending>>(emptyList()) }
    var total by remember { mutableStateOf(0L) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    val busy = remember { mutableStateMapOf<String, Boolean>() }
    val evidence = remember { mutableStateMapOf<String, List<CheckinRecovery.Evidence>>() }
    val evidenceExpanded = remember { mutableStateMapOf<String, Boolean>() }
    val latest = remember { mutableStateMapOf<String, String>() }
    val resolvedKeys = remember { mutableStateMapOf<String, Boolean>() }

    fun reload() {
        if (loading) return
        loading = true
        scope.launch {
            val page = CheckinRecovery.queryPending()
            loading = false
            if (page == null) error = "读取待核实记录失败，请检查内核状态"
            else {
                entries = page.entries
                total = page.total
                resolvedKeys.clear()
                error = null
            }
        }
    }

    DisposableEffect(Unit) {
        CheckinOps.configure(GatewayPrefs.apiKey(context))
        onDispose { }
    }
    LaunchedEffect(Unit) { reload() }

    fun loadEvidence(item: CheckinRecovery.Pending) {
        scope.launch {
            val page = CheckinRecovery.queryEvidence(item)
            if (page != null) evidence[item.key] = page.entries
        }
    }

    fun inspect(item: CheckinRecovery.Pending) {
        if (busy[item.key] == true) return
        busy[item.key] = true
        scope.launch {
            val result = CheckinRecovery.reconcile(item)
            busy[item.key] = false
            if (result == null) {
                latest[item.key] = "核实请求失败；原记录仍保持待核实"
            } else {
                if (result.resolved) resolvedKeys[item.key] = true
                latest[item.key] = when {
                    result.resolved && result.historyProjected -> "已确认当日签到完成，历史记录已恢复（证据 #${result.evidenceId ?: "—"}）"
                    result.resolved -> "已确认并解除当日隔离，但历史投影待恢复${result.projectionError?.let { "：$it" } ?: ""}"
                    result.verdict == "confirmed" -> "收到确认结果，但未满足解决条件；请检查证据"
                    else -> "仍无法确认：${result.reason ?: "证据不足，保持隔离"}"
                }
            }
            loadEvidence(item)
            if (result?.resolved != true) reload()
        }
    }

    Column(modifier.fillMaxSize().background(colors.bg)) {
        AgateSubPageHeader("签到异常核实", onBack = onBack) {
            TaskBtn(if (loading) "读取中…" else "刷新", outlined = true, enabled = !loading) { reload() }
        }
        Column(
            Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))
            TaskCard {
                TaskCardHeader("待核实签到", subtitle = "$total 条 · 本页已核实项保留到刷新，不会自动重试 Provider 请求")
                Spacer(Modifier.height(6.dp))
                TaskHint("请求可能已到达上游，但本地没有可靠结果。只有 Provider 专属只读状态给出正向证据时，才会解除当日隔离并恢复历史。")
                Spacer(Modifier.height(6.dp))
                TaskHint("核实失败、状态未知或仍可领取，不代表上游未执行；这些情况都会继续保留待核实状态。", color = colors.amber)
            }
            TaskGap()
            if (error != null) {
                TaskCard { Text(error!!, color = colors.red, fontSize = 13.sp) }
                TaskGap()
            }
            if (entries.isEmpty() && !loading && error == null) {
                TaskCard {
                    Text("当前没有待核实签到", color = colors.textMain, fontWeight = FontWeight.SemiBold)
                    TaskHint("已解决的记录仍保留在执行审计与签到历史中。")
                }
            }
            entries.forEach { item ->
                TaskCard {
                    TaskCardHeader(
                        item.accountName,
                        subtitle = "${PresetRegistry.labelOf(item.provider)} · ${sourceLabel(item.source)}",
                        actions = { TaskTag(if (resolvedKeys[item.key] == true) "已解决" else "UNCERTAIN", if (resolvedKeys[item.key] == true) colors.green else colors.amber) },
                    )
                    Spacer(Modifier.height(5.dp))
                    TaskHint("执行时间：${if (item.accountStartedAt > 0) beijingDateTime(item.accountStartedAt) else "—"}")
                    TaskHint("批次：${item.runId}")
                    if (!item.reason.isNullOrBlank()) TaskHint("触发原因：${item.reason}")
                    item.lastError?.let { TaskHint("错误：$it", color = colors.red) }
                    latest[item.key]?.let {
                        Spacer(Modifier.height(6.dp))
                        TaskHint(it, color = colors.textSub)
                    }
                    Spacer(Modifier.height(8.dp))
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                        TaskBtn(if (busy[item.key] == true) "核实中…" else if (resolvedKeys[item.key] == true) "已核实" else "只读核实", primary = true, enabled = busy[item.key] != true && resolvedKeys[item.key] != true) {
                            inspect(item)
                        }
                        TaskBtn(if (evidenceExpanded[item.key] == true) "收起证据" else "查看证据", outlined = true) {
                            val expand = evidenceExpanded[item.key] != true
                            evidenceExpanded[item.key] = expand
                            if (expand) loadEvidence(item)
                        }
                    }
                    if (evidenceExpanded[item.key] == true) {
                        Spacer(Modifier.height(8.dp))
                        val rows = evidence[item.key].orEmpty()
                        if (rows.isEmpty()) TaskHint("暂无证据记录或读取失败")
                        rows.forEach { row ->
                            TaskDivider()
                            Spacer(Modifier.height(5.dp))
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text("证据 #${row.evidenceId}", color = colors.textMain, fontSize = 12.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                                TaskTag(if (row.resolved) "已解决" else row.verdict.uppercase(), if (row.resolved) colors.green else colors.amber)
                            }
                            TaskHint("${row.evidenceType} · ${if (row.observedAt > 0) beijingDateTime(row.observedAt) else "时间未知"}")
                            val status = row.details.optString("upstreamStatus")
                            if (status.isNotBlank()) TaskHint("上游状态：$status")
                            val campaign = row.details.optString("campaignId")
                            if (campaign.isNotBlank()) TaskHint("活动 ID：$campaign")
                            val day = row.details.optString("businessDay")
                            if (day.isNotBlank()) TaskHint("业务日期：$day")
                            val reason = row.details.optString("reason")
                            if (reason.isNotBlank()) TaskHint(reason)
                        }
                    }
                }
                TaskGap()
            }
            Spacer(Modifier.height(24.dp))
        }
    }
}

private fun sourceLabel(source: String): String = when (source) {
    "schedule" -> "定时签到"
    "startup" -> "启动补签"
    "manual" -> "批量签到"
    "single" -> "单账号签到"
    "activity" -> "活动签到"
    else -> source.ifBlank { "未知来源" }
}

private fun beijingDateTime(ms: Long): String =
    java.text.SimpleDateFormat("yyyy-MM-dd HH:mm:ss", java.util.Locale.CHINA)
        .apply { timeZone = java.util.TimeZone.getTimeZone("GMT+08:00") }
        .format(java.util.Date(ms))
