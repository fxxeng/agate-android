package dev.agate.android.ui.tasks

import android.widget.Toast
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
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

/**
 * 签到中心 —— 对齐上游 3.0.8 `CenterTab`（`checkin-page.tsx`）。
 *
 * ── 结构 ───────────────────────────────────────────────────
 * 顶部导航卡（两行 chips：任务档切换 + 功能页跳转）→ 总览四格
 * → 每日签到（provider 筛选 chips + 按提供商分组、可展开、单账号可签）
 * → 不参与折叠说明 → 新手福利 → 活动福利 → 成长任务。
 *
 * 导航卡是**本页对上游的唯一结构性增量**（上游把 7 个入口散在路由里）。
 * 它只切「渲染哪张卡」与「显示哪家 provider 的账号行」，不参与任何取数：
 * 默认档 `all` + 默认 provider `全部` 时，下面四张卡与改造前逐行等价。
 *
 * ── 这一页零上游请求 ────────────────────────────────────────
 * 进页面只打 `GET /api/checkin-center` 一条。新手福利 / 活动套餐 / 成长任务的
 * 具体状态都是**点「查询」时才去查**（展开不自动查）—— 一次打开页面不替
 * 用户问十几个上游接口。这是内核那边刻意设计的边界（见 `api::checkin_center`
 * 的模块头：聚合快照必须保持「打开页面零上游请求」）。
 *
 * ── 国际版为什么是三颗按钮 ──────────────────────────────────
 * 判据是**分组 id**（`workbuddy-intl`），不是账号的 `edition` 字段 ——
 * Qoder 国际版也带 `edition:"intl"`，按 edition 判会把它派去 WorkBuddy
 * 日活接口并稳定 400。三档是 `keepalive` / `claim` / `full`：
 * 保活只维持活跃不领取、领取只探测并领奖、保活+领取两步都做。
 * 一颗按钮混做两件事，用户看不出「积分为什么没涨」。
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun CheckinCenterPage(
    onBack: () -> Unit,
    onOpenSchedule: () -> Unit = {},
    onOpenRecords: () -> Unit = {},
    onOpenRecovery: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val snap by CheckinCenter.snapshot.collectAsState()

    /** 展开的分组（provider id）。默认全收起：12 家全摊开会让首屏看不到福利卡。 */
    val expanded = remember { mutableStateMapOf<String, Boolean>() }
    /** 正在执行的键（账号 id + 动作），各自转圈互不牵连。 */
    var busyKey by remember { mutableStateOf<String?>(null) }
    var signingAll by remember { mutableStateOf(false) }

    /**
     * 导航卡选中的任务档：`all`（默认）/ `daily` / `onboarding` / `plans` / `growth`。
     *
     * 只决定**渲染哪张分组卡**，不参与任何取数：`all` 档与原版逐行等价（四张卡
     * 按原顺序全渲染）；选某一档时其余卡不进入组合树。`expanded` 留在页面级，
     * 所以切档来回，用户展开过的分组仍然记得。
     */
    var taskTier by remember { mutableStateOf("all") }

    /**
     * 每日签到卡内的 provider 筛选：`null` = 全部（默认）。
     *
     * 与 `expanded` 一样留在页面级 —— 切档来回不丢筛选（`DailyCard` 会离开
     * 组合树，状态留在这儿才记得住）。只做**渲染过滤**：分组与计数全部来自
     * `snap.groups`（后端已按 provider 聚合好），这里不重发请求、不改数据层。
     */
    var providerFilter by remember { mutableStateOf<String?>(null) }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    DisposableEffect(Unit) {
        val key = GatewayPrefs.apiKey(ctx)
        CheckinCenter.configure(key)
        CheckinOps.configure(key)
        CheckinCenter.startPolling()
        onDispose { CheckinCenter.stopPolling() }
    }

    fun signAll() {
        if (signingAll) return
        signingAll = true
        scope.launch {
            val r = CheckinOps.signAll()
            signingAll = false
            toast(if (r.ok) "已下发全量签到" else "签到失败：${r.error ?: "未知"}")
            CheckinCenter.refreshOnce()
        }
    }

    fun signOne(acc: CheckinCenter.AccountRow) {
        val key = "${acc.id}:sign"
        if (busyKey != null) return
        busyKey = key
        scope.launch {
            val r = CheckinOps.signAccount(acc.id)
            busyKey = null
            toast(
                if (r.ok) "${acc.name} 签到完成"
                else "${acc.name} 签到失败：${r.error ?: "未知"}"
            )
            CheckinCenter.refreshOnce()
        }
    }

    fun runActivity(acc: CheckinCenter.AccountRow, mode: String, label: String) {
        val key = "${acc.id}:$mode"
        if (busyKey != null) return
        busyKey = key
        scope.launch {
            val r = CheckinOps.runActivity(acc.id, mode)
            busyKey = null
            toast(
                if (r.ok) "${acc.name} $label 完成"
                else "${acc.name} $label 失败：${r.error ?: "未知"}"
            )
            CheckinCenter.refreshOnce()
        }
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgateSubPageHeader("签到中心", onBack = onBack) {
            if (signingAll) {
                AgateBadge("下发中…", BadgeTone.AMBER)
            } else {
                TaskBtn("全部签到", outlined = true) { signAll() }
            }
        }

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))

            // ── 导航卡：7 个入口收口成卡内两行 chips ──
            // 第一行只切「下面渲染哪张分组卡」，不碰任何卡内的取数、展开态与
            // 按钮转圈（busyKey 仍是页面级的）；第二行直接调 NavHost 传进来的
            // 三个回调 —— 本页自己不持有路由。
            TaskCard {
                TaskCardHeader(
                    title = "快速入口",
                    subtitle = if (taskTier == "all") "四档任务按原顺序全展开" else "只看当前档",
                )
                Spacer(Modifier.height(9.dp))
                // 徽标数字全部取自 snap 的真实计数字段；0 说明这档没账号 / 没欠账，
                // 此时不出数字（胶囊里留个「0」只是噪声，不许写死占位数字）。
                val dailyBadge = countBadge(snap.todayEligible)
                val onboardingBadge = countBadge(snap.unsettledOnboarding)
                val plansBadge = countBadge(snap.plans.size)
                val growthBadge = countBadge(snap.growth.size)
                FlowRow(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    NavChip("全部", taskTier == "all") { taskTier = "all" }
                    NavChip("每日签到", taskTier == "daily", dailyBadge) { taskTier = "daily" }
                    NavChip("新手福利", taskTier == "onboarding", onboardingBadge) { taskTier = "onboarding" }
                    NavChip("活动福利", taskTier == "plans", plansBadge) { taskTier = "plans" }
                    NavChip("成长任务", taskTier == "growth", growthBadge) { taskTier = "growth" }
                }
                Spacer(Modifier.height(6.dp))
                FlowRow(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    NavChip("定时签到", selected = false) { onOpenSchedule() }
                    NavChip("签到记录", selected = false) { onOpenRecords() }
                    NavChip("补签恢复", selected = false) { onOpenRecovery() }
                }
            }
            TaskGap()

            // ── 总览四格 ──
            AgateStatRow(
                cells = listOf(
                    StatCell("今日签到", "${snap.todayDone}/${snap.todayEligible}"),
                    StatCell("定时条目", "${snap.schedulesEnabled}/${snap.schedulesTotal}"),
                    StatCell(
                        "待查福利",
                        "${snap.unsettledOnboarding}",
                        if (snap.unsettledOnboarding > 0) C.amber else null,
                    ),
                    StatCell("累计轮次", "${snap.rounds.size}"),
                ),
            )
            Spacer(Modifier.height(4.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Spacer(Modifier.weight(1f))
                // snap 是由 collectAsState() 委托的属性，直接判 != null 无法智能转换，
                // 先取局部变量（Compose 里这个值与本次重组绑定，不会中途变）。
                val next = snap.nextRunAt
                Text(
                    if (next != null) relTime(next, prefix = "下次 ")
                    else if (!snap.autoEnabled) "总开关关"
                    else "无启用条目",
                    style = AgateType.Caption,
                    color = if (!snap.autoEnabled) C.textDim else C.accent,
                )
            }
            TaskGap()

            // ── 每日签到 ──
            if (taskTier == "all" || taskTier == "daily") {
                DailyCard(
                    snap = snap,
                    expanded = expanded,
                    busyKey = busyKey,
                    signingAll = signingAll,
                    providerFilter = providerFilter,
                    onProviderFilter = { providerFilter = it },
                    onToggleGroup = { id -> expanded[id] = !(expanded[id] ?: false) },
                    onSignAll = { signAll() },
                    onSignOne = { signOne(it) },
                    onActivity = { acc, mode, label -> runActivity(acc, mode, label) },
                )
                TaskGap()
            }

            // ── 新手福利 ──
            if (taskTier == "all" || taskTier == "onboarding") {
                OnboardingCard(snap = snap, busyKey = busyKey, onBusy = { busyKey = it }, onToast = { toast(it) })
                TaskGap()
            }

            // ── 活动福利 ──
            if (taskTier == "all" || taskTier == "plans") {
                PlanCard(snap = snap, busyKey = busyKey, onBusy = { busyKey = it }, onToast = { toast(it) })
                TaskGap()
            }

            // ── 成长任务 ──
            if (taskTier == "all" || taskTier == "growth") {
                GrowthCard(snap = snap, onToast = { toast(it) })
            }

            TaskGap(24)
        }
    }
}

/**
 * 导航卡胶囊（第一行档位切换 + 第二行功能页跳转共用）。
 *
 * 选中态：`accent@10%` 底 + `accent` 描边 + `accent` 粗字；未选中：`insetBg` 底 +
 * `border` 描边 + `textSub` 字。颜色全部走语义色，页面里不出现任何色值字面量。
 * `badge` 是可选的数量尾巴（`textDim` 9.5sp、距文字 6dp），调用方只在拿到真实
 * 计数时才传，没有就不传。
 */
@Composable
private fun NavChip(
    text: String,
    selected: Boolean,
    badge: String? = null,
    onClick: () -> Unit,
) {
    val C = agateColors()
    val shape = RoundedCornerShape(50)
    Row(
        modifier = Modifier
            .clickable(onClick = onClick)
            .background(if (selected) C.accent.copy(alpha = 0.10f) else C.insetBg, shape)
            .border(1.dp, if (selected) C.accent else C.border, shape)
            .padding(horizontal = 10.dp, vertical = 5.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text,
            color = if (selected) C.accent else C.textSub,
            fontSize = 11.5.sp,
            fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
        )
        if (badge != null) {
            Text(
                badge,
                color = C.textDim,
                fontSize = 9.5.sp,
                modifier = Modifier.padding(start = 6.dp),
            )
        }
    }
}

/** 档位徽标：只有拿到正数才显示；0 视为「这档没有账号」，不出数字。 */
private fun countBadge(n: Int): String? = if (n > 0) n.toString() else null

/* ── 每日签到 ── */

/** 每日签到卡：`FlowRow` 在卡内换行，需在函数级再 @OptIn 一次（页面的 opt-in 不传递）。 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun DailyCard(
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
            Spacer(Modifier.height(6.dp))
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
private fun AccountSubRow(
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
private fun groupDescription(id: String): String = when (id) {
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

/* ── 新手福利 ── */

@Composable
private fun OnboardingCard(
    snap: CheckinCenter.Snap,
    busyKey: String?,
    onBusy: (String?) -> Unit,
    onToast: (String) -> Unit,
) {
    val C = agateColors()
    val scope = rememberCoroutineScope()
    // 每个账号的查询结果（只在点「查询」后才填，展开不自动查）
    val results = remember { mutableStateMapOf<String, JSONObject>() }
    val names = remember(snap.onboarding) { snap.onboarding.associate { it.id to it.name } }

    var queryingAll by remember { mutableStateOf(false) }

    fun query(acc: CheckinCenter.OnboardingAccount, refresh: Boolean) {
        val key = "${acc.id}:onboarding"
        if (busyKey != null) return
        onBusy(key)
        scope.launch {
            val data = CheckinOps.onboardingStatus(acc.id, refresh)
            onBusy(null)
            if (data != null) results[acc.id] = data
            else onToast("${acc.name} 查询失败")
        }
    }

    fun claim(acc: CheckinCenter.OnboardingAccount) {
        val key = "${acc.id}:claim"
        if (busyKey != null) return
        onBusy(key)
        scope.launch {
            val r = CheckinOps.onboardingClaim(acc.id)
            onBusy(null)
            if (r.ok) {
                val claimed = r.data?.optInt("claimed", 0) ?: 0
                onToast("${acc.name} 已领取 $claimed 项")
                // 领取后响应里带最新状态，直接用它回填
                r.data?.let { results[acc.id] = it }
                CheckinCenter.refreshOnce()
            } else {
                onToast("${acc.name} 领取失败：${r.error ?: "未知"}")
            }
        }
    }

    TaskCard {
        TaskCardHeader(
            title = "新手福利",
            subtitle = "一次性福利 · 领取后记在本机，不再查上游",
        ) {
            if (queryingAll) {
                AgateBadge("查询中", BadgeTone.AMBER)
            } else if (snap.onboarding.isNotEmpty()) {
                TaskBtn("全部查询", outlined = true, enabled = busyKey == null) {
                    queryingAll = true
                    scope.launch {
                        snap.onboarding.forEach { acc ->
                            CheckinOps.onboardingStatus(acc.id, refresh = true)
                                ?.let { results[acc.id] = it }
                        }
                        queryingAll = false
                        CheckinCenter.refreshOnce()
                    }
                }
            }
        }

        if (snap.onboarding.isEmpty()) {
            Spacer(Modifier.height(6.dp))
            Text("没有支持一次性福利的账号", style = AgateType.BodyS, color = C.textDim)
        }

        snap.onboarding.forEach { acc ->
            val data = results[acc.id]
            val tasks = data?.optJSONArray("tasks")
            val unclaimed = data?.optInt("unclaimed", 0) ?: 0
            val earned = data?.optInt("earned", 0) ?: 0
            val total = data?.optInt("total", 0) ?: 0
            val settledFromData = data?.optBoolean("settled") ?: acc.settled
            val settledAtMs = data?.optLong("settledAt", 0L)?.takeIf { it > 0 } ?: acc.settledAt

            Column {
                TaskRow(
                    title = acc.name,
                    avatarColorKey = acc.provider,
                    letters = PresetRegistry.labelOf(acc.provider).take(1),
                    // 兜底头像的 label 尽量取产品名。`labelOf` 是**精确匹配**，
                    // 遇到 `workbuddy-intl` 这类变体会回落成 id 本身（小写英文），
                    // 那比账号名的观感还差 —— 所以猜不到就退回账号名，不硬显 id。
                    providerId = acc.provider,
                    label = PresetRegistry.byId(acc.provider)?.label ?: acc.name,
                    subtitle = buildString {
                        append(PresetRegistry.labelOf(acc.provider))
                        when {
                            settledFromData && settledAtMs != null ->
                                append(" · 已结清（${beijingStamp(settledAtMs)}）")
                            data != null -> append(" · 已领 $earned/$total · 待领 $unclaimed")
                            acc.settled -> append(" · 记录显示已结清")
                            else -> append(" · 尚未查询")
                        }
                    },
                ) {
                    val acting = busyKey == "${acc.id}:onboarding" || busyKey == "${acc.id}:claim"
                    TaskBtn(
                        if (busyKey == "${acc.id}:onboarding") "…" else "查询",
                        outlined = true,
                        enabled = busyKey == null,
                    ) { query(acc, refresh = true) }
                    if (unclaimed > 0) {
                        TaskBtn(
                            if (busyKey == "${acc.id}:claim") "…" else "一键领取",
                            primary = true,
                            enabled = busyKey == null,
                        ) { claim(acc) }
                    } else if (data != null && settledFromData) {
                        TaskTag("已领完", C.green)
                    }
                    if (acting) AgateBadge("…", BadgeTone.AMBER)
                }

                if (tasks != null && tasks.length() > 0) {
                    Column(
                        verticalArrangement = Arrangement.spacedBy(5.dp),
                        modifier = Modifier.padding(start = 8.dp, bottom = 4.dp, end = 2.dp),
                    ) {
                        for (i in 0 until tasks.length()) {
                            val t = tasks.optJSONObject(i) ?: continue
                            val done = t.optBoolean("done")
                            val blocked = t.optBoolean("blocked")
                            val points = t.optInt("points", 0)
                            TaskSubRow {
                                Text(
                                    if (done) "✓" else "○",
                                    color = if (done) C.green else C.textDim,
                                    fontSize = 12.sp,
                                )
                                Text(
                                    t.optString("title").ifBlank { t.optString("key") },
                                    color = if (done) C.textDim else C.textMain,
                                    fontSize = 12.sp,
                                    modifier = Modifier.weight(1f),
                                )
                                if (points > 0) {
                                    Text("+$points", style = AgateType.Caption, color = C.textSub)
                                }
                                Text(
                                    when {
                                        done -> "已领取"
                                        blocked -> "需先完成前置"
                                        else -> "可领取"
                                    },
                                    style = AgateType.Caption,
                                    color = when {
                                        done -> C.green
                                        blocked -> C.textDim
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
            "「已结清」是后端落盘的结算记忆：领完之后连查询都不再发上游 —— " +
                "手动点「查询」是唯一「重新问一次」的入口。"
        )
    }
}

/* ── 活动福利 ── */

@Composable
private fun PlanCard(
    snap: CheckinCenter.Snap,
    busyKey: String?,
    onBusy: (String?) -> Unit,
    onToast: (String) -> Unit,
) {
    val C = agateColors()
    val scope = rememberCoroutineScope()
    val previews = remember { mutableStateMapOf<String, JSONArray>() }

    fun preview(acc: CheckinCenter.PlanAccount) {
        val key = "${acc.id}:preview"
        if (busyKey != null) return
        onBusy(key)
        scope.launch {
            val r = CheckinOps.planPreview(acc.id)
            onBusy(null)
            if (r.ok) {
                val plans = r.data?.optJSONArray("plans") ?: JSONArray()
                previews[acc.id] = plans
                if (plans.length() == 0) {
                    val deployed = r.data?.optBoolean("deployed") ?: true
                    onToast(
                        if (!deployed) "这套接口在本区没部署"
                        else "${acc.name} 当前没有可领套餐"
                    )
                }
            } else {
                onToast("${acc.name} 探测失败：${r.error ?: "未知"}")
            }
        }
    }

    fun claim(acc: CheckinCenter.PlanAccount) {
        val key = "${acc.id}:claim"
        if (busyKey != null) return
        onBusy(key)
        scope.launch {
            val r = CheckinOps.planClaim(acc.id)
            onBusy(null)
            // 注意：失败也是 HTTP 200，要靠 failure 字段判
            val ok = r.data?.optBoolean("ok") ?: false
            when {
                ok -> {
                    onToast("${acc.name} 已领取套餐")
                    CheckinCenter.refreshOnce()
                }
                else -> {
                    val label = r.data?.optString("failureLabel").orEmpty()
                    val message = r.data?.optString("message").orEmpty()
                    onToast(
                        "${acc.name} 未领到：" +
                            (label.ifBlank { message }.ifBlank { r.error ?: "未知原因" })
                    )
                }
            }
        }
    }

    /** 今天领过几份：本地台账 `claimPlans` 按北京时间日界逐份比对。 */
    fun claimedToday(acc: CheckinCenter.PlanAccount): Int {
        val today = beijingDateKey(System.currentTimeMillis())
        return acc.claimPlans.values.count { beijingDateKey(it) == today }
    }

    TaskCard {
        TaskCardHeader(
            title = "活动福利",
            subtitle = "限时体验套餐 · 逐份查看与领取",
        )
        if (snap.plans.isEmpty()) {
            Spacer(Modifier.height(6.dp))
            Text("没有 ZCode 账号", style = AgateType.BodyS, color = C.textDim)
        }
        snap.plans.forEach { acc ->
            val claimed = claimedToday(acc)
            TaskRow(
                title = acc.name,
                avatarColorKey = "zcode",
                letters = "ZC",
                providerId = "zcode",
                label = PresetRegistry.labelOf("zcode"),
                subtitle = PresetRegistry.labelOf("zcode") + " · " +
                    if (claimed > 0) "今天已领 $claimed 份" else "今天未领",
            ) {
                TaskBtn(
                    if (busyKey == "${acc.id}:preview") "…" else "套餐明细",
                    outlined = true,
                    enabled = busyKey == null,
                ) { preview(acc) }
                TaskBtn(
                    if (busyKey == "${acc.id}:claim") "…" else "去领取",
                    primary = true,
                    enabled = busyKey == null,
                ) { claim(acc) }
            }
            previews[acc.id]?.let { plans ->
                if (plans.length() > 0) {
                    Column(
                        verticalArrangement = Arrangement.spacedBy(5.dp),
                        modifier = Modifier.padding(start = 8.dp, bottom = 4.dp, end = 2.dp),
                    ) {
                        for (i in 0 until plans.length()) {
                            val plan = plans.optJSONObject(i) ?: continue
                            val ents = plan.optJSONArray("entitlements")
                            val first = ents?.optJSONObject(0)
                            TaskSubRow {
                                Column(Modifier.weight(1f)) {
                                    Text(
                                        plan.optString("name").ifBlank { plan.optString("planId") },
                                        color = C.textMain,
                                        fontSize = 12.sp,
                                    )
                                    if (first != null) {
                                        Text(
                                            first.optString("showName") + " " +
                                                "${first.optInt("grantUnits")} ${first.optString("unitType")}",
                                            style = AgateType.Caption,
                                            color = C.textDim,
                                        )
                                    }
                                }
                                TaskTag("优先级 ${plan.optInt("priority")}", C.blue)
                            }
                        }
                    }
                }
            }
        }
        Spacer(Modifier.height(6.dp))
        TaskFold(
            "按钮始终可点，不按「今天领过没」堵死入口 —— 上游是按**套餐**判已领取的" +
                "（领了 A 之后 B 照样能领）。行上那句话只说今天领了几份，由本地台账按北京时间判。" +
                "领取要过阿里云滑块时本版**不做过码**，会如实显示「验证码未通过」。"
        )
    }
}

/* ── 成长任务 ── */

@Composable
private fun GrowthCard(snap: CheckinCenter.Snap, onToast: (String) -> Unit) {
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

/* ── 轮次读数的说明（首页/总览用） ── */

/**
 * 一轮签到的摘要文案（总览卡用）。
 * 轮次台账已降为读数 —— 上游把它换成了每账号明细，明细在「签到记录」页。
 */
internal fun roundSummary(row: CheckinCenter.RoundRow): String = buildString {
    append(row.reason.ifBlank { "签到" })
    append(" · 成功 ${row.succeeded}")
    if (row.active > 0) append(" · 保活 ${row.active}")
    if (row.skipped > 0) append(" · 跳过 ${row.skipped}")
    if (row.failedCount > 0) append(" · 失败 ${row.failedCount}")
}
