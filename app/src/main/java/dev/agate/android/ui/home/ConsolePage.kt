package dev.agate.android.ui.home

import androidx.compose.foundation.background
import androidx.compose.foundation.border
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
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import dev.agate.android.data.GatewayRepository
import dev.agate.android.navigation.Routes
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateChip
import dev.agate.android.ui.components.AgateKpiStrip
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.KpiCell
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import dev.agate.core.AgateForegroundService
import dev.agate.core.Gateway
import dev.agate.core.GatewayPrefs
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * Tab1 控制台（方案 §7 / §8 M3）：Hero 状态卡、KPI 四联、健康自检、
 * 3 步新手向导、实时出战队列与排队水位。
 *
 * 数据纪律：全部走 [GatewayRepository] 单一数据源（快照轮询 + SSE 事件流），
 * 本页不直连内核；JNI 只读 [Gateway.status] 做运行态对齐（方案 §3 控制面）。
 * 前台轮询 / 退后台挂起由 DisposableEffect 挂卸实现（方案 §3.1）。
 */
@Composable
fun ConsolePage(
    onNavigate: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current

    val snap by GatewayRepository.snapshot.collectAsState()
    val snapError by GatewayRepository.snapshotError.collectAsState()
    val link by GatewayRepository.link.collectAsState()
    val events by GatewayRepository.events.collectAsState()

    // JNI 运行态：轮询回填（控制面不进仓库层，方案 §3 双通信面边界）
    var gw by remember { mutableStateOf<Gateway.Status?>(null) }

    DisposableEffect(Unit) {
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
        GatewayRepository.startPolling()
        onDispose { GatewayRepository.stopPolling() }
    }

    LaunchedEffect(Unit) {
        while (true) {
            gw = withContext(Dispatchers.IO) {
                runCatching { Gateway.status() }.getOrNull()
            }
            GatewayRepository.onGatewayRunning(gw?.running == true)
            delay(5_000L)
        }
    }

    val running = gw?.running == true
    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 16.dp),
    ) {
        Spacer(Modifier.height(16.dp))

        HeroCard(
            running = running,
            status = gw,
            link = link,
            onToggle = {
                if (running) {
                    AgateForegroundService.stop(ctx)
                } else {
                    AgateForegroundService.start(ctx)
                }
            },
        )

        Spacer(Modifier.height(14.dp))
        AgateKpiStrip(
            cells = listOf(
                KpiCell(
                    label = "今日请求",
                    value = if (snap.fetchedAtMs == 0L) "—" else snap.requestsToday.toString(),
                ),
                KpiCell(
                    label = "成功率",
                    value = successRateOf(snap),
                    valueColor = if (snap.requestsToday > 0) C.green else null,
                ),
                KpiCell(
                    label = "今日 Tokens",
                    value = if (snap.fetchedAtMs == 0L) "—" else fmtTokens(snap.tokensToday),
                ),
                KpiCell(
                    label = "活跃账号",
                    value = if (snap.accountsTotal > 0) "${snap.accountsEnabled}/${snap.accountsTotal}" else "—",
                    valueColor = if (snap.accountsEnabled > 0) C.green else null,
                ),
            ),
        )

        Spacer(Modifier.height(14.dp))
        WizardCard(
            running = running,
            upstreamConfigured = snap.upstreamConfigured,
            requestsToday = snap.requestsToday,
            onNavigate = onNavigate,
            onStartGateway = { AgateForegroundService.start(ctx) },
        )

        Spacer(Modifier.height(14.dp))
        SectionLabel("健康自检")
        HealthCard(snap = snap, snapError = snapError)

        Spacer(Modifier.height(14.dp))
        Row(
            Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("实时出战队列", style = AgateType.SectionTitle, color = C.textSub, modifier = Modifier.weight(1f))
            AgateChip(
                text = "在途 ${snap.inFlight}",
                selected = snap.inFlight > 0,
            )
        }
        Spacer(Modifier.height(8.dp))
        EventQueueCard(events = events)

        Spacer(Modifier.height(28.dp))
    }
}

// ── Hero 状态卡 ──────────────────────────────────────────────────────────

@Composable
private fun HeroCard(
    running: Boolean,
    status: Gateway.Status?,
    link: GatewayRepository.LinkState,
    onToggle: () -> Unit,
) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(16.dp))
            .border(1.dp, C.border, RoundedCornerShape(16.dp))
            .padding(16.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        if (running) "网关运行中" else "网关已停止",
                        style = AgateType.NumXL,
                        color = if (running) C.green else C.textSub,
                    )
                    Spacer(Modifier.size(10.dp))
                    AgateBadge(
                        text = if (running) ":${status?.port ?: 8787}" else "未启动",
                        tone = if (running) BadgeTone.BLUE else BadgeTone.GRAY,
                        withDot = running,
                    )
                }
                Spacer(Modifier.height(6.dp))
                Text(
                    buildString {
                        append("内核 ")
                        append(status?.version?.ifBlank { "—" } ?: "—")
                        if (running) {
                            append(" · 运行 ").append(fmtUptime(status?.uptimeSec ?: 0))
                            append(" · ").append(if (status?.bindLan == true) "局域网 0.0.0.0" else "本机 127.0.0.1")
                        }
                    },
                    style = AgateType.Caption,
                    color = C.textDim,
                )
            }
            LinkBadge(link)
        }
        Spacer(Modifier.height(12.dp))
        Button(onClick = onToggle, modifier = Modifier.fillMaxWidth()) {
            Text(if (running) "停止网关" else "启动网关", style = AgateType.BodyS)
        }
    }
}

@Composable
private fun LinkBadge(link: GatewayRepository.LinkState) {
    val (text, tone) = when (link) {
        GatewayRepository.LinkState.CONNECTED -> "SSE 已连接" to BadgeTone.GREEN
        GatewayRepository.LinkState.CONNECTING -> "连接中…" to BadgeTone.AMBER
        GatewayRepository.LinkState.RETRYING -> "重连中…" to BadgeTone.AMBER
        GatewayRepository.LinkState.POLLING -> "轮询降级" to BadgeTone.BLUE
        GatewayRepository.LinkState.DISCONNECTED -> "事件流未连" to BadgeTone.GRAY
    }
    AgateBadge(text = text, tone = tone)
}

// ── 健康自检 ─────────────────────────────────────────────────────────────

@Composable
private fun HealthCard(snap: GatewayRepository.ConsoleSnapshot, snapError: String?) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(16.dp))
            .border(1.dp, C.border, RoundedCornerShape(16.dp))
            .padding(horizontal = 14.dp, vertical = 6.dp),
    ) {
        if (snapError != null) {
            Text(
                snapError,
                style = AgateType.Caption,
                color = C.red,
                modifier = Modifier.padding(vertical = 8.dp),
            )
        }
        HealthRow("上游配置") {
            when (snap.upstreamConfigured) {
                true -> AgateBadge("已配置", BadgeTone.GREEN)
                false -> AgateBadge("未配置", BadgeTone.RED)
                null -> Placeholder()
            }
        }
        HealthRow("面板登录") {
            when (snap.loggedIn) {
                true -> AgateBadge("已登录", BadgeTone.GREEN)
                false -> AgateBadge("未登录", BadgeTone.AMBER)
                null -> Placeholder()
            }
        }
        HealthRow("/v1 鉴权") {
            when (snap.authRequired) {
                true -> AgateBadge("已启用 API Key", BadgeTone.BLUE)
                false -> AgateBadge("未设置", BadgeTone.GRAY)
                null -> Placeholder()
            }
        }
        HealthRow("默认模型") { ValueText(snap.defaultModel) }
        HealthRow("模型目录") {
            ValueText(if (snap.modelCount > 0) "${snap.modelCount} 个" else null)
        }
        HealthRow("队首账号") {
            ValueText(snap.routedAccountName ?: snap.routedAccountId)
        }
        if (snap.fetchedAtMs > 0) {
            Text(
                "刷新于 ${fmtClock(snap.fetchedAtMs)}",
                style = AgateType.Caption,
                color = C.textDim,
                modifier = Modifier.padding(top = 2.dp, bottom = 8.dp),
            )
        }
    }
}

@Composable
private fun HealthRow(label: String, value: @Composable () -> Unit) {
    val C = agateColors()
    Row(
        Modifier
            .fillMaxWidth()
            .padding(vertical = 7.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, style = AgateType.Body, color = C.textSub, modifier = Modifier.weight(1f))
        value()
    }
}

@Composable
private fun ValueText(text: String?) {
    val C = agateColors()
    Text(
        text ?: "—",
        style = AgateType.BodyS,
        color = if (text.isNullOrBlank()) C.textDim else C.textMain,
    )
}

@Composable
private fun Placeholder() {
    val C = agateColors()
    Text("—", style = AgateType.BodyS, color = C.textDim)
}

// ── 3 步新手向导 ─────────────────────────────────────────────────────────

@Composable
private fun WizardCard(
    running: Boolean,
    upstreamConfigured: Boolean?,
    requestsToday: Long,
    onNavigate: (String) -> Unit,
    onStartGateway: () -> Unit,
) {
    // 三步全过即整卡收起（v44 紧凑向导语义）：接入完成的标志是今天已有真实流量。
    val step2Done = upstreamConfigured == true
    val allDone = running && step2Done && requestsToday > 0
    if (allDone) return

    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(16.dp))
            .border(1.dp, C.border, RoundedCornerShape(16.dp))
            .padding(14.dp),
    ) {
        Text("3 步开始使用", style = AgateType.SectionTitle, color = C.textMain)
        Spacer(Modifier.height(8.dp))
        WizardStep(
            index = 1,
            text = "启动网关（本机 :8787）",
            done = running,
            actionLabel = if (running) null else "启动",
            onAction = onStartGateway,
        )
        WizardStep(
            index = 2,
            text = "配置上游账号",
            done = step2Done,
            actionLabel = if (step2Done) null else "去提供商",
            onAction = { onNavigate(Routes.PROVIDERS) },
        )
        WizardStep(
            index = 3,
            text = "客户端接入 /v1（Key 在访问密钥页）",
            done = requestsToday > 0,
            actionLabel = "查看密钥",
            onAction = { onNavigate(Routes.KEYS) },
        )
    }
}

@Composable
private fun WizardStep(
    index: Int,
    text: String,
    done: Boolean,
    actionLabel: String?,
    onAction: () -> Unit,
) {
    val C = agateColors()
    Row(
        Modifier
            .fillMaxWidth()
            .padding(vertical = 5.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            Modifier
                .size(20.dp)
                .background(if (done) C.greenBg else C.insetBg, CircleShape)
                .border(1.dp, if (done) C.green else C.border, CircleShape),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                if (done) "✓" else index.toString(),
                style = AgateType.Caption,
                fontWeight = FontWeight.Bold,
                color = if (done) C.green else C.textSub,
            )
        }
        Spacer(Modifier.size(10.dp))
        Text(
            text,
            style = AgateType.BodyS,
            color = if (done) C.textDim else C.textMain,
            modifier = Modifier.weight(1f),
        )
        if (actionLabel != null) {
            AgateChip(text = actionLabel, selected = true, onClick = onAction)
        }
    }
}

// ── 实时出战队列 ─────────────────────────────────────────────────────────

@Composable
private fun EventQueueCard(events: List<GatewayRepository.RecentEvent>) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(16.dp))
            .border(1.dp, C.border, RoundedCornerShape(16.dp))
            .padding(horizontal = 14.dp, vertical = 10.dp),
    ) {
        if (events.isEmpty()) {
            Text(
                "尚无事件：连上后首帧 hello；转发完成 / 签到轮次 / 网关启动都会推到这里",
                style = AgateType.BodyS,
                color = C.textDim,
            )
            return@Column
        }
        events.take(8).forEach { ev ->
            Row(Modifier.padding(vertical = 5.dp), verticalAlignment = Alignment.CenterVertically) {
                val ok = ev.type != "request_done" || ev.summary.startsWith("✓")
                Box(
                    Modifier
                        .size(6.dp)
                        .background(
                            when {
                                ev.type == "request_done" && !ok -> C.red
                                ev.type == "request_done" -> C.green
                                else -> C.accent
                            },
                            CircleShape,
                        ),
                )
                Spacer(Modifier.size(10.dp))
                Text(
                    ev.summary,
                    style = AgateType.BodyS,
                    color = C.textMain,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    if (ev.tsMs > 0) fmtClock(ev.tsMs) else "",
                    style = AgateType.Caption,
                    color = C.textDim,
                )
            }
        }
    }
}

// ── 公共小件与格式化 ─────────────────────────────────────────────────────

@Composable
private fun SectionLabel(text: String) {
    val C = agateColors()
    Text(text, style = AgateType.SectionTitle, color = C.textSub)
}

private fun successRateOf(snap: GatewayRepository.ConsoleSnapshot): String {
    if (snap.fetchedAtMs == 0L || snap.requestsToday <= 0L) return "—"
    val pct = snap.successfulToday * 100 / snap.requestsToday
    return "$pct%"
}

private fun fmtUptime(sec: Long): String = when {
    sec >= 3600 -> "${sec / 3600}小时${(sec % 3600) / 60}分"
    sec >= 60 -> "${sec / 60}分${sec % 60}秒"
    else -> "${sec}秒"
}

private fun fmtTokens(n: Long): String = when {
    n >= 1_000_000L -> String.format(Locale.US, "%.1fM", n / 1_000_000.0)
    n >= 1_000L -> String.format(Locale.US, "%.1fk", n / 1_000.0)
    else -> n.toString()
}

private fun fmtClock(ms: Long): String =
    SimpleDateFormat("HH:mm:ss", Locale.CHINA).format(Date(ms))
