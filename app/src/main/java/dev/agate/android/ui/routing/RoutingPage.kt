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
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
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
import dev.agate.android.data.AccountOps
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.RoutingOps
import dev.agate.android.data.dto.AccountDto
import dev.agate.android.data.rateCountdown
import dev.agate.android.data.rateRecoverAt
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.ReorderState
import dev.agate.android.ui.components.ReorderableColumn
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.components.dragHandle
import dev.agate.android.ui.logs.FilterSheet
import dev.agate.android.ui.theme.agateColors
import dev.agate.core.GatewayPrefs
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * 路由预演页：选号方式 / 选号预演 / 候选队列（可排序）/ 自愈演练。
 *
 * ── 排序为什么放在这一页（2026-11-07）──────────────────────────
 * 控制台「实时出战队列」一直写着「点此去「路由」调整」，但这一页原本是
 * 纯只读的 —— 用户被指过来却没有任何按钮。内核早有
 * `POST /api/accounts/reorder`（整队重排），提供商页也已有拖拽实现，
 * 缺的只是这个入口。现在页头「⇅ 排序」进入编辑态，复用同一套
 * [ReorderableColumn] 手势与动画，松手一次落盘。
 *
 * ── 排序态为什么要切换数据源 ─────────────────────────────────
 * 普通态的 `ordered + blocked` 是**按模型收窄、按当前策略排过**的瞬时快照，
 * 既不是全部账号，也不等于 priority 顺序（负载均衡 / 同家轮询还会叠轮转
 * 游标）。拿它去拖会有两个错：提交的队列不完整被内核 400 拒；用户以为
 * 「拖到第 1 位就排第 1」，实际瞬时名次未必跟着变。
 * 所以排序态换成 `/api/session` 的**全量账号按 priority 升序**——写的和
 * 看到的是同一条队列，并在横幅里把「实际名次还受轮转影响」说清楚。
 *
 * ── 预演的诚实边界 ──────────────────────────────────────────
 * 内核 `/api/routing/queue` 的 PlanFacts 明确传 `cursor: None`（看一眼不该
 * 消耗一次轮转），客户端也只是按 `ordered[i % size]` 列前几位。所以这里
 * 写「前 6 位依次是」，不写「6 个并发请求会分给谁」——那句没真算过。
 */
@Composable
fun RoutingPage(
    onOpenDrawer: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val snap by GatewayRepository.snapshot.collectAsState()
    val reorderState = remember { ReorderState() }
    val pageScroll = rememberScrollState()

    var settings by remember { mutableStateOf<RoutingOps.Settings?>(null) }
    var plan by remember { mutableStateOf<RoutingOps.Plan?>(null) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var saving by remember { mutableStateOf(false) }
    var showModel by remember { mutableStateOf(false) }
    var savingOrder by remember { mutableStateOf(false) }
    var orderDirty by remember { mutableStateOf(false) }
    var expandedId by remember { mutableStateOf<String?>(null) }
    var previewOpen by remember { mutableStateOf<Int?>(null) }
    var commonModels by remember { mutableStateOf<List<String>>(emptyList()) }
    var nowMs by remember { mutableStateOf(System.currentTimeMillis()) }
    // 排序态的顺序：全部账号按 priority 升序（唯一事实源）
    val orderState = remember { mutableStateListOf<AccountDto>() }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    fun reload() {
        if (loading) return
        loading = true
        scope.launch {
            val s = RoutingOps.settings()
            val q = RoutingOps.queue(plan?.model)
            val m = RoutingOps.commonModels()
            loading = false
            if (s.ok) settings = s.data
            if (q.ok) plan = q.data
            commonModels = m
            error = if (!s.ok && !q.ok) (s.error ?: "读取失败") else null
        }
    }

    DisposableEffect(Unit) {
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
        AccountOps.configure(GatewayPrefs.apiKey(ctx))
        // 排序要用全量账号；轮询的第一个 5 秒刻度可能还没到，进页补拉一次。
        GatewayRepository.refreshOnce()
        onDispose { }
    }
    LaunchedEffect(Unit) { reload() }
    // 冷却倒计时精确到分钟，30 秒重算一次足够，不必让整页每秒重组。
    LaunchedEffect(Unit) {
        while (true) {
            delay(30_000L)
            nowMs = System.currentTimeMillis()
        }
    }
    // 非排序态才跟着快照同步；排序态保留用户刚拖出来的顺序，
    // 否则保存后一次刷新会把本地顺序冲掉，看起来像「拖了没生效」。
    LaunchedEffect(snap.accountList, reorderState.isSortingMode, reorderState.draggingKey) {
        if (!reorderState.isSortingMode && reorderState.draggingKey == null) {
            orderState.clear()
            orderState.addAll(snap.accountList.sortedBy { it.priority })
        }
    }

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

    /** 拖拽落点 → 本地重排 → 一次提交全量队列（内核按 0..n-1 重编 priority）。 */
    fun commitOrder(key: String, to: Int) {
        val from = orderState.indexOfFirst { it.id == key }
        if (from < 0 || to !in orderState.indices || from == to) return
        orderState.add(to, orderState.removeAt(from))
        // 连续拖拽时第二次提交不能与第一次并发：内核按请求到达顺序落盘，
        // 先发后到会把较新的顺序覆盖回旧的。这里串行、并只补一次最新队列。
        if (savingOrder) {
            orderDirty = true
            return
        }
        savingOrder = true
        scope.launch {
            var failed: String? = null
            do {
                orderDirty = false
                val submitted = orderState.map { it.id }
                val r = AccountOps.reorder(submitted)
                if (r.ok) {
                    GatewayRepository.refreshOnce()
                    val q = RoutingOps.queue(plan?.model)
                    if (q.ok) plan = q.data
                } else {
                    failed = r.error ?: "排序保存失败"
                    // 保存失败不能把「拖过了」的顺序留在屏幕上假装已生效
                    val byId = orderState.associateBy { it.id }
                    orderState.clear()
                    orderState.addAll(submitted.mapNotNull { byId[it] })
                    orderDirty = false
                }
            } while (orderDirty && failed == null)
            savingOrder = false
            toast(failed ?: "顺序已保存，下一个请求生效")
        }
    }

    fun toggleSorting() {
        if (reorderState.isSortingMode) {
            reorderState.exitSortingMode()
            return
        }
        val all = snap.accountList.sortedBy { it.priority }
        if (all.isEmpty()) {
            toast("账号清单还没到，稍后再试")
            return
        }
        orderState.clear()
        orderState.addAll(all)
        reorderState.enterSortingMode()
        expandedId = null
        previewOpen = null
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
            Spacer(Modifier.width(4.dp))
            Text(
                if (reorderState.isSortingMode) "完成" else "⇅ 排序",
                color = if (reorderState.isSortingMode) C.amber else C.accent,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(
                        if (reorderState.isSortingMode) C.amber.copy(alpha = 0.10f) else C.accent.copy(alpha = 0.08f),
                        RoundedCornerShape(8.dp),
                    )
                    .clickable { toggleSorting() }
                    .padding(horizontal = 10.dp, vertical = 5.dp),
            )
        }

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(pageScroll)
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))
            error?.let {
                Text(it, fontSize = 11.5.sp, color = C.red, modifier = Modifier.padding(bottom = 8.dp))
            }

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

            // ② 选号预演：2×3 等宽网格，不再让一个长名字把后面 5 格挤出屏幕
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
                    Text("按当前队列顺序，前 6 位依次是", fontSize = 10.5.sp, color = C.textDim)
                }
                Spacer(Modifier.height(8.dp))
                if (ordered.isEmpty()) {
                    Text("当前没有可用账号", fontSize = 12.sp, color = C.textDim)
                } else {
                    for (start in 0 until 6 step 3) {
                        Row(
                            Modifier.fillMaxWidth(),
                            horizontalArrangement = Arrangement.spacedBy(6.dp),
                        ) {
                            for (i in start until start + 3) {
                                val row = ordered[i % ordered.size]
                                PreviewChip(
                                    index = i + 1,
                                    name = row.name,
                                    selected = previewOpen == i,
                                    modifier = Modifier.weight(1f),
                                    onClick = { previewOpen = if (previewOpen == i) null else i },
                                )
                            }
                        }
                        Spacer(Modifier.height(6.dp))
                    }
                    previewOpen?.let { i ->
                        Text(
                            "${circleOf(i + 1)} 完整名称：${ordered[i % ordered.size].name} · ${ordered[i % ordered.size].providerLabel}",
                            fontSize = 11.sp,
                            color = C.textSub,
                            lineHeight = 15.sp,
                        )
                    }
                    Text(
                        "与转发层同一套排序函数；这里只列出当前队列的前几位，不推进轮转",
                        fontSize = 11.sp,
                        color = C.textDim,
                        lineHeight = 15.sp,
                    )
                }
            }

            Spacer(Modifier.height(10.dp))

            // ③ 候选队列 / 全局出战优先级（排序态）
            val sorting = reorderState.isSortingMode
            Column(
                Modifier
                    .fillMaxWidth()
                    .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
                    .border(1.dp, C.border, RoundedCornerShape(16.dp))
                    .padding(12.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        if (sorting) "全局出战优先级" else "候选队列",
                        fontSize = 13.5.sp,
                        fontWeight = FontWeight.SemiBold,
                        color = C.textMain,
                    )
                    Spacer(Modifier.weight(1f))
                    Text(
                        when {
                            sorting -> "全部账号"
                            plan?.model.isNullOrBlank() -> "不限模型 ▾"
                            else -> "模型 ${plan?.model} ▾"
                        },
                        fontSize = 10.5.sp,
                        color = if (sorting) C.textDim else C.accent,
                        modifier = Modifier.clickable(enabled = !sorting) { showModel = true },
                    )
                }
                Spacer(Modifier.height(6.dp))
                if (sorting) {
                    Text(
                        if (savingOrder) "保存中…" else "排序中 · 拖动行改全局优先级 · 松手即保存 · 5 秒无操作自动退出",
                        fontSize = 11.sp,
                        color = C.amber,
                        lineHeight = 15.sp,
                        modifier = Modifier
                            .fillMaxWidth()
                            .background(C.amber.copy(alpha = 0.08f), RoundedCornerShape(9.dp))
                            .padding(horizontal = 9.dp, vertical = 7.dp),
                    )
                    Spacer(Modifier.height(6.dp))
                    Text(
                        "全部 ${orderState.size} 个账号 · 按 priority 基线排列（不按模型收窄）",
                        fontSize = 11.sp,
                        color = C.textSub,
                    )
                    Spacer(Modifier.height(4.dp))
                    ReorderableColumn(
                        items = orderState,
                        keyOf = { it.id },
                        state = reorderState,
                        onCommit = { key, to -> commitOrder(key, to) },
                        verticalArrangement = Arrangement.spacedBy(2.dp),
                    ) { acc, index, isTarget ->
                        SortableRow(
                            acc = acc,
                            rank = index + 1,
                            isTarget = isTarget,
                            state = reorderState,
                        )
                    }
                    Spacer(Modifier.height(6.dp))
                    Text(
                        "改的是全局 priority：负载均衡 / 同家轮询下，实际出战名次还受轮转影响。",
                        fontSize = 11.sp,
                        color = C.textDim,
                        lineHeight = 15.sp,
                    )
                } else {
                    if (ordered.isEmpty() && (plan?.blocked ?: emptyList()).isEmpty()) {
                        Text("还没有账号，先去「提供商」里添加一个", fontSize = 12.sp, color = C.textDim)
                    }
                    ordered.forEachIndexed { index, row ->
                        CandidateRow(
                            row = row,
                            rank = index + 1,
                            isHead = row.id == plan?.pickedId,
                            dim = false,
                            expanded = expandedId == row.id,
                            onToggle = { expandedId = if (expandedId == row.id) null else row.id },
                            nowMs = nowMs,
                        )
                    }
                    (plan?.blocked ?: emptyList()).forEach { row ->
                        CandidateRow(
                            row = row,
                            rank = null,
                            isHead = false,
                            dim = true,
                            expanded = expandedId == row.id,
                            onToggle = { expandedId = if (expandedId == row.id) null else row.id },
                            nowMs = nowMs,
                        )
                    }
                    Spacer(Modifier.height(6.dp))
                    Text(
                        "点任一行看完整名称与原因；被跳过者也在列：禁用 / 冷却 / 并发满 / 限制器 / 无凭证",
                        fontSize = 11.sp,
                        color = C.textDim,
                        lineHeight = 15.sp,
                    )
                }
            }

            Spacer(Modifier.height(10.dp))

            // ④ 自愈演练（只读说明 + 当前参数）
            SelfHealCard(settings = settings)

            Spacer(Modifier.height(20.dp))
        }
    }

    if (showModel) {
        // 只有「不限模型」一个选项的弹层等于没有选择。这里给最近一次请求的
        // 模型 + 客户端对外清单的前若干个（/api/models/list-view 的 ids）。
        val modelOptions = mutableListOf<Pair<String?, String>>(null to "不限模型")
        val seen = HashSet<String>()
        plan?.lastRequestModel?.let { if (seen.add(it)) modelOptions += it to "最近使用 · $it" }
        commonModels.forEach { if (seen.add(it)) modelOptions += it to it }
        FilterSheet(
            title = "预演用的模型（可省）",
            options = modelOptions,
            selected = plan?.model,
            onPick = { value ->
                scope.launch {
                    val q = RoutingOps.queue(value)
                    if (q.ok) plan = q.data
                    expandedId = null
                    previewOpen = null
                }
            },
            onDismiss = { showModel = false },
        )
    }
}

// ══════════════════════════════════════════════════════════════
//  子件
// ══════════════════════════════════════════════════════════════

/** 预演序列里的一格：等宽、两行封顶，点一下在下方看完整名称。 */
@Composable
private fun PreviewChip(
    index: Int,
    name: String,
    selected: Boolean,
    modifier: Modifier = Modifier,
    onClick: () -> Unit,
) {
    val C = agateColors()
    Box(
        modifier
            .border(1.dp, if (selected) C.accent else C.border, RoundedCornerShape(9.dp))
            .background(C.insetBg, RoundedCornerShape(9.dp))
            .clickable { onClick() }
            .padding(horizontal = 8.dp, vertical = 5.dp),
    ) {
        Text(
            text = circleOf(index) + " " + name,
            fontSize = 11.sp,
            color = C.textSub,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
            lineHeight = 14.sp,
        )
    }
}

/** 1 → ①（超出 10 退回阿拉伯数字，避免糊成一团）。 */
private fun circleOf(value: Int): String = when (value) {
    1 -> "①"; 2 -> "②"; 3 -> "③"; 4 -> "④"; 5 -> "⑤"
    6 -> "⑥"; 7 -> "⑦"; 8 -> "⑧"; 9 -> "⑨"; 10 -> "⑩"
    else -> "$value."
}

/**
 * 排序态的一行：全量账号按 priority 排列，长按整行起拖。
 * 与提供商页的账号卡用同一套 [ReorderState] / [dragHandle] / `startColumn(…, 0f)`
 * 配置（那套在真机上验过；本页同样不接页面级自动滚动）。
 */
@Composable
private fun SortableRow(
    acc: AccountDto,
    rank: Int,
    isTarget: Boolean,
    state: ReorderState,
) {
    val C = agateColors()
    Row(
        Modifier
            .fillMaxWidth()
            .background(C.insetBg, RoundedCornerShape(12.dp))
            .then(
                if (isTarget) Modifier.border(1.5.dp, C.accent, RoundedCornerShape(12.dp)) else Modifier,
            )
            .dragHandle(
                key = acc.id,
                state = state,
                onStart = { state.startColumn(acc.id, 0f) },
            )
            .padding(horizontal = 12.dp, vertical = 9.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            Modifier
                .size(22.dp)
                .background(C.accent, RoundedCornerShape(11.dp)),
            contentAlignment = Alignment.Center,
        ) {
            Text(rank.toString(), fontSize = 11.sp, fontWeight = FontWeight.Bold, color = C.onAccent)
        }
        Spacer(Modifier.width(9.dp))
        Column(Modifier.weight(1f)) {
            Text(
                acc.displayName.ifBlank { acc.id },
                fontSize = 12.5.sp,
                color = C.textMain,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            // 不写旧 priority：提交后内核按新队列重编 0..n-1，屏幕上那个
            // 「第 1 位 · priority 4」会变成一句自相矛盾的读数。
            Text(
                acc.provider + if (acc.enabled) "" else " · 已禁用",
                fontSize = 10.5.sp,
                color = C.textDim,
                maxLines = 1,
            )
        }
        Text("⠿", fontSize = 15.sp, color = C.textDim)
    }
}

/**
 * 候选队列里的一行：紧凑态显示名字 / 家名·状态 / 原因各一行，
 * 点开后完整名称、完整原因、优先级与恢复时刻都不再截断。
 * `rank` 为 null = 被跳过（不参与排序）。
 */
@Composable
internal fun CandidateRow(
    row: RoutingOps.QueueRow,
    rank: Int?,
    isHead: Boolean,
    dim: Boolean,
    expanded: Boolean,
    onToggle: () -> Unit,
    nowMs: Long,
) {
    val C = agateColors()
    val stateText = GatewayRepository.stateLabel(row.state, dim)
    val countdown = if (row.resetAt > nowMs) " · ${rateCountdown(row.resetAt, nowMs)}" else ""
    Row(
        Modifier
            .fillMaxWidth()
            .clickable { onToggle() }
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
                text = row.name,
                fontSize = 13.sp,
                color = if (dim) C.textDim else C.textMain,
                maxLines = if (expanded) Int.MAX_VALUE else 1,
                overflow = if (expanded) TextOverflow.Clip else TextOverflow.Ellipsis,
                lineHeight = 17.sp,
            )
            Text(
                text = "${row.providerLabel} · $stateText$countdown",
                fontSize = 10.5.sp,
                color = C.textSub,
                maxLines = if (expanded) Int.MAX_VALUE else 1,
                overflow = if (expanded) TextOverflow.Clip else TextOverflow.Ellipsis,
            )
            Text(
                text = row.reason.ifBlank { "（无原因说明）" },
                fontSize = 10.5.sp,
                color = C.textDim,
                maxLines = if (expanded) Int.MAX_VALUE else 1,
                overflow = if (expanded) TextOverflow.Clip else TextOverflow.Ellipsis,
                lineHeight = 15.sp,
            )
            if (expanded) {
                Spacer(Modifier.height(4.dp))
                Text(
                    "全局优先级 ${if (row.priority >= 0) row.priority.toString() else "—"} · ${row.state.ifBlank { "状态未知" }}",
                    fontSize = 10.5.sp,
                    color = C.textSub,
                    lineHeight = 15.sp,
                )
                if (row.resetAt > nowMs) {
                    Text(
                        "预计 ${rateRecoverAt(row.resetAt, nowMs)} 恢复 · ${rateCountdown(row.resetAt, nowMs)}",
                        fontSize = 10.5.sp,
                        color = C.amber,
                        lineHeight = 15.sp,
                    )
                }
            }
        }
        if (isHead) AgateBadge("队首", tone = BadgeTone.GREEN)
        Text(if (expanded) "⌃" else "⌄", fontSize = 12.sp, color = C.textDim)
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
