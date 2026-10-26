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
import androidx.compose.foundation.layout.navigationBarsPadding
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
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.PresetRegistry
import dev.agate.android.data.dto.AccountDto
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgatePageHeader
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
    var skipped by remember { mutableStateOf(0) }
    var busy by remember { mutableStateOf(false) }
    var hint by remember { mutableStateOf<String?>(null) }
    var lastAt by remember { mutableStateOf<String?>(null) }
    var filter by remember { mutableStateOf(QuotaFilter.ALL) }
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
                if (usage != null) extractExpiry(usage)?.let { exp[id] = it }
            }
        }
        metrics = out
        expiries = exp
        skipped = data.optInt("skipped", 0)
        lastAt = android.text.format.DateFormat.format("HH:mm", System.currentTimeMillis()).toString()
    }

    LaunchedEffect(Unit) {
        AccountOps.configure(GatewayPrefs.apiKey(ctx))
        refresh(force = false)
    }

    Column(Modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "额度中心", onOpenDrawer = onOpenDrawer) {
            if (busy) AgateBadge("刷新中", BadgeTone.AMBER)
        }
        Column(
            Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp)
                .navigationBarsPadding(),
        ) {
            Spacer(Modifier.height(10.dp))

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
                        metrics = metrics,
                        expiries = expiries,
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
    metrics: Map<String, AcctMetric>,
    expiries: Map<String, Expiry>,
    expanded: Boolean,
    onToggle: () -> Unit,
    onOpenProvider: () -> Unit,
) {
    val C = agateColors()
    val label = PresetRegistry.labelOf(provider)

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
            Box(
                Modifier.size(34.dp).background(C.insetBg, RoundedCornerShape(11.dp)),
                contentAlignment = Alignment.Center,
            ) {
                Text(label.take(2), color = C.textSub, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
            }
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
