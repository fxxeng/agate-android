package dev.agate.android.ui.provider

import androidx.compose.animation.animateContentSize
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
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.AccountOps
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.ProviderPreset
import dev.agate.android.data.PresetRegistry
import dev.agate.android.data.dto.AccountDto
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateAvatar
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateDragList
import dev.agate.android.ui.components.AgateProgress
import dev.agate.android.ui.components.AgateSearchBar
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive

/** 折叠组视图：家 preset（可能为 null=自定义）+ 账号列表。 */
internal data class ProviderGroupView(
    val providerId: String,
    val label: String,
    val preset: ProviderPreset?,
    val accounts: List<AccountDto>,
) {
    val hasWarn: Boolean get() = accounts.any { !it.enabled || !it.hasCredentials || it.lowBalance }
}

/**
 * 提供商页（M3-3R，对齐 v44 Tab 2）：
 * - 页头三动作：⇅ 排序 / + 添加；搜索框 + 三档筛选（全部/有异常/正常）
 * - 折叠卡：两字母渐变头像 + 右下状态点 / 名称 + N 个账号 · 登录方式 / 「管理」→ 详情子页
 * - collapsed 显示摘要行（N 就绪 / N 停用徽标 + 启用占比细条）；展开显示 acct 块
 * - 排序模式：amber 虚线横幅 + AgateDragList 拖拽 + 5s 无操作自动退出；顺序存壳层偏好
 */
@Composable
fun ProviderPage(
    modifier: Modifier = Modifier,
    onOpenAdd: () -> Unit = {},
    onOpenDetail: (String) -> Unit = {},
) {
    val C = agateColors()
    val ctx = LocalContext.current

    val snap by GatewayRepository.snapshot.collectAsState()

    DisposableEffect(Unit) {
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
        AccountOps.configure(GatewayPrefs.apiKey(ctx))
        onDispose { }
    }

    // ── 分组：有账号的家按「偏好顺序 → PresetRegistry 顺序」；无账号家不出现 ──
    val savedOrder = remember { GatewayPrefs.providerOrder(ctx) }
    val groups = remember(snap.accountList) {
        val byProvider = snap.accountList?.groupBy { it.provider } ?: emptyMap()
        val seq = ArrayList<ProviderGroupView>(byProvider.size)
        val seen = HashSet<String>()
        savedOrder.forEach { pid ->
            byProvider[pid]?.let { list ->
                val preset = PresetRegistry.ALL.find { it.id == pid }
                seq += ProviderGroupView(pid, preset?.label ?: PresetRegistry.labelOf(pid), preset, list)
                seen += pid
            }
        }
        PresetRegistry.ALL.forEach { preset ->
            if (preset.id !in seen) {
                byProvider[preset.id]?.let { list ->
                    seq += ProviderGroupView(preset.id, preset.label, preset, list)
                    seen += preset.id
                }
            }
        }
        byProvider.forEach { (pid, list) ->
            if (pid !in seen) seq += ProviderGroupView(pid, PresetRegistry.labelOf(pid), null, list)
        }
        seq
    }

    var query by remember { mutableStateOf("") }
    var filter by remember { mutableStateOf(0) }
    var reordering by remember { mutableStateOf(false) }
    val orderState = remember { mutableStateListOf<ProviderGroupView>().also { l -> l.addAll(groups) } }
    LaunchedEffect(groups) { if (!reordering) { orderState.clear(); orderState.addAll(groups) } }

    // 排序模式 5s 无操作自动退出（v44 语义）
    var lastTouch by remember { mutableStateOf(0L) }
    LaunchedEffect(reordering, lastTouch) {
        if (reordering) {
            val deadline = lastTouch + 5_000L
            while (isActive && System.currentTimeMillis() < deadline) {
                delay(500L)
            }
            if (isActive && reordering) {
                reordering = false
                GatewayPrefs.setProviderOrder(ctx, orderState.map { it.providerId })
            }
        }
    }

    val visible = orderState.filter { g ->
        (query.isBlank() || g.label.contains(query, ignoreCase = true) || g.providerId.contains(query, ignoreCase = true)) &&
            when (filter) {
                1 -> g.hasWarn
                2 -> !g.hasWarn
                else -> true
            }
    }
    val warnCount = orderState.count { it.hasWarn }
    val okCount = orderState.size - warnCount

    Column(Modifier.fillMaxSize().background(C.bg)) {
        // ── 页头（v44 .header-bar）──
        Row(
            Modifier.fillMaxWidth().padding(start = 16.dp, end = 12.dp, top = 4.dp, bottom = 2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("提供商", color = C.textMain, fontSize = 20.sp, fontWeight = FontWeight.Bold, modifier = Modifier.weight(1f))
            Text(
                "⇅ 排序",
                color = C.accent,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(C.accent.copy(alpha = 0.10f), RoundedCornerShape(9.dp))
                    .clickable {
                        if (reordering) {
                            // 手动退出同样落盘（与 5s 自动退出同一条保存路径）
                            reordering = false
                            GatewayPrefs.setProviderOrder(ctx, orderState.map { it.providerId })
                        } else {
                            lastTouch = System.currentTimeMillis()
                            reordering = true
                        }
                    }
                    .padding(horizontal = 12.dp, vertical = 7.dp),
            )
            Spacer(Modifier.width(8.dp))
            Text(
                "+ 添加",
                color = C.onAccent,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(C.accent, RoundedCornerShape(9.dp))
                    .clickable { onOpenAdd() }
                    .padding(horizontal = 12.dp, vertical = 7.dp),
            )
        }

        if (reordering) {
            // v44 .reorder-banner：amber 虚线横幅
            Row(
                Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 6.dp)
                    .background(C.amber.copy(alpha = 0.08f), RoundedCornerShape(10.dp))
                    .border(1.dp, C.amber.copy(alpha = 0.4f), RoundedCornerShape(10.dp))
                    .padding(horizontal = 12.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Box(Modifier.size(4.dp).background(C.amber, CircleShape))
                Text(
                    "排序中 · 拖动卡片调整显示顺序 · 松手即保存 · 5 秒无操作自动退出",
                    color = C.amber,
                    fontSize = 12.sp,
                )
            }
        } else {
            Column(Modifier.padding(horizontal = 16.dp)) {
                Spacer(Modifier.height(6.dp))
                AgateSearchBar(value = query, onValueChange = { query = it }, hint = "搜索提供商")
                Spacer(Modifier.height(8.dp))
                AgateSegmented(
                    options = listOf("全部 · ${orderState.size}", "有异常 · $warnCount", "正常 · $okCount"),
                    selected = filter,
                    onSelect = { filter = it },
                )
                Spacer(Modifier.height(6.dp))
                Text(
                    "点头部展开账号 · 长按卡片或点「⇅ 排序」调整顺序 · 「管理」进详情",
                    color = C.textSub,
                    fontSize = 12.sp,
                )
            }
        }

        // ── 卡片列表 ──
        if (orderState.isEmpty()) {
            Column(
                Modifier.fillMaxSize().padding(32.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Center,
            ) {
                Text("还没有任何账号", color = C.textSub, fontSize = 14.sp)
                Spacer(Modifier.height(8.dp))
                Text(
                    "+ 添加首个提供商",
                    color = C.onAccent,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .background(C.accent, RoundedCornerShape(10.dp))
                        .clickable { onOpenAdd() }
                        .padding(horizontal = 16.dp, vertical = 10.dp),
                )
            }
        } else if (reordering) {
            AgateDragList(
                items = orderState,
                onMove = { from, to ->
                    lastTouch = System.currentTimeMillis()
                    val item = orderState.removeAt(from)
                    orderState.add(to, item)
                },
                modifier = Modifier.fillMaxSize().padding(horizontal = 16.dp),
                reorderHint = "拖动调整显示顺序 · 5 秒无操作自动保存退出",
            ) { g, _, _ ->
                ReorderRow(g)
                Spacer(Modifier.height(8.dp))
            }
        } else {
            LazyColumn(
                Modifier.fillMaxSize().padding(horizontal = 16.dp),
                contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = 10.dp),
            ) {
                items(visible, key = { it.providerId }) { g ->
                    ProviderCard(
                        g = g,
                        heads = snap.currentAccountIds,
                        onOpenDetail = { onOpenDetail(g.providerId) },
                    )
                    Spacer(Modifier.height(10.dp))
                }
            }
        }
    }
}

/* ── 折叠卡（v44 .prov-sort）：头部常显；collapsed→摘要行；展开→acct 块 ── */
@Composable
private fun ProviderCard(
    g: ProviderGroupView,
    heads: Map<String, String>,
    onOpenDetail: () -> Unit,
) {
    val C = agateColors()
    var expanded by remember(g.providerId) { mutableStateOf(false) }
    val readyCount = g.accounts.count { it.enabled && it.hasCredentials }
    val downCount = g.accounts.size - readyCount

    Column(
        Modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(16.dp))
            .border(
                1.dp,
                if (g.hasWarn) C.amber.copy(alpha = 0.45f) else C.border,
                RoundedCornerShape(16.dp),
            )
            .clickable { expanded = !expanded }
            .padding(14.dp)
            .animateContentSize(),
    ) {
        // 头部：头像（右下状态点）+ 名称/副行/meta + 管理 + chevron
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box {
                AgateAvatar(name = g.label, size = 40.dp, letters = initialsOf(g.label))
                Box(
                    Modifier
                        .align(Alignment.BottomEnd)
                        .size(11.dp)
                        .background(if (g.hasWarn) C.amber else C.green, CircleShape)
                        .border(2.dp, C.cardBg, CircleShape),
                )
            }
            Spacer(Modifier.width(10.dp))
            Column(Modifier.weight(1f)) {
                Text(g.label, color = C.textMain, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
                Text(
                    "${g.accounts.size} 个账号 · ${g.preset?.let { channelLabel(it) } ?: "自定义"}",
                    color = C.textSub,
                    fontSize = 12.sp,
                )
                if (!expanded) {
                    Text(
                        g.preset?.desc ?: "自定义 / 中转上游",
                        color = C.textDim,
                        fontSize = 11.sp,
                        maxLines = 1,
                    )
                }
            }
            Text(
                "管理",
                color = C.accent,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(C.accent.copy(alpha = 0.10f), RoundedCornerShape(9.dp))
                    .clickable(onClick = onOpenDetail)
                    .padding(horizontal = 10.dp, vertical = 6.dp),
            )
            Spacer(Modifier.width(4.dp))
            Icon(
                Icons.Filled.KeyboardArrowDown,
                contentDescription = if (expanded) "收起" else "展开",
                tint = C.textDim,
                modifier = Modifier.size(20.dp).rotate(if (expanded) 180f else 0f),
            )
        }

        if (!expanded) {
            // v44 .prov-sum：徽标集合 + 右侧 tnum + 细条
            Spacer(Modifier.height(10.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (readyCount > 0) AgateBadge("$readyCount 就绪", BadgeTone.GREEN)
                if (downCount > 0) {
                    if (readyCount > 0) Spacer(Modifier.width(6.dp))
                    AgateBadge("$downCount 停用", BadgeTone.AMBER)
                }
                Text(
                    "${g.accounts.count { it.enabled }}/${g.accounts.size} 启用",
                    color = C.textSub,
                    fontSize = 12.sp,
                    modifier = Modifier.weight(1f),
                    textAlign = androidx.compose.ui.text.style.TextAlign.End,
                )
            }
            Spacer(Modifier.height(4.dp))
            AgateProgress(
                fraction = if (g.accounts.isEmpty()) 0f else g.accounts.count { it.enabled }.toFloat() / g.accounts.size,
                thick = false,
                fillColor = if (g.hasWarn) C.amber else C.green,
            )
        } else {
            // v44 .acct 块
            Spacer(Modifier.height(10.dp))
            g.accounts.forEachIndexed { i, acc ->
                AcctBlock(
                    acc = acc,
                    rank = i + 1,
                    isHead = heads[g.providerId] == acc.id,
                )
                Spacer(Modifier.height(8.dp))
            }
        }
    }
}

/* ── v44 .acct：#序号 + 名称 + 状态徽标；★ = 该家队首（内核 currentAccountIds 同口径）── */
@Composable
private fun AcctBlock(acc: AccountDto, rank: Int, isHead: Boolean) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .background(C.insetBg, RoundedCornerShape(12.dp))
            .padding(horizontal = 12.dp, vertical = 10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("#$rank", color = C.textDim, fontSize = 12.sp)
            Spacer(Modifier.width(8.dp))
            Text(
                (if (isHead) "★ " else "") + acc.displayName,
                color = C.textMain,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f),
            )
            when {
                !acc.enabled -> AgateBadge("停用", BadgeTone.GRAY)
                !acc.hasCredentials -> AgateBadge("缺凭证", BadgeTone.RED)
                acc.lowBalance -> AgateBadge("余额低", BadgeTone.AMBER)
                else -> AgateBadge("就绪", BadgeTone.GREEN)
            }
        }
        Spacer(Modifier.height(6.dp))
        Text(
            "余额与用量待额度端点接入 · 详情页可启停删除",
            color = C.textDim,
            fontSize = 11.sp,
        )
    }
}

/* 排序模式单行（v44 .reordering：收成单行便于跨家拖动） */
@Composable
private fun ReorderRow(g: ProviderGroupView) {
    val C = agateColors()
    Row(
        Modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(14.dp))
            .border(1.dp, C.border, RoundedCornerShape(14.dp))
            .padding(horizontal = 12.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        AgateAvatar(name = g.label, size = 32.dp, corner = 10.dp, letters = initialsOf(g.label))
        Spacer(Modifier.width(10.dp))
        Text(g.label, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
        Text("${g.accounts.count { it.enabled }}/${g.accounts.size} 启用", color = C.textSub, fontSize = 12.sp)
    }
}
