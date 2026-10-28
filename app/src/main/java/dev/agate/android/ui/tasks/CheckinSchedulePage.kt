package dev.agate.android.ui.tasks

import android.widget.Toast
import androidx.compose.animation.AnimatedVisibility
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
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.CheckinCenter
import dev.agate.android.data.CheckinOps
import dev.agate.android.data.PresetRegistry
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateChip
import dev.agate.android.ui.components.AgateDragList
import dev.agate.android.ui.components.AgateFormInput
import dev.agate.android.ui.components.AgateSubPageHeader
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch

/**
 * 定时签到 —— 对齐上游 3.0.8 `ScheduleTab`（`checkin-schedule-tab.tsx`）。
 *
 * ── 三个要点 ────────────────────────────────────────────────
 * 1. **总开关说清管到哪一层**：关了所有条目都不跑。它不是「这条条目的开关」。
 * 2. **范围摘要全选时只说「全部 N 家」**：铺 12 个名字会把「几点」和
 *    「上次执行」淹没。这是上游 `providersSummary()` 的取舍，照抄。
 * 3. **保活链在同一个页面**：它和「几点签」是同一次配置动作。
 *    上游把它独立成一个 tab 的邻居（同一页里的一块卡），不是另一个一级页。
 *
 * ── 保活链为什么是拖动排序而不是逗号分隔输入框 ──────────────
 * 这条链的语义**就是顺序**。逗号分隔的输入框：加一个要挪光标、删一个要
 * 精确选中、换顺序得手动剪贴。所以是一行一个模型、长按拖动。
 * 清空后保存 = 恢复缺省链（内核 `keepalive::state` 的口径）。
 */
@Composable
fun CheckinSchedulePage(
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val snap by CheckinCenter.snapshot.collectAsState()

    /** 正在编辑的条目 id（null = 没有编辑态；"" = 新增）。 */
    var editingId by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    DisposableEffect(Unit) {
        val key = GatewayPrefs.apiKey(ctx)
        CheckinCenter.configure(key)
        CheckinOps.configure(key)
        CheckinCenter.startPolling()
        onDispose { CheckinCenter.stopPolling() }
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgateSubPageHeader("定时签到", onBack = onBack) {
            if (editingId == null) {
                TaskBtn("＋ 添加条目", outlined = true) { editingId = "" }
            }
        }

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))

            // ── 总开关 ──
            TaskCard {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text("总开关", style = AgateType.SectionTitle, fontWeight = FontWeight.SemiBold, color = C.textMain)
                        Text(
                            if (snap.autoEnabled) "开着：启用的条目按各自时刻执行"
                            else "关着：所有条目都不会自动跑（手动「立即执行」仍可用）",
                            style = AgateType.Caption,
                            color = C.textDim,
                        )
                    }
                    Switch(
                        checked = snap.autoEnabled,
                        enabled = !busy,
                        onCheckedChange = { on ->
                            busy = true
                            scope.launch {
                                val r = CheckinOps.setAutoEnabled(on)
                                busy = false
                                toast(if (r.ok) "定时签到已${if (on) "开启" else "关闭"}" else "操作失败：${r.error}")
                                CheckinCenter.refreshOnce()
                            }
                        },
                    )
                }
                Spacer(Modifier.height(8.dp))
                TaskFold(
                    "错过时点开机后会自动补签，不会因为当时没开机而漏掉。" +
                        "各家签到接口都是幂等的，重复执行不会重复领取。"
                )
            }
            TaskGap()

            // ── 条目列表 ──
            SchedulesCard(
                snap = snap,
                editingId = editingId,
                busy = busy,
                onEdit = { editingId = it },
                onCancelEdit = { editingId = null },
                onToast = { toast(it) },
                onBusy = { busy = it },
            )
            TaskGap()

            // ── 保活链 ──
            KeepaliveCard(
                snap = snap,
                busy = busy,
                onToast = { toast(it) },
                onBusy = { busy = it },
            )

            // ── 轮次读数（降级后的统计，不是一页台账）──
            if (snap.rounds.isNotEmpty()) {
                TaskGap()
                TaskCard {
                    TaskCardHeader(
                        title = "最近轮次",
                        subtitle = "只作读数 · 每账号明细在「签到记录」页",
                    )
                    TaskDivider()
                    snap.rounds.take(3).forEach { round ->
                        Row(
                            Modifier.fillMaxWidth().padding(vertical = 6.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(Modifier.weight(1f)) {
                                Text(
                                    beijingStamp(round.at),
                                    color = C.textMain,
                                    fontSize = 12.sp,
                                )
                                Text(roundSummary(round), style = AgateType.Caption, color = C.textDim)
                            }
                            if (round.failedCount > 0) {
                                TaskTag("${round.failedCount} 失败", C.red)
                            } else {
                                TaskTag("${round.succeeded} 成功", C.green)
                            }
                        }
                    }
                    if (snap.rounds.firstOrNull()?.failed?.isNotEmpty() == true) {
                        Spacer(Modifier.height(4.dp))
                        TaskFold(
                            "本轮失败：" + snap.rounds.first().failed.take(5).joinToString("；")
                        )
                    }
                }
            }

            TaskGap(24)
        }
    }
}

/* ── 条目列表 ── */

@Composable
private fun SchedulesCard(
    snap: CheckinCenter.Snap,
    editingId: String?,
    busy: Boolean,
    onEdit: (String) -> Unit,
    onCancelEdit: () -> Unit,
    onToast: (String) -> Unit,
    onBusy: (Boolean) -> Unit,
) {
    val C = agateColors()
    val scope = rememberCoroutineScope()

    TaskCard {
        TaskCardHeader(
            title = "条目",
            subtitle = "${snap.schedulesEnabled}/${snap.schedulesTotal} 启用 · 上限 10 条",
        )
        TaskDivider()

        if (snap.schedulesTotal == 0 && editingId == null) {
            Spacer(Modifier.height(8.dp))
            Text("还没有签到计划", style = AgateType.BodyS, color = C.textDim)
        }

        // 新增行必须**真渲染**且在首位。上游这里踩过坑并留了教训注释：
        // 曾经只把 editingId 置成 'new' 却没有一处渲染它，点了「添加」没反应。
        AnimatedVisibility(visible = editingId == "") {
            EntryEditor(
                existingId = null,
                initialLabel = "",
                initialTime = "08:00",
                initialEnabled = true,
                initialProviders = emptyList(),
                providerOptions = snap.groups.map { it.id to it.label },
                busy = busy,
                onCancel = onCancelEdit,
                onSave = { label, enabled, time, providers ->
                    onBusy(true)
                    scope.launch {
                        val r = CheckinOps.saveSchedule(null, label, enabled, time, providers)
                        onBusy(false)
                        onToast(if (r.ok) "条目已添加" else "保存失败：${r.error}")
                        if (r.ok) {
                            onCancelEdit()
                            CheckinCenter.refreshOnce()
                        }
                    }
                },
            )
            Spacer(Modifier.height(6.dp))
        }

        if (snap.fetchedAtMs == 0L) {
            Text("读取中…", style = AgateType.Caption, color = C.textDim)
        }

        // 条目明细由 CheckinCenter.snapshot.scheduleRows 提供（与分组计数同源）
        ScheduleRows(
            snap = snap,
            editingId = editingId,
            busy = busy,
            onEdit = onEdit,
            onCancelEdit = onCancelEdit,
            onToast = onToast,
            onBusy = onBusy,
        )
    }
}

/**
 * 条目行（含编辑态）。
 *
 * 数据来自 [CheckinCenter.Snap.scheduleRows] —— 与总览卡的「条目 N/M 启用」
 * 读的是同一份 JSON，不会出现「计数说有 3 条、列表只画了 2 条」这种分叉。
 */
@Composable
private fun ScheduleRows(
    snap: CheckinCenter.Snap,
    editingId: String?,
    busy: Boolean,
    onEdit: (String) -> Unit,
    onCancelEdit: () -> Unit,
    onToast: (String) -> Unit,
    onBusy: (Boolean) -> Unit,
) {
    val C = agateColors()
    val scope = rememberCoroutineScope()
    val rows = snap.scheduleRows

    rows.forEach { row ->
        val editing = editingId == row.id
        Column(
            Modifier
                .fillMaxWidth()
                .padding(vertical = 5.dp)
                .background(
                    if (editing) C.accent.copy(alpha = 0.04f) else C.insetBg.copy(alpha = 0.45f),
                    RoundedCornerShape(11.dp),
                )
                .padding(horizontal = 10.dp, vertical = 8.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Switch(
                    checked = row.enabled,
                    enabled = !busy,
                    onCheckedChange = { on ->
                        onBusy(true)
                        scope.launch {
                            val r = CheckinOps.saveSchedule(
                                row.id, row.label, on, row.time, row.providers,
                            )
                            onBusy(false)
                            onToast(if (r.ok) (if (on) "已启用" else "已停用") else "保存失败：${r.error}")
                            if (r.ok) CheckinCenter.refreshOnce()
                        }
                    },
                    modifier = Modifier.size(width = 40.dp, height = 24.dp),
                )
                Spacer(Modifier.width(8.dp))
                Text(
                    row.displayLabel,
                    color = if (row.enabled) C.textMain else C.textDim,
                    fontSize = 14.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    if (row.enabled && row.nextRunAt != null) relTime(row.nextRunAt, prefix = "下次 ")
                    else if (!row.enabled) "—"
                    else "总开关关",
                    style = AgateType.Caption,
                    color = if (row.enabled && row.nextRunAt != null) C.accent else C.textDim,
                )
            }
            Spacer(Modifier.height(4.dp))
            Text(
                "每天 ${row.time} · ${providersSummary(row.providers)}",
                style = AgateType.Caption,
                color = C.textSub,
            )
            if (row.lastResult != null) {
                Text(
                    "上次结果：${row.lastResult}",
                    style = AgateType.Caption,
                    color = if (row.lastFiredToday) C.textDim else C.amber,
                )
            }

            if (!editing) {
                Spacer(Modifier.height(6.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    TaskBtn("立即执行", enabled = !busy) {
                        onBusy(true)
                        scope.launch {
                            val r = CheckinOps.runSchedule(row.id)
                            onBusy(false)
                            onToast(if (r.ok) "已下发该条目" else "执行失败：${r.error}")
                            CheckinCenter.refreshOnce()
                        }
                    }
                    TaskBtn("编辑", enabled = !busy) { onEdit(row.id) }
                    TaskBtn("删除", danger = true, enabled = !busy) {
                        onBusy(true)
                        scope.launch {
                            val r = CheckinOps.deleteSchedule(row.id)
                            onBusy(false)
                            onToast(if (r.ok) "条目已删除" else "删除失败：${r.error}")
                            if (r.ok) CheckinCenter.refreshOnce()
                        }
                    }
                }
            }

            AnimatedVisibility(visible = editing) {
                EntryEditor(
                    existingId = row.id,
                    initialLabel = row.label,
                    initialTime = row.time,
                    initialEnabled = row.enabled,
                    initialProviders = row.providers,
                    providerOptions = snap.groups.map { it.id to it.label },
                    busy = busy,
                    onCancel = onCancelEdit,
                    onSave = { label, enabled, time, providers ->
                        onBusy(true)
                        scope.launch {
                            val r = CheckinOps.saveSchedule(row.id, label, enabled, time, providers)
                            onBusy(false)
                            onToast(if (r.ok) "条目已保存" else "保存失败：${r.error}")
                            if (r.ok) {
                                onCancelEdit()
                                CheckinCenter.refreshOnce()
                            }
                        }
                    },
                )
            }
        }
    }
}

/**
 * 条目的行内编辑表单：名称 / 每天时刻 / 签到范围。
 *
 * 范围是**复选框网格**而不是多选下拉：12 家的名字一眼要看全，
 * 下拉里滚一遍才知道自己勾了哪几家。
 */
@Composable
private fun EntryEditor(
    existingId: String?,
    initialLabel: String,
    initialTime: String,
    initialEnabled: Boolean,
    initialProviders: List<String>,
    providerOptions: List<Pair<String, String>>,
    busy: Boolean,
    onCancel: () -> Unit,
    onSave: (label: String, enabled: Boolean, time: String, providers: List<String>) -> Unit,
) {
    val C = agateColors()
    var label by remember(existingId) { mutableStateOf(initialLabel) }
    var time by remember(existingId) { mutableStateOf(initialTime) }
    var enabled by remember(existingId) { mutableStateOf(initialEnabled) }
    // 空集合 = 全部提供商（内核口径）。这里用「勾选的 id 集合」表达，
    // 保存时原样传空数组表示「不限」
    var picked by remember(existingId) { mutableStateOf(initialProviders.toSet()) }
    var allMode by remember(existingId) { mutableStateOf(initialProviders.isEmpty()) }

    Column(
        Modifier
            .fillMaxWidth()
            .padding(top = 8.dp)
            .background(C.cardBg.copy(alpha = 0.5f), RoundedCornerShape(10.dp))
            .border(1.dp, C.border, RoundedCornerShape(10.dp))
            .padding(10.dp),
    ) {
        FormLabel("名称")
        AgateFormInput(
            value = label,
            onValueChange = { label = it },
            hint = "如 早班（留空则显示时刻）",
        )
        Spacer(Modifier.height(8.dp))

        FormLabel("每天时刻（北京时间）")
        AgateFormInput(
            value = time,
            onValueChange = { text ->
                // 只收 HH:mm 形态：过滤掉非数字与冒号，再手插冒号
                val digits = text.filter { it.isDigit() }.take(4)
                time = if (digits.length <= 2) digits
                else digits.substring(0, 2) + ":" + digits.substring(2)
            },
            hint = "08:00",
            keyboardType = KeyboardType.Number,
        )
        Text(
            "错过时点开机后自动补签",
            style = AgateType.Caption,
            color = C.textDim,
            modifier = Modifier.padding(top = 3.dp),
        )
        Spacer(Modifier.height(8.dp))

        Row(verticalAlignment = Alignment.CenterVertically) {
            FormLabel("签到范围", Modifier.weight(1f))
            Text(
                if (allMode) "全部 ${providerOptions.size} 家"
                else "已勾 ${picked.size} / ${providerOptions.size} 家",
                style = AgateType.Caption,
                color = C.accent,
            )
            Spacer(Modifier.width(8.dp))
            TaskBtn(if (allMode) "改按家勾选" else "全选", enabled = !busy) {
                if (allMode) {
                    allMode = false
                    if (picked.isEmpty()) picked = providerOptions.map { it.first }.toSet()
                } else {
                    picked = providerOptions.map { it.first }.toSet()
                }
            }
        }
        Spacer(Modifier.height(5.dp))

        if (allMode) {
            TaskFold(
                "不勾具体家 = 到点时对全部 ${providerOptions.size} 家逐个签到。" +
                    "这样新接进来的家会自动纳入，不必回来改条目。"
            )
        } else {
            // 三列复选框网格：12 家正好四行，一屏放得下
            providerOptions.chunked(3).forEach { rowItems ->
                Row(
                    Modifier.fillMaxWidth().padding(vertical = 2.dp),
                    horizontalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    rowItems.forEach { (id, name) ->
                        val on = id in picked
                        Row(
                            Modifier
                                .weight(1f)
                                .clickable(enabled = !busy) {
                                    picked = if (on) picked - id else picked + id
                                }
                                .padding(vertical = 5.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Box(
                                Modifier
                                    .size(15.dp)
                                    .background(
                                        if (on) C.accent else C.cardBg,
                                        RoundedCornerShape(4.dp),
                                    )
                                    .border(
                                        1.dp,
                                        if (on) C.accent else C.border,
                                        RoundedCornerShape(4.dp),
                                    ),
                                contentAlignment = Alignment.Center,
                            ) {
                                if (on) {
                                    Text("✓", color = C.onAccent, fontSize = 10.sp, fontWeight = FontWeight.Bold)
                                }
                            }
                            Spacer(Modifier.width(5.dp))
                            Text(
                                name,
                                color = if (on) C.textMain else C.textSub,
                                fontSize = 11.sp,
                                maxLines = 1,
                            )
                        }
                    }
                    repeat(3 - rowItems.size) { Spacer(Modifier.weight(1f)) }
                }
            }
            if (picked.isEmpty()) {
                Text(
                    "一家都没勾：保存后会按「全部」处理（内核把空范围读作不限）",
                    style = AgateType.Caption,
                    color = C.amber,
                    modifier = Modifier.padding(top = 3.dp),
                )
            }
        }

        Spacer(Modifier.height(10.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(7.dp)) {
            Spacer(Modifier.weight(1f))
            TaskBtn("取消", enabled = !busy) { onCancel() }
            TaskBtn("保存", primary = true, enabled = !busy && time.length == 5) {
                onSave(label.trim(), enabled, time, if (allMode) emptyList() else picked.toList())
            }
        }
        if (time.length != 5) {
            Text(
                "时刻要写成 HH:mm（如 08:00）",
                style = AgateType.Caption,
                color = C.red,
                modifier = Modifier.padding(top = 4.dp),
            )
        }
    }
}

@Composable
private fun FormLabel(text: String, modifier: Modifier = Modifier) {
    val C = agateColors()
    Text(text, style = AgateType.Caption, color = C.textSub, modifier = modifier.padding(bottom = 3.dp))
}

/**
 * 范围摘要 —— 上游 `providersSummary()` 的取舍照抄：
 * 全选时只说「全部 N 家」，部分勾选才逐家列。
 *
 * 铺 12 个名字会把「几点」和「上次执行」挤到看不见的地方。
 */
internal fun providersSummary(providers: List<String>): String {
    if (providers.isEmpty()) return "全部提供商"
    val labels = providers.map { PresetRegistry.labelOf(it) }
    return if (labels.size <= 3) labels.joinToString("、")
    else "${labels.take(2).joinToString("、")} 等 ${labels.size} 家"
}

/* ── 保活链 ── */

@Composable
private fun KeepaliveCard(
    snap: CheckinCenter.Snap,
    busy: Boolean,
    onToast: (String) -> Unit,
    onBusy: (Boolean) -> Unit,
) {
    val C = agateColors()
    val scope = rememberCoroutineScope()

    // 本地草稿：拖动换位是本地行为，点「保存」才落盘。
    // 外部轮询回来的值只在草稿为空时写入 —— 否则拖到一半会被冲掉。
    var draft by remember { mutableStateOf<List<String>?>(null) }
    val models = draft ?: snap.keepalive

    LaunchedEffect(snap.keepalive) {
        if (draft == null) { /* 跟随外部值 */ }
    }

    var input by remember { mutableStateOf("") }

    TaskCard {
        TaskCardHeader(
            title = "保活模型链",
            subtitle = "WorkBuddy 国际版日活任务的免费模型 · 全局一份，与条目无关",
        ) {
            if (draft != null) {
                TaskBtn("撤销", enabled = !busy) { draft = null }
            }
            TaskBtn(
                "保存",
                primary = true,
                enabled = !busy && draft != null,
            ) {
                onBusy(true)
                scope.launch {
                    val r = CheckinOps.saveKeepalive(models)
                    onBusy(false)
                    onToast(if (r.ok) "保活链已保存" else "保存失败：${r.error}")
                    if (r.ok) {
                        draft = null
                        CheckinCenter.refreshOnce()
                    }
                }
            }
        }
        Spacer(Modifier.height(6.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                "按序逐个尝试、第一个成功即止，共 ${models.size} 个",
                style = AgateType.Caption,
                color = C.textSub,
                modifier = Modifier.weight(1f),
            )
            if (snap.keepaliveDefault.isNotEmpty() && models != snap.keepaliveDefault) {
                TaskBtn("填入缺省链", enabled = !busy) {
                    draft = snap.keepaliveDefault
                }
            }
        }
        Spacer(Modifier.height(7.dp))

        if (models.isEmpty()) {
            Text(
                "链是空的 —— 保存即恢复缺省链（${snap.keepaliveDefault.joinToString("、")}）",
                style = AgateType.BodyS,
                color = C.amber,
            )
        } else {
            AgateDragList(
                items = models,
                onMove = { from, to ->
                    val next = models.toMutableList()
                    if (from in next.indices && to in next.indices) {
                        next.add(to, next.removeAt(from))
                        draft = next
                    }
                },
                reorderHint = "拖拽排序中 · 松手不落盘，点「保存」才生效",
            ) { model, index, _ ->
                Row(
                    Modifier
                        .fillMaxWidth()
                        .background(C.insetBg, RoundedCornerShape(10.dp))
                        .border(1.dp, C.border, RoundedCornerShape(10.dp))
                        .padding(horizontal = 10.dp, vertical = 9.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text("⠿", color = C.textDim, fontSize = 14.sp)
                    Spacer(Modifier.width(8.dp))
                    Text(
                        model,
                        color = C.textMain,
                        fontSize = 13.sp,
                        modifier = Modifier.weight(1f),
                    )
                    Text("#${index + 1}", style = AgateType.Caption, color = C.textDim)
                    Spacer(Modifier.width(8.dp))
                    Text(
                        "×",
                        color = C.textDim,
                        fontSize = 14.sp,
                        modifier = Modifier
                            .clickable {
                                draft = models.toMutableList().also { it.removeAt(index) }
                            }
                            .padding(horizontal = 3.dp),
                    )
                }
            }
        }

        Spacer(Modifier.height(9.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            AgateFormInput(
                value = input,
                onValueChange = { input = it },
                hint = "模型名，如 glm-4-flash",
                modifier = Modifier.weight(1f),
            )
            Spacer(Modifier.width(6.dp))
            TaskBtn("添加", outlined = true, enabled = input.isNotBlank() && !busy) {
                val name = input.trim()
                if (name.isNotEmpty() && !models.contains(name)) {
                    draft = models + name
                    input = ""
                } else if (models.contains(name)) {
                    onToast("链里已经有这个模型了")
                }
            }
        }

        Spacer(Modifier.height(8.dp))
        TaskFold(
            "这条链的语义**就是顺序**，所以是一行一个模型、长按⠿拖动排序，" +
                "不是一个逗号分隔的输入框（加一个要挪光标、删一个要精确选中、换顺序得手动剪贴）。" +
                "清空后保存 = 恢复缺省链。"
        )
    }
}

/* ── 小件 ── */

@Composable
private fun SmallDot(color: androidx.compose.ui.graphics.Color) {
    Box(Modifier.size(6.dp).background(color, CircleShape))
}
