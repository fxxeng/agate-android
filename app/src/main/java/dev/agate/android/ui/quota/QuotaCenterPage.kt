package dev.agate.android.ui.quota

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.AccountOps
import dev.agate.core.GatewayPrefs
import org.json.JSONArray
import org.json.JSONObject
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.PresetRegistry
import dev.agate.android.data.dto.AccountDto
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.provider.AcctMetric
import dev.agate.android.ui.provider.parseUsageMetric
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch

/**
 * 额度中心（M4）：跨提供商的余额总览。
 *
 * 数据面只有两个接口：
 * - `GET /api/accounts`（经 `GatewayRepository` 快照）给账号名与归属；
 * - `GET /api/accounts/usage` 批量查余额，按账号 id 回填（`{results, skipped}`）。
 *
 * 口径说明（与内核一致，见 `core/usage_query.rs`）：
 * - 批量目标是**全部可用账号，不看 enabled** —— 禁用只表示「转发别用它」，
 *   余额照样要能查；
 * - `skipped` 是**数量**（凭证不完整的账号个数），不是 id 列表；
 * - 单账号的 `error` 可能是 JSON null，必须 `opt()` + 类型判断（`optString`
 *   会把 null 变成 "null"，真机上表现为「查询失败：null」）。
 *
 * 这里不复用 ProviderPage 的列表，是因为两者的组织维度不同：那边按 provider
 * 分组看「配置」，这边按**水位**看「谁快没了」——低余额与失败排在最前。
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
    // 账号 → 最近到期信息（各家字段不同，故用通用扫描，见 extractExpiry）
    var expiries by remember { mutableStateOf<Map<String, Expiry>>(emptyMap()) }
    var skipped by remember { mutableStateOf(0) }
    var busy by remember { mutableStateOf(false) }
    var hint by remember { mutableStateOf<String?>(null) }
    var lastAt by remember { mutableStateOf<String?>(null) }

    suspend fun refresh(force: Boolean = false) {
        busy = true
        hint = null
        // 进入页面走缓存（force=false），只有「刷新全部」才真的打上游
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
        lastAt = android.text.format.DateFormat.format("HH:mm:ss", System.currentTimeMillis()).toString()
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
            val lowOnes = accounts.filter { isLow(it, metrics[it.id]) }
            val failedOnes = accounts.filter { metrics[it.id]?.failed == true }
            val expiringSoon = accounts.filter { (expiries[it.id]?.days ?: Long.MAX_VALUE) <= 7 }

            // ── 概览 ──
            Column(
                Modifier
                    .fillMaxWidth()
                    .background(C.cardBg, RoundedCornerShape(20.dp))
                    .border(1.dp, C.border, RoundedCornerShape(20.dp))
                    .padding(14.dp),
            ) {
                Text("概览", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.height(10.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    StatCell("账号", accounts.size.toString(), C.textMain, Modifier.weight(1f))
                    StatCell("低余额", lowOnes.size.toString(), if (lowOnes.isEmpty()) C.textMain else C.red, Modifier.weight(1f))
                    StatCell("查询失败", failedOnes.size.toString(), if (failedOnes.isEmpty()) C.textMain else C.amber, Modifier.weight(1f))
                    StatCell("7 天内到期", expiringSoon.size.toString(), if (expiringSoon.isEmpty()) C.textMain else C.amber, Modifier.weight(1f))
                }
                Spacer(Modifier.height(10.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        buildString {
                            append(lastAt?.let { "上次刷新 $it" } ?: "尚未刷新")
                            if (skipped > 0) append(" · $skipped 个未就绪")
                        },
                        color = C.textDim, fontSize = 11.sp, modifier = Modifier.weight(1f),
                    )
                    Button(
                        onClick = { if (!busy) scope.launch { refresh(force = true) } },
                        enabled = !busy,
                        colors = ButtonDefaults.buttonColors(containerColor = C.accent, contentColor = C.onAccent),
                    ) { Text(if (busy) "查询中…" else "刷新全部", fontWeight = FontWeight.SemiBold) }
                }
                hint?.let {
                    Spacer(Modifier.height(8.dp))
                    Text(it, color = C.red, fontSize = 12.sp)
                }
            }

            if (accounts.isEmpty()) {
                Spacer(Modifier.height(14.dp))
                Text("还没有账号。先到「提供商」页添加，这里会自动列出余额。", color = C.textSub, fontSize = 12.sp)
                Spacer(Modifier.height(20.dp))
                return@Column
            }

            // ── 分组列表：低余额 / 失败优先 ──
            val grouped = accounts.groupBy { it.provider }
            grouped.entries
                .sortedWith(
                    compareBy(
                        { entry -> entry.value.minOfOrNull { expiries[it.id]?.days ?: Long.MAX_VALUE } ?: Long.MAX_VALUE },
                        { entry -> entry.value.none { isLow(it, metrics[it.id]) } },
                        { entry -> -entry.value.size },
                    ),
                )
                .forEach { (provider, list) ->
                    Spacer(Modifier.height(12.dp))
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            PresetRegistry.labelOf(provider),
                            color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold,
                            modifier = Modifier.weight(1f),
                        )
                        Text(
                            "${list.size} 个账号",
                            color = C.textDim, fontSize = 11.sp,
                            modifier = Modifier
                                .clickable { onOpenProvider(provider) }
                                .padding(4.dp),
                        )
                    }
                    Spacer(Modifier.height(6.dp))
                    list.sortedWith(
                        compareBy<AccountDto> { expiries[it.id]?.days ?: Long.MAX_VALUE }
                            .thenByDescending { isLow(it, metrics[it.id]) }
                            .thenByDescending { metrics[it.id]?.failed == true },
                    ).forEach { acc ->
                        val m = metrics[acc.id]
                        Row(
                            Modifier
                                .fillMaxWidth()
                                .padding(bottom = 8.dp)
                                .background(C.cardBg, RoundedCornerShape(16.dp))
                                .border(
                                    1.dp,
                                    if (isLow(acc, m)) C.red.copy(alpha = 0.45f) else C.border,
                                    RoundedCornerShape(16.dp),
                                )
                                .clickable { onOpenProvider(acc.provider) }
                                .padding(horizontal = 14.dp, vertical = 12.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(Modifier.weight(1f)) {
                                Text(acc.displayName, color = C.textMain, fontSize = 14.sp)
                                val expiry = expiries[acc.id]
                                Text(
                                    buildString {
                                        append(
                                            when {
                                                m == null -> "未查询"
                                                m.failed -> "查询失败"
                                                else -> m.text ?: "无额度数据"
                                            },
                                        )
                                        if (expiry != null) {
                                            append(" · ")
                                            append(expiry.describe())
                                        }
                                    },
                                    color = when {
                                        m?.failed == true -> C.red
                                        expiry != null && expiry.days <= 3 -> C.red
                                        m == null -> C.textDim
                                        else -> C.textSub
                                    },
                                    fontSize = 12.sp,
                                )
                            }
                            when {
                                m?.failed == true -> AgateBadge("失败", BadgeTone.RED)
                                expiries[acc.id]?.let { it.days <= 3 } == true ->
                                    AgateBadge("${expiries[acc.id]!!.days}天到期", BadgeTone.RED)
                                expiries[acc.id]?.let { it.days <= 7 } == true ->
                                    AgateBadge("${expiries[acc.id]!!.days}天到期", BadgeTone.AMBER)
                                isLow(acc, m) -> AgateBadge("低余额", BadgeTone.RED)
                                m?.pct != null -> AgateBadge("${m.pct}%", BadgeTone.GREEN)
                                !acc.enabled -> AgateBadge("已禁用", BadgeTone.GRAY)
                                else -> AgateBadge("正常", BadgeTone.GREEN)
                            }
                        }
                    }
                }
            Spacer(Modifier.height(24.dp))
        }
    }
}

// ── 到期解析（各家余额构成不同，统一走通用扫描）──────────────────
//
// 各家的 usage 形状差异很大，但有两个共同点可抓：
//   1. 「还剩多少」的键名在少数几个里轮转（left/remain/remaining/balance/
//      available/credits/points/totalPoint）；
//   2. 「什么时候作废」也一样（expireAt/expiresAt/expiredAt/endAt/endTime/
//      validEndTime/periodEnd/endsAt），单位有毫秒也有秒。
// 所以这里不按家写死，而是**递归扫一遍**：对象上同时能读到「剩余」和
// 「到期」就记成一个候选桶，最后取**最近一个未来到期**的桶作为展示口径。
//
// 为什么优先未来：已经过期但还剩数的桶是历史残留（上游常在过期后一段时间
// 才清零），拿它当「最近到期」会显示成「已过期」，让用户误以为整家作废。
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
    val days = (pick.second - now) / 86_400_000L
    return Expiry(atMs = pick.second, days = days)
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

@Composable
private fun StatCell(label: String, value: String, color: androidx.compose.ui.graphics.Color, modifier: Modifier) {
    Column(modifier) {
        Text(value, color = color, fontSize = 20.sp, fontWeight = FontWeight.Bold)
        Text(label, color = agateColors().textDim, fontSize = 11.sp)
    }
}
