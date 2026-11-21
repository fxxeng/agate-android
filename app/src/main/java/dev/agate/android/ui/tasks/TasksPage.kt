package dev.agate.android.ui.tasks

import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
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
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.CheckinCenter
import dev.agate.android.data.CheckinOps
import dev.agate.android.data.PresetRegistry
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.AgatePrimaryAction
import dev.agate.android.ui.components.AgateProgress
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch
import org.json.JSONObject

/**
 * 任务中心（一级 Tab）—— 对齐上游 3.0.8 的「签到中心 + 周期任务」两个一级入口。
 *
 * ── 这一页只做一件事：告诉你现在什么状态、往哪进 ─────────────
 * 原来这里是「今日 / 福利 / 定时」三档 seg，把上游的**三个 tab** 和
 * **另一个一级页**混在一起切。现在换成四个入口，副标全是真实读数。
 *
 * ── 为什么首页不直接铺签到分组 ──────────────────────────────
 * 上游「签到中心」是一个 tab 的位置，摊平到首页会让首页变成 12 家 × N 个
 * 账号的长列表。首页给判断，详情页给操作。
 *
 * ── 验证码池为什么只是一张读数卡 ────────────────────────────
 * 上游**没有这个 UI**：`ui/zcode-captcha-pool.js` 是后台守卫脚本（每 2.5s
 * 轮询，`needsTokens` 才补货），页面里没有挂载点。手机上不铸令牌，
 * 所以这里只给读数与节奏设置（见 [CaptchaSheet]）。
 *
 * ── 任务类型标签组 ──────────────────────────────────────────
 * 参考上游签到中心的 sections 分类：全部/每日签到/新手福利/活动福利/成长
 * 任务。标签只切「渲染哪张卡」，读数全来自 [CheckinCenter.Snap]，不额外打
 * 上游；点某类的「进入签到中心」带上该档，签到中心打开就停在对应档。
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun TasksPage(
    onOpenDrawer: () -> Unit = {},
    /** 进入签到中心；参数是目标档位（all/daily/onboarding/plans/growth）。 */
    onOpenCheckin: (String) -> Unit = {},
    onOpenSchedule: () -> Unit = {},
    onOpenRecords: () -> Unit = {},
    onOpenRecovery: () -> Unit = {},
    onOpenPeriodic: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val snap by CheckinCenter.snapshot.collectAsState()
    val error by CheckinCenter.error.collectAsState()

    var busy by remember { mutableStateOf(false) }
    var captcha by remember { mutableStateOf<JSONObject?>(null) }
    var showCaptchaSheet by remember { mutableStateOf(false) }
    /** 任务类型标签组选中档；只决定渲染哪张卡，不参与取数。 */
    var taskTier by remember { mutableStateOf(CheckinTierFocus.ALL) }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    DisposableEffect(Unit) {
        val key = GatewayPrefs.apiKey(ctx)
        CheckinCenter.configure(key)
        CheckinOps.configure(key)
        CheckinCenter.startPolling()
        onDispose { CheckinCenter.stopPolling() }
    }

    // 验证码池读数：与签到中心不同一条端点，单独拉（页面上只有一张卡要看它）
    LaunchedEffect(Unit) {
        captcha = CheckinOps.captchaStats()
    }

    fun signAll() {
        if (busy) return
        busy = true
        scope.launch {
            toast("已下发全量签到…")
            val r = CheckinOps.signAll()
            busy = false
            toast(if (r.ok) "签到指令已下发" else "签到失败：${r.error ?: "未知"}")
            CheckinCenter.refreshOnce()
        }
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "任务中心", onOpenDrawer = onOpenDrawer) {
            if (busy) {
                AgateBadge("执行中…", BadgeTone.AMBER)
            } else {
                AgatePrimaryAction("立即全签") { signAll() }
            }
        }

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(12.dp))

            if (error != null && snap.fetchedAtMs == 0L) {
                TaskCard {
                    Text(error!!, color = C.red, fontSize = 13.sp)
                }
                TaskGap()
            }

            TodayProgressCard(snap = snap, onOpenCheckin = { onOpenCheckin(CheckinTierFocus.ALL) })
            TaskGap()

            TierSection(
                snap = snap,
                tier = taskTier,
                onTier = { taskTier = it },
                onOpenCheckin = onOpenCheckin,
                onOpenSchedule = onOpenSchedule,
                onOpenRecords = onOpenRecords,
                onOpenRecovery = onOpenRecovery,
                onOpenPeriodic = onOpenPeriodic,
            )
            TaskGap()

            CaptchaCard(
                stats = captcha,
                onOpenSettings = { showCaptchaSheet = true },
            )

            TaskGap(24)
        }
    }

    if (showCaptchaSheet) {
        CaptchaSheet(
            onDismiss = { showCaptchaSheet = false },
            onSaved = {
                showCaptchaSheet = false
                scope.launch { captcha = CheckinOps.captchaStats() }
            },
        )
    }
}

/* ── 今日进度 ── */

@Composable
private fun TodayProgressCard(snap: CheckinCenter.Snap, onOpenCheckin: () -> Unit) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .background(
                C.accent.copy(alpha = 0.08f),
                RoundedCornerShape(20.dp),
            )
            .border(1.dp, C.accent.copy(alpha = 0.35f), RoundedCornerShape(20.dp))
            .padding(16.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("今日签到进度", color = C.textSub, fontSize = 12.sp, modifier = Modifier.weight(1f))
            val outCount = snap.outOfScope.sumOf { it.count }
            if (outCount > 0) {
                TaskTag("$outCount 个不参与", C.amber)
            }
        }
        Spacer(Modifier.height(6.dp))
        Row(verticalAlignment = Alignment.Bottom) {
            Text(
                if (snap.fetchedAtMs == 0L) "—" else "${snap.todayDone}",
                color = C.textMain,
                fontSize = 28.sp,
                fontWeight = FontWeight.Bold,
            )
            Text(
                " / ${snap.todayEligible}",
                color = C.textSub,
                fontSize = 15.sp,
                fontWeight = FontWeight.Medium,
                modifier = Modifier.padding(bottom = 3.dp),
            )
        }
        Spacer(Modifier.height(7.dp))
        AgateProgress(fraction = snap.progress, thick = true)
        Spacer(Modifier.height(7.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                buildString {
                    append(
                        when {
                            !snap.autoEnabled -> "总开关关：定时条目不会自动跑"
                            snap.nextRunAt != null -> relTime(snap.nextRunAt, prefix = "下次 ").replace("约 ", "")
                            else -> "总开关开"
                        }
                    )
                    if (snap.schedulesTotal > 0) {
                        append(" · 条目 ${snap.schedulesEnabled}/${snap.schedulesTotal} 启用")
                    }
                },
                style = AgateType.Caption,
                color = C.textSub,
                modifier = Modifier.weight(1f),
            )
            Text(
                "看签到中心 ›",
                color = C.accent,
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(C.accent.copy(alpha = 0.12f), RoundedCornerShape(8.dp))
                    .padding(horizontal = 9.dp, vertical = 5.dp)
                    .then(Modifier),
            )
        }
        Spacer(Modifier.height(6.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(7.dp)) {
            TaskBtn("进入签到中心", primary = true) { onOpenCheckin() }
        }
    }
}

/* ── 任务类型标签组（全部 / 每日 / 新手 / 活动 / 成长）── */

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun TierSection(
    snap: CheckinCenter.Snap,
    tier: String,
    onTier: (String) -> Unit,
    onOpenCheckin: (String) -> Unit,
    onOpenSchedule: () -> Unit,
    onOpenRecords: () -> Unit,
    onOpenRecovery: () -> Unit,
    onOpenPeriodic: () -> Unit,
) {
    val C = agateColors()
    TaskCard {
        TaskCardHeader(title = "任务类型")
        TaskDivider()
        Spacer(Modifier.height(8.dp))

        // 第一行：任务类型标签（切渲染哪张卡，不取数）。复用签到中心的 NavChip。
        FlowRow(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            CheckinTierFocus.TIERS.forEach { (value, label) ->
                NavChip(text = label, selected = tier == value, onClick = { onTier(value) })
            }
        }
        Spacer(Modifier.height(10.dp))

        // 第二行内容区：全部档给四行摘要；单类型档给该类读数 + 直达按钮。
        when (tier) {
            CheckinTierFocus.ALL -> TierSummary(snap, onOpenCheckin)
            CheckinTierFocus.DAILY -> TierDetail(
                title = "每日签到",
                reading = buildString {
                    append("今日 ${snap.todayDone}/${snap.todayEligible} 已签")
                    if (snap.keepalive.isNotEmpty()) append(" · 保活链 ${snap.keepalive.size} 个模型")
                },
                enterLabel = "进入签到中心 · 每日",
                onEnter = { onOpenCheckin(CheckinTierFocus.DAILY) },
            )
            CheckinTierFocus.ONBOARDING -> TierDetail(
                title = "新手福利",
                reading = "待查 ${snap.unsettledOnboarding} 项 · 一次性福利，领完不重复查",
                enterLabel = "进入签到中心 · 新手",
                onEnter = { onOpenCheckin(CheckinTierFocus.ONBOARDING) },
            )
            CheckinTierFocus.PLANS -> TierDetail(
                title = "活动福利",
                reading = "${snap.plans.size} 个套餐账号 · 领取需滑块验证",
                enterLabel = "进入签到中心 · 活动",
                onEnter = { onOpenCheckin(CheckinTierFocus.PLANS) },
            )
            CheckinTierFocus.GROWTH -> TierDetail(
                title = "成长任务",
                reading = "${snap.growth.size} 个账号 · 接单 + 点亮 + 领奖",
                enterLabel = "进入签到中心 · 成长",
                onEnter = { onOpenCheckin(CheckinTierFocus.GROWTH) },
            )
        }

        Spacer(Modifier.height(10.dp))
        TaskDivider()
        Spacer(Modifier.height(8.dp))

        // 功能页入口：不属于任务类型，常驻在卡尾，不受标签切换影响。
        NavRowLabel("功能页")
        Spacer(Modifier.height(4.dp))
        FlowRow(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            NavChip("定时签到", selected = false) { onOpenSchedule() }
            NavChip("签到记录", selected = false) { onOpenRecords() }
            NavChip("补签恢复", selected = false) { onOpenRecovery() }
            NavChip("周期任务", selected = false) { onOpenPeriodic() }
        }
    }
}

/** 「全部」档：四类各一行摘要，点行进签到中心对应档（不是只切标签）。 */
@Composable
private fun TierSummary(
    snap: CheckinCenter.Snap,
    onOpenCheckin: (String) -> Unit,
) {
    val C = agateColors()
    val unsettled = snap.unsettledOnboarding
    val planCount = snap.plans.size
    val growthCount = snap.growth.size

    Column {
        TaskRow(
            title = "每日签到",
            avatarColorKey = "每日签到",
            letters = "签",
            subtitle = "今日 ${snap.todayDone}/${snap.todayEligible} · 保活链 ${snap.keepalive.size} 个模型",
            onClick = { onOpenCheckin(CheckinTierFocus.DAILY) },
        ) {
            if (snap.todayEligible > 0 && snap.todayDone >= snap.todayEligible) TaskTag("已签完", C.green)
            Text("›", color = C.textDim, fontSize = 16.sp)
        }
        TaskRow(
            title = "新手福利",
            avatarColorKey = "新手福利",
            letters = "新",
            subtitle = if (unsettled > 0) "一次性福利 · 待查 $unsettled 项" else "一次性福利 · 已结清",
            onClick = { onOpenCheckin(CheckinTierFocus.ONBOARDING) },
        ) {
            if (unsettled > 0) TaskTag("$unsettled 项", C.amber)
            Text("›", color = C.textDim, fontSize = 16.sp)
        }
        TaskRow(
            title = "活动福利",
            avatarColorKey = "活动福利",
            letters = "活",
            subtitle = "${planCount} 个套餐账号 · 领取需滑块验证",
            onClick = { onOpenCheckin(CheckinTierFocus.PLANS) },
        ) {
            Text("›", color = C.textDim, fontSize = 16.sp)
        }
        TaskRow(
            title = "成长任务",
            avatarColorKey = "成长任务",
            letters = "成",
            subtitle = "$growthCount 个账号 · 接单 + 点亮 + 领奖",
            onClick = { onOpenCheckin(CheckinTierFocus.GROWTH) },
        ) {
            Text("›", color = C.textDim, fontSize = 16.sp)
        }
    }
}

/** 单类型档：真实读数 + 直达签到中心对应档的按钮。 */
@Composable
private fun TierDetail(title: String, reading: String, enterLabel: String, onEnter: () -> Unit) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .background(C.insetBg, RoundedCornerShape(12.dp))
            .padding(horizontal = 12.dp, vertical = 11.dp),
    ) {
        Text(title, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(5.dp))
        Text(reading, style = AgateType.Caption, color = C.textSub, lineHeight = 17.sp)
        Spacer(Modifier.height(9.dp))
        TaskBtn(enterLabel, primary = true, onClick = onEnter)
    }
}

/* ── 验证码池读数卡 ── */

@Composable
private fun CaptchaCard(stats: JSONObject?, onOpenSettings: () -> Unit) {
    val C = agateColors()
    val ready = stats?.optInt("ready") ?: -1
    val target = stats?.optInt("target") ?: 0
    val needs = stats?.optBoolean("needsTokens") ?: false
    val startPlan = stats?.optInt("startPlanAccounts") ?: 0

    TaskCard {
        TaskCardHeader(title = "验证码池 · ZCode 活动套餐") {
            when {
                stats == null -> AgateBadge("读取中", BadgeTone.GRAY)
                needs -> AgateBadge("待补货", BadgeTone.AMBER)
                else -> AgateBadge("$ready / $target 就绪",
                    if (target > 0 && ready >= target) BadgeTone.GREEN else BadgeTone.GRAY)
            }
        }
        Spacer(Modifier.height(6.dp))
        if (stats == null) {
            Text("尚未读到池子概况", style = AgateType.Caption, color = C.textDim)
        } else {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    buildString {
                        append("库存 ${stats.optInt("ready")} / 目标 $target")
                        append(" · 今日已铸 ${stats.optInt("minted")}")
                        append(" · 已消耗 ${stats.optInt("consumed")}")
                        append(" · 被拒 ${stats.optInt("rejected")}")
                    },
                    style = AgateType.BodyS,
                    color = C.textSub,
                    lineHeight = 18.sp,
                    modifier = Modifier.weight(1f),
                )
                TaskBtn("节奏", outlined = true) { onOpenSettings() }
            }
            if (startPlan == 0) {
                Spacer(Modifier.height(6.dp))
                Text(
                    "没有走活动套餐那条路的账号，池子空着也不会补 —— 这不代表坏了。",
                    style = AgateType.Caption,
                    color = C.textDim,
                )
            }
        }
        Spacer(Modifier.height(8.dp))
        TaskFold(
            "令牌一次性、120 秒过期。铸造是后台守卫在做（库存 < 目标且有活动套餐账号时才补，" +
                "避免白烧阿里云风控配额）—— 手机上不铸，所以这里只给读数与节奏。"
        )
    }
}
