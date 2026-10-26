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
    var skipped by remember { mutableStateOf(0) }
    var busy by remember { mutableStateOf(false) }
    var hint by remember { mutableStateOf<String?>(null) }
    var lastAt by remember { mutableStateOf<String?>(null) }

    suspend fun refresh() {
        busy = true
        hint = null
        val data = AccountOps.usage()
        busy = false
        if (data == null) {
            hint = "查询失败：内核未响应"
            return
        }
        val out = HashMap<String, AcctMetric>()
        data.optJSONArray("results")?.let { arr ->
            for (i in 0 until arr.length()) {
                val row = arr.optJSONObject(i) ?: continue
                val id = row.optString("id")
                if (id.isBlank()) continue
                // ⚠️ error 为 JSON null 时不能用 optString（会得到字符串 "null"）
                val err = (row.opt("error") as? String)?.trim().orEmpty()
                out[id] = when {
                    err.isNotEmpty() -> AcctMetric(text = "查询失败：$err", failed = true)
                    row.optJSONObject("usage") == null -> AcctMetric(text = "未返回余额数据")
                    else -> parseUsageMetric(row.optJSONObject("usage"))
                }
            }
        }
        metrics = out
        skipped = data.optInt("skipped", 0)
        lastAt = android.text.format.DateFormat.format("HH:mm:ss", System.currentTimeMillis()).toString()
    }

    LaunchedEffect(Unit) {
        AccountOps.configure(GatewayPrefs.apiKey(ctx))
        refresh()
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
                    StatCell("未就绪", skipped.toString(), C.textSub, Modifier.weight(1f))
                }
                Spacer(Modifier.height(10.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        lastAt?.let { "上次刷新 $it" } ?: "尚未刷新",
                        color = C.textDim, fontSize = 11.sp, modifier = Modifier.weight(1f),
                    )
                    Button(
                        onClick = { if (!busy) scope.launch { refresh() } },
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
                        compareByDescending<AccountDto> { isLow(it, metrics[it.id]) }
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
                                Text(
                                    when {
                                        m == null -> "未查询"
                                        m.failed -> "查询失败"
                                        else -> m.text ?: "无额度数据"
                                    },
                                    color = when {
                                        m?.failed == true -> C.red
                                        m == null -> C.textDim
                                        else -> C.textSub
                                    },
                                    fontSize = 12.sp,
                                )
                            }
                            when {
                                m?.failed == true -> AgateBadge("失败", BadgeTone.RED)
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
