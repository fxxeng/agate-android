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

/**
 * 签到中心 —— 对齐上游 3.0.8 `CenterTab`（`checkin-page.tsx`）。
 *
 * ── 结构 ───────────────────────────────────────────────────
 * 总览四格 → 导航卡（两行 chips：任务档切换 + 功能页跳转）
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
    /** 从任务中心带过来的目标档（all/daily/onboarding/plans/growth）；空回落全部。 */
    initialTier: String? = null,
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
    var taskTier by remember { mutableStateOf(CheckinTierFocus.normalize(initialTier)) }

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

            // ── 导航卡：7 个入口收口成卡内两行 chips（总览四格之后、分组卡之前）──
            // 第一行只切「下面渲染哪张分组卡」，不碰任何卡内的取数、展开态与
            // 按钮转圈（busyKey 仍是页面级的）；第二行直接调 NavHost 传进来的
            // 三个回调 —— 本页自己不持有路由。
            NavCard {
                NavRowLabel("任务档")
                Spacer(Modifier.height(4.dp))
                FlowRow(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    CheckinTierFocus.TIERS.forEach { (value, label) ->
                        NavChip(label, taskTier == value) { taskTier = value }
                    }
                }
                Spacer(Modifier.height(8.dp))
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
                }
            }
            TaskGap()

            // ── 每日签到 ──
            if (taskTier == CheckinTierFocus.ALL || taskTier == CheckinTierFocus.DAILY) {
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
            if (taskTier == CheckinTierFocus.ALL || taskTier == CheckinTierFocus.ONBOARDING) {
                OnboardingCard(snap = snap, busyKey = busyKey, onBusy = { busyKey = it }, onToast = { toast(it) })
                TaskGap()
            }

            // ── 活动福利 ──
            if (taskTier == CheckinTierFocus.ALL || taskTier == CheckinTierFocus.PLANS) {
                PlanCard(snap = snap, busyKey = busyKey, onBusy = { busyKey = it }, onToast = { toast(it) })
                TaskGap()
            }

            // ── 成长任务 ──
            if (taskTier == CheckinTierFocus.ALL || taskTier == CheckinTierFocus.GROWTH) {
                GrowthCard(snap = snap, onToast = { toast(it) })
            }

            TaskGap(24)
        }
    }
}
