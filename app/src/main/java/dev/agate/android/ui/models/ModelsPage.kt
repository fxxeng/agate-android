package dev.agate.android.ui.models

import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
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
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import android.widget.Toast
import kotlinx.coroutines.launch
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.ModelOps
import dev.agate.android.data.PresetRegistry
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateProviderAvatar
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.components.AgateAvatar
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateHeaderAction
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.AgateSearchBar
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors

/**
 * 模型页（M3-5，对齐 v44 Tab 3）：
 * - 客户端所见清单卡（GET /v1/models 口径）：总数 + 两档视图（按模型聚合 / 按提供商分组）
 * - 搜索 + 提供商分组折叠（mgroups）或按模型聚合（提供商候选列表 + 倍率）
 * - 自定义模型登记 / 模型目录刷新入口
 *
 * 数据源：/api/session.models（与 /v1/models 同源）；倍率由 credits 串解析。
 * v44 的「暴露模式三档（按模型/按提供商/混合）+ pinned 展开」内核暂无对应端点，
 * 界面按两档视图呈现（诚实标注，不做假开关）。
 */
@Composable
fun ModelsPage(
    onOpenDrawer: () -> Unit = {},
    onOpenSpeedtest: () -> Unit = {},
    onOpenCaps: () -> Unit = {},
    onOpenCatalog: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val snap by GatewayRepository.snapshot.collectAsState()
    val models = snap.models

    DisposableEffect(Unit) {
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
        ModelOps.configure(GatewayPrefs.apiKey(ctx))
        onDispose { }
    }

    var view by remember { mutableIntStateOf(1) }   // 0 按模型聚合 / 1 按提供商分组（默认分组）
    var query by remember { mutableStateOf("") }
    var refreshing by remember { mutableStateOf(false) }

    fun doRefresh() {
        if (refreshing) return
        refreshing = true
        scope.launch {
            val r = ModelOps.refresh()
            refreshing = false
            Toast.makeText(
                ctx,
                if (r.ok) "已刷新模型清单" else "刷新失败：${r.error ?: "未知"}",
                Toast.LENGTH_SHORT,
            ).show()
            if (r.ok) GatewayRepository.refreshOnce()
        }
    }

    val filtered = models.filter {
        query.isBlank() || it.id.contains(query, ignoreCase = true) || it.providerLabel.contains(query, ignoreCase = true)
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "模型", onOpenDrawer = onOpenDrawer) {
            AgateHeaderAction("测速") { onOpenSpeedtest() }
        }

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))

            // ── 客户端所见清单（v44 #xp-card）──
            XpCard(total = models.size, fetched = snap.fetchedAtMs > 0)

            Spacer(Modifier.height(12.dp))
            AgateSearchBar(value = query, onValueChange = { query = it }, hint = "搜索模型 / 提供商…")
            Spacer(Modifier.height(10.dp))
            AgateSegmented(
                options = listOf("按模型聚合", "按提供商分组"),
                selected = view,
                onSelect = { view = it },
            )
            Spacer(Modifier.height(10.dp))

            if (models.isEmpty()) {
                EmptyHint(fetched = snap.fetchedAtMs > 0)
            } else if (view == 1) {
                // ── 按提供商分组（v44 #view-provider）──
                val byProvider = filtered.groupBy { it.provider }
                val order = PresetRegistry.ALL.map { it.id }
                byProvider.entries
                    .sortedBy { e -> order.indexOf(e.key).let { if (it < 0) Int.MAX_VALUE else it } }
                    .forEach { (pid, list) ->
                        ProviderModelGroup(
                            providerId = pid,
                            label = list.firstOrNull()?.providerLabel ?: PresetRegistry.labelOf(pid),
                            models = list,
                            onOpenCaps = onOpenCaps,
                        )
                        Spacer(Modifier.height(10.dp))
                    }
            } else {
                // ── 按模型聚合（v44 #view-model）──
                val byId = filtered.groupBy { it.id }
                byId.forEach { (id, list) ->
                    ModelAggregateCard(
                        id = id,
                        providers = list,
                        onOpenCaps = onOpenCaps,
                    )
                    Spacer(Modifier.height(10.dp))
                }
            }

            Spacer(Modifier.height(4.dp))
            // ── 模型目录刷新入口（v44 quota-entry）──
            EntryCard(
                icon = "↻",
                title = "模型目录刷新",
                sub = if (refreshing) "正在刷新…" else if (models.isEmpty()) "点按拉取各家清单（每 6 小时自动）" else "已加载 ${models.size} 条 · 点按强制刷新",
                onClick = { doRefresh() },
            )
            Spacer(Modifier.height(8.dp))
            Text(
                "本页聚焦「谁出战」· 账号资产与额度见「提供商」",
                style = AgateType.Caption,
                color = C.textDim,
                modifier = Modifier.fillMaxWidth(),
                textAlign = androidx.compose.ui.text.style.TextAlign.Center,
            )
            Spacer(Modifier.height(28.dp))
        }
    }
}

/* ── v44 #xp-card：客户端所见清单 ── */
@Composable
private fun XpCard(total: Int, fetched: Boolean) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(14.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("客户端所见清单", style = AgateType.SectionTitle, fontWeight = FontWeight.SemiBold, color = C.textMain, modifier = Modifier.weight(1f))
            AgateBadge("GET /v1/models", BadgeTone.GRAY)
        }
        Spacer(Modifier.height(10.dp))
        Row(verticalAlignment = Alignment.Bottom) {
            Text(
                if (fetched) total.toString() else "—",
                color = C.textMain,
                fontSize = 26.sp,
                fontWeight = FontWeight.Bold,
            )
            Spacer(Modifier.width(8.dp))
            Text("个模型会出现在客户端的列表里", color = C.textSub, fontSize = 13.sp)
        }
        Spacer(Modifier.height(8.dp))
        Text(
            "清单只决定「客户端看见什么」；两种写法在任何模式下都能调用：" +
                "裸名由「路由」队列选账号（可跨家转移），provider/id 锁定这一家。",
            style = AgateType.Caption,
            color = C.textDim,
        )
    }
}

/* ── v44 .mgroup：按提供商分组折叠卡 ── */
@Composable
private fun ProviderModelGroup(
    providerId: String,
    label: String,
    models: List<GatewayRepository.ModelRow>,
    onOpenCaps: () -> Unit,
) {
    val C = agateColors()
    var expanded by remember(providerId) { mutableStateOf(false) }
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .clickable { expanded = !expanded }
            .padding(14.dp)
            .animateContentSize(),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            AgateProviderAvatar(providerId = providerId, label = label, size = 32.dp, corner = 10.dp)
            Spacer(Modifier.width(8.dp))
            Text(label, color = C.textMain, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.width(8.dp))
            Text("${models.size} 个模型", color = C.textSub, fontSize = 12.sp, modifier = Modifier.weight(1f))
            Text("⌄", color = C.textDim, fontSize = 14.sp, modifier = Modifier.rotate(if (expanded) 180f else 0f))
        }
        if (expanded) {
            Spacer(Modifier.height(10.dp))
            models.forEachIndexed { i, m ->
                Row(
                    Modifier.fillMaxWidth().padding(vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text("↳ ${m.id}", color = C.textSub, fontSize = 12.sp, modifier = Modifier.weight(1f))
                    m.credits?.let { CreditsTag(it) }
                }
                if (i < models.size - 1) Box(Modifier.fillMaxWidth().height(1.dp).background(C.border.copy(alpha = 0.4f)))
            }
        }
    }
}

/* ── v44 #view-model：按模型聚合卡（候选提供商 + 倍率 + 能力位）── */
@Composable
private fun ModelAggregateCard(
    id: String,
    providers: List<GatewayRepository.ModelRow>,
    onOpenCaps: () -> Unit,
) {
    val C = agateColors()
    val first = providers.first()
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(14.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(id, color = C.textMain, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
                Text(
                    "可用提供商: " + providers.joinToString(", ") { it.providerLabel },
                    color = C.textSub,
                    fontSize = 12.sp,
                )
            }
            if (providers.size > 1) AgateBadge("${providers.size} 家", BadgeTone.AMBER) else AgateBadge("单家", BadgeTone.GREEN)
        }
        Spacer(Modifier.height(8.dp))
        providers.forEachIndexed { i, m ->
            Row(Modifier.fillMaxWidth().padding(vertical = 3.dp), verticalAlignment = Alignment.CenterVertically) {
                Text("${i + 1}. ${m.providerLabel}", color = C.textSub, fontSize = 13.sp, modifier = Modifier.weight(1f))
                m.credits?.let { CreditsTag(it) }
            }
        }
        Spacer(Modifier.height(8.dp))
        Box(Modifier.fillMaxWidth().height(1.dp).background(C.border.copy(alpha = 0.4f)))
        Spacer(Modifier.height(8.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                "能力位：" + capsOf(first),
                color = C.textDim,
                fontSize = 12.sp,
                modifier = Modifier.weight(1f),
            )
            Text(
                "编辑 ❯",
                color = C.accent,
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(C.accent.copy(alpha = 0.10f), RoundedCornerShape(9.dp))
                    .clickable(onClick = onOpenCaps)
                    .padding(horizontal = 10.dp, vertical = 5.dp),
            )
        }
    }
}

@Composable
private fun CreditsTag(credits: String) {
    val C = agateColors()
    val mult = parseMultiplier(credits)
    val tone = when {
        mult == null -> BadgeTone.GRAY
        mult <= 0.0 -> BadgeTone.GREEN
        mult <= 0.2 -> BadgeTone.GREEN
        mult <= 0.5 -> BadgeTone.BLUE
        else -> BadgeTone.AMBER
    }
    AgateBadge(credits, tone)
}

@Composable
private fun EmptyHint(fetched: Boolean) {
    val C = agateColors()
    Column(
        Modifier.fillMaxWidth().padding(vertical = 28.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            if (fetched) "还没有可暴露的模型" else "网关未运行 / 尚未加载清单",
            color = C.textSub,
            fontSize = 14.sp,
        )
        Spacer(Modifier.height(6.dp))
        Text(
            "添加提供商并登录后，模型会出现在这里；也可点下方「模型目录刷新」强制拉取",
            color = C.textDim,
            fontSize = 12.sp,
            textAlign = androidx.compose.ui.text.style.TextAlign.Center,
        )
    }
}

@Composable
private fun EntryCard(icon: String, title: String, sub: String, onClick: () -> Unit) {
    val C = agateColors()
    Row(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .clickable(onClick = onClick)
            .padding(16.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(icon, fontSize = 22.sp, color = C.accent)
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(title, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            Text(sub, color = C.textSub, fontSize = 12.sp)
        }
        Text("❯", color = C.textDim, fontSize = 13.sp)
    }
}

internal fun initialsOf(label: String): String =
    label.split(" ", "-", "_").filter { it.isNotBlank() }.take(2)
        .joinToString("") { it.first().uppercase() }.ifBlank { label.take(2).uppercase() }

/** 解析上游 credits 串（如 "x0.16 credits"）为倍率数值；无法解析返回 null。 */
internal fun parseMultiplier(credits: String?): Double? {
    if (credits.isNullOrBlank()) return null
    return Regex("([0-9]+(?:\\.[0-9]+)?)").find(credits)?.groupValues?.get(1)?.toDoubleOrNull()
}

private fun capsOf(m: GatewayRepository.ModelRow): String {
    val parts = ArrayList<String>()
    m.maxInput?.let { parts += "${fmtK(it)} 上下文" }
    m.maxOutput?.let { parts += "${fmtK(it)} 输出" }
    if (m.toolCall) parts += "工具 ✓"
    if (m.images) parts += "图像 ✓"
    if (m.reasoning) parts += "思考 ✓"
    return if (parts.isEmpty()) "未声明" else parts.joinToString(" · ")
}

private fun fmtK(n: Int): String = when {
    n >= 1_000_000 -> "${n / 1_000_000}M"
    n >= 1000 -> "${n / 1000}K"
    else -> n.toString()
}