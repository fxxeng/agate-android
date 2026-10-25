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
import android.widget.Toast
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.rememberCoroutineScope
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
import dev.agate.android.data.TaskCenter
import dev.agate.android.data.dto.AccountDto
import dev.agate.android.ui.models.parseMultiplier
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateProviderAvatar
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.components.AgateAvatar
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateDragList
import dev.agate.android.ui.components.AgateHeaderAction
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.AgatePrimaryAction
import dev.agate.android.ui.components.AgateProgress
import dev.agate.android.ui.components.AgateSearchBar
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import org.json.JSONObject

/** 折叠组视图：家 preset（可能为 null=自定义）+ 账号列表。 */
internal data class ProviderGroupView(
    val providerId: String,
    val label: String,
    val preset: ProviderPreset?,
    val accounts: List<AccountDto>,
) {
    val hasWarn: Boolean get() = accounts.any { !it.enabled || !it.hasCredentials || it.lowBalance }
}

/** 单账号的余额/在途读数（来自 /api/accounts/usage 与 /connections）。 */
internal data class AcctMetric(
    val text: String? = null,
    val pct: Int? = null,
    val failed: Boolean = false,
)

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
    onOpenDrawer: () -> Unit = {},
    onOpenAdd: () -> Unit = {},
    onOpenDetail: (String) -> Unit = {},
) {
    val C = agateColors()
    val ctx = LocalContext.current

    val snap by GatewayRepository.snapshot.collectAsState()
    val taskSnap by TaskCenter.snapshot.collectAsState()
    val scope = rememberCoroutineScope()

    var metrics by remember { mutableStateOf<Map<String, AcctMetric>>(emptyMap()) }
    var inflight by remember { mutableStateOf<Map<String, Int>>(emptyMap()) }
    var querying by remember { mutableStateOf(false) }

    DisposableEffect(Unit) {
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
        AccountOps.configure(GatewayPrefs.apiKey(ctx))
        TaskCenter.configure(GatewayPrefs.apiKey(ctx))
        TaskCenter.refreshOnce()
        onDispose { }
    }

    /** 并发查询全部账号余额 + 在途连接数（v44 页头「查余额」）。silent=首屏自动拉取不弹提示。 */
    fun loadBalances(silent: Boolean = false) {
        if (querying) return
        querying = true
        scope.launch {
            val usage = AccountOps.usage()
            val conn = AccountOps.connections()
            querying = false
            if (usage != null) {
                val out = HashMap<String, AcctMetric>()
                val arr = usage.optJSONArray("results")
                if (arr != null) for (i in 0 until arr.length()) {
                    val row = arr.optJSONObject(i) ?: continue
                    val id = row.optString("id")
                    if (id.isBlank()) continue
                    val err = row.optString("error")
                    out[id] = if (err.isNotBlank()) AcctMetric(text = "查询失败：$err", failed = true)
                    else parseUsageMetric(row.optJSONObject("usage"))
                }
                metrics = out
            }
            conn?.optJSONObject("counts")?.let { c ->
                val map = HashMap<String, Int>()
                c.keys().forEach { k -> map[k] = c.optInt(k) }
                inflight = map
            }
            if (!silent) {
                Toast.makeText(
                    ctx,
                    if (usage == null) "余额查询失败（网关未运行？）" else "已刷新 ${metrics.size} 个账号余额",
                    Toast.LENGTH_SHORT,
                ).show()
            }
        }
    }

    // 首屏自动拉一次余额/在途（有账号时，静默）
    LaunchedEffect(snap.accountList.size) {
        if (snap.accountList.isNotEmpty() && metrics.isEmpty()) loadBalances(silent = true)
    }

    // 倍率区间：从客户端清单（/v1/models 同源）按家聚合 credits
    val multByProvider = remember(snap.models) {
        snap.models.groupBy { it.provider }.mapValues { (_, rows) ->
            val nums = rows.mapNotNull { parseMultiplier(it.credits) }
            if (nums.isEmpty()) null else nums.min() to nums.max()
        }
    }
    val checkinProviders = remember(taskSnap.providerOptions) {
        taskSnap.providerOptions.map { it.first }.toSet()
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
        // ── 页头（v44 .header-bar：菜单 + 标题 + 排序/添加）──
        AgatePageHeader(title = "提供商", onOpenDrawer = onOpenDrawer) {
            AgateHeaderAction(if (querying) "查询中…" else "查余额") { loadBalances() }
            AgateHeaderAction(if (reordering) "保存" else "⇅ 排序") {
                if (reordering) {
                    // 手动退出同样落盘（与 5s 自动退出同一条保存路径）
                    reordering = false
                    GatewayPrefs.setProviderOrder(ctx, orderState.map { it.providerId })
                } else {
                    lastTouch = System.currentTimeMillis()
                    reordering = true
                }
            }
            AgatePrimaryAction("+ 添加") { onOpenAdd() }
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
                        metrics = metrics,
                        inflight = inflight,
                        mult = multByProvider[g.providerId],
                        checkinSupported = g.providerId in checkinProviders,
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
    metrics: Map<String, AcctMetric>,
    inflight: Map<String, Int>,
    mult: Pair<Double, Double>?,
    checkinSupported: Boolean,
    onOpenDetail: () -> Unit,
) {
    val C = agateColors()
    var expanded by remember(g.providerId) { mutableStateOf(false) }
    val readyCount = g.accounts.count { it.enabled && it.hasCredentials }
    val downCount = g.accounts.size - readyCount

    // 剩余额度占比（各家账号 pct 的均值；无数据显示 —）
    val pcts = g.accounts.mapNotNull { metrics[it.id]?.pct }
    val avgPct = if (pcts.isEmpty()) null else pcts.average().toInt()

    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(
                1.dp,
                if (g.hasWarn) C.amber.copy(alpha = 0.45f) else C.border,
                RoundedCornerShape(20.dp),
            )
            .clickable { expanded = !expanded }
            .padding(14.dp)
            .animateContentSize(),
    ) {
        // 头部：头像（右下状态点）+ 名称/副行/meta + 管理 + chevron
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box {
                AgateProviderAvatar(providerId = g.providerId, label = g.label, size = 40.dp)
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
                // v44 .pv-meta：倍率区间 + 签到支持（真实数据，缺则省略）
                val meta = buildList {
                    mult?.let { (lo, hi) ->
                        add(
                            if (kotlin.math.abs(lo - hi) < 0.001) "倍率 ×${fmtMult(lo)}"
                            else "倍率 ×${fmtMult(lo)}~${fmtMult(hi)}"
                        )
                    }
                    add(if (checkinSupported) "支持签到" else "不支持签到")
                }.joinToString(" · ")
                Text(meta, color = C.textDim, fontSize = 11.sp, maxLines = 1)
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
                    avgPct?.let { "剩余 $it%" } ?: "余额未查询",
                    color = C.textSub,
                    fontSize = 12.sp,
                    modifier = Modifier.weight(1f),
                    textAlign = androidx.compose.ui.text.style.TextAlign.End,
                )
            }
            Spacer(Modifier.height(4.dp))
            AgateProgress(
                fraction = when {
                    avgPct != null -> avgPct / 100f
                    else -> if (g.accounts.isEmpty()) 0f else g.accounts.count { it.enabled }.toFloat() / g.accounts.size
                },
                thick = false,
                fillColor = when {
                    avgPct == null -> if (g.hasWarn) C.amber else C.green
                    avgPct >= 50 -> C.green
                    avgPct >= 20 -> C.accent
                    else -> C.amber
                },
            )
        } else {
            // v44 .acct 块
            Spacer(Modifier.height(10.dp))
            g.accounts.forEachIndexed { i, acc ->
                AcctBlock(
                    acc = acc,
                    rank = i + 1,
                    isHead = heads[g.providerId] == acc.id,
                    metric = metrics[acc.id],
                    inflight = inflight[acc.id] ?: 0,
                )
                Spacer(Modifier.height(8.dp))
            }
        }
    }
}

/* ── v44 .acct：序号 + 名称 + 状态；余额行 + 细条 + 在途并发点 ── */
@Composable
private fun AcctBlock(
    acc: AccountDto,
    rank: Int,
    isHead: Boolean,
    metric: AcctMetric?,
    inflight: Int,
) {
    val C = agateColors()
    val cap = acc.raw.optInt("maxConcurrent", 0)
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
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text("剩余额度", color = C.textSub, fontSize = 12.sp, modifier = Modifier.weight(1f))
            Text(
                metric?.text ?: "--",
                color = if (metric?.failed == true) C.red else C.textSub,
                fontSize = 12.sp,
            )
        }
        Spacer(Modifier.height(4.dp))
        AgateProgress(
            fraction = (metric?.pct ?: 0) / 100f,
            thick = false,
            fillColor = when {
                metric?.pct == null -> C.border
                metric.pct >= 50 -> C.green
                metric.pct >= 20 -> C.accent
                else -> C.amber
            },
        )
        if (metric?.failed == true && cap >= 0) {
            // 查询失败时给一行说明，不画在途点（信息已在上方）
            Spacer(Modifier.height(6.dp))
            Text(metric.text ?: "", color = C.red, fontSize = 11.sp, maxLines = 1)
        }
        Spacer(Modifier.height(6.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                "在途 $inflight" + if (cap > 0) "/$cap" else "",
                color = C.textDim,
                fontSize = 11.sp,
            )
            if (cap in 1..6) {
                Spacer(Modifier.width(8.dp))
                repeat(cap) { i ->
                    Box(
                        Modifier
                            .padding(end = 3.dp)
                            .size(6.dp)
                            .background(if (i < inflight) C.green else C.border, CircleShape),
                    )
                }
            }
        }
    }
}

/** 余额百分比/文案：usage 各家字段不同，按候选键取值。 */
internal fun parseUsageMetric(o: JSONObject?): AcctMetric {
    if (o == null) return AcctMetric()
    fun d(vararg ks: String): Double? = ks.firstNotNullOfOrNull { k ->
        if (o.has(k)) o.optDouble(k, Double.NaN).takeIf { !it.isNaN() } else null
    }
    val left = d("left", "remain", "remaining", "balance", "available", "total_granted")
    val total = d("total", "quota", "granted")
    val used = d("used", "usage", "consumed")
    val pct = when {
        total != null && total > 0 && left != null -> ((left / total) * 100).toInt().coerceIn(0, 100)
        total != null && total > 0 && used != null -> (((total - used) / total) * 100).toInt().coerceIn(0, 100)
        else -> null
    }
    val text = when {
        left != null && pct != null -> "${fmtNum(left)} · $pct%"
        left != null -> fmtNum(left)
        used != null -> "已用 ${fmtNum(used)}"
        else -> null
    }
    return AcctMetric(text = text, pct = pct)
}

/** 倍率展示：整数不带小数，其余去掉多余的 0。 */
internal fun fmtMult(v: Double): String =
    if (v % 1.0 == 0.0) String.format("%.0f", v)
    else String.format("%.2f", v).trimEnd('0').trimEnd('.')

private fun fmtNum(v: Double): String =
    if (v >= 1000) String.format("%,.0f", v)
    else String.format("%.2f", v).trimEnd('0').trimEnd('.')

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
        AgateProviderAvatar(providerId = g.providerId, label = g.label, size = 32.dp, corner = 10.dp)
        Spacer(Modifier.width(10.dp))
        Text(g.label, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
        Text("${g.accounts.count { it.enabled }}/${g.accounts.size} 启用", color = C.textSub, fontSize = 12.sp)
    }
}
