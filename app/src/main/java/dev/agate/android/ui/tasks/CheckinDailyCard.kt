package dev.agate.android.ui.tasks

import android.widget.Toast
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
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
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.CheckinCenter
import dev.agate.android.data.CheckinOps
import dev.agate.android.data.PresetRegistry
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateSubPageHeader
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.StatCell
import dev.agate.android.ui.components.AgateStatRow
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject

/* ── 每日签到 ── */

/** 每日签到卡：`FlowRow` 在卡内换行，需在函数级再 @OptIn 一次（页面的 opt-in 不传递）。 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun DailyCard(
    snap: CheckinCenter.Snap,
    expanded: MutableMap<String, Boolean>,
    busyKey: String?,
    signingAll: Boolean,
    providerFilter: String?,
    onProviderFilter: (String?) -> Unit,
    onToggleGroup: (String) -> Unit,
    onSignAll: () -> Unit,
    onSignOne: (CheckinCenter.AccountRow) -> Unit,
    onActivity: (CheckinCenter.AccountRow, String, String) -> Unit,
) {
    val C = agateColors()
    // 选中的 provider 若在刷新后消失了（分组是后端给的，数量会变），按「全部」
    // 处理：胶囊选中态与渲染列表都由这一处推导，不会出现「筛选仍在但什么都
    // 不画」的死角，也不用在组合期去改页面级状态。
    val activeProvider = providerFilter?.takeIf { id -> snap.groups.any { it.id == id } }
    val visibleGroups = if (activeProvider == null) {
        snap.groups
    } else {
        snap.groups.filter { it.id == activeProvider }
    }
    TaskCard {
        TaskCardHeader(
            title = "每日签到",
            subtitle = "点分组展开账号明细 · 单账号签到不受定时范围限制",
        ) {
            if (signingAll) {
                AgateBadge("下发中", BadgeTone.AMBER)
            } else {
                TaskBtn("立即全部签到", primary = true) { onSignAll() }
            }
        }
        Spacer(Modifier.height(6.dp))
        AgateStatRow(
            cells = listOf(
                StatCell("今日已签", "${snap.todayDone}"),
                StatCell("可签账号", "${snap.todayEligible}"),
                StatCell("范围外", "${snap.outOfScope.sumOf { it.count }}"),
            ),
        )

        // ── provider 筛选（列表之上）──
        // 计数直接用后端聚合好的 `totalCount`，不在这里重算、更不重查上游；
        // 一家 provider 都没有（读取中 / 无账号）时不出这一行，免得只剩「全部」。
        if (snap.groups.isNotEmpty()) {
            val allBadge = countBadge(snap.groups.sumOf { it.totalCount })
            Spacer(Modifier.height(8.dp))
            FlowRow(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                NavChip("全部", activeProvider == null, allBadge) { onProviderFilter(null) }
                snap.groups.forEach { group ->
                    NavChip(
                        group.label,
                        selected = activeProvider == group.id,
                        badge = countBadge(group.totalCount),
                    ) { onProviderFilter(group.id) }
                }
            }
            Spacer(Modifier.height(8.dp))
        }

        if (snap.groups.isEmpty()) {
            Spacer(Modifier.height(10.dp))
            Text(
                if (snap.fetchedAtMs == 0L) "读取中…" else "没有支持签到的账号",
                style = AgateType.BodyS,
                color = C.textDim,
            )
        }

        visibleGroups.forEach { group ->
            val open = expanded[group.id] ?: false
            val tone = when {
                group.totalCount == 0 -> C.textDim
                group.doneCount == group.totalCount -> C.green
                group.doneCount > 0 -> C.amber
                else -> C.blue
            }
            TaskRow(
                title = group.label,
                avatarColorKey = group.id,
                letters = group.label.take(1),
                // 提供商分组行走官方 logo（logo 缺失才回落字母块）。`label` 必须传
                // 产品名：兜底头像用它取色与首字母，传账号名会让同一产品每行一个样。
                providerId = group.id,
                label = group.label,
                subtitle = groupDescription(group.id),
                onClick = if (group.totalCount > 0) ({ onToggleGroup(group.id) }) else null,
            ) {
                TaskTag(
                    when {
                        group.totalCount == 0 -> "无账号"
                        group.doneCount == group.totalCount -> "已签完"
                        group.doneCount > 0 -> "部分"
                        else -> "待签"
                    },
                    tone,
                )
                Text("${group.totalCount} 账号 · ${group.doneCount} 已签", style = AgateType.Caption, color = C.textDim)
                Text(if (open) "⌃" else "⌄", color = C.textDim, fontSize = 14.sp)
            }

            AnimatedVisibility(visible = open) {
                Column(verticalArrangement = Arrangement.spacedBy(5.dp)) {
                    group.accounts.forEach { acc ->
                        AccountSubRow(
                            acc = acc,
                            intl = group.id == "workbuddy-intl",
                            busyKey = busyKey,
                            onSign = { onSignOne(acc) },
                            onActivity = { mode, label -> onActivity(acc, mode, label) },
                        )
                    }
                }
            }
        }

        if (snap.outOfScope.isNotEmpty()) {
            Spacer(Modifier.height(6.dp))
            TaskFold(
                "不参与每日签到：" + snap.outOfScope.joinToString("；") {
                    "${it.label} ×${it.count}（${it.reason}）"
                } + "。这些原因由后端给，界面不自己造。"
            )
        }
    }
}

@Composable
internal fun AccountSubRow(
    acc: CheckinCenter.AccountRow,
    intl: Boolean,
    busyKey: String?,
    onSign: () -> Unit,
    onActivity: (mode: String, label: String) -> Unit,
) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .background(C.insetBg.copy(alpha = 0.55f), RoundedCornerShape(9.dp))
            .padding(horizontal = 10.dp, vertical = 8.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(acc.name, color = C.textMain, fontSize = 13.sp, modifier = Modifier.weight(1f))
            Text(
                when {
                    acc.checkedInToday -> beijingClock(acc.checkinAt ?: 0L)
                    acc.checkinAt != null -> "上次 " + beijingMonthDay(acc.checkinAt)
                    else -> "—"
                },
                style = AgateType.Caption,
                color = C.textDim,
            )
            Spacer(Modifier.width(7.dp))
            TaskTag(
                when {
                    acc.checkedInToday -> "已签"
                    !acc.available -> "不可用"
                    else -> "待签"
                },
                when {
                    acc.checkedInToday -> C.green
                    !acc.available -> C.red
                    else -> C.blue
                },
            )
        }
        Spacer(Modifier.height(6.dp))
        if (intl) {
            // 三颗按钮，各自转圈：混成一颗会让用户看不出积分为什么没涨
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                val modes = listOf(
                    Triple("keepalive", "保活", "保活"),
                    Triple("claim", "领取", "领取"),
                    Triple("full", "保活+领取", "全量"),
                )
                modes.forEach { (mode, label, _) ->
                    val acting = busyKey == "${acc.id}:$mode"
                    TaskBtn(
                        if (acting) "…" else label,
                        outlined = mode == "full",
                        enabled = busyKey == null && acc.available,
                    ) { onActivity(mode, label) }
                }
            }
        } else {
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                val acting = busyKey == "${acc.id}:sign"
                TaskBtn(
                    text = when {
                        acting -> "签到中…"
                        acc.checkedInToday -> "重签"
                        else -> "签到"
                    },
                    outlined = !acc.checkedInToday,
                    enabled = busyKey == null && acc.available,
                ) { onSign() }
            }
        }
    }
}

/** 每家的一句话说明（与上游 `checkin-page.tsx` 的分组副标同义）。 */
internal fun groupDescription(id: String): String = when (id) {
    "workbuddy" -> "腾讯每日签到接口 · 仅国内版"
    "workbuddy-intl" -> "每日活跃任务 · 保活点亮日活，领取才加积分"
    "raccoon" -> "桌面端每日积分链路"
    "autoclaw" -> "每日签到（AutoClaw）"
    "autoclaw-intl" -> "每日签到（AutoClaw 国际版）"
    "qoder" -> "活动（campaign）领取 · 每天 10:00 刷新"
    "qoder-intl" -> "每日 100 Credits · 10:00 窗口"
    "trae" -> "每日签到（Trae）"
    "loomy" -> "每日签到（Loomy）"
    "kuku" -> "每日签到（KukuAI）"
    "officeace" -> "每日福利与新手任务同一个上游动作"
    "codearts" -> "每日福利 · 领取 → 确认 → 回读核实 · 按北京时间重置"
    else -> ""
}

/* ── 轮次读数的说明（首页/总览用） ── */
internal fun roundSummary(row: CheckinCenter.RoundRow): String = buildString {
    append(row.reason.ifBlank { "签到" })
    append(" · 成功 ${row.succeeded}")
    if (row.active > 0) append(" · 保活 ${row.active}")
    if (row.skipped > 0) append(" · 跳过 ${row.skipped}")
    if (row.failedCount > 0) append(" · 失败 ${row.failedCount}")
}
