package dev.agate.android.ui.tasks

import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
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
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Switch
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.CheckinOps
import dev.agate.android.data.PeriodicTaskOps
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateSubPageHeader
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * 周期任务 —— 对齐上游 3.0.8 的 `data-page="tasks"`（另一个一级页，不是签到中心的 tab）。
 *
 * ── 六条分两类 ──────────────────────────────────────────────
 * - `backend`（凭证自动维护 / 模型目录刷新 / 版本检查）：网关循环执行，
 *   给「立即执行」。
 * - `frontend`（日志页 / 请求日志页 / 报表页自动刷新）：**执行者就是页面自己的
 *   定时器**，`canRun = false`。点了没有任何东西可跑，所以**不给按钮** ——
 *   一个点了没反应的按钮比没有按钮更糟。
 *
 * ── 「软件版本检查」不渲染 ──────────────────────────────────
 * 它的配置入口收在「设置 → 更新设置」弹窗里。同一件事两个入口会让人以为是
 * 两件事。后端照常执行，这里只留一句话说明它去哪了。
 *
 * ── 间隔输入的坑 ────────────────────────────────────────────
 * 编辑期间由**本地草稿**接管 value，外部轮询回来的值只在草稿为空时写入 ——
 * 否则用户敲到一半的数字会被后台刷新冲掉。这是上游在同一个地方踩过的坑。
 */
@Composable
fun PeriodicTasksPage(
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    var snap by remember { mutableStateOf<PeriodicTaskOps.Snap?>(null) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    fun reload() {
        scope.launch {
            val s = PeriodicTaskOps.load()
            if (s != null) {
                snap = s
                error = null
            } else {
                error = "读取失败（内核未就绪或端点不可用）"
            }
        }
    }

    DisposableEffect(Unit) {
        CheckinOps.configure(GatewayPrefs.apiKey(ctx))
        reload()
        onDispose { }
    }

    // 停留本页 20 秒一轮（与上游「定时任务」页的轮询节奏同量级）
    LaunchedEffect(Unit) {
        while (true) {
            delay(20_000L)
            reload()
        }
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgateSubPageHeader("周期任务", onBack = onBack) {
            val s = snap
            if (s != null) {
                AgateBadge("${s.enabledCount} / ${s.visible.size} 启用",
                    if (s.enabledCount > 0) BadgeTone.GREEN else BadgeTone.GRAY)
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
                TaskCard { Text(error!!, color = C.red, fontSize = 13.sp) }
                TaskGap()
            }

            val s = snap
            if (s == null) {
                TaskCard { Text("读取中…", style = AgateType.BodyS, color = C.textDim) }
            } else {
                SectionLabel("网关执行 · backend")
                Spacer(Modifier.height(6.dp))
                TaskCard {
                    s.backend.forEachIndexed { index, row ->
                        if (index > 0) TaskDivider()
                        BackendTaskRow(
                            row = row,
                            busy = busy,
                            onToggle = { on ->
                                busy = true
                                scope.launch {
                                    val r = PeriodicTaskOps.configure(row.id, enabled = on)
                                    busy = false
                                    toast(if (r.ok) "已${if (on) "启用" else "停用"}「${row.label}」"
                                    else "保存失败：${r.error}")
                                    reload()
                                }
                            },
                            onInterval = { value ->
                                busy = true
                                scope.launch {
                                    val r = PeriodicTaskOps.configure(row.id, interval = value)
                                    busy = false
                                    toast(if (r.ok) "间隔已改为 $value ${PeriodicTaskOps.Row.unitLabel(row.unit)}"
                                    else "保存失败：${r.error}")
                                    reload()
                                }
                            },
                            onRun = {
                                busy = true
                                scope.launch {
                                    val r = PeriodicTaskOps.runNow(row.id)
                                    busy = false
                                    toast(
                                        if (r.ok) "「${row.label}」已执行"
                                        else "执行失败：${r.error}"
                                    )
                                    reload()
                                }
                            },
                        )
                    }
                    // 被隐藏的那一条给一句话交代，不留悬空
                    Spacer(Modifier.height(6.dp))
                    TaskFold(
                        "「软件版本检查」不在这里 —— 它的配置入口在「设置 → 更新设置」。" +
                            "后端照常按间隔执行，只是不在本页渲染（同一件事两个入口会让人以为是两件事）。"
                    )
                }

                TaskGap()

                SectionLabel("页面执行 · frontend（停留在对应页时才刷新）")
                Spacer(Modifier.height(6.dp))
                TaskCard {
                    s.frontend.forEachIndexed { index, row ->
                        if (index > 0) TaskDivider()
                        FrontendTaskRow(
                            row = row,
                            busy = busy,
                            onToggle = { on ->
                                busy = true
                                scope.launch {
                                    val r = PeriodicTaskOps.configure(row.id, enabled = on)
                                    busy = false
                                    toast(if (r.ok) "已${if (on) "启用" else "停用"}「${row.label}」"
                                    else "保存失败：${r.error}")
                                    reload()
                                }
                            },
                            onInterval = { value ->
                                busy = true
                                scope.launch {
                                    val r = PeriodicTaskOps.configure(row.id, interval = value)
                                    busy = false
                                    toast(if (r.ok) "间隔已改为 $value ${PeriodicTaskOps.Row.unitLabel(row.unit)}"
                                    else "保存失败：${r.error}")
                                    reload()
                                }
                            },
                        )
                    }
                    Spacer(Modifier.height(6.dp))
                    TaskFold(
                        "这三条为什么没有「立即执行」：执行者是页面自己的定时器。" +
                            "按钮点了也没有任何东西可跑，所以宁可不给。"
                    )
                }
            }

            TaskGap(24)
        }
    }
}

@Composable
private fun SectionLabel(text: String) {
    val C = agateColors()
    Text(text, style = AgateType.Caption, color = C.textDim, fontWeight = FontWeight.SemiBold)
}

/** backend 任务行：开关 + 描述 + 间隔编辑 + 立即执行。 */
@Composable
private fun BackendTaskRow(
    row: PeriodicTaskOps.Row,
    busy: Boolean,
    onToggle: (Boolean) -> Unit,
    onInterval: (Int) -> Unit,
    onRun: () -> Unit,
) {
    val C = agateColors()
    var editing by remember(row.id) { mutableStateOf(false) }
    var draft by remember(row.id) { mutableStateOf("") }

    Column(Modifier.fillMaxWidth().padding(vertical = 9.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(
                    row.label,
                    color = C.textMain,
                    fontSize = 14.sp,
                    fontWeight = FontWeight.Medium,
                )
            }
            if (row.running) {
                AgateBadge("执行中", BadgeTone.AMBER)
                Spacer(Modifier.width(6.dp))
            }
            Switch(
                checked = row.enabled,
                enabled = !busy,
                onCheckedChange = onToggle,
            )
        }
        if (row.description.isNotBlank()) {
            Text(
                row.description,
                style = AgateType.Caption,
                color = C.textDim,
                lineHeight = 15.sp,
                modifier = Modifier.padding(top = 2.dp),
            )
        }
        Spacer(Modifier.height(6.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            // 间隔：点开就地编辑（草稿接管，避免轮询冲掉输入）
            if (editing) {
                Row(
                    Modifier
                        .background(C.insetBg, RoundedCornerShape(9.dp))
                        .padding(horizontal = 8.dp, vertical = 5.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    BasicTextField(
                        value = draft,
                        onValueChange = { draft = it.filter { ch -> ch.isDigit() }.take(6) },
                        singleLine = true,
                        textStyle = TextStyle(color = C.textMain, fontSize = 13.sp),
                        keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                            keyboardType = KeyboardType.Number
                        ),
                        cursorBrush = androidx.compose.ui.graphics.SolidColor(C.accent),
                        modifier = Modifier.width(48.dp),
                    )
                    Text(
                        PeriodicTaskOps.Row.unitLabel(row.unit),
                        style = AgateType.Caption,
                        color = C.textDim,
                    )
                }
                Spacer(Modifier.width(5.dp))
                TaskBtn("取消", enabled = !busy) {
                    editing = false
                    draft = ""
                }
                Spacer(Modifier.width(5.dp))
                TaskBtn("保存", primary = true, enabled = !busy && draft.toIntOrNull() != null) {
                    val v = draft.toIntOrNull()
                    if (v == null) {
                        return@TaskBtn
                    }
                    // 越界就地拦下并说明范围 —— 让内核 400 只会说「超出范围」，
                    // 用户还得回头看这条任务允许多少
                    if (v < row.min || v > row.max) {
                        draft = row.interval.toString()
                    } else {
                        editing = false
                        draft = ""
                        onInterval(v)
                    }
                }
            } else {
                Text(
                    row.intervalText,
                    color = C.textMain,
                    fontSize = 12.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .background(C.insetBg, RoundedCornerShape(9.dp))
                        .clickable(enabled = !busy) {
                            draft = row.interval.toString()
                            editing = true
                        }
                        .padding(horizontal = 8.dp, vertical = 5.dp),
                )
                if (editing) {
                    Text(
                        "范围 ${row.min}–${row.max}",
                        style = AgateType.Caption,
                        color = C.red,
                        modifier = Modifier.padding(start = 5.dp),
                    )
                }
            }

            Spacer(Modifier.weight(1f))
            Text(
                buildString {
                    when {
                        !row.enabled -> append("已停用")
                        row.nextRunAt > 0 -> append(relTime(row.nextRunAt, prefix = "下次 "))
                        else -> append("待排期")
                    }
                    row.lastResult?.let { append(" · $it") }
                },
                style = AgateType.Caption,
                color = if (row.lastError != null) C.red else C.textDim,
                modifier = Modifier.padding(end = 6.dp),
            )
            // 只有 canRun 的任务才有「立即执行」—— 后端按 runner 算好这个布尔
            if (row.canRun) {
                TaskBtn("立即执行", outlined = true, enabled = !busy && !row.running) { onRun() }
            }
        }
        if (editing && (draft.toIntOrNull()?.let { it < row.min || it > row.max } == true)) {
            Text(
                "范围 ${row.min}–${row.max} ${PeriodicTaskOps.Row.unitLabel(row.unit)}",
                style = AgateType.Caption,
                color = C.red,
                modifier = Modifier.padding(top = 3.dp),
            )
        }
    }
}

/** frontend 任务行：开关 + 间隔 + 一句「谁在跑」。**没有**执行按钮。 */
@Composable
private fun FrontendTaskRow(
    row: PeriodicTaskOps.Row,
    busy: Boolean,
    onToggle: (Boolean) -> Unit,
    onInterval: (Int) -> Unit,
) {
    val C = agateColors()
    var editing by remember(row.id) { mutableStateOf(false) }
    var draft by remember(row.id) { mutableStateOf("") }

    Row(
        Modifier.fillMaxWidth().padding(vertical = 9.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(row.label, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.Medium)
            Text(
                row.description.ifBlank { "停留在对应页面时按间隔重新拉取" },
                style = AgateType.Caption,
                color = C.textDim,
                lineHeight = 15.sp,
            )
            Spacer(Modifier.height(5.dp))
            if (editing) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Row(
                        Modifier
                            .background(C.insetBg, RoundedCornerShape(9.dp))
                            .padding(horizontal = 8.dp, vertical = 5.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        BasicTextField(
                            value = draft,
                            onValueChange = { draft = it.filter { ch -> ch.isDigit() }.take(6) },
                            singleLine = true,
                            textStyle = TextStyle(color = C.textMain, fontSize = 13.sp),
                            keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                                keyboardType = KeyboardType.Number
                            ),
                            cursorBrush = androidx.compose.ui.graphics.SolidColor(C.accent),
                            modifier = Modifier.width(48.dp),
                        )
                        Text(
                            PeriodicTaskOps.Row.unitLabel(row.unit),
                            style = AgateType.Caption,
                            color = C.textDim,
                        )
                    }
                    Spacer(Modifier.width(5.dp))
                    TaskBtn("取消") { editing = false }
                    Spacer(Modifier.width(5.dp))
                    TaskBtn(
                        "保存",
                        primary = true,
                        enabled = !busy && draft.toIntOrNull() in row.min..row.max,
                    ) {
                        val v = draft.toIntOrNull() ?: return@TaskBtn
                        editing = false
                        onInterval(v)
                    }
                }
                if (draft.toIntOrNull()?.let { it < row.min || it > row.max } == true) {
                    Text(
                        "范围 ${row.min}–${row.max}",
                        style = AgateType.Caption,
                        color = C.red,
                        modifier = Modifier.padding(top = 3.dp),
                    )
                }
            } else {
                Text(
                    row.intervalText,
                    color = C.textMain,
                    fontSize = 12.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .background(C.insetBg, RoundedCornerShape(9.dp))
                        .clickable(enabled = !busy) {
                            draft = row.interval.toString()
                            editing = true
                        }
                        .padding(horizontal = 8.dp, vertical = 5.dp),
                )
            }
        }
        Spacer(Modifier.width(8.dp))
        Switch(checked = row.enabled, enabled = !busy, onCheckedChange = onToggle)
    }
}
