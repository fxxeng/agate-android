package dev.agate.android.ui.pages

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Menu
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Icon
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.runtime.collectAsState
import dev.agate.android.data.GatewayRepository
import dev.agate.core.AgateForegroundService
import dev.agate.core.AlarmScheduler
import dev.agate.core.BatteryWhitelist
import dev.agate.core.Gateway
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateAccordion
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.delay
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONObject
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.concurrent.TimeUnit

/**
 * 网关控制页：M1 验收 Demo 的 Compose 版（功能 1:1 迁移）。
 * M0 探测（启停/status/health/models/chat）+ M1 保活（3 开关 + 白名单/精确闹钟/tick）。
 * M3 起由控制台真数据源接管，本页保留为运维后门。
 *
 * ── 2026-11-09 重排：三层信息架构（状态 → 操作 → 诊断）──────────
 * 改前 11 个按钮全是同款实心整宽 Button、8 行裸事件 Text 平铺、验收探针与
 * 日常启停平级，一眼看不出主次。现在：
 *   · 常驻区 = 状态条 + 操作卡（主/次按钮）+ 最近事件卡 + 保活设置卡；
 *   · 验收探针 / 系统授权 / 输出默认折叠进 `AgateAccordion`（点开才占空间）；
 *   · 分组统一 `agateCard()` + `C.cardBg` + 1dp `C.border`。深色是纯黑，
 *     阴影等于没画，层次只能靠描边 —— 别改回投影。
 * 功能、开关语义、端口防抖重启、事件采集逻辑一律不动，只改渲染。
 */
@Composable
fun GatewayControlPage(modifier: Modifier = Modifier, onOpenDrawer: () -> Unit = {}) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = remember { CoroutineScope(Dispatchers.IO) }
    val client = remember {
        OkHttpClient.Builder()
            .connectTimeout(5, TimeUnit.SECONDS)
            .readTimeout(180, TimeUnit.SECONDS)
            .build()
    }
    var output by remember { mutableStateOf("Agate 网关控制就绪（内核版本待加载后显示）") }
    var lastOutput by remember { mutableStateOf<String?>(null) }
    var bindLan by remember { mutableStateOf(GatewayPrefs.bindLan(ctx)) }
    var bootStart by remember { mutableStateOf(GatewayPrefs.bootAutoStart(ctx)) }
    var checkinAlarm by remember { mutableStateOf(GatewayPrefs.checkinAlarmEnabled(ctx)) }

    fun append(line: String) {
        output = "$output\n\n────\n$line"
        lastOutput = line.take(60)
    }

    fun bg(fn: suspend () -> String) = scope.launch { withContext(Dispatchers.Main) { append(fn()) } }

    // 端口的**唯一来源**是 GatewayPrefs：原先这里写死 8787，用户把端口改成 8888
    // 后四个验收探针仍打旧口，而页面上看不出为什么全失败。状态条读的也是它。
    fun probe(label: String, body: String? = null): String = try {
        val activePort = GatewayPrefs.port(ctx)
        val b = Request.Builder().url("http://127.0.0.1:$activePort${pathOf(label)}")
        if (body != null) b.post(body.toRequestBody("application/json".toMediaType())) else b.get()
        client.newCall(b.build()).execute().use { resp ->
            val text = resp.body?.string().orEmpty()
            "$label → HTTP ${resp.code}\n" + (if (text.length > 1200) text.take(1200) + "…" else text)
        }
    } catch (t: Throwable) {
        "$label → 失败：${t.message}"
    }

    // ── 端口防抖（原「保活开关」区提到顶部：状态条也要读它）────────
    // ⚠️ 输入**防抖后自动重启**（2026-10-30 改）：旧文案「重启网关生效」把
    // 生效时机留给用户猜 —— 改完端口，监听还钉在旧口上，外部设备连不上，
    // 而页面上看不出任何异常。现在停笔 1.5s 后自动按新配置重启（重启是
    // 「先停、等端口释放、再起」，见 FGS 的 runRestart）；网关没在跑时
    // 服务端是 no-op（不会顺手把网关拉起来）。
    var portText by remember { mutableStateOf(GatewayPrefs.port(ctx).toString()) }
    // 「上次已生效」的端口：防抖的基准。必须记住它而不是跟落盘值比 ——
    // 首次组合时 LaunchedEffect 也会跑一次，没有这个基准的话，打开本页
    // 1.5s 后就会无故重启一次网关。改回原值也要重启（网关此刻在新口上）。
    var appliedPort by remember { mutableStateOf(GatewayPrefs.port(ctx).toString()) }
    LaunchedEffect(portText) {
        if (portText == appliedPort) return@LaunchedEffect
        delay(1500)
        if (portText.toIntOrNull() in 1..65535) {
            AgateForegroundService.restart(ctx)
            appliedPort = portText
        }
    }

    Column(
        modifier = modifier
            .fillMaxSize()
            .background(C.bg)
            .statusBarsPadding()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 16.dp),
    ) {
        Spacer(Modifier.height(6.dp))
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Icon(
                Icons.Filled.Menu,
                contentDescription = "打开侧栏",
                tint = C.textMain,
                modifier = Modifier.size(24.dp).clickable(onClick = onOpenDrawer),
            )
            Spacer(Modifier.width(10.dp))
            Text("网关控制", style = AgateType.PageTitle, color = C.textMain)
        }
        Spacer(Modifier.height(4.dp))
        AgateBadge("M0/M1 验收 · 常驻保留", BadgeTone.AMBER)
        Spacer(Modifier.height(12.dp))

        val link = GatewayRepository.link.collectAsState().value
        val recentEvents = GatewayRepository.events.collectAsState().value

        // ── ① 状态条：只写**真算过的** ─────────────────────────────
        // 仓里没有「网关进程是否在跑」的实时状态源（服务内部状态读不到），
        // 所以不写「运行中」这种猜出来的结论；只给 SSE 链路状态与配置值。
        SectionCard {
            Row(verticalAlignment = Alignment.CenterVertically) {
                AgateBadge(linkLabel(link), linkTone(link), withDot = true)
                Spacer(Modifier.width(10.dp))
                Text("配置端口 :$portText", style = AgateType.Caption, color = C.textSub)
            }
            Spacer(Modifier.height(6.dp))
            Text(
                if (bindLan) "监听 0.0.0.0（局域网）" else "监听 127.0.0.1（仅回环）",
                style = AgateType.Caption,
                color = C.textDim,
            )
        }
        Spacer(Modifier.height(12.dp))

        // ── ② 操作卡：主动作实心、反动作描边，互斥动作仍都保留 ────
        // 没有运行态可判哪个该禁用，所以只降视觉权重，不擅自隐藏。
        SectionCard {
            SectionLabel("网关")
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                RowButton("启动网关", primary = true, modifier = Modifier.weight(1f)) {
                    AgateForegroundService.start(ctx)
                    append("→ 已请求启动，见通知栏；几秒后点验收探针")
                }
                RowButton("停止网关", modifier = Modifier.weight(1f)) {
                    AgateForegroundService.stop(ctx)
                    GatewayRepository.stop()
                    append("→ 已请求停止（事件流已断开）")
                }
            }
            Spacer(Modifier.height(12.dp))
            SectionLabel("事件流")
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                RowButton("连接事件流", primary = true, modifier = Modifier.weight(1f)) {
                    GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
                    GatewayRepository.start()
                }
                RowButton("断开事件流", modifier = Modifier.weight(1f)) {
                    GatewayRepository.stop()
                }
            }
        }
        Spacer(Modifier.height(12.dp))

        // ── ③ 最近事件：时间对齐、单行省略、成败分色 ───────────────
        // `RecentEvent.tsMs` 本来就在数据流里，改前完全没用；摘要里的 ✓/✗
        // 来自 `GatewayRepository.summarize`（2xx 判成功），不是界面自己猜。
        SectionCard {
            SectionLabel("最近事件")
            if (recentEvents.isEmpty()) {
                Text(
                    "尚无事件：连上后首帧 hello；转发完成 / 签到轮次 / 网关启动都会推到这里",
                    style = AgateType.BodyS,
                    color = C.textSub,
                )
            } else {
                recentEvents.take(5).forEach { ev ->
                    val tone = when {
                        ev.summary.startsWith("✗") || ev.summary.contains("失败") -> C.red
                        ev.summary.startsWith("✓") -> C.green
                        else -> C.textSub
                    }
                    Text(
                        "${fmtTime(ev.tsMs)}  ${ev.type} · ${ev.summary}",
                        style = AgateType.BodyS,
                        color = tone,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
        Spacer(Modifier.height(12.dp))

        // ── ④ 保活设置：日常会动，保持常驻 ────────────────────────
        SectionCard {
            SectionLabel("保活开关")
            OutlinedTextField(
                value = portText,
                onValueChange = { raw ->
                    val digits = raw.filter { it.isDigit() }.take(5)
                    portText = digits
                    digits.toIntOrNull()?.let { if (it in 1..65535) GatewayPrefs.setPort(ctx, it) }
                },
                label = { Text("内核端口（1-65535；运行中改它，停笔后自动重启生效）") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            ToggleRow(
                label = "局域网监听 0.0.0.0",
                subtitle = "运行中切换会自动按新配置重启；无 API Key 时 /v1 拒绝服务",
                checked = bindLan,
            ) { on ->
                bindLan = on
                GatewayPrefs.setBindLan(ctx, on)
                // 运行中的网关要立刻按新地址重绑 —— 旧实现把生效时机留给用户猜
                //（重启网关生效），改完开关监听还钉在旧地址上，LAN 地址展示了、
                // 外部设备却连不上。restart 在服务里判「确实在跑」才动，没跑时
                // 是 no-op（见 FGS 的 gatewayRunning）。
                AgateForegroundService.restart(ctx)
                append("局域网监听 = $on（运行中已按新配置重启）")
            }
            ToggleRow("开机自启", "BOOT_COMPLETED / MY_PACKAGE_REPLACED", bootStart) { on ->
                bootStart = on
                GatewayPrefs.setBootAutoStart(ctx, on)
                append("开机自启 = $on")
            }
            ToggleRow("签到闹钟", "默认 08:00, 20:00 北京时间", checkinAlarm) { on ->
                checkinAlarm = on
                GatewayPrefs.setCheckinAlarm(ctx, on)
                if (on) {
                    val at = AlarmScheduler.scheduleNext(ctx)
                    append(
                        if (at != null) {
                            "签到闹钟已注册，下次触发 → " + fmt(at) +
                                if (!AlarmScheduler.canExact(ctx)) "（非精确：未授精确闹钟权限）" else ""
                        } else {
                            "时刻表非法，未注册"
                        },
                    )
                } else {
                    AlarmScheduler.cancel(ctx)
                    append("签到闹钟已取消")
                }
            }
            // B 包⑤：后台任务调度独立于网关（进程活着就跑签到/凭证维护/模型刷新）
            var schedulerOn by remember { mutableStateOf(GatewayPrefs.schedulerOn(ctx)) }
            ToggleRow("后台任务调度", "独立于网关 · 进程活着就跑", schedulerOn) { on ->
                schedulerOn = on
                GatewayPrefs.setSchedulerOn(ctx, on)
                val r = runCatching { Gateway.schedulerSet(on) }.getOrNull()
                append(
                    if (r?.optBoolean("ok") == true) "后台调度 = $on"
                    else "后台调度 = $on（内核未响应：${r?.optString("error") ?: "无 Core"}）",
                )
            }
        }
        Spacer(Modifier.height(12.dp))

        // ── ⑤ 诊断区：M0/M1 验收工具，默认折叠 ─────────────────────
        AgateAccordion(
            title = "验收探针",
            modifier = Modifier.agateCard(CardShape),
            badge = { AgateBadge("4 项", BadgeTone.GRAY) },
        ) {
            Text(
                "按当前配置端口请求；chat 探针需先配上游账号",
                style = AgateType.Caption,
                color = C.textDim,
                modifier = Modifier.padding(bottom = 8.dp),
            )
            RowButton("Gateway.status()") {
                bg { "status → " + runCatching { Gateway.status().toString() }.getOrElse { "异常 ${it.message}" } }
            }
            RowButton("GET /health") { bg { probe("GET /health") } }
            RowButton("GET /v1/models") { bg { probe("GET /v1/models") } }
            RowButton("POST /v1/chat/completions") {
                bg {
                    val body = JSONObject()
                        .put("model", "auto")
                        .put("stream", false)
                        .put(
                            "messages",
                            listOf(mapOf("role" to "user", "content" to "你好，请回复：Agate 链路 OK")),
                        )
                        .toString()
                    probe("POST /v1/chat/completions", body)
                }
            }
        }
        Spacer(Modifier.height(12.dp))

        AgateAccordion(
            title = "系统授权与链路",
            modifier = Modifier.agateCard(CardShape),
            badge = { AgateBadge("3 项", BadgeTone.GRAY) },
        ) {
            RowButton("电池白名单：状态/申请") {
                val ignoring = BatteryWhitelist.isIgnoring(ctx)
                val asked = BatteryWhitelist.requestIfNeeded(ctx)
                append(
                    when {
                        ignoring -> "已在电池优化白名单（Doze 不限网）"
                        asked -> "已拉起系统申请弹窗，请选「允许」"
                        else -> "未知状态"
                    },
                )
            }
            RowButton("精确闹钟权限：状态/申请") {
                append(
                    if (AlarmScheduler.canExact(ctx)) "精确闹钟可用（setExactAndAllowWhileIdle）"
                    else {
                        BatteryWhitelist.requestExactAlarmIfNeeded(ctx)
                        "未授权精确闹钟 → 已跳设置页（已降级为 setAndAllowWhileIdle）"
                    },
                )
            }
            RowButton("立即触发 scheduled_tick（闹钟同款链路）") {
                AgateForegroundService.tick(ctx)
                append("→ 已发 tick（见 logcat tag Agate）")
            }
        }
        Spacer(Modifier.height(12.dp))

        AgateAccordion(
            title = "输出",
            modifier = Modifier.agateCard(CardShape),
            badge = {
                lastOutput?.let {
                    Text(
                        it.trim().take(18),
                        style = AgateType.Caption,
                        color = C.textDim,
                        maxLines = 1,
                    )
                }
            },
        ) {
            Text(
                output,
                style = AgateType.BodyS.copy(fontWeight = FontWeight.Normal),
                color = C.textSub,
            )
        }
        Spacer(Modifier.height(28.dp))

        if (output.startsWith("Agate 网关控制就绪")) {
            bg { "Gateway.version() → " + runCatching { Gateway.version() }.getOrElse { "未加载：${it.message}" } }
        }
    }
}

private val CardShape = RoundedCornerShape(20.dp)

private fun pathOf(label: String): String = when {
    label.startsWith("GET /health") -> "/health"
    label.startsWith("GET /v1/models") -> "/v1/models"
    else -> "/v1/chat/completions"
}

private fun fmt(epochMs: Long): String =
    SimpleDateFormat("yyyy-MM-dd HH:mm:ss", Locale.CHINA).format(Date(epochMs))

private fun fmtTime(epochMs: Long): String =
    SimpleDateFormat("HH:mm:ss", Locale.CHINA).format(Date(epochMs))

/** SSE 链路状态的文案与色调只有这一处（状态条读它，别再各写一遍 when）。 */
private fun linkLabel(state: GatewayRepository.LinkState): String = when (state) {
    GatewayRepository.LinkState.CONNECTED -> "SSE 已连接"
    GatewayRepository.LinkState.CONNECTING -> "连接中…"
    GatewayRepository.LinkState.RETRYING -> "重连中…"
    GatewayRepository.LinkState.POLLING -> "轮询降级（/health 5s）"
    GatewayRepository.LinkState.DISCONNECTED -> "未连接"
}

private fun linkTone(state: GatewayRepository.LinkState): BadgeTone = when (state) {
    GatewayRepository.LinkState.CONNECTED -> BadgeTone.GREEN
    GatewayRepository.LinkState.CONNECTING,
    GatewayRepository.LinkState.RETRYING,
    -> BadgeTone.AMBER
    GatewayRepository.LinkState.POLLING -> BadgeTone.BLUE
    GatewayRepository.LinkState.DISCONNECTED -> BadgeTone.RED
}

/** 统一分组容器：纯黑下不投影，靠 cardBg + 1dp 描边分层。 */
@Composable
private fun SectionCard(content: @Composable () -> Unit) {
    val C = agateColors()
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .agateCard(CardShape)
            .background(C.cardBg, CardShape)
            .border(1.dp, C.border, CardShape)
            .padding(14.dp),
    ) { content() }
}

@Composable
private fun SectionLabel(text: String) {
    val C = agateColors()
    Text(
        text,
        style = AgateType.SectionTitle,
        color = C.textSub,
        modifier = Modifier.padding(bottom = 8.dp),
    )
}

/**
 * 主/次按钮：只有主动作用实心 accent，其余一律描边。
 * `modifier` 允许调用方在 Row 里给 weight(1f) 做两列。
 */
@Composable
private fun RowButton(
    label: String,
    primary: Boolean = false,
    modifier: Modifier = Modifier,
    onClick: () -> Unit,
) {
    val C = agateColors()
    val shape = RoundedCornerShape(12.dp)
    val base = modifier.fillMaxWidth().padding(bottom = 6.dp)
    if (primary) {
        Button(
            onClick = onClick,
            modifier = base,
            shape = shape,
            colors = ButtonDefaults.buttonColors(
                containerColor = C.accent,
                contentColor = C.onAccent,
            ),
        ) { Text(label, style = AgateType.BodyS, maxLines = 1) }
    } else {
        OutlinedButton(
            onClick = onClick,
            modifier = base,
            shape = shape,
            border = BorderStroke(1.dp, C.border),
            colors = ButtonDefaults.outlinedButtonColors(contentColor = C.textSub),
        ) { Text(label, style = AgateType.BodyS, maxLines = 1) }
    }
}

@Composable
private fun ToggleRow(label: String, subtitle: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    val C = agateColors()
    Row(
        Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(label, style = AgateType.Body, color = C.textMain)
            Text(subtitle, style = AgateType.Caption, color = C.textDim)
        }
        Switch(checked = checked, onCheckedChange = onChange)
    }
}
