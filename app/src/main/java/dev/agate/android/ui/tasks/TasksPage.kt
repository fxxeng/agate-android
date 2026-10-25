package dev.agate.android.ui.tasks

import android.widget.Toast
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
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.PresetRegistry
import dev.agate.android.data.TaskCenter
import dev.agate.android.data.TaskOps
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.components.AgateAvatar
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateChip
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.AgatePrimaryAction
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch

/**
 * 任务中心（M4，对齐 v44 Tab 4）：
 * - 三档 seg：今日 / 福利 / 定时
 * - 今日：一键签到 + 签到进度（按提供商）+ 待处理（v44 聚合项，内核无对应端点→诚实占位）
 * - 福利：福利中心 / 验证码池（内核无对应端点 → 诚实占位）
 * - 定时：日活保活链（真数据，可保存）+ 定时签到计划（真数据，可立即执行）+ 周期任务入口
 */
@Composable
fun TasksPage(
    onOpenDrawer: () -> Unit = {},
    onOpenWelfare: () -> Unit = {},
    onOpenCaptcha: () -> Unit = {},
    onOpenCheckin: () -> Unit = {},
    onOpenKeepalive: () -> Unit = {},
    onOpenSchedules: () -> Unit = {},
    onOpenHistory: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val snap by GatewayRepository.snapshot.collectAsState()
    val task by TaskCenter.snapshot.collectAsState()

    DisposableEffect(Unit) {
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
        TaskCenter.configure(GatewayPrefs.apiKey(ctx))
        TaskOps.configure(GatewayPrefs.apiKey(ctx))
        TaskCenter.startPolling()
        onDispose { TaskCenter.stopPolling() }
    }

    var tab by remember { mutableIntStateOf(0) }
    var busy by remember { mutableStateOf(false) }

    fun toast(msg: String) = Toast.makeText(ctx, msg, Toast.LENGTH_SHORT).show()

    fun runNow() {
        if (busy) return
        busy = true
        scope.launch {
            toast("已加入签到队列…")
            val r = TaskOps.runNow()
            busy = false
            toast(if (r.ok) "签到指令已下发" else "签到失败：${r.error ?: "未知"}")
            TaskCenter.refreshOnce()
        }
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "任务中心", onOpenDrawer = onOpenDrawer) {
            if (busy) {
                AgateBadge("执行中…", BadgeTone.AMBER)
            } else {
                AgatePrimaryAction("一键签到") { runNow() }
            }
        }

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))
            AgateSegmented(
                options = listOf("今日", "福利", "定时"),
                selected = tab,
                onSelect = { tab = it },
            )
            Spacer(Modifier.height(12.dp))

            when (tab) {
                0 -> TodayView(snap = snap, task = task, onOpenCheckin = onOpenCheckin, onOpenHistory = onOpenHistory, onRunNow = { runNow() })
                1 -> WelfareView(onOpenWelfare = onOpenWelfare, onOpenCaptcha = onOpenCaptcha)
                else -> ScheduleView(
                    task = task,
                    onToggleMaster = { on ->
                        scope.launch {
                            val r = TaskOps.setEnabled(on)
                            toast(if (r.ok) "定时签到已${if (on) "开启" else "关闭"}" else "操作失败：${r.error}")
                            TaskCenter.refreshOnce()
                        }
                    },
                    onRunSchedule = { id ->
                        scope.launch {
                            val r = TaskOps.runSchedule(id)
                            toast(if (r.ok) "已下发该计划" else "执行失败：${r.error}")
                            TaskCenter.refreshOnce()
                        }
                    },
                    onSaveKeepalive = { models ->
                        scope.launch {
                            val r = TaskOps.saveKeepalive(models)
                            toast(if (r.ok) "保活链已保存" else "保存失败：${r.error}")
                            TaskCenter.refreshOnce()
                        }
                    },
                    onOpenSchedules = onOpenSchedules,
                    onOpenKeepalive = onOpenKeepalive,
                )
            }
            Spacer(Modifier.height(28.dp))
        }
    }
}

/* ── 今日 ── */
@Composable
private fun TodayView(
    snap: GatewayRepository.ConsoleSnapshot,
    task: TaskCenter.TaskSnapshot,
    onOpenCheckin: () -> Unit,
    onOpenHistory: () -> Unit,
    onRunNow: () -> Unit,
) {
    val C = agateColors()
    val okCount = task.schedules.count { it.lastFiredToday && it.enabled }
    val totalEnabled = task.schedules.count { it.enabled }

    // ── 签到进度（v44 「今日福利收益」卡的位置，这里接真实签到态）──
    Column(
        Modifier
            .fillMaxWidth()
            .background(C.accent.copy(alpha = 0.08f), RoundedCornerShape(20.dp))
            .border(1.dp, C.accent.copy(alpha = 0.35f), RoundedCornerShape(20.dp))
            .padding(16.dp),
    ) {
        Text("定时签到进度", color = C.textSub, fontSize = 12.sp)
        Spacer(Modifier.height(6.dp))
        Row(verticalAlignment = Alignment.Bottom) {
            Text(
                if (task.fetchedAtMs == 0L) "—" else "$okCount / $totalEnabled",
                color = C.textMain,
                fontSize = 26.sp,
                fontWeight = FontWeight.Bold,
            )
            Spacer(Modifier.width(6.dp))
            Text("条计划今日已跑", color = C.textSub, fontSize = 13.sp)
        }
        Spacer(Modifier.height(8.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            AgateBadge(if (task.enabled) "总开关开" else "总开关关", if (task.enabled) BadgeTone.GREEN else BadgeTone.GRAY)
            if (task.running) {
                Spacer(Modifier.width(6.dp))
                AgateBadge("正在执行", BadgeTone.AMBER)
            }
            Spacer(Modifier.weight(1f))
            nextRunText(task.nextRunAt)?.let {
                Text(it, color = C.accent, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
            }
        }
        Spacer(Modifier.height(12.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            SmallButton("立即全量签到", primary = true, onClick = onRunNow)
            SmallButton("签到台账", primary = false, onClick = onOpenHistory)
        }
    }

    Spacer(Modifier.height(12.dp))

    // ── 待处理（v44 聚合项；内核无对应端点，列账号池真实状态供人工判断）──
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(16.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("待处理", style = AgateType.SectionTitle, fontWeight = FontWeight.SemiBold, color = C.textMain, modifier = Modifier.weight(1f))
            AgateBadge("${snap.accountsTotal - snap.accountsEnabled} 项停用", if (snap.accountsTotal > snap.accountsEnabled) BadgeTone.AMBER else BadgeTone.GRAY)
        }
        Spacer(Modifier.height(10.dp))
        val pending = snap.accountList.filter { !it.enabled || !it.hasCredentials }
        if (pending.isEmpty()) {
            Text("账号池无停用 / 缺凭证账号", color = C.textSub, fontSize = 13.sp)
        } else {
            pending.take(6).forEach { acc ->
                Row(Modifier.fillMaxWidth().padding(vertical = 7.dp), verticalAlignment = Alignment.CenterVertically) {
                    AgateAvatar(name = acc.displayName, size = 34.dp, corner = 11.dp, letters = acc.displayName.take(2).uppercase())
                    Spacer(Modifier.width(10.dp))
                    Column(Modifier.weight(1f)) {
                        Text(acc.displayName, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.Medium)
                        Text(PresetRegistry.labelOf(acc.provider) + (if (!acc.hasCredentials) " · 缺凭证" else " · 已停用"), color = C.textSub, fontSize = 12.sp)
                    }
                    AgateBadge(if (!acc.hasCredentials) "缺凭证" else "停用", if (!acc.hasCredentials) BadgeTone.RED else BadgeTone.GRAY)
                }
            }
        }
        Spacer(Modifier.height(6.dp))
        Text(
            "福利领取类聚合项（每日签到/新手/活动）需任务领取端点，当前内核未暴露，先以账号状态呈现。",
            style = AgateType.Caption,
            color = C.textDim,
        )
    }

    Spacer(Modifier.height(12.dp))

    // ── 签到进度 · 按提供商（真数据：各计划覆盖的提供商）──
    if (task.schedules.isNotEmpty()) {
        Column(
            Modifier
                .fillMaxWidth()
                .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
                .border(1.dp, C.border, RoundedCornerShape(20.dp))
                .padding(16.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("签到计划 · 按条目", style = AgateType.SectionTitle, fontWeight = FontWeight.SemiBold, color = C.textMain, modifier = Modifier.weight(1f))
                AgateBadge("${task.schedules.size} 条", BadgeTone.GRAY)
            }
            Spacer(Modifier.height(8.dp))
            task.schedules.forEach { s ->
                Row(Modifier.fillMaxWidth().padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(s.time, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                    Spacer(Modifier.width(10.dp))
                    Text(
                        if (s.providers.isEmpty()) "全部提供商" else s.providers.joinToString("·") { PresetRegistry.labelOf(it) },
                        color = C.textSub,
                        fontSize = 12.sp,
                        modifier = Modifier.weight(1f),
                    )
                    AgateBadge(
                        if (!s.enabled) "停用" else if (s.lastFiredToday) "今日已跑" else "待执行",
                        when {
                            !s.enabled -> BadgeTone.GRAY
                            s.lastFiredToday -> BadgeTone.GREEN
                            else -> BadgeTone.AMBER
                        },
                    )
                }
            }
        }
        Spacer(Modifier.height(12.dp))
    }

    val keep = task.keepalive
    if (keep.isNotEmpty()) {
        Column(
            Modifier
                .fillMaxWidth()
                .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
                .border(1.dp, C.border, RoundedCornerShape(20.dp))
                .padding(16.dp),
        ) {
            Text("日活保活链", style = AgateType.SectionTitle, fontWeight = FontWeight.SemiBold, color = C.textMain)
            Spacer(Modifier.height(8.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                keep.take(6).forEach { AgateChip(it, selected = true) }
            }
            Spacer(Modifier.height(6.dp))
            Text("在「定时」页可编辑保活模型链", style = AgateType.Caption, color = C.textDim)
        }
    }
}

/* ── 福利（诚实占位：内核无对应端点）── */
@Composable
private fun WelfareView(onOpenWelfare: () -> Unit, onOpenCaptcha: () -> Unit) {
    val C = agateColors()
    InfoCard(
        title = "福利中心",
        badge = "待接入",
        badgeTone = BadgeTone.AMBER,
        body = "v44 的三类福利（每日签到 / 新手福利 / 活动福利）需要任务领取聚合端点，" +
            "当前内核未暴露。已就绪的是「定时签到」与「日活保活链」，见「定时」页。",
        action = "查看占位页 ❯",
        onAction = onOpenWelfare,
    )
    Spacer(Modifier.height(12.dp))
    InfoCard(
        title = "验证码池 · ZCode 活动套餐",
        badge = "待接入",
        badgeTone = BadgeTone.AMBER,
        body = "票据池水位 / 自动铸造 / 手动过码（阿里 V3 滑块）需 captcha 铸造与水位端点，" +
            "当前内核未暴露；接口就绪后在本卡接入。",
        action = "查看占位页 ❯",
        onAction = onOpenCaptcha,
    )
}

/* ── 定时（真数据）── */
@Composable
private fun ScheduleView(
    task: TaskCenter.TaskSnapshot,
    onToggleMaster: (Boolean) -> Unit,
    onRunSchedule: (String) -> Unit,
    onSaveKeepalive: (List<String>) -> Unit,
    onOpenSchedules: () -> Unit,
    onOpenKeepalive: () -> Unit,
) {
    val C = agateColors()

    // ── 日活保活链（可编辑）──
    var keepText by remember(task.keepalive) { mutableStateOf(task.keepalive.joinToString(", ")) }
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(16.dp),
    ) {
        Text("日活保活链", style = AgateType.SectionTitle, fontWeight = FontWeight.SemiBold, color = C.textMain)
        Spacer(Modifier.height(6.dp))
        Text("按序点名免费模型，逗号 / 顿号分隔", style = AgateType.Caption, color = C.textDim)
        Spacer(Modifier.height(8.dp))
        androidx.compose.foundation.text.BasicTextField(
            value = keepText,
            onValueChange = { keepText = it },
            textStyle = androidx.compose.ui.text.TextStyle(color = C.textMain, fontSize = 13.sp),
            cursorBrush = androidx.compose.ui.graphics.SolidColor(C.accent),
            modifier = Modifier
                .fillMaxWidth()
                .background(C.insetBg, RoundedCornerShape(12.dp))
                .border(1.dp, C.border, RoundedCornerShape(12.dp))
                .padding(12.dp),
        )
        Spacer(Modifier.height(10.dp))
        SmallButton("保存保活链", primary = true) {
            onSaveKeepalive(keepText.split(',', '，', '、').map { it.trim() }.filter { it.isNotBlank() })
        }
    }

    Spacer(Modifier.height(12.dp))

    // ── 定时签到（真数据）──
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(16.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("定时签到", style = AgateType.SectionTitle, fontWeight = FontWeight.SemiBold, color = C.textMain, modifier = Modifier.weight(1f))
            Switch(checked = task.enabled, onCheckedChange = onToggleMaster)
        }
        Spacer(Modifier.height(8.dp))
        if (task.schedules.isEmpty()) {
            Text("暂无签到计划 · 可在「定时签到」页新增", color = C.textSub, fontSize = 13.sp)
        } else {
            task.schedules.forEach { s ->
                Row(
                    Modifier
                        .fillMaxWidth()
                        .padding(vertical = 4.dp)
                        .background(C.insetBg, RoundedCornerShape(12.dp))
                        .clickable { onRunSchedule(s.id) }
                        .padding(horizontal = 12.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text("⏱ ${s.time}", color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                    Spacer(Modifier.width(10.dp))
                    Text(
                        s.displayLabel,
                        color = C.textSub,
                        fontSize = 12.sp,
                        modifier = Modifier.weight(1f),
                    )
                    AgateBadge(if (s.enabled) "启用" else "停用", if (s.enabled) BadgeTone.GREEN else BadgeTone.GRAY)
                }
            }
        }
        Spacer(Modifier.height(8.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("下次闹钟（北京时间）", color = C.textSub, fontSize = 12.sp, modifier = Modifier.weight(1f))
            Text(nextRunText(task.nextRunAt) ?: "—", color = C.accent, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
        }
        Spacer(Modifier.height(8.dp))
        Text("点条目立即执行 · 计划 CRUD 在「定时签到」页", style = AgateType.Caption, color = C.textDim)
    }
}

@Composable
private fun InfoCard(title: String, badge: String, badgeTone: BadgeTone, body: String, action: String, onAction: () -> Unit) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(16.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(title, style = AgateType.SectionTitle, fontWeight = FontWeight.SemiBold, color = C.textMain, modifier = Modifier.weight(1f))
            AgateBadge(badge, badgeTone)
        }
        Spacer(Modifier.height(8.dp))
        Text(body, style = AgateType.BodyS, color = C.textSub)
        Spacer(Modifier.height(10.dp))
        Text(action, color = C.accent, fontSize = 12.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.clickable(onClick = onAction))
    }
}

@Composable
private fun SmallButton(text: String, primary: Boolean, onClick: () -> Unit) {
    val C = agateColors()
    Text(
        text,
        color = if (primary) C.onAccent else C.textMain,
        fontSize = 13.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier
            .background(if (primary) C.accent else C.btnBg, RoundedCornerShape(9.dp))
            .clickable(onClick = onClick)
            .padding(horizontal = 14.dp, vertical = 8.dp),
    )
}

private fun nextRunText(nextRunAt: Long?): String? {
    if (nextRunAt == null || nextRunAt <= 0) return null
    val diff = nextRunAt - System.currentTimeMillis()
    if (diff <= 0) return "即将执行"
    val mins = diff / 60000
    val h = mins / 60
    val m = mins % 60
    return if (h > 0) "下次约 ${h}h${m}m 后" else "下次约 ${m}m 后"
}