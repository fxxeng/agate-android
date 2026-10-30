package dev.agate.android.ui.quota

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
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.AccountOps
import dev.agate.android.data.CustomProviderOps
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.PresetRegistry
import dev.agate.android.data.dto.AccountDto
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.AgateProviderAvatar
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.provider.AcctMetric
import dev.agate.android.ui.provider.parseUsageMetric
import dev.agate.android.ui.theme.agateColors
import dev.agate.core.GatewayPrefs
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject

/**
 * 额度中心（对齐 UI 设计基线 v44 · v18.1 形态）。
 *
 * 版式四段，与基线一致：
 * 1. **KPI 三卡**：账号数 / 即将到期（≤7 天）/ 查询失败；
 * 2. **分段筛选**：全部 / 即将到期 / 低余额 / 有失败；
 * 3. **每家一张可折叠卡**（`qcard`）：首字母块 + 家名 + 到期徽章 + 副行 +
 *    右侧主读数 + 水位条；点 summary 展开；
 * 4. **展开后是细项行**（`q-line`）：逐账号余额、到期、最近到期、入口。
 *
 * ── 与基线的一处有意差异 ──
 * 基线 KPI 里有「5h 窗口剩余」——那是 CatPaw / antigravity 这类窗口制独有的读数，
 * 各家口径不统一（有的给窗口剩余时长、有的只给百分比、有的压根没有窗口）。
 * 这里换成通用的「即将到期」，其余形状照抄。
 *
 * ── 数据口径 ──
 * 只调两个接口：`/api/accounts`（快照，给账号与归属）与 `/api/accounts/usage`
 * （批量余额）。后者会打到各家上游，所以进页面**只读缓存**，只有「刷新全部」
 * 才强制请求（见 `UsageCache`）。
 */
@Composable
fun QuotaCenterPage(
    onOpenDrawer: () -> Unit,
    onOpenProvider: (String) -> Unit,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val snap by GatewayRepository.snapshot.collectAsState()

    var metrics by remember { mutableStateOf<Map<String, AcctMetric>>(emptyMap()) }
    var expiries by remember { mutableStateOf<Map<String, Expiry>>(emptyMap()) }
    // 原始 usage JSON（诊断用）：解析不出数时，展开能看到内核到底回了什么
    var raws by remember { mutableStateOf<Map<String, String>>(emptyMap()) }
    // 逐份额度包：workbuddy 的 credits[]、trae/officeace 的 credits[]…
    // 汇总读数回答「还剩多少」，明细回答「哪几份、各剩多少、什么时候作废」
    var packs by remember { mutableStateOf<Map<String, List<QuotaPack>>>(emptyMap()) }
    var skipped by remember { mutableStateOf(0) }
    var busy by remember { mutableStateOf(false) }
    var hint by remember { mutableStateOf<String?>(null) }
    var lastAt by remember { mutableStateOf<String?>(null) }
    var filter by remember { mutableStateOf(QuotaFilter.ALL) }
    // ── 自定义提供商的真实名称 ──────────────────────────────────
    // 这一页拿到的 provider 是 `custom-xxxx` 这种 id，`PresetRegistry.labelOf`
    // 查不到就**原样回显 id** —— 截图里那句「custom-77605lb5fcd0」就是这么来的
    // （用户建的那家明明有名字）。与「提供商」页同一条口径：拿一次列表、建一个
    // id→name 的 map，展示名与图标反查都靠它。
    var customNames by remember { mutableStateOf<Map<String, String>>(emptyMap()) }
    /** 名字表这次取到了没有（null = 取到了；非 null = 失败原因，界面上要说出来）。 */
    var customNamesError by remember { mutableStateOf<String?>(null) }
    // 展开状态（基线用 details 折叠，默认收起）
    var expanded by remember { mutableStateOf<Set<String>>(emptySet()) }

    suspend fun refresh(force: Boolean = false) {
        busy = true
        hint = null
        val data = AccountOps.usage(force = force)
        busy = false
        if (data == null) {
            hint = "查询失败：内核未响应"
            return
        }
        val out = HashMap<String, AcctMetric>()
        val exp = HashMap<String, Expiry>()
        val raw = HashMap<String, String>()
        val pk = HashMap<String, List<QuotaPack>>()
        data.optJSONArray("results")?.let { arr ->
            for (i in 0 until arr.length()) {
                val row = arr.optJSONObject(i) ?: continue
                val id = row.optString("id")
                if (id.isBlank()) continue
                // ⚠️ error 为 JSON null 时不能用 optString（会得到字符串 "null"）
                val err = (row.opt("error") as? String)?.trim().orEmpty()
                val usage = row.optJSONObject("usage")
                out[id] = when {
                    err.isNotEmpty() -> AcctMetric(text = "查询失败：$err", failed = true)
                    usage == null -> AcctMetric(text = "未返回余额数据")
                    else -> parseUsageMetric(usage)
                }
                if (usage != null) {
                    extractExpiry(usage)?.let { exp[id] = it }
                    raw[id] = usage.toString()
                    parsePacks(usage).takeIf { it.isNotEmpty() }?.let { pk[id] = it }
                }
            }
        }
        metrics = out
        expiries = exp
        raws = raw
        packs = pk
        skipped = data.optInt("skipped", 0)
        lastAt = android.text.format.DateFormat.format("HH:mm", System.currentTimeMillis()).toString()
    }

    LaunchedEffect(Unit) {
        AccountOps.configure(GatewayPrefs.apiKey(ctx))
        refresh(force = false)
    }

    // 名字表只拉一次（自定义家的名称不是高频变动数据）。
    //
    // ⚠️ 失败与「确实没有」必须分开（2026-10-30 修）：旧写法失败时留空 map，
    // 而空 map 与「确实没有自定义家」在界面上完全一样 —— 表现是每个自定义家
    // 都显示成 `custom-xxxx`（id），图标也按名反查不到而变成两字母色块。
    // 一次临时失败被伪装成「数据就是这样」，用户既判断不出哪家坏了，也
    // 不知道重试一下就好。
    LaunchedEffect(Unit) {
        val result = CustomProviderOps.listResult()
        if (result.ok) {
            customNames = result.records
                .filter { it.name.isNotBlank() }
                .associate { it.id to it.name }
            customNamesError = null
        } else {
            // 保留上一份好数据（别把已经显示对的名字弄丢），只把原因带出去
            customNamesError = result.error
        }
    }

    Column(Modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "额度中心", onOpenDrawer = onOpenDrawer) {
            if (busy) AgateBadge("刷新中", BadgeTone.AMBER)
        }
        Column(
            Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))

            // 名字表取不到时的提示（与「提供商」页同一口径）：
            // 说清「为什么显示的是占位名」，而不是让用户对着 `custom-xxx`
            // 一串乱码猜哪里坏了。
            val namesError = customNamesError
            if (namesError != null) {
                Text(
                    "部分自定义提供商的名称暂时取不到（$namesError），列表里显示的是占位名",
                    color = C.amber,
                    fontSize = 11.sp,
                )
                Spacer(Modifier.height(8.dp))
            }

            val accounts = snap.accountList
            val byProvider = accounts.groupBy { it.provider }
            val expiringSoon = accounts.filter { (expiries[it.id]?.days ?: Long.MAX_VALUE) <= 7 }
            val failedOnes = accounts.filter { metrics[it.id]?.failed == true }

            // ── 1) KPI 三卡 ──
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                KpiCell("账号", accounts.size.toString(), C.textMain, Modifier.weight(1f))
                KpiCell(
                    "即将到期",
                    expiringSoon.size.toString(),
                    if (expiringSoon.isEmpty()) C.textMain else C.amber,
                    Modifier.weight(1f),
                )
                KpiCell(
                    "查询失败",
                    failedOnes.size.toString(),
                    if (failedOnes.isEmpty()) C.textMain else C.red,
                    Modifier.weight(1f),
                )
            }
            Spacer(Modifier.height(10.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    buildString {
                        append(lastAt?.let { "缓存于 $it" } ?: "尚未查询")
                        if (skipped > 0) append(" · $skipped 个未就绪")
                    },
                    color = C.textDim, fontSize = 11.sp, modifier = Modifier.weight(1f),
                )
                SmallPill(
                    text = if (busy) "查询中…" else "刷新全部",
                    enabled = !busy,
                    onClick = { if (!busy) scope.launch { refresh(force = true) } },
                )
            }
            hint?.let {
                Spacer(Modifier.height(6.dp))
                Text(it, color = C.red, fontSize = 12.sp)
            }

            // ── 2) 分段筛选 ──
            Spacer(Modifier.height(10.dp))
            Row(
                Modifier
                    .fillMaxWidth()
                    .background(C.insetBg, RoundedCornerShape(12.dp))
                    .padding(3.dp),
                horizontalArrangement = Arrangement.spacedBy(3.dp),
            ) {
                QuotaFilter.entries.forEach { item ->
                    val active = filter == item
                    Box(
                        Modifier
                            .weight(1f)
                            .background(if (active) C.cardBg else Color.Transparent, RoundedCornerShape(10.dp))
                            .clickable { filter = item }
                            .padding(vertical = 7.dp),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text(
                            item.label,
                            color = if (active) C.textMain else C.textSub,
                            fontSize = 12.sp,
                            fontWeight = if (active) FontWeight.SemiBold else FontWeight.Normal,
                        )
                    }
                }
            }

            if (accounts.isEmpty()) {
                Spacer(Modifier.height(14.dp))
                Text("还没有账号。先到「提供商」页添加，这里会自动列出余额。", color = C.textSub, fontSize = 12.sp)
                Spacer(Modifier.height(20.dp))
                return@Column
            }

            // ── 3) 每家一张折叠卡 ──
            Spacer(Modifier.height(10.dp))
            byProvider.entries
                .sortedWith(
                    compareBy(
                        { entry -> entry.value.minOfOrNull { expiries[it.id]?.days ?: Long.MAX_VALUE } ?: Long.MAX_VALUE },
                        { entry -> -entry.value.size },
                    ),
                )
                .forEach { (provider, list) ->
                    val visible = list.filter { acc ->
                        when (filter) {
                            QuotaFilter.ALL -> true
                            QuotaFilter.EXPIRING -> (expiries[acc.id]?.days ?: Long.MAX_VALUE) <= 7
                            QuotaFilter.LOW -> isLow(acc, metrics[acc.id])
                            QuotaFilter.FAILED -> metrics[acc.id]?.failed == true
                        }
                    }
                    if (visible.isEmpty()) return@forEach
                    ProviderQuotaCard(
                        provider = provider,
                        accounts = visible,
                        customNames = customNames,
                        metrics = metrics,
                        expiries = expiries,
                        raws = raws,
                        packs = packs,
                        expanded = provider in expanded,
                        onToggle = {
                            expanded = if (provider in expanded) expanded - provider else expanded + provider
                        },
                        onOpenProvider = { onOpenProvider(provider) },
                    )
                    Spacer(Modifier.height(8.dp))
                }
            Spacer(Modifier.height(20.dp))
        }
    }
}

private enum class QuotaFilter(val label: String) {
    ALL("全部"),
    EXPIRING("即将到期"),
    LOW("低余额"),
    FAILED("有失败"),
}

/** 一家一张卡：summary 行 + 展开后的细项行（基线 `qcard` / `q-line`）。 */
@Composable
private fun ProviderQuotaCard(
    provider: String,
    accounts: List<AccountDto>,
    customNames: Map<String, String>,
    metrics: Map<String, AcctMetric>,
    expiries: Map<String, Expiry>,
    raws: Map<String, String>,
    packs: Map<String, List<QuotaPack>>,
    expanded: Boolean,
    onToggle: () -> Unit,
    onOpenProvider: () -> Unit,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    // 先查自定义家的真名，再回落内置注册表 —— 顺序与「提供商」页一致。
    //
    // ⚠️ 自定义家取不到名字时**不拿 id 冒充**：那会让用户以为自家这家就叫
    // `custom-77605lb5fcd0`。给一个明确的占位名（页头有提示说明原因）。
    val label = customNames[provider]
        ?: if (CustomProviderOps.isCustomProviderId(provider)) {
            "（名称未取到）"
        } else {
            PresetRegistry.labelOf(provider)
        }

    // 家级读数：优先「最早到期」的账号，失败项排前
    val ranked = accounts.sortedWith(
        compareBy<AccountDto> { expiries[it.id]?.days ?: Long.MAX_VALUE }
            .thenByDescending { metrics[it.id]?.failed == true },
    )
    val head = ranked.firstOrNull()
    val headMetric = head?.let { metrics[it.id] }
    val headExpiry = head?.let { expiries[it.id] }
    val lowCount = accounts.count { isLow(it, metrics[it.id]) }
    val failCount = accounts.count { metrics[it.id]?.failed == true }
    val mainValue = when {
        headMetric?.failed == true -> "失败"
        headMetric?.pct != null -> "${headMetric.pct}%"
        headMetric?.text != null -> headMetric.text!!.substringBefore(" · ")
        else -> "—"
    }
    val tone = when {
        failCount > 0 -> BadgeTone.RED
        headExpiry != null && headExpiry.days <= 3 -> BadgeTone.RED
        lowCount > 0 || (headExpiry != null && headExpiry.days <= 7) -> BadgeTone.AMBER
        else -> BadgeTone.GREEN
    }

    Column(
        Modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(18.dp))
            .border(
                1.dp,
                if (tone == BadgeTone.RED) C.red.copy(alpha = 0.45f) else C.border,
                RoundedCornerShape(18.dp),
            ),
    ) {
        // ── summary ──
        Row(
            Modifier.fillMaxWidth().clickable { onToggle() }.padding(14.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            // 官方 logo（内置家走注册表，自定义家按名字反查预置模板），
            // 都查不到才回落两字母色块 —— 与「提供商」页同一口径。
            // 之前这里手写了一个 `Text(label.take(2))`，于是每一家都只有字母、
            // 连 WorkBuddy / CodeArts 这些有 logo 的也一样（用户反馈的问题）。
            AgateProviderAvatar(
                providerId = provider,
                label = label,
                size = 34.dp,
                corner = 11.dp,
                customName = customNames[provider].orEmpty(),
            )
            Spacer(Modifier.size(10.dp))
            Column(Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(label, color = C.textMain, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
                    headExpiry?.let {
                        Spacer(Modifier.size(6.dp))
                        AgateBadge(if (it.days <= 0) "已过期" else "${it.days}天", tone)
                    }
                }
                Spacer(Modifier.height(2.dp))
                Text(
                    buildString {
                        append("${accounts.size} 个账号")
                        when {
                            failCount > 0 -> append(" · $failCount 个失败")
                            lowCount > 0 -> append(" · $lowCount 个低余额")
                            headExpiry != null -> append(" · ${headExpiry.describe()}")
                        }
                    },
                    color = C.textSub, fontSize = 12.sp,
                )
            }
            Text(
                mainValue,
                color = when {
                    headMetric?.failed == true -> C.red
                    tone == BadgeTone.AMBER -> C.amber
                    else -> C.accent
                },
                fontSize = 15.sp, fontWeight = FontWeight.Bold,
            )
            Spacer(Modifier.size(6.dp))
            Text(if (expanded) "⌄" else "›", color = C.textDim, fontSize = 13.sp)
        }

        // ── 水位条（有百分比才画）──
        headMetric?.pct?.let { pct ->
            Box(
                Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 14.dp)
                    .height(4.dp)
                    .background(C.insetBg, RoundedCornerShape(2.dp)),
            ) {
                Box(
                    Modifier
                        .fillMaxWidth(pct / 100f)
                        .height(4.dp)
                        .background(
                            if (pct <= 20) C.red else if (pct <= 40) C.amber else C.accent,
                            RoundedCornerShape(2.dp),
                        ),
                )
            }
            Spacer(Modifier.height(10.dp))
        }

        // ── 展开：逐账号细项 ──
        if (expanded) {
            Column(Modifier.fillMaxWidth().padding(horizontal = 14.dp).padding(bottom = 12.dp)) {
                Box(Modifier.fillMaxWidth().height(1.dp).background(C.border))
                accounts.forEach { acc ->
                    val m = metrics[acc.id]
                    QuotaLine(
                        left = acc.displayName,
                        right = when {
                            m == null -> "未查询"
                            m.failed -> "查询失败"
                            else -> m.text ?: "无额度数据"
                        },
                        valueColor = if (m?.failed == true) C.red else C.textMain,
                    )
                    expiries[acc.id]?.let { e ->
                        QuotaLine(left = "到期", right = e.describe(), valueColor = C.textMain)
                    }
                }
                QuotaLine(
                    left = "最近到期",
                    right = headExpiry?.describe() ?: "该家未返回",
                    valueColor = C.textMain,
                )

                // ── 额度包明细（只列「还能花」的）──
                // 已用完 / 已过期的包**不显示**：上游发得勤时一列几十份、大半是花完
                // 的，混排的结果是「打开先看到一堆没用的」，列表也会长到读不完。
                // 口径与 3.0.8 的 `CreditRow.spent` 一致：剩余读到 0、或（剩余读不到
                // 但）已用 ≥ 总量、或已过期，都算花不出去；两者都读不到时**不猜**
                // ——宁留在列表里让人看见，也不凭空折叠。
                head?.let { acc ->
                    val live = packs[acc.id].orEmpty().filterNot { it.spent() }
                    if (live.isNotEmpty()) {
                        Spacer(Modifier.height(6.dp))
                        Text("可用额度包 ${live.size} 份", color = C.textDim, fontSize = 11.sp)
                        Spacer(Modifier.height(2.dp))
                        // 顺序照后端给的：各家都在后端按自己的口径排好了
                        // （Trae 最近到期在前、WorkBuddy 套餐优先级 + 到期升序）。
                        // 前端再排一次只会让两处口径分叉。
                        live.forEach { pack ->
                            PackRow(pack)
                        }
                    }
                }
                // 诊断行：解析不出数时（界面显示 —）照着它就能定位是上游形状不认识
                // 还是那次请求确实没返回读数。取第一个账号的原始 JSON。
                head?.let { acc ->
                    raws[acc.id]?.let { raw ->
                        Column(Modifier.fillMaxWidth().padding(vertical = 6.dp)) {
                            Text("原始读数（诊断）", color = C.textDim, fontSize = 11.sp)
                            Spacer(Modifier.height(2.dp))
                            Text(
                                raw.take(400),
                                color = C.textSub,
                                fontSize = 10.sp,
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .background(C.insetBg, RoundedCornerShape(8.dp))
                                    .clickable {
                                        val clipboard = ctx.getSystemService(android.content.Context.CLIPBOARD_SERVICE)
                                            as android.content.ClipboardManager
                                        clipboard.setPrimaryClip(
                                            android.content.ClipData.newPlainText("usage", raw),
                                        )
                                    }
                                    .padding(8.dp),
                            )
                        }
                    }
                }
                Row(
                    Modifier.fillMaxWidth().clickable { onOpenProvider() }.padding(vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text("查看该家详情", color = C.accent, fontSize = 12.sp, modifier = Modifier.weight(1f))
                    Text("›", color = C.accent, fontSize = 13.sp)
                }
            }
        }
    }
}

@Composable
private fun QuotaLine(left: String, right: String, valueColor: Color) {
    val C = agateColors()
    Row(
        Modifier.fillMaxWidth().padding(vertical = 7.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(left, color = C.textSub, fontSize = 12.sp, modifier = Modifier.weight(1f))
        Text(right, color = valueColor, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
    }
    Box(Modifier.fillMaxWidth().height(1.dp).background(C.border))
}

@Composable
private fun KpiCell(label: String, value: String, color: Color, modifier: Modifier) {
    val C = agateColors()
    Column(
        modifier
            .background(C.insetBg, RoundedCornerShape(14.dp))
            .padding(vertical = 12.dp, horizontal = 10.dp),
    ) {
        Text(value, color = color, fontSize = 20.sp, fontWeight = FontWeight.Bold)
        Text(label, color = C.textDim, fontSize = 11.sp)
    }
}

@Composable
private fun SmallPill(text: String, enabled: Boolean, onClick: () -> Unit) {
    val C = agateColors()
    Text(
        text,
        color = if (enabled) C.onAccent else C.textDim,
        fontSize = 12.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier
            .background(if (enabled) C.accent else C.insetBg, RoundedCornerShape(999.dp))
            .clickable(enabled = enabled) { onClick() }
            .padding(horizontal = 12.dp, vertical = 7.dp),
    )
}

/**
 * 数字格式化：与 `ui.provider.ProviderPage` 的同名函数**同一口径**（那边是
 * private，跨模块拿不到；两边都只做「千分位 / 去尾零」这一件事，改一处要改两处）。
 */
private fun fmtNum(v: Double): String =
    if (v >= 1000) String.format("%,.0f", v)
    else String.format("%.2f", v).trimEnd('0').trimEnd('.')

/** 一份额度包（明细行）。字段全部来自上游，缺失不补 0。 */
internal data class QuotaPack(
    val name: String,
    val remain: Double?,
    val total: Double?,
    val used: Double?,
    /** 这一行自己的单位（与主读数不同单位时才由后端带上）。 */
    val unit: String?,
    val grantAt: Long?,
    val startAt: Long?,
    val expireAt: Long?,
    /** 每日重置的包：`expireAt` 对它是「今天 24 点」，不是作废。 */
    val daily: Boolean,
    /** 上游给的「不限量」标记（写成 -1 那种哨兵值会被读成欠费）。 */
    val unlimited: Boolean,
    /** 发放原因（WorkBuddy 的官方活动 / 拉新奖励…）。 */
    val grantReason: String?,
) {
    /**
     * 「这份包还花不花得出去」。口径照 3.0.8 的 `CreditRow.spent`：
     *   - 剩余读到 0 → 用完；
     *   - 剩余读不到、但已用 ≥ 总量 → 也算用完（上游只给 used/total 那一档
     *     同样答得出这个问题，不该因为缺 `remain` 就漏判）；
     *   - 到期时刻已过 → 过期（**日包除外**：它的 expireAt 是今天 24 点，
     *     含义是「重置回满」而不是作废）；
     *   - 两者都读不到 → **不猜**（返回 false，留在列表里让人看见）。
     */
    fun spent(now: Long = System.currentTimeMillis()): Boolean {
        val exhausted = (remain != null && remain <= 0.0) ||
            (remain == null && used != null && total != null && used >= total)
        val expired = expireAt != null && expireAt > 0 && expireAt <= now
        return !unlimited && !daily && (exhausted || expired)
    }

    /** 剩余比例（有真总量才算；画不出比例就不装作有）。 */
    val percent: Int?
        get() = if (total != null && total > 0 && remain != null) {
            ((remain / total) * 100).toInt().coerceIn(0, 100)
        } else {
            null
        }
}

/**
 * 从 usage 里取额度包明细。
 *
 * 两个来源，形状不同但语义一致：
 *   - `credits[]`（workbuddy / trae / officeace / zcode）：`{name, remain|left, total, used, expireAt}`；
 *   - `wallets[]`（归一化形状）：`{type|displayName, balance, quota, used}`，没有 credits 时才用。
 * 都没有就返回空 —— 界面不显示这一节（不是错误）。
 */
private fun parsePacks(usage: JSONObject): List<QuotaPack> {
    fun num(o: JSONObject, vararg keys: String): Double? = keys.firstNotNullOfOrNull { k ->
        when (val raw = o.opt(k)) {
            is Number -> raw.toDouble().takeIf { it.isFinite() }
            is String -> raw.trim().toDoubleOrNull()?.takeIf { it.isFinite() }
            else -> null
        }
    }

    fun row(o: JSONObject, index: Int): QuotaPack = QuotaPack(
        name = o.optString("name").trim().ifBlank { "额度包 ${index + 1}" },
        remain = num(o, "remain", "left", "balance", "available"),
        total = num(o, "total", "quota", "granted"),
        used = num(o, "used", "usage", "consumed"),
        unit = o.optString("unit").trim().ifBlank { null },
        grantAt = parseEpochMs(o.opt("grantAt")),
        startAt = parseEpochMs(o.opt("startAt")),
        expireAt = parseEpochMs(o.opt("expireAt")),
        daily = o.optBoolean("daily", false),
        // 后端只在「不限量」时给这个键（true）；缺键按 false，不猜
        unlimited = (o.opt("unlimited") as? Boolean) == true,
        grantReason = o.optString("grantReason").trim().ifBlank { null },
    )

    // 优先 `credits[]`（12 家产出：workbuddy / trae / officeace / zcode / qoder /
    // autoclaw / catpaw / codearts / loomy / raccoon / chatglm …）
    usage.optJSONArray("credits")?.let { arr ->
        val out = ArrayList<QuotaPack>(arr.length())
        for (i in 0 until arr.length()) {
            arr.optJSONObject(i)?.let { out += row(it, i) }
        }
        if (out.isNotEmpty()) return out
    }
    // 没有 credits 才退 wallets（归一化形状）
    usage.optJSONArray("wallets")?.let { arr ->
        val out = ArrayList<QuotaPack>(arr.length())
        for (i in 0 until arr.length()) {
            val o = arr.optJSONObject(i) ?: continue
            out += QuotaPack(
                name = o.optString("displayName").ifBlank { o.optString("type").ifBlank { "钱包 ${i + 1}" } },
                remain = num(o, "balance", "remain", "left"),
                total = num(o, "quota", "total"),
                used = num(o, "used"),
                unit = o.optString("unit").trim().ifBlank { null },
                grantAt = null,
                startAt = null,
                expireAt = parseEpochMs(o.opt("expireAt")),
                daily = false,
                unlimited = false,
                grantReason = null,
            )
        }
        return out
    }
    return emptyList()
}

/**
 * 额度包明细行（契约与 3.0.8 的 `CreditRow` 对齐）。
 *
 * 版式：左边名称，右边读数「剩 x / y 单位」，下面一行小字是「领取 → 到期」这条
 * 时间线。三点是刻意的：
 *   - **单位贴在整串末尾**（「剩 400 / 500 额度」），不夹在中间 —— 中间那版读起来
 *     像「剩 400 额度 / 共 500 额度」的省略写法，数字与单位的归属全靠猜；
 *   - 左边那一半**名字跟着来源变**：`grantAt`（上游发放时刻）才叫「领取」，
 *     只有 `startAt`（抵扣开始）时写「生效」—— 缺了发放时刻还标「领取」等于替
 *     上游编一个到手时间；
 *   - 日包的「到期」写「每日重置」：它的 expireAt 是今天 24 点，照「到期」写会让
 *     人以为额度明天就没了（实际是重置回满）。
 */
@Composable
private fun PackRow(pack: QuotaPack) {
    val C = agateColors()
    val now = System.currentTimeMillis()
    val expired = pack.expireAt != null && pack.expireAt > 0 && pack.expireAt <= now

    // ── 读数 ──
    val amount = when {
        pack.unlimited -> "不限"
        pack.remain != null && pack.total != null && pack.total > 0 ->
            "剩 ${fmtNum(pack.remain)} / ${fmtNum(pack.total)}"
        pack.remain != null -> "剩 ${fmtNum(pack.remain)}"
        pack.used != null -> "已用 ${fmtNum(pack.used)}"
        else -> "—"
    }
    val unitSuffix = if (!pack.unlimited && pack.unit != null && amount != "—") " ${pack.unit}" else ""

    // ── 时间线：领取/生效 → 到期 ──
    val timeline = buildString {
        when {
            pack.grantAt != null && pack.grantAt > 0 -> append("${shortTime(pack.grantAt)} 领取")
            pack.startAt != null && pack.startAt > 0 -> append("${shortTime(pack.startAt)} 生效")
        }
        when {
            pack.daily -> {
                if (isNotEmpty()) append(" · ")
                append("每日重置")
            }
            pack.expireAt != null && pack.expireAt > 0 -> {
                if (isNotEmpty()) append(" · ")
                append(if (expired) "已过期" else "${shortTime(pack.expireAt)} 到期")
            }
        }
        pack.grantReason?.let {
            if (isNotEmpty()) append(" · ")
            append(it)
        }
    }

    Column(Modifier.fillMaxWidth().padding(vertical = 6.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                pack.name,
                color = C.textSub,
                fontSize = 11.sp,
                modifier = Modifier.weight(1f),
                maxLines = 1,
                overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
            )
            Text(
                amount + unitSuffix,
                color = if (expired) C.textDim else C.textMain,
                fontSize = 11.sp,
                fontWeight = FontWeight.SemiBold,
            )
        }
        if (timeline.isNotEmpty()) {
            Spacer(Modifier.height(1.dp))
            Text(
                timeline,
                color = when {
                    expired -> C.textDim
                    pack.expireAt != null && (pack.expireAt - now) <= 3L * 86_400_000L -> C.red
                    pack.expireAt != null && (pack.expireAt - now) <= 7L * 86_400_000L -> C.amber
                    else -> C.textDim
                },
                fontSize = 10.sp,
                maxLines = 1,
                overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
            )
        }
        // 剩余比例条：有真总量才画（画不出比例就不装作有）
        pack.percent?.let { pct ->
            Spacer(Modifier.height(3.dp))
            Box(
                Modifier.fillMaxWidth().height(2.dp).background(C.insetBg, RoundedCornerShape(1.dp)),
            ) {
                Box(
                    Modifier
                        .fillMaxWidth(pct / 100f)
                        .height(2.dp)
                        .background(
                            if (pct <= 20) C.red else if (pct <= 40) C.amber else C.accent,
                            RoundedCornerShape(1.dp),
                        ),
                )
            }
        }
    }
    Box(Modifier.fillMaxWidth().height(1.dp).background(C.border))
}

/** `MM-DD HH:mm`：弹层要一眼扫完，年份是噪音（完整时刻在别处）。 */
private fun shortTime(atMs: Long): String =
    java.time.Instant.ofEpochMilli(atMs)
        .atZone(java.time.ZoneId.systemDefault())
        .let { "%02d-%02d %02d:%02d".format(it.monthValue, it.dayOfMonth, it.hour, it.minute) }

// ── 到期解析（各家余额构成不同，统一走通用扫描）──────────────────
//
// 各家的 usage 形状差异很大，但有两个共同点可抓：
//   1. 「还剩多少」的键名在少数几个里轮转（left/remain/remaining/balance/
//      available/credits/points/totalPoint）；
//   2. 「什么时候作废」也一样（expireAt/expiresAt/expiredAt/endAt/endTime/
//      validEndTime/periodEnd/endsAt），单位有毫秒也有秒。
// 所以这里不按家写死，而是**递归扫一遍**：对象上同时能读到「剩余」和「到期」
// 就记成一个候选桶，最后取**最近一个未来到期**的桶作为展示口径。
private val EXPIRY_KEYS = listOf(
    "expireAt", "expiresAt", "expiredAt", "endAt", "endTime", "validEndTime",
    "periodEnd", "period_end", "endsAt", "expireTime",
)
private val LEFT_KEYS = listOf(
    "left", "remain", "remaining", "balance", "available", "credits", "points", "totalPoint",
)

/** 一个可展示的到期信息（`days` 为剩余天数，负数表示已过期）。 */
internal data class Expiry(val atMs: Long, val days: Long) {
    fun describe(): String = when {
        days < 0 -> "已过期"
        days == 0L -> "今天到期"
        days <= 30 -> "$days 天后到期"
        else -> java.time.Instant.ofEpochMilli(atMs)
            .atZone(java.time.ZoneId.systemDefault())
            .toLocalDate()
            .let { "${it.monthValue}月${it.dayOfMonth}日到期" }
    }
}

/** 从 usage 里挑出**最近一个未来到期**的额度桶；没有到期信息返回 null。 */
private fun extractExpiry(usage: JSONObject): Expiry? {
    val candidates = ArrayList<Pair<Double, Long>>() // (剩余, 到期毫秒)
    scanQuota(usage, candidates, 0)
    if (candidates.isEmpty()) return null
    val now = System.currentTimeMillis()
    val hasLeft = candidates.filter { it.first > 0.0 }
    val pool = if (hasLeft.isNotEmpty()) hasLeft else candidates
    val pick = pool.filter { it.second > now }.minByOrNull { it.second }
        ?: pool.maxByOrNull { it.second } ?: return null
    return Expiry(atMs = pick.second, days = (pick.second - now) / 86_400_000L)
}

private fun scanQuota(node: Any?, out: MutableList<Pair<Double, Long>>, depth: Int) {
    if (depth > 6 || node == null) return
    when (node) {
        is JSONObject -> {
            val left = LEFT_KEYS.firstNotNullOfOrNull { key ->
                if (node.has(key)) node.optDouble(key, Double.NaN).takeIf { !it.isNaN() } else null
            }
            val at = EXPIRY_KEYS.firstNotNullOfOrNull { key -> parseEpochMs(node.opt(key)) }
            if (left != null && at != null) out += left to at
            for (key in node.keys()) scanQuota(node.opt(key), out, depth + 1)
        }
        is JSONArray -> for (i in 0 until node.length()) scanQuota(node.opt(i), out, depth + 1)
    }
}

/** 到期时间三种形态：毫秒数 / 秒数 / 可解析的时间串。 */
private fun parseEpochMs(value: Any?): Long? {
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
    // 秒级（10 位）与毫秒级（13 位）混用：小于 1e11 视为秒
    return if (raw < 100_000_000_000L) raw * 1000 else raw
}

/** 低余额判据：内核标记优先，其次按比例（≤20%）。 */
private fun isLow(acc: AccountDto, metric: AcctMetric?): Boolean {
    if (acc.lowBalance) return true
    val pct = metric?.pct ?: return false
    return pct <= 20
}
