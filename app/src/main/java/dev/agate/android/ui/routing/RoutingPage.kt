package dev.agate.android.ui.routing

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
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
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
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.RoutingOps
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.logs.FilterSheet
import dev.agate.android.ui.theme.AgateShape
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import dev.agate.core.GatewayPrefs
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * 路由预演页（原 M3 骨架，本次补实）—— 对齐 v44 设置页下的「选号方式 /
 * 候选队列 / 自愈演练 / 并发闸门」四段。
 *
 * ── 这一段为什么值得单独一页 ──────────────────────────────────
 * 「我的请求到底会打到哪个账号」是网关里最难解释、也最常被问的一件事：
 * 账号很多、冷却 / 熔断 / 并发闸门各有各的状态，一旦实际走的和用户以为的
 * 不一样，排查成本极高。把它摊开画出来，比任何说明都管用。
 *
 * 四段：
 *   ① 选号方式 —— 三策略单选，存后下一个请求立即生效
 *   ② 选号预演 —— 接下来 N 个并发请求会分给谁（按当前队列序推导）
 *   ③ 候选队列 —— 全部账号按序，被跳过者也在列并写明原因
 *   ④ 自愈演练 —— 模拟 429 / 5xx / 余额不足，看离队 / 改道 / 回队倒计时
 *
 * ── 关于 ④ 的诚实边界 ────────────────────────────────────────
 * 上游的「自愈演练」是**真的注入一次故障**再观察。内核没有这个注入端点，
 * 所以这里做成**只读的故障说明 + 当前熔断参数**：它能回答「发生 429 时规则是
 * 什么、多久回来」，但不会真的去打一次失败请求。宁可少一个按钮，
 * 也不做一个会把用户账号真的推进退避的假开关。
 */
@Composable
fun RoutingPage(
    onOpenDrawer: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    var settings by remember { mutableStateOf<RoutingOps.Settings?>(null) }
    var plan by remember { mutableStateOf<RoutingOps.Plan?>(null) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var saving by remember { mutableStateOf(false) }
    var showModel by remember { mutableStateOf(false) }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    fun reload() {
        if (loading) return
        loading = true
        scope.launch {
            val s = RoutingOps.settings()
            val q = RoutingOps.queue(plan?.model)
            loading = false
            if (s.ok) settings = s.data
            if (q.ok) plan = q.data
            error = if (!s.ok && !q.ok) (s.error ?: "读取失败") else null
        }
    }

    DisposableEffect(Unit) {
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
        onDispose { }
    }
    LaunchedEffect(Unit) { reload() }

    fun commit(strategy: String? = null, sticky: String? = null) {
        if (saving) return
        saving = true
        scope.launch {
            val r = RoutingOps.save(strategy = strategy, sticky = sticky)
            saving = false
            if (r.ok) {
                settings = r.data
                // 队列排序依赖策略，策略变了队列要跟着重算
                val q = RoutingOps.queue(r.data?.let { plan?.model })
                if (q.ok) plan = q.data
                toast("已保存，下一个请求生效")
            } else {
                toast(r.error ?: "保存失败")
            }
        }
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "路由预演", onOpenDrawer = onOpenDrawer) {
            Text(
                if (loading) "刷新中…" else "刷新",
                color = C.accent,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clickable(enabled = !loading) { reload() }
                    .padding(horizontal = 8.dp, vertical = 5.dp),
            )
        }

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))

            // ① 选号方式
            Column(
                Modifier
                    .fillMaxWidth()
                    .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
                    .border(1.dp, C.border, RoundedCornerShape(16.dp))
                    .padding(12.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("选号方式", fontSize = 13.5.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
                    Spacer(Modifier.width(8.dp))
                    Text("下一个请求立即生效", fontSize = 10.5.sp, color = C.textDim)
                }
                Spacer(Modifier.height(8.dp))
                val cur = settings
                AgateSegmented(
                    options = listOf("顺序优先", "负载均衡", "同家轮询"),
                    selected = when (cur?.strategy) {
                        "rotate" -> 1
                        "provider" -> 2
                        else -> 0
                    },
                    onSelect = { index ->
                        commit(strategy = when (index) {
                            1 -> "rotate"
                            2 -> "provider"
                            else -> "order"
                        })
                    },
                )
                Spacer(Modifier.height(8.dp))
                Text(
                    cur?.strategyDesc ?: "严格按队列名次，队首不可用才下探下一位",
                    fontSize = 11.5.sp,
                    color = C.textSub,
                    lineHeight = 16.sp,
                )
            }

            Spacer(Modifier.height(10.dp))

            // ② 选号预演
            val ordered = plan?.ordered ?: emptyList()
            Column(
                Modifier
                    .fillMaxWidth()
                    .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
                    .border(1.dp, C.border, RoundedCornerShape(16.dp))
                    .padding(12.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("选号预演", fontSize = 13.5.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
                    Spacer(Modifier.width(8.dp))
                    Text("接下来 6 个并发请求会分给", fontSize = 10.5.sp, color = C.textDim)
                }
                Spacer(Modifier.height(8.dp))
                if (ordered.isEmpty()) {
                    Text("当前没有可用账号", fontSize = 12.sp, color = C.textDim)
                } else {
                    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        for (i in 0 until 6) {
                            val row = ordered[i % ordered.size]
                            BadgeWithIndex(index = i + 1, name = row.name)
                        }
                    }
                    Spacer(Modifier.height(6.dp))
                    Text(
                        "与转发层同一套排序函数，不是另算一遍",
                        fontSize = 11.sp,
                        color = C.textDim,
                    )
                }
            }

            Spacer(Modifier.height(10.dp))

            // ③ 候选队列
            Column(
                Modifier
                    .fillMaxWidth()
                    .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
                    .border(1.dp, C.border, RoundedCornerShape(16.dp))
                    .padding(12.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("候选队列", fontSize = 13.5.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
                    Spacer(Modifier.weight(1f))
                    Text(
                        if (plan?.model.isNullOrBlank()) "不限模型" else "模型 ${plan?.model}",
                        fontSize = 10.5.sp,
                        color = C.accent,
                        modifier = Modifier.clickable { showModel = true },
                    )
                }
                Spacer(Modifier.height(6.dp))
                if (ordered.isEmpty() && (plan?.blocked ?: emptyList()).isEmpty()) {
                    Text("还没有账号，先去「提供商」里添加一个", fontSize = 12.sp, color = C.textDim)
                }
                ordered.forEachIndexed { index, row ->
                    CandidateRow(
                        rank = index + 1,
                        name = row.name,
                        providerLabel = row.providerLabel,
                        reason = row.reason,
                        dim = false,
                        isHead = row.id == plan?.pickedId,
                    )
                }
                (plan?.blocked ?: emptyList()).forEach { row ->
                    CandidateRow(
                        rank = null,
                        name = row.name,
                        providerLabel = row.providerLabel,
                        reason = row.reason,
                        dim = true,
                        isHead = false,
                    )
                }
                Spacer(Modifier.height(6.dp))
                Text(
                    "被跳过者也在列：禁用 / 冷却 / 并发满 / 限制器 / 无凭证 —— 各有各的原因，不用猜",
                    fontSize = 11.sp,
                    color = C.textDim,
                    lineHeight = 15.sp,
                )
            }

            Spacer(Modifier.height(10.dp))

            // ④ 自愈演练（只读说明 + 当前参数）
            SelfHealCard(settings = settings)

            Spacer(Modifier.height(20.dp))
        }
    }

    if (showModel) {
        FilterSheet(
            title = "预演用的模型（可省）",
            options = listOf(null to "不限模型"),
            selected = plan?.model,
            onPick = { value ->
                scope.launch {
                    val q = RoutingOps.queue(value)
                    if (q.ok) plan = q.data
                }
            },
            onDismiss = { showModel = false },
        )
    }
}

// ══════════════════════════════════════════════════════════════
//  子件
// ══════════════════════════════════════════════════════════════

/** 预演序列里的一格：`① 名称`。 */
@Composable
private fun BadgeWithIndex(index: Int, name: String) {
    val C = agateColors()
    Box(
        Modifier
            .border(1.dp, C.border, RoundedCornerShape(9.dp))
            .background(C.insetBg, RoundedCornerShape(9.dp))
            .padding(horizontal = 8.dp, vertical = 5.dp),
    ) {
        Text(
            text = circleOf(index) + " " + name,
            fontSize = 11.sp,
            color = C.textSub,
            maxLines = 1,
        )
    }
}

/** 1 → ①（超出 10 退回阿拉伯数字，避免糊成一团）。 */
private fun circleOf(value: Int): String = when (value) {
    1 -> "①"; 2 -> "②"; 3 -> "③"; 4 -> "④"; 5 -> "⑤"
    6 -> "⑥"; 7 -> "⑦"; 8 -> "⑧"; 9 -> "⑨"; 10 -> "⑩"
    else -> "$value."
}

/** 候选队列里的一行。`rank` 为 null = 被跳过（不参与排序）。 */
@Composable
internal fun CandidateRow(
    rank: Int?,
    name: String,
    providerLabel: String,
    reason: String,
    dim: Boolean,
    isHead: Boolean,
) {
    val C = agateColors()
    Row(
        Modifier
            .fillMaxWidth()
            .padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            modifier = Modifier
                .size(22.dp)
                .background(
                    if (rank == null) C.textDim.copy(alpha = 0.16f) else C.accent.copy(alpha = 0.18f),
                    RoundedCornerShape(11.dp),
                ),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                text = rank?.toString() ?: "–",
                fontSize = 11.sp,
                fontWeight = FontWeight.Bold,
                color = if (rank == null) C.textDim else C.accent,
            )
        }
        Spacer(Modifier.width(9.dp))
        Column(Modifier.weight(1f)) {
            Text(
                text = name,
                fontSize = 13.sp,
                color = if (dim) C.textDim else C.textMain,
                maxLines = 1,
                overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
            )
            Text(
                text = if (reason.isBlank()) providerLabel else reason,
                fontSize = 10.5.sp,
                color = C.textDim,
                maxLines = 1,
                overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
            )
        }
        if (isHead) AgateBadge("队首", tone = BadgeTone.GREEN)
    }
}

/** 自愈演练：三种故障的规则说明 + 当前熔断参数。 */
@Composable
private fun SelfHealCard(settings: RoutingOps.Settings?) {
    val C = agateColors()
    var open by remember { mutableStateOf(false) }
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
        .border(1.dp, C.border, RoundedCornerShape(16.dp)),
    ) {
        Row(
            Modifier
                .fillMaxWidth()
                .clickable { open = !open }
                .padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("自愈演练", fontSize = 13.5.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
            Spacer(Modifier.weight(1f))
            Text(if (open) "⌃" else "⌄", fontSize = 13.sp, color = C.textDim)
        }
        if (open) {
            Box(Modifier.fillMaxWidth().height(1.dp).background(C.border))
            Column(Modifier.fillMaxWidth().padding(12.dp)) {
                Text(
                    "模拟上游故障时账号怎样离开队列、预演如何改道、倒计时到点自动回队。",
                    fontSize = 11.5.sp,
                    color = C.textSub,
                    lineHeight = 16.sp,
                )
                Spacer(Modifier.height(8.dp))
                FailRuleRow("429 限流", "对该模型记冷却，到上游给的恢复时间自动回来；没给时间按家默认 TTL 垫底")
                FailRuleRow("5xx 退避", "连续失败累计，到阈值熔断，冷却结束后放行试一次")
                FailRuleRow("余额不足", "被限制器拦下，等余额 / Token 周期窗口滚过去再回来")
                Spacer(Modifier.height(10.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(14.dp)) {
                    Text(
                        "熔断阈值 ${settings?.breakerThreshold ?: "—"} 次",
                        fontSize = 11.5.sp,
                        color = C.textSub,
                    )
                    Text(
                        "冷却 ${settings?.breakerTimeoutSeconds ?: "—"} 秒",
                        fontSize = 11.5.sp,
                        color = C.textSub,
                    )
                    Text(
                        "恢复需连续成功 ${settings?.breakerSuccessThreshold ?: "—"} 次",
                        fontSize = 11.5.sp,
                        color = C.textSub,
                    )
                }
            }
        }
    }
}

@Composable
private fun FailRuleRow(name: String, desc: String) {
    val C = agateColors()
    Column(Modifier.fillMaxWidth().padding(vertical = 6.dp)) {
        Text(name, fontSize = 12.5.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
        Text(desc, fontSize = 11.sp, color = C.textDim, lineHeight = 15.sp)
    }
}
