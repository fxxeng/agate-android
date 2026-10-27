package dev.agate.android.ui.home

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.widget.Toast
import androidx.compose.animation.animateContentSize
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
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
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.scale
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.core.AgateForegroundService
import dev.agate.core.Gateway
import dev.agate.core.GatewayPrefs
import dev.agate.android.data.GatewayRepository
import dev.agate.android.navigation.Routes
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateChip
import dev.agate.android.ui.components.AgateHeaderAction
import dev.agate.android.ui.components.AgateKpiStrip
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.KpiCell
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext

/**
 * 首页·控制台（M3-2R，对齐 v44 Tab 1 结构，自上而下）：
 * 页头（汉堡/控制台/实时日志）→ 新手 3 步向导（accent 边框，可 ✕）→
 * Hero 渐变状态卡（呼吸点 + URL 复制胶囊 + meta 行 +「网络测速 ›」）→
 * 停止态独立卡 → KPI 四联（成功率/平均时延/今日请求/活跃账号）→
 * 健康自检（默认收起，v44 hc-card）→ 实时出战队列（/api/routing/queue）→
 * 额度中心入口 → 今日 Token 消耗。
 * SSE 事件列表不再放首页（网关控制页已有事件总线区块）。
 */
@Composable
fun ConsolePage(
    onNavigate: (String) -> Unit,
    onOpenDrawer: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current

    val snap by GatewayRepository.snapshot.collectAsState()
    val snapError by GatewayRepository.snapshotError.collectAsState()

    var gw by remember { mutableStateOf<Gateway.Status?>(null) }
    DisposableEffect(Unit) {
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
        GatewayRepository.startPolling()
        onDispose { GatewayRepository.stopPolling() }
    }
    LaunchedEffect(Unit) {
        while (true) {
            gw = withContext(Dispatchers.IO) { runCatching { Gateway.status() }.getOrNull() }
            GatewayRepository.onGatewayRunning(gw?.running == true)
            delay(5_000L)
        }
    }
    val running = gw?.running == true
    var hcOpen by remember { mutableStateOf(false) }

    Column(modifier = modifier.fillMaxSize()) {
        // ── 页头（v18 吸顶：菜单 + 标题 + 实时日志）──
        AgatePageHeader(title = "控制台", onOpenDrawer = onOpenDrawer) {
            AgateHeaderAction("实时日志") { onNavigate(Routes.LOGS) }
        }

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
        Spacer(Modifier.height(10.dp))

        // ── 新手 3 步向导（accent 边框；三步完成自动收起，✕ 永久关闭）──
        val wizardDone = running && snap.upstreamConfigured == true && snap.requestsToday > 0
        var wizardDismissed by remember { mutableStateOf(GatewayPrefs.wizardDismissed(ctx)) }
        if (!wizardDone && !wizardDismissed) {
            WizardCard(onNavigate = onNavigate, onDismiss = {
                wizardDismissed = true
                GatewayPrefs.setWizardDismissed(ctx, true)
            })
            Spacer(Modifier.height(12.dp))
        }

        // ── Hero / 停止卡 ──
        if (running) {
            HeroCard(
                status = gw,
                requestsToday = snap.requestsToday,
                hcOpen = hcOpen,
                onToggleHc = { hcOpen = !hcOpen },
                onStop = { AgateForegroundService.stop(ctx) },
            )
        } else {
            StoppedCard(onStart = { AgateForegroundService.start(ctx) })
        }

        Spacer(Modifier.height(12.dp))

        // ── KPI 四联（v44 语义：成功率 / 平均时延 / 今日请求 / 活跃账号）──
        AgateKpiStrip(
            cells = listOf(
                KpiCell(
                    label = "成功率",
                    value = if (snap.fetchedAtMs == 0L) "—" else successRateOf(snap),
                    valueColor = if (snap.requestsToday > 0) C.green else null,
                ),
                KpiCell(
                    label = "平均时延",
                    value = snap.avgLatencyMs?.let { "${it}ms" } ?: "—",
                    valueColor = C.accent,
                ),
                KpiCell(
                    label = "今日请求",
                    value = if (snap.fetchedAtMs == 0L) "—" else snap.requestsToday.toString(),
                    valueColor = C.textMain,
                ),
                KpiCell(
                    label = "活跃账号",
                    value = if (snap.accountsTotal > 0) "${snap.accountsEnabled}/${snap.accountsTotal}" else "—",
                    valueColor = if (snap.accountsEnabled > 0) C.green else null,
                ),
            ),
        )

        Spacer(Modifier.height(12.dp))

        // ── 健康自检（v44 hc-card：默认收起，点「网络测速 ›」展开）──
        if (hcOpen) {
            HealthCard(snap = snap, snapError = snapError, onClose = { hcOpen = false })
            Spacer(Modifier.height(12.dp))
        }

        // ── 实时出战队列（v44 hq：编号 = 候选队列位置）──
        QueueCard(
            queue = snap.routingQueue,
            inFlight = snap.inFlight,
            onNavigate = onNavigate,
        )

        Spacer(Modifier.height(12.dp))

        // ── 额度中心入口（v44 .quota-entry）──
        EntryCard(
            icon = "◔",
            title = "额度中心",
            sub = "余额与水位待额度端点接入",
            onClick = { onNavigate(Routes.QUOTA) },
        )

        Spacer(Modifier.height(12.dp))

        // ── 今日 Token 消耗（v44 token-big）──
        TokenCard(tokens = snap.tokensToday, fetched = snap.fetchedAtMs > 0, onNavigate = onNavigate)

        Spacer(Modifier.height(28.dp))
        }
    }
}

/* ── Hero 卡（v44 .hero：深蓝渐变 + live 呼吸点 + url-pill + meta 行）── */
@Composable
private fun HeroCard(
    status: Gateway.Status?,
    requestsToday: Long,
    hcOpen: Boolean,
    onToggleHc: () -> Unit,
    onStop: () -> Unit,
) {
    val C = agateColors()
    val lightHero = C.bg.luminance() > 0.5f
    val H = if (lightHero) HeroPalette(
        bg1 = Color(0xFFE0F2FE), bg2 = Color(0xFFBAE6FD), border = Color(0xFF7DD3FC),
        text = Color(0xFF0C4A6E), sub = Color(0xFF0369A1),
        pillBg = Color(0x66FFFFFF), pillText = Color(0xFF0C4A6E),
    ) else HeroPalette(
        bg1 = Color(0xFF12324D), bg2 = Color(0xFF0C4A6E), border = Color(0xFF0369A1),
        text = Color(0xFFF8FAFC), sub = Color(0xFF7DD3FC),
        pillBg = Color(0x59020623), pillText = Color(0xFFE0F2FE),
    )
    val ctx = LocalContext.current

    Column(
        Modifier
            .fillMaxWidth()
            .background(Brush.linearGradient(listOf(H.bg1, H.bg2)), RoundedCornerShape(20.dp))
            .border(1.dp, H.border, RoundedCornerShape(20.dp))
            .padding(16.dp),
    ) {
        // hero-top：呼吸点 + 状态词 + 端口 + 停止
        Row(verticalAlignment = Alignment.CenterVertically) {
            val pulse by rememberInfiniteTransition(label = "live").animateFloat(
                initialValue = 0.75f,
                targetValue = 1f,
                animationSpec = infiniteRepeatable(tween(900), RepeatMode.Reverse),
                label = "pulse",
            )
            Box(
                Modifier
                    .size(10.dp)
                    .scale(pulse)
                    .background(C.green, CircleShape),
            )
            Spacer(Modifier.width(8.dp))
            Text("服务运行中", color = H.text, fontSize = 16.sp, fontWeight = FontWeight.Bold)
            Spacer(Modifier.width(10.dp))
            Text(":${status?.port ?: GatewayPrefs.port(ctx)}", color = H.sub, fontSize = 13.sp, modifier = Modifier.weight(1f))
            Text(
                "停止",
                color = Color(0xFFF87171),
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .border(1.dp, Color(0x59F87171), RoundedCornerShape(9.dp))
                    .clickable(onClick = onStop)
                    .padding(horizontal = 12.dp, vertical = 6.dp),
            )
        }

        // url-pill：点按复制 BaseURL
        Row(
            Modifier
                .fillMaxWidth()
                .padding(top = 14.dp, bottom = 10.dp)
                .background(H.pillBg, RoundedCornerShape(12.dp))
                .clickable {
                    val cm = ctx.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                    cm.setPrimaryClip(ClipData.newPlainText("BaseURL", "http://127.0.0.1:${status?.port ?: GatewayPrefs.port(ctx)}/v1"))
                    Toast.makeText(ctx, "已复制本机地址", Toast.LENGTH_SHORT).show()
                }
                .padding(horizontal = 14.dp, vertical = 11.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                "http://127.0.0.1:${status?.port ?: GatewayPrefs.port(ctx)}/v1",
                color = H.pillText,
                fontSize = 13.sp,
                maxLines = 1,
                modifier = Modifier.weight(1f),
            )
            Icon(Icons.Filled.ContentCopy, contentDescription = "复制", tint = H.pillText.copy(alpha = 0.8f), modifier = Modifier.size(16.dp))
        }

        // hero-meta：运行时长 · 今日请求 · 网络测速 ›
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("已运行 ${fmtUptime(status?.uptimeSec ?: 0)}", color = H.sub, fontSize = 12.sp)
            Dot()
            Text("今日 $requestsToday 次请求", color = H.sub, fontSize = 12.sp)
            Spacer(Modifier.weight(1f))
            Text(
                if (hcOpen) "收起自检 ⌄" else "网络测速 ›",
                color = H.sub,
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.clickable(onClick = onToggleHc),
            )
        }
    }
}

@Composable
private fun Dot() {
    val C = agateColors()
    Box(Modifier.padding(horizontal = 8.dp).size(3.dp).background(C.textDim, CircleShape))
}

/* ── 停止态独立卡（v44 .state-stopped）── */
@Composable
private fun StoppedCard(onStart: () -> Unit) {
    val ctx = LocalContext.current
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(16.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(Modifier.size(10.dp).background(C.textDim, CircleShape))
            Spacer(Modifier.width(8.dp))
            Text("服务已停止", color = C.textSub, fontSize = 16.sp, fontWeight = FontWeight.Bold, modifier = Modifier.weight(1f))
            Text(
                "启动网关",
                color = C.onAccent,
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(C.accent, RoundedCornerShape(9.dp))
                    .clickable(onClick = onStart)
                    .padding(horizontal = 14.dp, vertical = 7.dp),
            )
        }
        Spacer(Modifier.height(8.dp))
        Text("BaseURL 将在启动后生成 · 端口 :${GatewayPrefs.port(ctx)}", color = C.textDim, fontSize = 12.sp)
    }
}

/* ── 新手向导（v44 ob-card：accent 边框 + 收起单行 + 展开 3 步）── */
@Composable
private fun WizardCard(onNavigate: (String) -> Unit, onDismiss: () -> Unit) {
    val ctx = LocalContext.current
    val C = agateColors()
    var expanded by remember { mutableStateOf(false) }
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.accent, RoundedCornerShape(20.dp))
            .padding(14.dp)
            .animateContentSize(),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                if (expanded) "🚀 新手向导 · 3 步跑通" else "🚀 新手 3 步向导 · 点此展开 ❯",
                color = C.textMain,
                fontSize = 13.sp,
                fontWeight = FontWeight.Bold,
                modifier = Modifier
                    .weight(1f)
                    .clickable { expanded = !expanded },
            )
            Text("✕", color = C.textDim, fontSize = 13.sp, modifier = Modifier.clickable(onClick = onDismiss).padding(6.dp))
        }
        if (expanded) {
            Spacer(Modifier.height(10.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                listOf(
                    Triple("1", "添加首个提供商", "中转 / 24 张预置卡") to Routes.PROVIDER_ADD,
                    // 本机模式不需要 Key（内核只在 LAN 模式注入），文案别让人以为必须填
                    if (GatewayPrefs.bindLan(ctx)) {
                        Triple("2", "复制地址与 Key", ":${GatewayPrefs.port(ctx)} · agate-…") to Routes.KEYS
                    } else {
                        Triple("2", "复制接口地址", ":${GatewayPrefs.port(ctx)} · 本机免 Key") to Routes.KEYS
                    },
                    Triple("3", "测速验证", "并发跑一轮模型测速") to Routes.MODEL_SPEEDTEST,
                ).forEach { (step, route) ->
                    Column(
                        Modifier
                            .weight(1f)
                            .background(C.insetBg, RoundedCornerShape(12.dp))
                            .border(1.dp, C.border, RoundedCornerShape(12.dp))
                            .clickable { onNavigate(route) }
                            .padding(10.dp),
                    ) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Box(
                                Modifier
                                    .size(22.dp)
                                    .background(C.accent, CircleShape),
                                contentAlignment = Alignment.Center,
                            ) {
                                Text(step.first, color = C.onAccent, fontSize = 12.sp, fontWeight = FontWeight.Bold)
                            }
                            Spacer(Modifier.width(6.dp))
                            Text(step.second, color = C.textMain, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
                        }
                        Spacer(Modifier.height(4.dp))
                        Text(step.third, color = C.textSub, fontSize = 11.sp)
                    }
                }
            }
        }
    }
}

/* ── 健康自检（v44 hc-card：cfg-row × 4 + 协议 chips）── */
@Composable
private fun HealthCard(snap: GatewayRepository.ConsoleSnapshot, snapError: String?, onClose: () -> Unit) {
    val C = agateColors()
    val ctx = LocalContext.current
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(16.dp)
            .animateContentSize(),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("健康自检（GET /health 口径）", color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
            if (snapError == null) AgateBadge("status ok", BadgeTone.GREEN) else AgateBadge("异常", BadgeTone.RED)
        }
        HcRow(
            "壳侧就绪",
            if (snap.upstreamConfigured == true) "上游已配置 · 会话摘要正常" else "上游未配置 / 未登录",
            if (snap.upstreamConfigured == true) "ok" else "待配置",
            snap.upstreamConfigured == true,
        )
        HcRow("回环探活", snapError ?: "127.0.0.1:8787 /health · 正常", "通", snapError == null)
        HcRow(
            "凭证状态",
            "登录态 " + (snap.loggedIn?.let { if (it) "已登录" else "未登录" } ?: "未知") + " · 模型 ${snap.modelCount}",
            snap.loggedIn?.let { if (it) "可刷新" else "缺失" } ?: "—",
            snap.loggedIn == true,
        )
        HcRow("出网连通", "上游延迟均值（测速样本）", "—", null)

        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            listOf("/v1/chat/completions", "/v1/messages", "/v1/responses").forEach { ep ->
                AgateChip(
                    text = ep,
                    onClick = {
                        val cm = ctx.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                        cm.setPrimaryClip(ClipData.newPlainText("endpoint", ep))
                        Toast.makeText(ctx, "已复制 $ep", Toast.LENGTH_SHORT).show()
                    },
                )
            }
        }
        Spacer(Modifier.height(8.dp))
        Text("协议入口按客户端能力选 · 点击复制 · 点「收起自检」折叠本面板", color = C.textSub, fontSize = 12.sp)
    }
}

@Composable
private fun HcRow(name: String, desc: String, value: String, ok: Boolean?) {
    val C = agateColors()
    Row(Modifier.fillMaxWidth().padding(vertical = 9.dp), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(name, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            Text(desc, color = C.textSub, fontSize = 12.sp)
        }
        AgateBadge(
            value,
            when (ok) {
                true -> BadgeTone.GREEN
                false -> BadgeTone.AMBER
                null -> BadgeTone.GRAY
            },
        )
    }
}

/* ── 实时出战队列（v44 hq / .qrow）── */
@Composable
private fun QueueCard(
    queue: List<GatewayRepository.QueueRow>,
    inFlight: Int,
    onNavigate: (String) -> Unit,
) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(16.dp)
            .animateContentSize(),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("实时出战队列", color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
            AgateChip(text = "排队 $inFlight", selected = inFlight > 0)
        }
        Spacer(Modifier.height(4.dp))
        Text("编号 = 候选队列位置 · 在「路由」里调整", color = C.textDim, fontSize = 11.sp)
        Spacer(Modifier.height(8.dp))
        if (queue.isEmpty()) {
            Text(
                if (inFlight > 0) "队列数据待 /api/routing/queue 结构核对" else "队列空闲 · 暂无在途请求",
                color = C.textSub,
                fontSize = 12.sp,
                modifier = Modifier.padding(vertical = 6.dp),
            )
        } else {
            queue.take(6).forEachIndexed { i, row ->
                Row(
                    Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 10.dp, vertical = 9.dp)
                        .background(C.insetBg, RoundedCornerShape(14.dp))
                        .border(
                            1.dp,
                            if (i == 0) C.accent.copy(alpha = 0.55f) else C.border,
                            RoundedCornerShape(14.dp),
                        )
                        .padding(horizontal = 12.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Box(
                        Modifier.size(24.dp).background(if (i == 0) C.accent else C.textDim.copy(alpha = 0.2f), CircleShape),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text("${i + 1}", color = if (i == 0) C.onAccent else C.textSub, fontSize = 12.sp, fontWeight = FontWeight.Bold)
                    }
                    Spacer(Modifier.width(12.dp))
                    Column(Modifier.weight(1f)) {
                        Text(row.account, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                        Text(row.provider + (row.model?.let { " · $it" } ?: ""), color = C.textSub, fontSize = 12.sp)
                    }
                    row.note?.let { AgateBadge(it, BadgeTone.BLUE) }
                }
                Spacer(Modifier.height(6.dp))
            }
        }
    }
}

/* ── 入口卡（v44 .quota-entry：图标 + 标题 + 副行 + ❯）── */
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

/* ── 今日 Token 消耗（v44 .token-big）── */
@Composable
private fun TokenCard(tokens: Long, fetched: Boolean, onNavigate: (String) -> Unit) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(16.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("今日 Token 消耗", color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
            if (fetched) AgateBadge("tnum", BadgeTone.GRAY)
        }
        Spacer(Modifier.height(6.dp))
        Text(
            if (fetched) fmtTokens(tokens) else "—",
            color = C.textMain,
            fontSize = 24.sp,
            fontWeight = FontWeight.Bold,
        )
        Spacer(Modifier.height(4.dp))
        Text("输入 / 输出拆分与各家占比待 stats 端点字段接入", color = C.textSub, fontSize = 12.sp)
        Spacer(Modifier.height(6.dp))
        Text(
            "完整报表 ❯",
            color = C.accent,
            fontSize = 12.sp,
            fontWeight = FontWeight.SemiBold,
            modifier = Modifier.clickable { onNavigate(Routes.REPORTS) },
        )
    }
}

/** Hero 卡配色（v44 .hero 深浅两套渐变，按当前主题底色亮度选择）。 */
private data class HeroPalette(
    val bg1: Color,
    val bg2: Color,
    val border: Color,
    val text: Color,
    val sub: Color,
    val pillBg: Color,
    val pillText: Color,
)

/* ── 小件 ── */
private fun successRateOf(snap: GatewayRepository.ConsoleSnapshot): String =
    if (snap.requestsToday <= 0) "—" else "%.1f%%".format(snap.successfulToday * 100.0 / snap.requestsToday)

private fun fmtUptime(sec: Long): String = when {
    sec >= 3600 -> "%02d:%02d:%02d".format(sec / 3600, (sec % 3600) / 60, sec % 60)
    sec >= 60 -> "%02d:%02d".format(sec / 60, sec % 60)
    else -> "${sec}s"
}

private fun fmtTokens(n: Long): String = when {
    n >= 100_000_000 -> "%.1f亿".format(n / 100_000_000.0)
    n >= 10_000 -> "%.1f万".format(n / 10_000.0)
    else -> n.toString()
}
