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
import dev.agate.android.data.CustomProviderOps
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.ProviderPreset
import dev.agate.android.data.PresetRegistry
import dev.agate.android.data.CheckinCenter
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
import org.json.JSONArray
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
    /**
     * 可累加的数值读数（无则 null）。
     *
     * 卡片折叠态要显示**这家全部账号的累加余额** —— 展示串（[text]）是拼好的
     * （「共8,200 · 套餐0 · 奖励8,200」），拼不回去，所以数值要单独带出来。
     * 单位只在同一家内一致（WorkBuddy 是积分、ZCode 是 token、Cline 是 credit），
     * 因此按家累加安全，跨家不可比。
     */
    val amount: Double? = null,
    /** 读数单位（积分 / token / credits…），上游没给则 null。 */
    val unit: String? = null,
    /** 最近一份会作废额度的到期时刻（毫秒）与那一份的剩余量。 */
    val expiryAt: Long? = null,
    val expiryAmount: Double? = null,
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
    onOpenQuotaCenter: () -> Unit = {},
    onOpenLimiter: (AccountDto) -> Unit = {},
) {
    val C = agateColors()
    val ctx = LocalContext.current

    val snap by GatewayRepository.snapshot.collectAsState()
    // 签到能力标读聚合快照里的 providerOptions（= CHECKIN_PROVIDERS ∩ supports_checkin）
    // —— 那是「哪家能签到」的唯一权威答案，本页不另维护一份清单
    val checkinSnap by CheckinCenter.snapshot.collectAsState()
    val scope = rememberCoroutineScope()

    var metrics by remember { mutableStateOf<Map<String, AcctMetric>>(emptyMap()) }
    // 「额度设限」的编辑目标（null = 弹窗关闭）
    var limiterTarget by remember { mutableStateOf<AccountDto?>(null) }
    var inflight by remember { mutableStateOf<Map<String, Int>>(emptyMap()) }
    var querying by remember { mutableStateOf(false) }

    DisposableEffect(Unit) {
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
        AccountOps.configure(GatewayPrefs.apiKey(ctx))
        CheckinCenter.configure(GatewayPrefs.apiKey(ctx))
        CustomProviderOps.configure(GatewayPrefs.apiKey(ctx))
        // 进本页立刻补拉一次：轮询已提到导航宿主，但它的第一个 5 秒刻度可能
        // 还没到；本页的主内容就是账号清单，等那 5 秒会是空白屏。refreshOnce
        // 自带在途去重，与轮询撞车也不会打两次。
        GatewayRepository.refreshOnce()
        CheckinCenter.refreshOnce()
        onDispose { }
    }

    /** 并发查询全部账号余额 + 在途连接数。silent=首屏自动拉取不弹提示。 */
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
                    // ⚠️ error 是 JSON null 时 `optString` 会返回字符串 "null"
                    //（org.json 的 objectToString），旧实现把它当成错误文案，
                    // 真机上显示「查询失败：null」。这里只认真正的字符串。
                    val errRaw = row.opt("error")
                    val err = (errRaw as? String)?.trim().orEmpty()
                    out[id] = when {
                        err.isNotEmpty() -> AcctMetric(text = "查询失败：$err", failed = true)
                        row.optJSONObject("usage") == null -> AcctMetric(text = "未返回余额数据")
                        else -> parseUsageMetric(row.optJSONObject("usage"))
                    }
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
                    if (usage == null) "余额查询失败" else "已刷新 ${metrics.size} 个账号余额",
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
    val checkinProviders = remember(checkinSnap.providerOptions) {
        checkinSnap.providerOptions.map { it.first }.toSet()
    }
    // ── 自定义提供商的展示名 ──────────────────────────────────
    // `custom-xxxx` 这种 id 在内置注册表里查不到，`labelOf` 会**原样回显 id** ——
    // 于是列表里出现「custom-7a3f9c21」这种名字（用户建的「NewAPI 中转」根本
    // 看不见）。上游 3.0.8 的做法是把 id→name 并进 `wbProviders.labelOf` 的
    // 查找链，一处合并、账号表/筛选器/日志处处生效；这里照同一口径。
    var customNames by remember { mutableStateOf<Map<String, String>>(emptyMap()) }
    LaunchedEffect(Unit) {
        customNames = CustomProviderOps.Record.listOf(CustomProviderOps.list())
            .associate { it.id to it.name }
    }
    val labelOf = remember(customNames) {
        { pid: String -> customNames[pid] ?: PresetRegistry.labelOf(pid) }
    }
    // ── 分组：有账号的家按「偏好顺序 → PresetRegistry 顺序」；无账号家不出现 ──
    val savedOrder = remember { GatewayPrefs.providerOrder(ctx) }
    val groups = remember(snap.accountList, customNames) {
        val byProvider = snap.accountList?.groupBy { it.provider } ?: emptyMap()
        val seq = ArrayList<ProviderGroupView>(byProvider.size)
        val seen = HashSet<String>()
        savedOrder.forEach { pid ->
            byProvider[pid]?.let { list ->
                val preset = PresetRegistry.ALL.find { it.id == pid }
                seq += ProviderGroupView(pid, preset?.label ?: labelOf(pid), preset, list)
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
            if (pid !in seen) seq += ProviderGroupView(pid, labelOf(pid), null, list)
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
                        usageCapableProviders = snap.providerUsage,
                        onOpenDetail = { onOpenDetail(g.providerId) },
                        onOpenQuotaCenter = onOpenQuotaCenter,
                        onOpenLimiter = onOpenLimiter,
                    )
                    Spacer(Modifier.height(10.dp))
                }
            }
        }
    }
    // ── 额度设限弹窗 ──
    // 只在用户点了某账号的入口后出现；保存走 PATCH /api/accounts 的 `limiters`
    // （整块替换），内核归一化后同步旧 `lowBalance` 供降级兼容。
    limiterTarget?.let { acc ->
        AccountLimiterDialog(
            accountName = acc.displayName,
            current = limiterRulesOf(acc.raw),
            onDismiss = { limiterTarget = null },
            onSave = { result ->
                limiterTarget = null
                scope.launch {
                    // 整块替换：内核 `apply_patch` 归一化校验后同步旧 lowBalance；
                    // 空数组 = 不限制（与 3.0.8「没有生效的规则就是不限制」一致）
                    val body = JSONObject()
                    body.put("limiters", JSONArray().apply { result.forEach { put(it.toJson()) } })
                    val r = AccountOps.update(acc.id, body)
                    Toast.makeText(
                        ctx,
                        if (r.ok) (if (result.isEmpty()) "已取消额度限制" else "已设置额度限制")
                        else (r.error ?: "保存失败"),
                        Toast.LENGTH_SHORT,
                    ).show()
                    if (r.ok) GatewayRepository.refreshOnce()
                }
            },
        )
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
    /** provider id → 该家是否支持余额查询（内核能力投影）。 */
    usageCapableProviders: Map<String, Boolean>,
    onOpenDetail: () -> Unit,
    onOpenQuotaCenter: () -> Unit = {},
    onOpenLimiter: (AccountDto) -> Unit = {},
) {
    val C = agateColors()
    var expanded by remember(g.providerId) { mutableStateOf(false) }
    // 这一家有没有余额能力（决定要不要显示「额度限设」入口）
    val usageCapable = usageCapableProviders[g.providerId] ?: false
    // ── 就绪数 vs 账号总数 ────────────────────────────────────
    // 两者不是一回事，这是「N 就绪」徽章存在的理由：
    //   账号总数 = 这家名下所有账号（含停用、缺凭证的）；
    //   就绪    = 队列里**真正能用**的：启用 + 有凭证 + 未被低余额规则跳过 + 无查询失败。
    // 「余额低」只有配了跳过规则时才算不可用（只是徽章提示的不影响选号）。
    // 该家不支持余额查询时，就绪 = 启用 + 有凭证（不看余额）。
    val readyCount = g.accounts.count { acc ->
        if (!acc.enabled || !acc.hasCredentials) return@count false
        if (!usageCapable) return@count true
        val m = metrics[acc.id] ?: return@count true   // 还没查过：不因此判为不可用
        if (m.failed) return@count false
        val skippedByBalance = limiterRulesOf(acc.raw).any { rule ->
            rule.enabled && rule.type == "balance" && rule.action == "skip" &&
                m.amount != null && m.amount < rule.threshold
        }
        !skippedByBalance
    }

    // ── 这家整体的读数（折叠态那一行）─────────────────────────
    // 1) 累加余额：只累加**读到了数值**的账号（同家单位一致，可相加）；
    // 2) 最近到期：全部账号里最早到期的那一份，带上它自己的剩余量；
    //    日包也在内 —— 它的额度是真实可用的，到期即重置（文案落到「今日到期」）。
    val mine = g.accounts.mapNotNull { metrics[it.id] }
    val amounts = mine.mapNotNull { it.amount }
    val totalAmount = if (amounts.isEmpty()) null else amounts.sum()
    val balanceUnit = mine.firstNotNullOfOrNull { it.unit }
    val failedCount = mine.count { it.failed }
    val headExpiry = mine.filter { it.expiryAt != null }.minByOrNull { it.expiryAt!! }

    // 2.5) 水位条只在有真比例时画 —— 无数据画满格会被读成「额度充足」
    val avgPct = mine.mapNotNull { it.pct }.takeIf { it.isNotEmpty() }?.average()?.toInt()

    // 3) 副行：常态「N 个账号 · 最低 ×倍率」；有问题时整行换成待办
    val minMult = mult?.first
    val pending = buildList {
        if (failedCount > 0) add("$failedCount 个查询失败")
        val lowCount = g.accounts.count { it.lowBalance }
        if (lowCount > 0) add("$lowCount 个余额低")
        val noCred = g.accounts.count { !it.hasCredentials }
        if (noCred > 0) add("$noCred 个缺凭证")
        val soonest = g.accounts.mapNotNull { credentialExpiryMs(it) }.minOrNull()
        val leftMs = soonest?.minus(System.currentTimeMillis())
        if (leftMs != null && leftMs <= 3L * 86_400_000L) {
            add("凭证${expiryPhrase(leftMs).replace("到期", "")}到期")
        }
    }
    val subLine = if (pending.isNotEmpty()) pending.joinToString(" · ")
    else buildString {
        append("${g.accounts.size} 个账号")
        minMult?.let { append(" · 最低 ×${fmtMult(it)}") }
    }

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
        // ── 第 1 行：头像 + 名称 + 就绪徽章 + 管理 + 折叠箭头 ──
        // 名称独占剩余宽度并单行省略：数值挪到第 3 行，不再和名称抢位置
        //（旧版挤在一行，窄屏上名称会被截成「WorkBuddy...」）。
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box {
                AgateProviderAvatar(providerId = g.providerId, label = g.label, size = 38.dp)
                Box(
                    Modifier
                        .align(Alignment.BottomEnd)
                        .size(11.dp)
                        .background(if (g.hasWarn) C.amber else C.green, CircleShape)
                        .border(2.dp, C.cardBg, CircleShape),
                )
            }
            Spacer(Modifier.width(9.dp))
            Text(
                g.label,
                color = C.textMain,
                fontSize = 16.sp,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
            // 自定义家（含预置 API 建出来的）挂一枚角标：它们在注册表里没有位置，
            // 不标的话「NewAPI 中转」这种名字与内置的 24 家混在一起分不出来。
            if (CustomProviderOps.isCustomProviderId(g.providerId)) {
                Spacer(Modifier.width(6.dp))
                AgateBadge("自定义", BadgeTone.BLUE)
            }
            Spacer(Modifier.width(8.dp))
            // 就绪徽章：有账号掉队时写 n/N（分子可用 / 分母总数）
            if (g.accounts.isNotEmpty()) {
                if (readyCount == g.accounts.size) {
                    AgateBadge("$readyCount 就绪", BadgeTone.GREEN)
                } else {
                    AgateBadge(
                        "$readyCount/${g.accounts.size} 就绪",
                        if (readyCount == 0) BadgeTone.RED else BadgeTone.AMBER,
                    )
                }
            }
            Spacer(Modifier.weight(1f))
            Text(
                "管理",
                color = C.accent,
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(C.accent.copy(alpha = 0.10f), RoundedCornerShape(9.dp))
                    .clickable(onClick = onOpenDetail)
                    .padding(horizontal = 10.dp, vertical = 5.dp),
            )
            Spacer(Modifier.width(2.dp))
            Icon(
                Icons.Filled.KeyboardArrowDown,
                contentDescription = if (expanded) "收起" else "展开",
                tint = C.textDim,
                modifier = Modifier.size(18.dp).rotate(if (expanded) 180f else 0f),
            )
        }

        // ── 第 2、3 行：缩进到与名称左对齐，各自独占整行 ──
        // 缩进 38+9=47dp，与头像右侧的名称起点对齐
        Spacer(Modifier.height(3.dp))
        Column(Modifier.padding(start = 47.dp)) {
            // 第 2 行：状态副行（常态是账号数 + 最低倍率；有问题时换成待办）
            Text(
                subLine,
                color = if (pending.isNotEmpty()) C.amber else C.textSub,
                fontSize = 12.sp,
                maxLines = 1,
                overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
            )
            // 第 3 行：额度与最近到期**同一行**（不再换行）
            val amountText = totalAmount?.let { fmtNum(it) + (balanceUnit?.let { u -> " $u" } ?: "") }
            val expText = headExpiry?.let { exp ->
                val remain = exp.expiryAmount?.let { "${fmtNum(it)} · " } ?: ""
                remain + expiryPhrase(exp.expiryAt!! - System.currentTimeMillis())
            }
            if (amountText != null || expText != null) {
                Spacer(Modifier.height(2.dp))
                Row(verticalAlignment = Alignment.Bottom) {
                    if (amountText != null) {
                        Text(
                            amountText,
                            color = C.textMain,
                            fontSize = 15.sp,
                            fontWeight = FontWeight.SemiBold,
                            maxLines = 1,
                            overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                        )
                    }
                    if (amountText != null && expText != null) Spacer(Modifier.width(9.dp))
                    if (expText != null) {
                        val leftMs = headExpiry!!.expiryAt!! - System.currentTimeMillis()
                        Text(
                            expText,
                            color = when {
                                leftMs <= 0 -> C.red
                                leftMs <= 3L * 86_400_000L -> C.red
                                leftMs <= 7L * 86_400_000L -> C.amber
                                else -> C.textDim
                            },
                            fontSize = 12.sp,
                            maxLines = 1,
                            overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                        )
                    }
                }
            }
        }

        if (!expanded) {
            // 水位条：**只在有真比例时画**。旧实现在无数据时按「启用账号占比」画
            // 一条满格绿条 —— 那会被读成「额度充足」，是最误导的一处。
            if (avgPct != null) {
                Spacer(Modifier.height(9.dp))
                AgateProgress(
                    fraction = avgPct / 100f,
                    thick = false,
                    fillColor = when {
                        avgPct >= 50 -> C.green
                        avgPct >= 20 -> C.accent
                        else -> C.amber
                    },
                )
            }
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
                    usageCapable = usageCapable,
                    onOpenQuotaCenter = onOpenQuotaCenter,
                    onOpenLimiter = onOpenLimiter,
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
    usageCapable: Boolean,
    onOpenQuotaCenter: () -> Unit = {},
    onOpenLimiter: (AccountDto) -> Unit = {},
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
        // ── 余额 ──
        // 读数行只放「还剩多少 + 单位」，不再重复拼「套餐 0 · 奖励 0」（那截长文字
        // 会挤压读数）；额度包明细去点「明细 ›」。
        val balanceText = metric?.amount?.let { fmtNum(it) + (metric.unit?.let { u -> " $u" } ?: "") }
            ?: metric?.text
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text("余额", color = C.textSub, fontSize = 12.sp, modifier = Modifier.weight(1f))
            Text(
                balanceText ?: "未查询",
                color = when {
                    metric?.failed == true -> C.red
                    balanceText == null -> C.textDim
                    else -> C.textMain
                },
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(2f, fill = false),
                textAlign = androidx.compose.ui.text.style.TextAlign.End,
                maxLines = 1,
                overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
            )
            Spacer(Modifier.size(6.dp))
            Text(
                "明细 ›",
                color = C.accent,
                fontSize = 11.sp,
                modifier = Modifier
                    .clickable { onOpenQuotaCenter() }
                    .padding(horizontal = 2.dp, vertical = 2.dp),
            )
        }

        // ── 凭证：有效期与自动刷新合并一行 ──
        // 字段名按家不同（`expiresAt` / `tokenExpiresAt`），两个都认；
        // 没有该字段的家（kuku / antigravity / chatglm）整行不渲染。
        credentialExpiryMs(acc)?.let { at ->
            val leftMs = at - System.currentTimeMillis()
            Row(Modifier.fillMaxWidth().padding(top = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                Text("凭证", color = C.textSub, fontSize = 12.sp, modifier = Modifier.weight(1f))
                Text(
                    expiryPhrase(leftMs) + " · 自动刷新",
                    color = when {
                        leftMs <= 0 -> C.red
                        leftMs <= 24L * 3_600_000L -> C.red
                        leftMs <= 3L * 86_400_000L -> C.amber
                        else -> C.textSub
                    },
                    fontSize = 12.sp,
                    maxLines = 1,
                    overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                )
            }
        }

        // ── 额度设限：**配置了才显示** ──
        // 两个条件都满足才渲染：该家支持余额查询 + 该账号确实配了规则。
        // 没配就不占一行（常态界面更干净）；想设限的入口在详情页「设置」。
        if (usageCapable) {
            limiterRulesOf(acc.raw).firstOrNull()?.let { rule ->
                LimiterEntryRow(
                    summary = rule.summary(),
                    onOpen = { onOpenLimiter(acc) },
                )
            }
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
        // 布尔不能当数字（JSONObject.getBoolean 是真假两值；这里只认数字与数字串）
        val raw = o.opt(k) ?: return@firstNotNullOfOrNull null
        when (raw) {
            is Number -> raw.toDouble().takeIf { it.isFinite() }
            is String -> raw.trim().toDoubleOrNull()?.takeIf { it.isFinite() }
            else -> null
        }
    }

    // ── 形状 1：WorkBuddy 既有契约 `{kind, unlimited, totalLeft, planLeft, bonusLeft}` ──
    // 这个形状**没有** total/used 一类字段，当年必须在这里单开一支；否则
    // `totalLeft` 认不出来，界面就恒显示「--」（真机踩到过）。
    // 判据是字段存在性，与内核 `usage_records::extract_remaining` 同源。
    if (o.has("totalLeft")) {
        val unlimited = o.optBoolean("unlimited", false) ||
            (o.opt("unlimited")?.let { it !is Boolean && it.toString().isNotEmpty() } == true)
        val total = d("totalLeft")
        if (unlimited) return AcctMetric(text = "∞ 不限量")

        // 三份明细（总 / 套餐 / 奖励）都是上游给的数，只做展示不做二次计算
        val parts = ArrayList<String>(3)
        total?.let { parts += "共${fmtNum(it)}" }
        d("planLeft")?.let { parts += "套餐${fmtNum(it)}" }
        d("bonusLeft")?.let { parts += "奖励${fmtNum(it)}" }
        val exp1 = extractExpiry(o)
        return when {
            total != null && parts.isNotEmpty() -> AcctMetric(
                text = parts.joinToString(" · "),
                pct = null,
                amount = total,
                unit = o.optString("unit").ifBlank { null },
                expiryAt = exp1?.atMs,
                expiryAmount = exp1?.amount,
            )
            else -> AcctMetric(text = null)
        }
    }

    // ── 形状 2：归一化形状 `{available, availableView, unit, wallets, total, used}` ──
    // `availableView` 是后端给的**紧凑展示串**（如「1亿 token」）；余额列只有
    // 几十像素宽，数字太长会挤爆，有展示串就用它，没有才按数值拼。
    val left = d("available", "left", "remain", "remaining", "balance", "total_granted")
    val total = d("total", "quota", "granted")
    val used = d("used", "usage", "consumed")
    val pct = when {
        total != null && total > 0 && left != null -> ((left / total) * 100).toInt().coerceIn(0, 100)
        total != null && total > 0 && used != null -> (((total - used) / total) * 100).toInt().coerceIn(0, 100)
        else -> null
    }

    // 钱包/额度桶明细：主读数缺失（上游只给比例，如 Accio 的 usagePercent）时，
    // 从有实际读数的钱包里兜一个出来 —— 有可读的就不再显示「可用 —」。
    val wallets = o.optJSONArray("wallets")
    val fallbackWallet = if (left == null && wallets != null) {
        var picked: Pair<Double, String>? = null
        for (i in 0 until wallets.length()) {
            val w = wallets.optJSONObject(i) ?: continue
            val value = w.opt("balance")?.let { raw ->
                when (raw) {
                    is Number -> raw.toDouble()
                    is String -> raw.trim().toDoubleOrNull()
                    else -> null
                }
            } ?: continue
            val unit = w.optString("unit").ifBlank { "" }
            if (picked == null) picked = value to unit
        }
        picked
    } else {
        null
    }

    val unit = o.optString("unit").ifBlank { "" }
    val view = o.optString("availableView").ifBlank { null }
    val text = when {
        view != null -> view
        left != null && pct != null -> "${fmtNum(left)} · $pct%"
        left != null -> "${fmtNum(left)}${if (unit.isNotEmpty()) " $unit" else ""}"
        fallbackWallet != null ->
            "${fmtNum(fallbackWallet.first)}" + if (fallbackWallet.second.isNotEmpty()) " ${fallbackWallet.second}" else ""
        used != null -> "已用 ${fmtNum(used)}"
        else -> null
    }

    // 半次失败：一台网关读到、另一台没读到（CodeArts 的订阅统计 / 福利网关）。
    // 读数是真的但不完整 —— 不能显示成一片绿，否则会被读成「额度用完了」。
    val partial = listOf(
        o.optString("statisticsError"),
        o.optString("benefitError"),
    ).count { it.isNotBlank() }

    // 累加口径：优先 `available`，退回钱包里的实际读数
    val exp2 = extractExpiry(o)
    return AcctMetric(
        text = when {
            text == null -> null
            partial > 0 -> "$text · ⚠ 部分未读到"
            else -> text
        },
        pct = pct,
        amount = left ?: fallbackWallet?.first,
        unit = unit.ifBlank { fallbackWallet?.second ?: "" }.ifBlank { null },
        expiryAt = exp2?.atMs,
        expiryAmount = exp2?.amount,
    )
}

/**
 * 到期文案（全局唯一口径）：`今日到期` / `N 天后到期` / `已过期`。
 *
 * 「0 天后到期」这种说法是机器话 —— 到期当天就该写「今日到期」。
 * 卡片读数行、账号卡的凭证行、额度包明细共用这一个函数，避免几处各写一套。
 */
internal fun expiryPhrase(leftMs: Long): String {
    if (leftMs <= 0) return "已过期"
    val days = leftMs / 86_400_000L
    return if (days <= 0L) "今日到期" else "${days} 天后到期"
}

/**
 * 从 usage 里挑出**最近一份到期**的额度：到期时刻 + 那一份的剩余量。
 *
 * ── 日包按普通包处理（这是刻意的）──────────────────────────
 * 上游确实有「每日赠送 / 日额度」这类包（loomy 的每日赠送积分、workbuddy 的日额度
 * 包、zcode 的 `period == "daily"` 权益、autoclaw 的赠送积分），它们的 `expireAt`
 * 是「今天 24 点」。**但那个包里的额度是真实可用的**（比如「每日刷新给 100 积分」
 * 就是 100），所以它跟普通额度包一样参与「最近到期」的比较与展示，只是到期文案
 * 会落到「今日到期」—— 语义正确：今天用完就重置。
 * 把它排除会让这类账号永远没有到期信息，反而更误导。
 *
 * ── 选取规则 ──────────────────────────────────────────────
 * 优先在「剩余 > 0」的包里挑最早到期的（有余额的才是用户关心的）；
 * 全都没剩余时退回全部里最早到期的那个；再没有未来时刻就取最晚的一个。
 * 递归扫描的理由与额度中心一致：各家 usage 形状不同，但「剩余键」与「到期键」
 * 就那么几个名字在轮转，按字段存在性探测比按 25 家写死映射表稳。
 */
internal data class ExpiryInfo(
    val atMs: Long,
    val amount: Double?,
)

internal fun extractExpiry(usage: JSONObject): ExpiryInfo? {
    val candidates = ArrayList<Pair<Double, Long>>() // (剩余, 到期毫秒)
    scanQuota(usage, candidates, 0)
    if (candidates.isEmpty()) return null
    val now = System.currentTimeMillis()

    val withLeft = candidates.filter { it.first > 0.0 }
    val pool = if (withLeft.isNotEmpty()) withLeft else candidates
    val picked = pool.filter { it.second > now }.minByOrNull { it.second }
        ?: pool.maxByOrNull { it.second }
    return picked?.let { ExpiryInfo(it.second, it.first) }
}

private val EXPIRY_KEYS = listOf(
    "expireAt", "expiresAt", "expiredAt", "endAt", "endTime", "validEndTime",
    "periodEnd", "period_end", "endsAt", "expireTime",
)
private val LEFT_KEYS = listOf(
    "left", "remain", "remaining", "balance", "available", "credits", "points",
    "totalPoint", "totalLeft",
)

private fun scanQuota(
    node: Any?,
    out: MutableList<Pair<Double, Long>>,
    depth: Int,
) {
    if (depth > 6 || node == null) return
    when (node) {
        is JSONObject -> {
            val left = LEFT_KEYS.firstNotNullOfOrNull { k ->
                when (val raw = node.opt(k)) {
                    is Number -> raw.toDouble().takeIf { it.isFinite() }
                    is String -> raw.trim().toDoubleOrNull()?.takeIf { it.isFinite() }
                    else -> null
                }
            }
            val at = EXPIRY_KEYS.firstNotNullOfOrNull { k -> parseEpochMs(node.opt(k)) }
            if (left != null && at != null) out += left to at
            for (key in node.keys()) scanQuota(node.opt(key), out, depth + 1)
        }
        is org.json.JSONArray -> for (i in 0 until node.length()) {
            scanQuota(node.opt(i), out, depth + 1)
        }
    }
}

/**
 * 到期时间三种形态：毫秒数 / 秒数 / 可解析的时间串。
 * `< 1e11` 视为秒（Trae 的凭据从 auth 文件进来是 10 位秒，不归一会得到 1970）。
 */
internal fun parseEpochMs(value: Any?): Long? {
    val raw = when (value) {
        null, JSONObject.NULL -> return null
        is Number -> value.toLong()
        is String -> {
            val text = value.trim()
            if (text.isEmpty()) return null
            text.toLongOrNull() ?: runCatching {
                java.time.OffsetDateTime.parse(text).toInstant().toEpochMilli()
            }.getOrNull() ?: return null
        }
        else -> return null
    }
    if (raw <= 0) return null
    return if (raw < 100_000_000_000L) raw * 1000 else raw
}

/** 倍率展示：整数不带小数，其余去掉多余的 0。 */
internal fun fmtMult(v: Double): String =
    if (v % 1.0 == 0.0) String.format("%.0f", v)
    else String.format("%.2f", v).trimEnd('0').trimEnd('.')

/**
 * 凭证到期时刻（毫秒）。两个字段名都认：内置家里 `expiresAt`（workbuddy /
 * officeace / trae / zcode / qoder / accio / cline …）与 `tokenExpiresAt`
 * （raccoon / autoclaw / catpaw …）各占一半，取值口径一致。
 *
 * **秒与毫秒要归一**（照 3.0.8 的 `expiryMillis`）：落盘的到期值在两者之间漂过
 * —— Trae 的凭据从 CPA auth 文件或手工粘贴进来的是 10 位秒，不归一时
 * `1791009732` 当毫秒读就是 1970-01-21，账号一进面板就红着报「已过期」。
 *
 * 没有该字段（或值不可解析）→ null，界面不渲染这一行。
 */
internal fun credentialExpiryMs(acc: AccountDto): Long? {
    val raw = acc.raw.opt("expiresAt") ?: acc.raw.opt("tokenExpiresAt") ?: return null
    val value = when (raw) {
        is Number -> raw.toLong()
        is String -> raw.trim().toLongOrNull()
        else -> null
    } ?: return null
    if (value <= 0) return null
    return if (value < 100_000_000_000L) value * 1000 else value
}

/**
 * 凭证有效期一行：文案收短成「30 天后」，已过期标红。
 *
 * `internal` 而非 private：列表页与**详情页**两处账号卡都要用它 —— 同一个事实
 * 在两处必须长得一样，复制一份迟早会漂。
 */
@Composable
internal fun ExpiryLine(expiresAtMs: Long) {
    val C = agateColors()
    val left = expiresAtMs - System.currentTimeMillis()
    // 不足一天时给更细的刻度（小时 / 分钟），否则统一走 expiryPhrase
    val text = when {
        left <= 0 -> "已过期"
        left < 3_600_000L -> "${(left / 60_000L).coerceAtLeast(1)} 分钟后到期"
        left < 86_400_000L -> "${(left / 3_600_000L).coerceAtLeast(1)} 小时后到期"
        else -> expiryPhrase(left)
    }
    Row(
        Modifier.fillMaxWidth().padding(vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text("凭证有效期", color = C.textSub, fontSize = 12.sp, modifier = Modifier.weight(1f))
        Text(
            text,
            color = if (left <= 0) C.red else if (left < 3_600_000L) C.amber else C.textSub,
            fontSize = 12.sp,
        )
    }
    Box(Modifier.fillMaxWidth().height(1.dp).background(C.border))
}

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
