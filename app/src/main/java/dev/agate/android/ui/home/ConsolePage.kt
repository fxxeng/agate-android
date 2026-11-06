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
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.core.AgateForegroundService
import dev.agate.core.Gateway
import dev.agate.core.GatewayPrefs
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.LogOps
import dev.agate.android.data.UsageCache
import dev.agate.android.navigation.Routes
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateChip
import dev.agate.android.ui.components.AgateHeaderAction
import dev.agate.android.ui.components.AgateKpiStrip
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.AgateStatRow
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.KpiCell
import dev.agate.android.ui.components.StatCell
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import org.json.JSONObject

/**
 * 首页·控制台（M3-2R，对齐 v44 Tab 1 结构，自上而下）：
 * 页头（汉堡/控制台/实时日志）→ 新手 3 步向导（accent 边框，可 ✕）→
 * Hero 渐变状态卡（呼吸点 + URL 复制胶囊 + meta 行 +「健康自检 ›」）→
 * 停止态独立卡 → KPI 四联（成功率/平均时延/今日请求/活跃账号）→
 * 健康自检（默认收起，v44 hc-card）→ 实时出战队列（/api/routing/queue）→
 * 额度中心入口 → 今日 Token 消耗。
 *
 * ── 卡片读数口径（2026-11-06 全量补齐）─────────────────────────
 * 每张卡只显示**真算过**的数：总请求 / Token 来自 `request_daily`（历史明细
 * 裁掉后仍在），平均时延 / 输入 / 输出来自按区间现算的 `requests` 明细，
 * 各家占比来自 `providers[].totalTokens`，队列来自 `ordered` + `blocked`。
 * 明细覆盖不全时明说「基于 N/M 条明细」，不用占位文案假装功能还在路上。
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
    // 快照轮询与网关探活**已移到导航宿主**（`AgateNavHost`）：那里是全应用
    // 唯一的启动点。这一段曾在这里，于是没进过控制台就直接打开提供商页时，
    // 快照永远是初始空值 —— 覆盖安装后「账号不见了」的观感就是这么来的。
    // 本页只读快照，不再自己起轮询（两处都起会让 SSE 连两次、`started` 反复置位）。
    // `gw` 这颗仍保留：本页要显示内核 uptime / 端口 / 调度器开关这些控件级读数。
    LaunchedEffect(Unit) {
        while (true) {
            gw = withContext(Dispatchers.IO) { runCatching { Gateway.status() }.getOrNull() }
            delay(5_000L)
        }
    }
    val running = gw?.running == true
    var hcOpen by remember { mutableStateOf(false) }

    // ── 崩溃自愈闭环（2026-10-30 加）──────────────────────────
    // 内核 `crash_marker` 只在**真的 panic** 时落痕（后台被系统回收不算，
    // 那条判据在 AppLogPage 的说明里）。以前这份痕迹只有用户主动点进
    // 「日志」页才看得到 —— 崩溃发生过、AlarmReceiver 自动拉起恢复了，
    // 用户全程无感，「这功能是不是自己好了」无从回答。现在进控制台就问
    // 一次（读即清，与日志页同口径），有痕就地给横幅：复制栈给开发者 /
    // 跳日志页看崩溃前的输出 / 关掉。
    var crash by remember { mutableStateOf<LogOps.Crash?>(null) }
    val clipboard = LocalClipboardManager.current
    LaunchedEffect(Unit) {
        crash = LogOps.takeCrash().takeIf { it.ok }?.data
    }

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

        crash?.let { c ->
            CrashRecoveredBanner(
                crash = c,
                onCopy = {
                    clipboard.setText(AnnotatedString(crashDump(c)))
                    Toast.makeText(ctx, "崩溃栈已复制，可直接发给开发者", Toast.LENGTH_SHORT).show()
                },
                onOpenLogs = { onNavigate(Routes.LOGS) },
                onDismiss = { crash = null },
            )
            Spacer(Modifier.height(10.dp))
        }

        // ── 新手 3 步向导（accent 边框；有账号即自动收起，✕ 永久关闭）──
        //
        // ── 完成的判据（2026-11-01 改口径）────────────────────────────
        // **添加过任意账号（accountsTotal > 0）就不再显示**。
        //   旧判据是 `upstreamConfigured && requestsToday > 0`：
        //   · `upstreamConfigured` 读的是**内核默认登录态**（env 凭证那条，
        //     `get_config_summary().configured`），与「有没有添加过账号」是两回事 ——
        //     用户加了账号后它仍可能为 false，向导赖着不走；
        //   · `requestsToday > 0` 还要求「今天发过请求」，账号加了、只是还没
        //     用，向导也一直挂着。
        //   两条件合起来就是「有登录账号还显示向导」——本条要修的正是这个。
        // 现在的口径：
        //   accountsTotal > 0（含禁用：加过就算「不是新手」）
        //   ‖ upstreamConfigured（本机 LAN 模式没账号也可能跑通，老语义保留）
        //   `running` 保留：网关没起时三步一步都做不了，向导反而该在。
        val wizardDone = running &&
            (snap.accountsTotal > 0 || snap.upstreamConfigured == true)
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
        // 用紧凑统计条而不是四张独立卡：四个读数在首屏占的高度差近三分之一，
        // 而这台设备上首屏下面还压着健康自检与账号列表，纵向空间要给它们。
        AgateStatRow(
            cells = listOf(
                StatCell(
                    label = "成功率",
                    value = if (snap.fetchedAtMs == 0L) "—" else successRateOf(snap),
                    valueColor = if (snap.requestsToday > 0) C.green else null,
                ),
                StatCell(
                    label = "平均时延",
                    // 内核 `avgDurationMs`：区间内「有耗时的终态明细」的均值。
                    // 没有样本就是 —，不拿 0ms 的转发前失败行凑一个假快的数。
                    value = snap.avgLatencyMs?.let { "${it}ms" } ?: "—",
                    valueColor = C.accent,
                ),
                StatCell(
                    label = "今日请求",
                    value = if (snap.fetchedAtMs == 0L) "—" else snap.requestsToday.toString(),
                ),
                StatCell(
                    label = "活跃账号",
                    value = if (snap.accountsTotal > 0) "${snap.accountsEnabled}/${snap.accountsTotal}" else "—",
                    valueColor = if (snap.accountsEnabled > 0) C.green else null,
                ),
            ),
        )

        Spacer(Modifier.height(12.dp))

        // ── 健康自检（v44 hc-card：默认收起，点「健康自检 ›」展开）──
        if (hcOpen) {
            HealthCard(snap = snap, snapError = snapError, onClose = { hcOpen = false })
            Spacer(Modifier.height(12.dp))
        }

        // ── 实时出战队列（v44 hq：编号 = 候选队列位置）──
        QueueCard(
            queue = snap.routingQueue,
            fetched = snap.routingQueueFetched,
            model = snap.routingQueueModel,
            inFlight = snap.inFlight,
            onNavigate = onNavigate,
        )

        Spacer(Modifier.height(12.dp))

        // ── 额度中心入口（v44 .quota-entry）──
        // 只读额度查询的**本地缓存**（UsageCache），绝不为控制台每 5 秒的快照
        // 去打各家上游 —— 余额接口有风控。没查过就如实说「点此查看」。
        val quota = remember(snap.fetchedAtMs) { quotaDigest() }
        EntryCard(
            icon = "◔",
            title = "额度中心",
            sub = quotaSub(quota),
            onClick = { onNavigate(Routes.QUOTA) },
        )

        Spacer(Modifier.height(12.dp))

        // ── 今日 Token 消耗（v44 token-big）──
        TokenCard(snap = snap, onNavigate = onNavigate)

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

        // hero-meta：运行时长 · 今日请求 · 健康自检 ›
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("已运行 ${fmtUptime(status?.uptimeSec ?: 0)}", color = H.sub, fontSize = 12.sp)
            Dot()
            Text("今日 $requestsToday 次请求", color = H.sub, fontSize = 12.sp)
            Spacer(Modifier.weight(1f))
            Text(
                if (hcOpen) "收起自检 ⌄" else "健康自检 ›",
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
            Text("健康自检（本机只读端点）", color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
            if (snapError == null) AgateBadge("status ok", BadgeTone.GREEN) else AgateBadge("异常", BadgeTone.RED)
        }
        HcRow(
            "壳侧就绪",
            if (snap.upstreamConfigured == true) "上游已配置 · 会话摘要正常" else "上游未配置 / 未登录",
            if (snap.upstreamConfigured == true) "ok" else "待配置",
            snap.upstreamConfigured == true,
        )
        // 端口跟着设置走：写死 8787 时改过端口的用户会看到一条不存在的探活地址。
        HcRow(
            "回环探活",
            snapError ?: "127.0.0.1:${GatewayPrefs.port(ctx)} /health · 正常",
            if (snapError == null) "通" else "断",
            snapError == null,
        )
        HcRow(
            "凭证状态",
            "登录态 " + (snap.loggedIn?.let { if (it) "已登录" else "未登录" } ?: "未知") + " · 模型 ${snap.modelCount}",
            snap.loggedIn?.let { if (it) "可刷新" else "缺失" } ?: "—",
            snap.loggedIn == true,
        )
        // 原先这行是「出网连通 / 上游延迟均值（测速样本） / —」：既没测过速，
        // 也没有一个真读数，纯占位。这里换成同一份区间明细算出的平均上游耗时，
        // 并**明确写它不是实时测速** —— 界面只写真算过的东西。
        HcRow(
            "上游响应",
            if (snap.detailSamples > 0) "今日 ${snap.detailSamples} 条终态明细均值 · 非实时测速"
            else "暂无终态明细样本",
            snap.avgLatencyMs?.let { "${it}ms" } ?: "—",
            null,
        )

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
    fetched: Boolean,
    model: String?,
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
            // 这个数是 /api/stats/requests 的 running（在途请求），不是队列长度 ——
            // 写「排队」会让人以为队列里有 N 个待选账号。
            AgateChip(text = "在途 $inFlight", selected = inFlight > 0)
        }
        Spacer(Modifier.height(4.dp))
        Text(
            // 这个名可能来自 ?model=，也可能只是「最近一次请求的模型」——
            // 两种都写「模型」，不冒充当前选路正在用的模型。
            (model?.let { "模型 $it · " } ?: "") + "编号 = 候选优先级 · 点此去「路由」调整 ❯",
            color = C.textDim,
            fontSize = 11.sp,
            modifier = Modifier.clickable { onNavigate(Routes.ROUTING) },
        )
        Spacer(Modifier.height(8.dp))
        if (queue.isEmpty()) {
            Text(
                when {
                    !fetched -> "队列端点暂不可用（/api/routing/queue 未响应）"
                    inFlight > 0 -> "当前无排队候选 · 另有 $inFlight 个在途请求（队列是即时快照）"
                    else -> "当前没有候选账号（未添加，或全部被门禁排除）"
                },
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
                            if (i == 0 && !row.blocked) C.accent.copy(alpha = 0.55f) else C.border,
                            RoundedCornerShape(14.dp),
                        )
                        .padding(horizontal = 12.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Box(
                        Modifier.size(24.dp).background(
                            if (i == 0 && !row.blocked) C.accent else C.textDim.copy(alpha = 0.2f),
                            CircleShape,
                        ),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text(
                            if (row.blocked) "·" else "${i + 1}",
                            color = if (i == 0 && !row.blocked) C.onAccent else C.textSub,
                            fontSize = 12.sp,
                            fontWeight = FontWeight.Bold,
                        )
                    }
                    Spacer(Modifier.width(12.dp))
                    Column(Modifier.weight(1f)) {
                        Text(row.account, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, maxLines = 1)
                        Text(row.provider + (row.model?.let { " · $it" } ?: ""), color = C.textSub, fontSize = 12.sp, maxLines = 1)
                        row.detail?.let {
                            Text(it, color = C.textDim, fontSize = 11.sp, maxLines = 1)
                        }
                    }
                    row.note?.let {
                        AgateBadge(it, if (row.blocked) BadgeTone.AMBER else BadgeTone.BLUE)
                    }
                }
                Spacer(Modifier.height(6.dp))
            }
            if (queue.size > 6) {
                Text("另有 ${queue.size - 6} 个候选 · 完整选路见「路由」", color = C.textDim, fontSize = 11.sp)
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
private fun TokenCard(snap: GatewayRepository.ConsoleSnapshot, onNavigate: (String) -> Unit) {
    val C = agateColors()
    val fetched = snap.fetchedAtMs > 0
    val hasDetails = snap.detailSamples > 0
    // 占比与「大头」来自 providers[].totalTokens（request_daily 口径），
    // 与总量同源；输入 / 输出来自区间明细，覆盖度可能不足。两件事分开说。
    val providerPart = snap.topProviderLabel?.let { label ->
        val pct = if (snap.tokensToday > 0) {
            snap.topProviderTokens * 100.0 / snap.tokensToday
        } else 0.0
        " · $label ${"%.0f".format(pct)}%"
    }.orEmpty()
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(16.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("今日 Token 消耗", color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
            // 原来这颗徽标写的是 CSS 类名「tnum」（等宽数字样式），被当成文案
            // 抄进了界面。换成真说明：总量取自按日汇总，明细覆盖多少也在这里。
            if (fetched) AgateBadge(
                if (hasDetails) "明细 ${snap.detailSamples}/${snap.requestsToday}" else "按日汇总",
                BadgeTone.GRAY,
            )
        }
        Spacer(Modifier.height(6.dp))
        Text(
            if (fetched) fmtTokens(snap.tokensToday) else "—",
            color = C.textMain,
            fontSize = 24.sp,
            fontWeight = FontWeight.Bold,
        )
        Spacer(Modifier.height(4.dp))
        Text(
            when {
                !fetched -> "等待首份快照"
                snap.requestsToday <= 0 -> "今日暂无请求记录"
                hasDetails -> "输入 ${fmtTokens(snap.inputTokensToday)} · 输出 ${fmtTokens(snap.outputTokensToday)}$providerPart"
                else -> "输入 / 输出暂无明细样本$providerPart"
            },
            color = C.textSub,
            fontSize = 12.sp,
        )
        if (fetched && hasDetails && snap.detailSamples < snap.requestsToday) {
            Text(
                "拆分只覆盖上述明细，其余请求仅有按日汇总",
                color = C.textDim,
                fontSize = 11.sp,
            )
        }
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

/**
 * 额度中心入口卡的一行**真读数**，只读 [UsageCache] 里的上一次批量结果。
 *
 * 为什么不把它塞进 `GatewayRepository` 的 5 秒快照：批量余额查询会真的打到
 * 各家上游，官方有频次风控；控制台每 5 秒刷一次等于把风控当心跳打。缓存是
 * `UsageCache` 的唯一语义（5 分钟新鲜期 + 20 秒硬节流），进额度中心时才可能
 * 真正发请求 —— 这里只是把那份已有结果读出来，没查过就返回 null。
 */
private data class QuotaDigest(
    val checked: Int,
    val failed: Int,
    val noData: Int,
    /** 凭证不完整的账号数（内核 `core::usage_query::resolve_batch_targets` 的口径）。 */
    val skipped: Int,
    val atMs: Long?,
)

private fun quotaDigest(): QuotaDigest? {
    val body: JSONObject = UsageCache.cached(null) ?: return null
    val rows = body.optJSONArray("results") ?: return null
    var checked = 0
    var failed = 0
    var noData = 0
    for (i in 0 until rows.length()) {
        val row = rows.optJSONObject(i) ?: continue
        checked++
        // error 是 JSON null 时 optString 会得到字符串 "null"，必须按类型取。
        // 「没返回 usage」与「查询报错」是两件事：额度中心也把它们分开显示。
        val err = (row.opt("error") as? String)?.trim().orEmpty()
        when {
            err.isNotEmpty() -> failed++
            row.optJSONObject("usage") == null -> noData++
        }
    }
    return QuotaDigest(
        checked = checked,
        failed = failed,
        noData = noData,
        skipped = body.optInt("skipped", 0),
        atMs = UsageCache.updatedAt(null),
    )
}

private fun quotaSub(digest: QuotaDigest?): String {
    if (digest == null) {
        return "余额 / 水位 / 额度包 · 点此查看（默认读缓存，不主动打上游）"
    }
    return buildString {
        append("已查 ${digest.checked} 个账号")
        if (digest.failed > 0) append(" · 失败 ${digest.failed}")
        if (digest.noData > 0) append(" · 无数据 ${digest.noData}")
        if (digest.skipped > 0) append(" · 凭证不全 ${digest.skipped}")
        digest.atMs?.let {
            append(" · ").append(android.text.format.DateFormat.format("HH:mm", it))
            append(" 缓存")
        }
    }
}

/** 崩溃痕迹的文本形态（与日志页 `dump` 同一字段序，复制出去能直接读）。 */
private fun crashDump(c: LogOps.Crash): String = buildString {
    appendLine("agate crash @ ${c.at}")
    if (c.thread.isNotBlank()) appendLine("thread: ${c.thread}")
    if (c.location.isNotBlank()) appendLine("at: ${c.location}")
    appendLine("message: ${c.message}")
}

/**
 * 控制台的「上次崩溃已自动恢复」横幅。
 *
 * 与日志页那条的差异：这里的用户**没主动看日志** —— 所以除了复制，还要给
 * 一个直达日志页的入口（崩溃前的输出就在那儿）；文案也点明「已自动恢复」，
 * 回答「刚才是不是挂了」这个必然的疑问。
 */
@Composable
private fun CrashRecoveredBanner(
    crash: LogOps.Crash,
    onCopy: () -> Unit,
    onOpenLogs: () -> Unit,
    onDismiss: () -> Unit,
) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .background(C.amberBg, RoundedCornerShape(12.dp))
            .border(1.dp, C.amber.copy(alpha = 0.35f), RoundedCornerShape(12.dp))
            .padding(horizontal = 10.dp, vertical = 8.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                "上次运行异常退出，已自动恢复",
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                color = C.amber,
                modifier = Modifier.weight(1f),
            )
            Text(
                "✕",
                fontSize = 14.sp,
                color = C.amber,
                modifier = Modifier.clickable(onClick = onDismiss).padding(horizontal = 6.dp),
            )
        }
        Spacer(Modifier.height(3.dp))
        Text(
            "崩溃于 ${crash.message.take(80)}",
            fontSize = 11.sp,
            color = C.textSub,
        )
        Spacer(Modifier.height(6.dp))
        Row {
            Text(
                "查看日志",
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                color = C.accent,
                modifier = Modifier
                    .clickable(onClick = onOpenLogs)
                    .padding(horizontal = 8.dp, vertical = 3.dp),
            )
            Text(
                "复制崩溃栈",
                fontSize = 12.sp,
                color = C.textSub,
                modifier = Modifier
                    .clickable(onClick = onCopy)
                    .padding(horizontal = 8.dp, vertical = 3.dp),
            )
        }
    }
}
