package dev.agate.android.ui.pages

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
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
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Icon
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.runtime.collectAsState
import dev.agate.android.data.GatewayRepository
import dev.agate.core.AgateForegroundService
import dev.agate.core.AlarmScheduler
import dev.agate.core.BatteryWhitelist
import dev.agate.core.Gateway
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
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
    val port = 8787
    var output by remember { mutableStateOf("Agate 网关控制就绪（内核版本待加载后显示）") }
    var bindLan by remember { mutableStateOf(GatewayPrefs.bindLan(ctx)) }
    var bootStart by remember { mutableStateOf(GatewayPrefs.bootAutoStart(ctx)) }
    var checkinAlarm by remember { mutableStateOf(GatewayPrefs.checkinAlarmEnabled(ctx)) }

    fun append(line: String) {
        output = "$output\n\n────\n$line"
    }

    fun bg(fn: suspend () -> String) = scope.launch { withContext(Dispatchers.Main) { append(fn()) } }

    fun probe(label: String, body: String? = null): String = try {
        val b = Request.Builder().url("http://127.0.0.1:$port${pathOf(label)}")
        if (body != null) b.post(body.toRequestBody("application/json".toMediaType())) else b.get()
        client.newCall(b.build()).execute().use { resp ->
            val text = resp.body?.string().orEmpty()
            "$label → HTTP ${resp.code}\n" + (if (text.length > 1200) text.take(1200) + "…" else text)
        }
    } catch (t: Throwable) {
        "$label → 失败：${t.message}"
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
        Spacer(Modifier.height(14.dp))

        SectionLabel("内核探测")
        RowButton("启动网关（前台服务）") {
            AgateForegroundService.start(ctx)
            append("→ 已请求启动，见通知栏；几秒后点下面的探测")
        }
        RowButton("停止网关") {
            AgateForegroundService.stop(ctx)
            GatewayRepository.stop()
            append("→ 已请求停止（事件流已断开）")
        }

        Spacer(Modifier.height(14.dp))

        // ── M3 · SSE 事件总线（方案 §3.2 / O9）─────────────────────
        // SSE 为主 + 轮询降级的可视化验收位：状态徽标 + 最近事件流。
        SectionLabel("事件总线（M3 · SSE /api/events）")
        val link = GatewayRepository.link.collectAsState().value
        val recentEvents = GatewayRepository.events.collectAsState().value
        AgateBadge(
            when (link) {
                GatewayRepository.LinkState.CONNECTED -> "SSE 已连接"
                GatewayRepository.LinkState.CONNECTING -> "连接中…"
                GatewayRepository.LinkState.RETRYING -> "重连中…"
                GatewayRepository.LinkState.POLLING -> "轮询降级（/health 5s）"
                GatewayRepository.LinkState.DISCONNECTED -> "未连接"
            },
            when (link) {
                GatewayRepository.LinkState.CONNECTED -> BadgeTone.GREEN
                GatewayRepository.LinkState.CONNECTING,
                GatewayRepository.LinkState.RETRYING,
                -> BadgeTone.AMBER
                GatewayRepository.LinkState.POLLING -> BadgeTone.BLUE
                GatewayRepository.LinkState.DISCONNECTED -> BadgeTone.RED
            },
        )
        Spacer(Modifier.height(8.dp))
        RowButton("连接事件流") {
            GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
            GatewayRepository.start()
        }
        RowButton("断开事件流") {
            GatewayRepository.stop()
        }
        if (recentEvents.isEmpty()) {
            Text(
                "尚无事件：连上后首帧 hello；转发完成 / 签到轮次 / 网关启动都会推到这里",
                style = AgateType.BodyS,
                color = C.textSub,
            )
        } else {
            recentEvents.take(8).forEach { ev ->
                Text(
                    "· ${ev.type}｜${ev.summary}",
                    style = AgateType.BodyS,
                    color = C.textMain,
                )
            }
        }
        RowButton("Gateway.status()") {
            bg { "status → " + runCatching { Gateway.status().toString() }.getOrElse { "异常 ${it.message}" } }
        }
        RowButton("GET /health") { bg { probe("GET /health") } }
        RowButton("GET /v1/models") { bg { probe("GET /v1/models") } }
        RowButton("POST /v1/chat/completions（需先配上游账号）") {
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

        Spacer(Modifier.height(10.dp))
        SectionLabel("保活开关")
        // B 包①：内核端口单一来源（Application/FGS 共用）；改后重启网关生效
        var portText by remember { mutableStateOf(GatewayPrefs.port(ctx).toString()) }
        OutlinedTextField(
            value = portText,
            onValueChange = { raw ->
                val digits = raw.filter { it.isDigit() }.take(5)
                portText = digits
                digits.toIntOrNull()?.let { if (it in 1..65535) GatewayPrefs.setPort(ctx, it) }
            },
            label = { Text("内核端口（1-65535，重启网关生效）") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        ToggleRow(
            label = "局域网监听 0.0.0.0",
            subtitle = "重启网关生效；无 API Key 时 /v1 拒绝服务",
            checked = bindLan,
        ) { on ->
            bindLan = on
            GatewayPrefs.setBindLan(ctx, on)
            append("局域网监听 = $on")
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

        Spacer(Modifier.height(10.dp))
        SectionLabel("系统授权与链路")
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

        Spacer(Modifier.height(14.dp))
        SectionLabel("输出")
        Column(
            Modifier
                .fillMaxWidth()
                .background(C.insetBg, RoundedCornerShape(12.dp))
                .padding(12.dp),
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

private fun pathOf(label: String): String = when {
    label.startsWith("GET /health") -> "/health"
    label.startsWith("GET /v1/models") -> "/v1/models"
    else -> "/v1/chat/completions"
}

private fun fmt(epochMs: Long): String =
    SimpleDateFormat("yyyy-MM-dd HH:mm:ss", Locale.CHINA).format(Date(epochMs))

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

@Composable
private fun RowButton(label: String, onClick: () -> Unit) {
    Button(
        onClick = onClick,
        modifier = Modifier
            .fillMaxWidth()
            .padding(bottom = 6.dp),
    ) { Text(label, style = AgateType.BodyS) }
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
