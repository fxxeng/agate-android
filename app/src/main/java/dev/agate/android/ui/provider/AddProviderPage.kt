package dev.agate.android.ui.provider

import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
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
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import android.content.Intent
import android.net.Uri
import dev.agate.android.data.AuthChannel
import dev.agate.android.data.AccountOps
import dev.agate.android.data.CustomProviderOps
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.LoginClient
import dev.agate.android.data.LoginKind
import dev.agate.android.data.ProviderPreset
import dev.agate.android.data.PresetRegistry
import dev.agate.core.GatewayPrefs
import dev.agate.android.auth.AuthWebViewSheet
import dev.agate.android.auth.LoginSession
import dev.agate.android.ui.components.AgateSearchBar
import dev.agate.android.ui.components.AgateProviderAvatar
import dev.agate.android.ui.components.AgateAvatar
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateChip
import dev.agate.android.ui.components.AgateFormInput
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/**
 * 「新增提供商」子页（M3-3R，对齐 v44 sub-addprovider）：
 * - ptabs：官方 AGENT（中转 / 自定义为占位，内核 custom providers 后续接）
 * - pick-cur 当前选中卡 + 「更换 ❯」底部弹层选择器 + 最近添加快捷 chips
 * - 凭据卡按登录通道 inline 表单：网页登录（回跳+轮询）/ 手机验证码 / 粘贴凭证
 */
@Composable
fun AddProviderPage(onBack: () -> Unit, initialProviderId: String? = null) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    var current by remember {
        mutableStateOf<ProviderPreset?>(
            initialProviderId?.let { id -> PresetRegistry.ALL.find { it.id == id } }
        )
    }
    var showPicker by remember { mutableStateOf(false) }
    var loginSession by remember { mutableStateOf<LoginSession?>(null) }

    DisposableEffect(Unit) {
        LoginClient.configure(GatewayPrefs.apiKey(ctx))
        AccountOps.configure(GatewayPrefs.apiKey(ctx))
        CustomProviderOps.configure(GatewayPrefs.apiKey(ctx))
        onDispose { }
    }

    Column(Modifier.fillMaxSize().background(C.bg).statusBarsPadding().navigationBarsPadding()) {
        // 顶栏（v44 .header-bar）：返回 + 标题
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(onClick = onBack) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "返回", tint = C.textMain)
            }
            Text("新增提供商", color = C.textMain, fontSize = 18.sp, fontWeight = FontWeight.Bold)
        }

        Column(
            Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp)
                .padding(bottom = 24.dp)
        ) {
            Spacer(Modifier.height(8.dp))

            // ptabs：v44 胶囊页签（官方 / 中转 / 自定义），中转与自定义为占位
            var tab by remember { mutableStateOf(0) }
            Row(
                modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                listOf("官方 AGENT · ${PresetRegistry.ALL.size}", "中转 API", "自定义", "导入 · 更多").forEachIndexed { i, label ->
                    val active = tab == i
                    Text(
                        label,
                        maxLines = 1,
                        color = if (active) C.onAccent else C.textSub,
                        fontSize = 13.sp,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier
                            .background(
                                if (active) C.accent else C.insetBg,
                                RoundedCornerShape(999.dp),
                            )
                            .border(
                                1.dp,
                                if (active) Color.Transparent else C.border,
                                RoundedCornerShape(999.dp),
                            )
                            .clickable { tab = i }
                            .padding(horizontal = 14.dp, vertical = 9.dp),
                    )
                }
            }

            Spacer(Modifier.height(12.dp))

            if (tab == 0) {
                // ── 选择提供商卡：pick-cur + 最近 chips ──
                val picked = current
                Column(
                    Modifier
                        .fillMaxWidth()
                        .background(C.cardBg, RoundedCornerShape(16.dp))
                        .border(1.dp, C.border, RoundedCornerShape(16.dp))
                        .padding(14.dp),
                ) {
                    Text("选择提供商", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                    Spacer(Modifier.height(10.dp))

                    val shown = picked
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .background(C.insetBg, RoundedCornerShape(14.dp))
                            .border(1.dp, C.accent.copy(alpha = 0.45f), RoundedCornerShape(14.dp))
                            .clickable { showPicker = true }
                            .padding(horizontal = 12.dp, vertical = 10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        if (shown == null) {
                            Column(Modifier.weight(1f)) {
                                Text("选择要添加的家", color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                                Text("24 家官方 Agent · 点此打开选择器", color = C.textSub, fontSize = 12.sp)
                            }
                        } else {
                            AgateProviderAvatar(providerId = shown.id, label = shown.label, size = 40.dp)
                            Spacer(Modifier.width(12.dp))
                            Column(Modifier.weight(1f)) {
                                Text(shown.label, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                                Text("${channelLabel(shown)} · ${shown.desc}", color = C.textSub, fontSize = 12.sp)
                            }
                        }
                        Text("更换 ❯", color = C.accent, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
                    }

                    // v44 .rchips「最近」：有记录才显示，没有则整行隐藏（按你的要求）。
                    // 数据来自 GatewayPrefs.recentProviders —— 每次添加成功时 push。
                    val recents = GatewayPrefs.recentProviders(ctx)
                        .mapNotNull { id -> PresetRegistry.ALL.find { it.id == id } }
                        .take(4)
                    if (recents.isNotEmpty()) {
                        Spacer(Modifier.height(10.dp))
                        Row(
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                        ) {
                            Text("最近", color = C.textDim, fontSize = 12.sp)
                            recents.forEach { p ->
                                AgateChip(
                                    text = p.label,
                                    selected = current?.id == p.id,
                                    onClick = { current = p },
                                )
                            }
                        }
                    }
                }

                // ── 凭据卡：按通道 inline 表单 ──
                if (picked != null) {
                    Spacer(Modifier.height(12.dp))
                    Column(
                        Modifier
                            .fillMaxWidth()
                            .background(C.cardBg, RoundedCornerShape(16.dp))
                            .border(1.dp, C.border, RoundedCornerShape(16.dp))
                            .padding(14.dp),
                    ) {
                        Text("凭据", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                        Spacer(Modifier.height(10.dp))

                        // 添加成功后的统一收尾（三条通道共用）
                        val onAdded: (Boolean, String) -> Unit = { ok, msg ->
                            if (ok) {
                                GatewayPrefs.pushRecentProvider(ctx, picked.id)
                                GatewayRepository.refreshOnce()
                                Toast.makeText(ctx, msg, Toast.LENGTH_LONG).show()
                                onBack()
                            }
                        }
                        // 通道按内核核实结果渲染（IMPORT_DESKTOP 在 Android 不出现）
                        val usable = picked.channels.filter { it != AuthChannel.IMPORT_DESKTOP }
                        var channel by remember(picked.id) { mutableStateOf(usable.first()) }
                        ChannelChips(usable, channel) { channel = it }
                        Spacer(Modifier.height(12.dp))

                        when (channel) {
                            AuthChannel.EMBEDDED -> WebLoginForm(preset = picked, onStart = { p ->
                                scope.launch {
                                    when (val r = LoginClient.start(p.id)) {
                                        is LoginClient.StartResult.Started ->
                                            loginSession = LoginSession(r.provider, r.state, r.authUrl)
                                        is LoginClient.StartResult.Failed ->
                                            Toast.makeText(ctx, r.reason, Toast.LENGTH_LONG).show()
                                    }
                                }
                            })
                            AuthChannel.EXTERNAL, AuthChannel.DEVICE_CODE ->
                                ExternalLoginForm(preset = picked, onDone = onAdded)
                            AuthChannel.SMS -> SmsLoginForm(preset = picked, onDone = onAdded)
                            AuthChannel.PASTE -> PasteLoginForm(preset = picked, onDone = onAdded)
                            AuthChannel.IMPORT_FILE -> ImportFileNote()
                            AuthChannel.IMPORT_DESKTOP -> Unit
                        }
                    }
                } else {
                    Spacer(Modifier.height(12.dp))
                    Text(
                        "选择提供商后在此完成登录 · 凭据失效自动标记退避并通知",
                        color = C.textSub,
                        fontSize = 12.sp,
                    )
                }
            } else {
                Spacer(Modifier.height(12.dp))
                val done: (Boolean, String) -> Unit = { ok, msg ->
                    Toast.makeText(ctx, msg, Toast.LENGTH_LONG).show()
                    if (ok) {
                        GatewayRepository.refreshOnce()
                        onBack()
                    }
                }
                when (tab) {
                    1 -> RelayForm(onDone = done)
                    2 -> CustomForm(onDone = done)
                    else -> ImportPlaceholder()
                }
            }
        }
    }

    // ── 选择器弹层（v44 .sheet：78% 高度、pk-row 行 + 分节）──
    if (showPicker) {
        ProviderPickerSheet(
            currentId = current?.id,
            onPick = {
                current = it
                showPicker = false
            },
            onDismiss = { showPicker = false },
        )
    }

    // 网页登录 WebView（复用 M3-3 引擎）
    loginSession?.let { session ->
        AuthWebViewSheet(
            session = session,
            onDismiss = { loginSession = null },
            onResult = { success, message ->
                loginSession = null
                Toast.makeText(ctx, message, Toast.LENGTH_LONG).show()
                if (success) {
                    current?.let { GatewayPrefs.pushRecentProvider(ctx, it.id) }
                    GatewayRepository.refreshOnce()
                    onBack()
                }
            },
        )
    }
}

/** 名称 → 两字母头像（v44 CP/CB/CA 风格：取前两个词首字母，中文取前两字）。 */
internal fun initialsOf(label: String): String {
    val words = label.split(' ', '　').filter { it.isNotBlank() }
    return if (words.size >= 2) {
        words.take(2).map { it.first().uppercaseChar() }.joinToString("")
    } else {
        label.take(2).uppercase()
    }
}

internal fun channelLabel(p: ProviderPreset): String = when (p.kind) {
    LoginKind.WEBVIEW -> "网页登录 · 回跳 + 轮询"
    LoginKind.SMS -> "手机验证码登录"
    LoginKind.PASTE -> "粘贴凭证"
}

/* ── 网页登录通道：hint + 主按钮 ── */
@Composable
private fun WebLoginForm(preset: ProviderPreset, onStart: (ProviderPreset) -> Unit) {
    val C = agateColors()
    Text(
        "${preset.desc} · 登录后在浏览器完成授权 · 回跳与轮询双通道赛跑",
        color = C.textSub,
        fontSize = 12.sp,
    )
    Spacer(Modifier.height(12.dp))
    Button(
        onClick = { onStart(preset) },
        modifier = Modifier.fillMaxWidth(),
        colors = ButtonDefaults.buttonColors(containerColor = C.accent, contentColor = C.onAccent),
    ) { Text("开始网页登录", fontWeight = FontWeight.SemiBold) }
}

/* ── 验证码通道：手机号 → 发码（60s 倒计时）→ 输码登录 ── */
@Composable
private fun SmsLoginForm(preset: ProviderPreset, onDone: (Boolean, String) -> Unit) {
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    var phone by remember { mutableStateOf("") }
    var code by remember { mutableStateOf("") }
    var name by remember { mutableStateOf("") }
    var msgid by remember { mutableStateOf<String?>(null) }
    var sent by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var countdown by remember { mutableStateOf(0) }
    var hint by remember { mutableStateOf<String?>(null) }
    val C = agateColors()

    LaunchedEffect(sent) {
        if (sent && countdown == 0) countdown = 60
        while (isActive && countdown > 0) {
            delay(1_000L)
            countdown -= 1
        }
    }

    AgateFormInput(value = phone, onValueChange = { phone = it.trim() }, hint = "手机号", keyboardType = KeyboardType.Phone)
    Spacer(Modifier.height(8.dp))
    Row(verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.weight(1f)) {
            AgateFormInput(
                value = code,
                onValueChange = { code = it.trim() },
                hint = "验证码",
                keyboardType = KeyboardType.Number,
            )
        }
        Spacer(Modifier.width(10.dp))
        Text(
            if (countdown > 0) "${countdown}s" else if (sent) "重发" else "发送验证码",
            color = if (countdown == 0 && !busy && phone.length >= 5) C.accent else C.textDim,
            fontSize = 12.sp,
            fontWeight = FontWeight.SemiBold,
            modifier = Modifier
                .background(C.insetBg, RoundedCornerShape(12.dp))
                .border(1.dp, C.border, RoundedCornerShape(12.dp))
                .clickable(enabled = countdown == 0 && !busy && phone.length >= 5) {
                    busy = true; hint = null
                    scope.launch {
                        val r = AccountOps.smsSend(preset.id, phone)
                        busy = false
                        if (r.ok) {
                            sent = true
                            msgid = r.data?.optString("msgid")?.ifBlank { null }
                            hint = "验证码已发送，请查收短信"
                        } else hint = r.error ?: "发送失败"
                    }
                }
                .padding(horizontal = 14.dp, vertical = 12.dp),
        )
    }
    Spacer(Modifier.height(8.dp))
    AgateFormInput(value = name, onValueChange = { name = it }, hint = "备注名（可选）")
    hint?.let {
        Spacer(Modifier.height(8.dp))
        Text(it, color = if (it.contains("已发送")) C.green else C.red, fontSize = 12.sp)
    }
    Spacer(Modifier.height(12.dp))
    Button(
        onClick = {
            busy = true; hint = null
            scope.launch {
                val r = AccountOps.smsVerify(preset.id, phone, code, msgid, name.ifBlank { null })
                busy = false
                if (r.ok) onDone(true, "已添加 ${preset.label} 账号")
                else hint = r.error ?: "登录失败"
            }
        },
        enabled = !busy && sent && phone.length >= 5 && code.length >= 4,
        modifier = Modifier.fillMaxWidth(),
        colors = ButtonDefaults.buttonColors(containerColor = C.accent, contentColor = C.onAccent),
    ) { Text(if (busy) "处理中…" else "登录", fontWeight = FontWeight.SemiBold) }
}

/* ── 粘贴凭证通道：按家出表单（kuku Cookie / commandcode Key / antigravity token）── */
@Composable
private fun PasteLoginForm(preset: ProviderPreset, onDone: (Boolean, String) -> Unit) {
    val C = agateColors()
    var primary by remember { mutableStateOf("") }
    var secondary by remember { mutableStateOf("") }
    var name by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var hint by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()

    // 粘贴提示统一来自 PresetRegistry（按家核实过），不再在此硬编码
    val primaryHint: String = preset.pasteHint ?: "凭证"
    val secondaryHint: String? = preset.pasteSecondaryHint
    val minLines: Int = if (preset.pasteMultiline) maxOf(3, primaryHint.length / 24) else 1

    AgateFormInput(value = primary, onValueChange = { primary = it }, hint = primaryHint, singleLine = false, minLines = minLines)
    if (secondaryHint != null) {
        Spacer(Modifier.height(8.dp))
        AgateFormInput(value = secondary, onValueChange = { secondary = it.trim() }, hint = secondaryHint)
    }
    Spacer(Modifier.height(8.dp))
    AgateFormInput(value = name, onValueChange = { name = it }, hint = "备注名（可选）")
    hint?.let {
        Spacer(Modifier.height(8.dp))
        Text(it, color = C.red, fontSize = 12.sp)
    }
    Spacer(Modifier.height(12.dp))
    Button(
        onClick = {
            busy = true; hint = null
            scope.launch {
                val r = AccountOps.addAccountFor(preset.id, primary, secondary.ifBlank { null }, name.ifBlank { null })
                busy = false
                if (r.ok) onDone(true, "已添加 ${preset.label} 账号")
                else hint = r.error ?: "添加失败"
            }
        },
        enabled = !busy && primary.isNotBlank(),
        modifier = Modifier.fillMaxWidth(),
        colors = ButtonDefaults.buttonColors(containerColor = C.accent, contentColor = C.onAccent),
    ) { Text(if (busy) "校验中…" else "添加", fontWeight = FontWeight.SemiBold) }
}

/** v44 .sheet 选择器：按通道分节（网页 / 验证码 / 粘贴），pk-row 行 + 选中勾。 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ProviderPickerSheet(
    currentId: String?,
    onPick: (ProviderPreset) -> Unit,
    onDismiss: () -> Unit,
) {
    val C = agateColors()
    // v44 .sheet：高度 78%，不到顶；顶部标题 + 关闭、搜索、通道筛选、列表滚动
    val sheetH = (LocalConfiguration.current.screenHeightDp * 0.78f).dp
    var q by remember { mutableStateOf("") }
    var kindFilter by remember { mutableStateOf<LoginKind?>(null) }

    val filtered = PresetRegistry.ALL.filter { p ->
        (kindFilter == null || p.kind == kindFilter) &&
            (q.isBlank() || p.label.contains(q, ignoreCase = true) || p.desc.contains(q, ignoreCase = true))
    }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        // true：跳过「半展开」中间态，直接落在内容高度（=78% 屏高）；
        // 内容已固定 78%，所以不会再顶到顶，也不会出现两段式拖动。
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = C.cardBg,
    ) {
        Column(Modifier.fillMaxWidth().height(sheetH)) {
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    "选择官方 AGENT",
                    color = C.textMain,
                    fontSize = 16.sp,
                    fontWeight = FontWeight.Bold,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    "关闭",
                    color = C.accent,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.clickable(onClick = onDismiss).padding(6.dp),
                )
            }
            Spacer(Modifier.height(8.dp))
            Box(Modifier.padding(horizontal = 16.dp)) {
                AgateSearchBar(value = q, onValueChange = { q = it }, hint = "搜索提供商 / 登录方式")
            }
            Spacer(Modifier.height(10.dp))
            // 通道筛选 chips
            Row(
                Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 16.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                val chips = listOf<Pair<String, LoginKind?>>(
                    "全部" to null,
                    "网页登录" to LoginKind.WEBVIEW,
                    "手机验证码" to LoginKind.SMS,
                    "粘贴凭证" to LoginKind.PASTE,
                )
                chips.forEach { (label, kind) ->
                    val active = kindFilter == kind
                    Text(
                        label,
                        maxLines = 1,
                        color = if (active) C.onAccent else C.textSub,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier
                            .background(if (active) C.accent else C.insetBg, RoundedCornerShape(999.dp))
                            .border(1.dp, if (active) Color.Transparent else C.border, RoundedCornerShape(999.dp))
                            .clickable { kindFilter = kind }
                            .padding(horizontal = 14.dp, vertical = 8.dp),
                    )
                }
            }
            Spacer(Modifier.height(8.dp))
            Text(
                "${if (kindFilter == null) "全部" else "筛选"} · ${filtered.size}",
                color = C.textDim,
                fontSize = 12.sp,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )
            Column(
                Modifier
                    .weight(1f)
                    .verticalScroll(rememberScrollState())
                    .padding(horizontal = 16.dp)
                    .padding(bottom = 20.dp),
            ) {
                filtered.forEach { p ->
                    Row(
                        Modifier.fillMaxWidth().clickable { onPick(p) }.padding(vertical = 10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        AgateProviderAvatar(providerId = p.id, label = p.label, size = 38.dp, corner = 12.dp)
                        Spacer(Modifier.width(12.dp))
                        Column(Modifier.weight(1f)) {
                            Text(p.label, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                            Text(p.desc, color = C.textSub, fontSize = 12.sp)
                        }
                        if (p.id == currentId) {
                            Text("✓", color = C.accent, fontSize = 16.sp, fontWeight = FontWeight.Bold)
                        }
                    }
                }
            }
        }
    }
}

/* ══════════ 中转 API（对齐 v44 data-for="relay"）══════════ */
@Composable
private fun RelayForm(onDone: (Boolean, String) -> Unit) {
    val C = agateColors()
    val scope = rememberCoroutineScope()
    var relayType by remember { mutableStateOf(0) }   // OpenAI 兼容 / NewAPI / 聚合面板（展示口径）
    var name by remember { mutableStateOf("") }
    var baseUrl by remember { mutableStateOf("") }
    var key by remember { mutableStateOf("") }
    var noAuth by remember { mutableStateOf(false) }
    var protocol by remember { mutableStateOf(CustomProviderOps.Protocol.OPENAI) }
    var scheme by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    var hint by remember { mutableStateOf<String?>(null) }

    val valid = name.isNotBlank() && baseUrl.startsWith("http") && (noAuth || key.isNotBlank())

    FormCard("接入信息") {
        AgateFormInput(value = name, onValueChange = { name = it }, hint = "名称，如 NewAPI 中转")
        Spacer(Modifier.height(8.dp))
        AgateFormInput(value = baseUrl, onValueChange = { baseUrl = it.trim() }, hint = "Base URL，如 https://api.example.com/v1")
        Spacer(Modifier.height(8.dp))
        AgateFormInput(
            value = if (noAuth) "" else key,
            onValueChange = { key = it.trim() },
            hint = if (noAuth) "已勾选无需鉴权" else "API Key（可带 Bearer 前缀）",
        )
        Spacer(Modifier.height(10.dp))
        ToggleRow(
            title = "该上游无需鉴权",
            desc = "本地 Ollama / 免费档勾上 · 鉴权头一个都不发，填 Key 自动取消",
            checked = noAuth,
            onChange = { on -> noAuth = on; if (on) key = "" },
        )
        Spacer(Modifier.height(12.dp))
        Text("类型", color = C.textSub, fontSize = 12.sp)
        Spacer(Modifier.height(8.dp))
        ChipRow(listOf("OpenAI 兼容", "NewAPI", "聚合面板"), relayType) { i ->
            relayType = i
            // 类型决定余额直查的默认方案（可再手改）
            scheme = when (i) {
                1 -> "newapi"
                2 -> "sub2api"
                else -> null
            }
        }
        Spacer(Modifier.height(12.dp))
        Text("协议", color = C.textSub, fontSize = 12.sp)
        Spacer(Modifier.height(8.dp))
        ProtocolChips(protocol) { protocol = it }
        Spacer(Modifier.height(10.dp))
        Text(
            "同一地址可换协议（如 DeepSeek 的 Anthropic 兼容端点）· 无需登录，Key 失效自动退避并通知",
            color = C.textDim, fontSize = 11.sp,
        )
    }

    Spacer(Modifier.height(12.dp))
    FormCard("余额直查（可选）") {
        Text(
            "按方案从上游读余额；选「不设置」则不做余额查询。",
            color = C.textSub, fontSize = 12.sp,
        )
        Spacer(Modifier.height(8.dp))
        WrapChips(
            options = listOf("不设置" to "") + CustomProviderOps.balanceSchemes,
            selected = scheme ?: "",
            onSelect = { scheme = it.ifBlank { null } },
        )
    }

    hint?.let {
        Spacer(Modifier.height(8.dp))
        Text(it, color = C.red, fontSize = 12.sp)
    }
    Spacer(Modifier.height(12.dp))
    SubmitButton(if (busy) "创建中…" else "创建并接入", valid && !busy) {
        busy = true; hint = null
        scope.launch {
            val r = CustomProviderOps.create(name, baseUrl, key, protocol, noAuth = noAuth, balanceScheme = scheme)
            busy = false
            if (r.ok) onDone(true, "已添加 " + name) else hint = r.error ?: "创建失败"
        }
    }
    Spacer(Modifier.height(6.dp))
    Text("保存后立即参与选号 · 额度信息在首次请求后回填", color = C.textDim, fontSize = 11.sp)
}

/* ══════════ 自定义（对齐 v44 #ap-custom）══════════ */
@Composable
private fun CustomForm(onDone: (Boolean, String) -> Unit) {
    val C = agateColors()
    val scope = rememberCoroutineScope()
    var name by remember { mutableStateOf("") }
    var baseUrl by remember { mutableStateOf("") }
    var key by remember { mutableStateOf("") }
    var noAuth by remember { mutableStateOf(false) }
    var autoFetch by remember { mutableStateOf(true) }
    var protocol by remember { mutableStateOf(CustomProviderOps.Protocol.OPENAI) }
    var busy by remember { mutableStateOf(false) }
    var hint by remember { mutableStateOf<String?>(null) }

    val valid = name.isNotBlank() && baseUrl.startsWith("http") && (noAuth || key.isNotBlank())

    FormCard("自定义提供商") {
        AgateFormInput(value = name, onValueChange = { name = it }, hint = "名称，如 我的上游")
        Spacer(Modifier.height(8.dp))
        AgateFormInput(value = baseUrl, onValueChange = { baseUrl = it.trim() }, hint = "Base URL · https://api.example.com/v1")
        Spacer(Modifier.height(8.dp))
        AgateFormInput(
            value = if (noAuth) "" else key,
            onValueChange = { key = it.trim() },
            hint = if (noAuth) "已勾选无需鉴权" else "API Key · sk-...",
        )
        Spacer(Modifier.height(10.dp))
        ToggleRow(
            title = "该上游无需鉴权",
            desc = "本地 Ollama / 免费档",
            checked = noAuth,
            onChange = { on -> noAuth = on; if (on) key = "" },
        )
        ToggleRow(
            title = "模型发现",
            desc = "创建后自动拉取 /v1/models",
            checked = autoFetch,
            onChange = { autoFetch = it },
        )
        Spacer(Modifier.height(12.dp))
        Text("协议", color = C.textSub, fontSize = 12.sp)
        Spacer(Modifier.height(8.dp))
        ProtocolChips(protocol) { protocol = it }
        Spacer(Modifier.height(10.dp))
        Text(
            "余额查询：内核仅支持预置方案（NewAPI / Sub2API / DeepSeek 等），自定义端点与 JSON 路径暂不支持。" +
                "能力位编辑在「模型 → 能力位」页。",
            color = C.textDim, fontSize = 11.sp,
        )
    }

    hint?.let {
        Spacer(Modifier.height(8.dp))
        Text(it, color = C.red, fontSize = 12.sp)
    }
    Spacer(Modifier.height(12.dp))
    SubmitButton(if (busy) "创建中…" else "创建并接入", valid && !busy) {
        busy = true; hint = null
        scope.launch {
            val r = CustomProviderOps.create(name, baseUrl, key, protocol, noAuth = noAuth)
            if (!r.ok) {
                busy = false
                hint = r.error ?: "创建失败"
                return@launch
            }
            // 模型发现：创建成功后按开关自动拉取
            if (autoFetch) {
                val pid = r.data?.optJSONObject("provider")?.optString("id").orEmpty()
                if (pid.isNotBlank()) runCatching { CustomProviderOps.fetchModels(pid) }
            }
            busy = false
            onDone(true, "已添加 " + name)
        }
    }
    Spacer(Modifier.height(6.dp))
    Text(
        "上游定位：仅 LLM 请求 + 模型列表 · 签到 / 续期 / 余额查询均不做 · 兼容 OpenAI /v1/chat/completions 即可接入",
        color = C.textDim, fontSize = 11.sp,
    )
}

/* ══════════ 导入 · 更多（占位，说明 Android 语义）══════════ */
@Composable
private fun ImportPlaceholder() {
    val C = agateColors()
    FormCard("跨端凭证导入") {
        Text(
            "手机上扫不到 PC 的本机目录 · 在 PC 端导出后，经下面任一通道进手机：",
            color = C.textSub, fontSize = 12.sp,
        )
        Spacer(Modifier.height(10.dp))
        listOf(
            "① 上传凭证文件" to "cc-switch 备份 / auth.json / providers.json · 系统文件选择器",
            "② 局域网扫码 / 剪贴板粘贴" to "PC 导出 JSON 直接粘贴 · enc: 前缀自动解密",
            "③ 导入 SQLite Cookies" to "桌面浏览器导出的 Cookie 库 · 解析后按域名预填",
        ).forEach { (title, desc) ->
            Row(Modifier.fillMaxWidth().padding(vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(title, color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                    Text(desc, color = C.textSub, fontSize = 12.sp)
                }
                AgateBadge("待接入", BadgeTone.AMBER)
            }
        }
        Spacer(Modifier.height(8.dp))
        Text("导入通道依赖 Android SAF / 剪贴板能力，后续版本接入。", color = C.textDim, fontSize = 11.sp)
    }
}

/* ══════════ 公用小件 ══════════ */
@Composable
private fun FormCard(title: String, content: @Composable androidx.compose.foundation.layout.ColumnScope.() -> Unit) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(16.dp))
            .border(1.dp, C.border, RoundedCornerShape(16.dp))
            .padding(14.dp),
    ) {
        Text(title, color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(10.dp))
        content()
    }
}

@Composable
private fun ChipRow(options: List<String>, selected: Int, onSelect: (Int) -> Unit) {
    val C = agateColors()
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        options.forEachIndexed { i, label ->
            val active = selected == i
            Text(
                label,
                color = if (active) C.onAccent else C.textSub,
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(if (active) C.accent else C.insetBg, RoundedCornerShape(999.dp))
                    .border(1.dp, if (active) Color.Transparent else C.border, RoundedCornerShape(999.dp))
                    .clickable { onSelect(i) }
                    .padding(horizontal = 14.dp, vertical = 8.dp),
            )
        }
    }
}

@Composable
private fun ProtocolChips(selected: CustomProviderOps.Protocol, onSelect: (CustomProviderOps.Protocol) -> Unit) {
    val C = agateColors()
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        CustomProviderOps.Protocol.entries.forEach { p ->
            val active = selected == p
            Text(
                p.label,
                color = if (active) C.onAccent else C.textSub,
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(if (active) C.accent else C.insetBg, RoundedCornerShape(999.dp))
                    .border(1.dp, if (active) Color.Transparent else C.border, RoundedCornerShape(999.dp))
                    .clickable { onSelect(p) }
                    .padding(horizontal = 14.dp, vertical = 8.dp),
            )
        }
    }
}

@Composable
private fun WrapChips(options: List<Pair<String, String>>, selected: String, onSelect: (String) -> Unit) {
    val C = agateColors()
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        options.chunked(3).forEach { row ->
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                row.forEach { (value, label) ->
                    val active = selected == value
                    Text(
                        label,
                        color = if (active) C.onAccent else C.textSub,
                        fontSize = 11.sp,
                        modifier = Modifier
                            .background(if (active) C.accent else C.insetBg, RoundedCornerShape(999.dp))
                            .border(1.dp, if (active) Color.Transparent else C.border, RoundedCornerShape(999.dp))
                            .clickable { onSelect(value) }
                            .padding(horizontal = 12.dp, vertical = 7.dp),
                    )
                }
            }
        }
    }
}

@Composable
private fun ToggleRow(title: String, desc: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    val C = agateColors()
    Row(Modifier.fillMaxWidth().padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(title, color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
            Text(desc, color = C.textSub, fontSize = 11.sp)
        }
        androidx.compose.material3.Switch(checked = checked, onCheckedChange = onChange)
    }
}

@Composable
private fun SubmitButton(text: String, enabled: Boolean, onClick: () -> Unit) {
    val C = agateColors()
    Button(
        onClick = onClick,
        enabled = enabled,
        modifier = Modifier.fillMaxWidth(),
        colors = ButtonDefaults.buttonColors(containerColor = C.accent, contentColor = C.onAccent),
    ) { Text(text, fontWeight = FontWeight.SemiBold) }
}

/* ── 通道 chips（按家可用性渲染）── */
@Composable
private fun ChannelChips(
    options: List<AuthChannel>,
    selected: AuthChannel,
    onSelect: (AuthChannel) -> Unit,
) {
    val C = agateColors()
    Row(
        Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        options.forEach { ch ->
            val active = ch == selected
            Text(
                ch.label,
                maxLines = 1,
                color = if (active) C.onAccent else C.textSub,
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(if (active) C.accent else C.insetBg, RoundedCornerShape(999.dp))
                    .border(1.dp, if (active) Color.Transparent else C.border, RoundedCornerShape(999.dp))
                    .clickable { onSelect(ch) }
                    .padding(horizontal = 12.dp, vertical = 8.dp),
            )
        }
    }
}

/* ── 浏览器登录（含设备码类）：跳系统浏览器 + 轮询 + 取消 ── */
@Composable
private fun ExternalLoginForm(preset: ProviderPreset, onDone: (Boolean, String) -> Unit) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    var state by remember { mutableStateOf<String?>(null) }
    var authUrl by remember { mutableStateOf<String?>(null) }
    var waiting by remember { mutableStateOf(false) }
    var seconds by remember { mutableStateOf(0) }
    var hint by remember { mutableStateOf<String?>(null) }

    fun openBrowser(url: String) {
        runCatching {
            ctx.startActivity(
                Intent(Intent.ACTION_VIEW, Uri.parse(url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            )
        }.onFailure { hint = "无法打开浏览器：${it.message}" }
    }

    // 每 2 秒问一次内核；Done/Failed 即落定
    LaunchedEffect(waiting, state) {
        if (!waiting) return@LaunchedEffect
        while (isActive && waiting) {
            delay(2_000L)
            seconds += 2
            val st = state ?: break
            when (val r = LoginClient.wait(st)) {
                is LoginClient.WaitResult.Done -> {
                    waiting = false
                    onDone(true, "已添加 ${preset.label} 账号")
                }
                is LoginClient.WaitResult.Failed -> {
                    waiting = false
                    hint = r.reason
                }
                else -> Unit
            }
        }
    }

    DisposableEffect(Unit) {
        onDispose { state?.let { s -> scope.launch { LoginClient.cancel(s) } } }
    }

    Text(
        if (preset.channels.contains(AuthChannel.DEVICE_CODE))
            "在浏览器完成设备授权（授权页会显示设备码，可复制到官网确认）"
        else "在系统浏览器完成授权 · 回到 App 自动继续",
        color = C.textSub, fontSize = 12.sp,
    )
    Spacer(Modifier.height(12.dp))

    if (!waiting) {
        Button(
            onClick = {
                hint = null
                scope.launch {
                    when (val r = LoginClient.start(preset.id)) {
                        is LoginClient.StartResult.Started -> {
                            state = r.state
                            authUrl = r.authUrl
                            seconds = 0
                            waiting = true
                            openBrowser(r.authUrl)
                        }
                        is LoginClient.StartResult.Failed -> hint = r.reason
                    }
                }
            },
            modifier = Modifier.fillMaxWidth(),
            colors = ButtonDefaults.buttonColors(containerColor = C.accent, contentColor = C.onAccent),
        ) { Text("在浏览器中打开", fontWeight = FontWeight.SemiBold) }
    } else {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text("等待授权 · 已等待 ${seconds}s", color = C.accent, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
            Text(
                "取消",
                color = C.red, fontSize = 13.sp, fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(C.redBg, RoundedCornerShape(9.dp))
                    .clickable {
                        val st = state
                        waiting = false
                        if (st != null) scope.launch { LoginClient.cancel(st) }
                    }
                    .padding(horizontal = 12.dp, vertical = 8.dp),
            )
        }
        authUrl?.let { url ->
            Spacer(Modifier.height(8.dp))
            Text(
                "没打开浏览器？点此重开",
                color = C.accent, fontSize = 12.sp,
                modifier = Modifier.clickable { openBrowser(url) },
            )
        }
    }
    hint?.let {
        Spacer(Modifier.height(8.dp))
        Text(it, color = C.red, fontSize = 12.sp)
    }
}

/* ── 导入凭证文件（下一轮接 SAF，先如实说明）── */
@Composable
private fun ImportFileNote() {
    val C = agateColors()
    Text(
        "从系统文件选择器导入桌面客户端导出的凭证文件（auth.json / providers.json / Cookie 库）。" +
            "文本 JSON 直接解析，二进制 Cookie 库走 base64。",
        color = C.textSub, fontSize = 12.sp,
    )
    Spacer(Modifier.height(8.dp))
    AgateBadge("下一轮接入", BadgeTone.AMBER)
}
