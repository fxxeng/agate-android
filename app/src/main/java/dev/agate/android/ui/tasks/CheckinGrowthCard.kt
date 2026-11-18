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

/* ── 成长任务 ── */

@Composable
internal fun GrowthCard(snap: CheckinCenter.Snap, onToast: (String) -> Unit) {
    val C = agateColors()
    val scope = rememberCoroutineScope()
    val states = remember { mutableStateMapOf<String, JSONObject>() }
    var busyId by remember { mutableStateOf<String?>(null) }
    var runningAll by remember { mutableStateOf<String?>(null) }
    /** 任务明细展开状态（账号 id → 是否展开）。默认全收起，见明细块的注释。 */
    val expandedTasks = remember { mutableStateMapOf<String, Boolean>() }

    fun query(acc: CheckinCenter.GrowthAccount) {
        if (busyId != null) return
        busyId = acc.id
        scope.launch {
            val data = CheckinOps.growthStatus(acc.id)
            busyId = null
            if (data != null) states[acc.id] = data
            else onToast("${acc.name} 查询失败")
        }
    }

    fun claimAll(acc: CheckinCenter.GrowthAccount) {
        if (busyId != null) return
        val data = states[acc.id] ?: run {
            onToast("先点「查询」，看清有哪些任务可完成")
            return
        }
        val tasks = data.optJSONObject("tasks")?.optJSONArray("tasks") ?: JSONArray()
        // 只对「在映射表里 + 不是伪造不了的」任务执行：上游能查到但脚本伪造不出来的
        // 那类（邀请好友）点了也只会失败，界面上已标「需真实操作」
        val runnable = (0 until tasks.length()).mapNotNull { i -> tasks.optJSONObject(i) }
            .filter { it.optBoolean("supported") && !it.optBoolean("unforgeable") }
            .map { it.optString("code") }
            .filter { it.isNotBlank() }
        if (runnable.isEmpty()) {
            onToast("没有可执行的任务")
            return
        }
        busyId = acc.id
        runningAll = acc.id
        scope.launch {
            var okCount = 0
            var lastError: String? = null
            // 内核只暴露**单任务**端点（全量一批会超 60 秒转发超时，见 api::growth
            // 的模块头），所以这里本地逐条调 —— 也顺带能显示逐条进度
            runnable.forEach { code ->
                val r = CheckinOps.growthTask(acc.id, code, "full")
                if (r.ok) okCount++ else lastError = r.error
            }
            runningAll = null
            busyId = null
            onToast(
                if (okCount == runnable.size) "已完成 $okCount 项"
                else "完成 $okCount/${runnable.size}" + (lastError?.let { " · $it" } ?: "")
            )
            CheckinOps.growthStatus(acc.id)?.let { states[acc.id] = it }
        }
    }

    TaskCard {
        TaskCardHeader(
            title = "成长任务",
            subtitle = "WorkBuddy 国内版 · 任务树点亮与领奖",
        )
        if (snap.growth.isEmpty()) {
            Spacer(Modifier.height(6.dp))
            Text("没有 WorkBuddy 国内版账号", style = AgateType.BodyS, color = C.textDim)
        }
        snap.growth.forEach { acc ->
            val data = states[acc.id]
            val tasks = data?.optJSONObject("tasks")?.optJSONArray("tasks")
            val energy = data?.optInt("energy", -1) ?: -1
            val streak = data?.optInt("streak", -1) ?: -1
            val doneCount = if (tasks != null) {
                (0 until tasks.length()).count {
                    tasks.optJSONObject(it)?.optString("status") == "claimed"
                }
            } else 0

            TaskRow(
                title = acc.name,
                avatarColorKey = "workbuddy",
                letters = "WB",
                providerId = "workbuddy",
                label = PresetRegistry.labelOf("workbuddy"),
                subtitle = buildString {
                    when {
                        data == null -> append("尚未查询")
                        else -> {
                            if (tasks != null) append("${tasks.length()} 项任务 · 已领 $doneCount")
                            if (energy >= 0) append(" · 能量 $energy")
                            if (streak >= 0) append(" · 连登 $streak 天")
                        }
                    }
                },
            ) {
                TaskBtn(
                    if (busyId == acc.id && runningAll == null) "…" else "查询",
                    outlined = true,
                    enabled = busyId == null,
                ) { query(acc) }
                if (data != null && tasks != null && tasks.length() > 0) {
                    // 明细开关：只改本地展开状态，不重查上游（结果还在 `states` 里）
                    val open = expandedTasks[acc.id] ?: false
                    TaskBtn(if (open) "收起" else "明细", outlined = true) {
                        expandedTasks[acc.id] = !open
                    }
                    TaskBtn(
                        if (runningAll == acc.id) "执行中…" else "一键完成",
                        primary = true,
                        enabled = busyId == null,
                    ) { claimAll(acc) }
                }
            }

            // ── 任务明细：默认收起 ───────────────────────────────
            // 查询完就整棵任务树摊开，一屏能吃掉十来行，把后面的卡片全顶下去 ——
            // 而「今天有几项、领了几个」摘要里就够看了。默认收起，点「明细」展开。
            //
            // 展开状态按**账号 id**存：一个账号一份，互不牵连；`states` 里的查询
            // 结果仍在内存里，收起只是不画，不会丢数据、也不重打上游。
            // 开关是行右侧那颗「明细 / 收起」按钮（上面那颗），不是整行点击 ——
            // 整行点击会和「查询」抢同一个手势，点了不知道会发生哪件事。
            if (tasks != null && tasks.length() > 0) {
                val open = expandedTasks[acc.id] ?: false
                if (open) {
                    Column(
                        verticalArrangement = Arrangement.spacedBy(5.dp),
                        modifier = Modifier.padding(start = 8.dp, bottom = 4.dp, end = 2.dp),
                    ) {
                        for (i in 0 until tasks.length()) {
                            val t = tasks.optJSONObject(i) ?: continue
                            val status = t.optString("status")
                            val current = t.optInt("current", 0)
                            val target = t.optInt("target", 0)
                            val unforgeable = t.optBoolean("unforgeable")
                            val supported = t.optBoolean("supported")
                            TaskSubRow {
                                Text(
                                    if (status == "claimed") "✓" else "○",
                                    color = if (status == "claimed") C.green else C.textDim,
                                    fontSize = 12.sp,
                                )
                                Text(
                                    t.optString("title").ifBlank { t.optString("code") },
                                    color = if (status == "claimed") C.textDim else C.textMain,
                                    fontSize = 12.sp,
                                    modifier = Modifier.weight(1f),
                                )
                                if (target > 0 && status != "claimed") {
                                    Text("$current/$target", style = AgateType.Caption, color = C.textDim)
                                }
                                Text(
                                    when {
                                        status == "claimed" -> "已领取"
                                        unforgeable -> "需真实操作"
                                        !supported -> "未映射"
                                        else -> "可完成"
                                    },
                                    style = AgateType.Caption,
                                    color = when {
                                        status == "claimed" -> C.green
                                        unforgeable || !supported -> C.textDim
                                        else -> C.amber
                                    },
                                )
                            }
                        }
                    }
                }
            }
        }
        Spacer(Modifier.height(6.dp))
        TaskFold(
            "「需真实操作」那类不给执行按钮 —— 上游能查到但从脚本伪造不了，标出来比假装能点诚实。" +
                "一键完成是本地逐条调用：内核只暴露单任务端点（全量一批会超 60 秒转发超时）。"
        )
    }
}

