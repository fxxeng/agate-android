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
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
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
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                listOf("官方 AGENT · ${PresetRegistry.ALL.size}", "中转 / 自定义").forEachIndexed { i, label ->
                    val active = tab == i
                    Text(
                        label,
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
                            .padding(horizontal = 16.dp, vertical = 9.dp),
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
                            AgateAvatar(name = shown.label, size = 40.dp, letters = initialsOf(shown.label))
                            Spacer(Modifier.width(12.dp))
                            Column(Modifier.weight(1f)) {
                                Text(shown.label, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                                Text("${channelLabel(shown)} · ${shown.desc}", color = C.textSub, fontSize = 12.sp)
                            }
                        }
                        Text("更换 ❯", color = C.accent, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
                    }

                    // 最近添加快捷 chips（v44 .rchips）
                    val recents = remember { GatewayPrefs.recentProviders(ctx) }
                        .mapNotNull { id -> PresetRegistry.ALL.find { it.id == id } }
                    if (recents.isNotEmpty()) {
                        Spacer(Modifier.height(10.dp))
                        Row(
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Text("最近", color = C.textDim, fontSize = 12.sp, modifier = Modifier.padding(top = 6.dp))
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
                        when (picked.kind) {
                            LoginKind.WEBVIEW -> WebLoginForm(preset = picked, onStart = { p ->
                                scope.launch {
                                    when (val r = LoginClient.start(p.id)) {
                                        is LoginClient.StartResult.Started ->
                                            loginSession = LoginSession(r.provider, r.state, r.authUrl)
                                        is LoginClient.StartResult.Failed ->
                                            Toast.makeText(ctx, r.reason, Toast.LENGTH_LONG).show()
                                    }
                                }
                            })
                            LoginKind.SMS -> SmsLoginForm(preset = picked, onDone = { ok, msg ->
                                if (ok) {
                                    GatewayPrefs.pushRecentProvider(ctx, picked.id)
                                    GatewayRepository.refreshOnce()
                                    Toast.makeText(ctx, msg, Toast.LENGTH_LONG).show()
                                    onBack()
                                }
                            })
                            LoginKind.PASTE -> PasteLoginForm(preset = picked, onDone = { ok, msg ->
                                if (ok) {
                                    GatewayPrefs.pushRecentProvider(ctx, picked.id)
                                    GatewayRepository.refreshOnce()
                                    Toast.makeText(ctx, msg, Toast.LENGTH_LONG).show()
                                    onBack()
                                }
                            })
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
                // 中转 / 自定义：Base URL + Key + 协议，走 /api/custom-providers
                Spacer(Modifier.height(12.dp))
                CustomProviderForm(onDone = { ok, msg ->
                    Toast.makeText(ctx, msg, Toast.LENGTH_LONG).show()
                    if (ok) {
                        GatewayRepository.refreshOnce()
                        onBack()
                    }
                })
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

    val primaryHint: String
    val minLines: Int
    val secondaryHint: String?
    when (preset.id) {
        "kuku" -> { primaryHint = "Cookie 整串（wenku.baidu.com 登录后复制，含 BDUSS 与 STOKEN）"; minLines = 4; secondaryHint = null }
        "commandcode" -> { primaryHint = "API Key（user_ 开头）"; minLines = 1; secondaryHint = null }
        "antigravity" -> { primaryHint = "Google refresh token（1// 开头）"; minLines = 3; secondaryHint = "Google 账号邮箱（可选，同账号去重）" }
        else -> { primaryHint = "凭证"; minLines = 3; secondaryHint = null }
    }

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
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = C.cardBg,
    ) {
        Column(Modifier.padding(horizontal = 16.dp)) {
            Text("官方 AGENT · ${PresetRegistry.ALL.size}", color = C.textMain, fontSize = 15.sp, fontWeight = FontWeight.Bold)
            Text("按登录通道分节 · 点击选择", color = C.textDim, fontSize = 12.sp)
            Spacer(Modifier.height(6.dp))
            Column(Modifier.verticalScroll(rememberScrollState()).padding(bottom = 20.dp)) {
                listOf(
                    "网页登录" to LoginKind.WEBVIEW,
                    "手机验证码" to LoginKind.SMS,
                    "粘贴凭证" to LoginKind.PASTE,
                ).forEach { (section, kind) ->
                    val items = PresetRegistry.ALL.filter { it.kind == kind }
                    if (items.isNotEmpty()) {
                        Text(section, color = C.textDim, fontSize = 12.sp, modifier = Modifier.padding(top = 12.dp, bottom = 2.dp))
                        items.forEach { p ->
                            Row(
                                Modifier
                                    .fillMaxWidth()
                                    .clickable { onPick(p) }
                                    .padding(vertical = 11.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                AgateAvatar(name = p.label, size = 36.dp, corner = 11.dp, letters = initialsOf(p.label))
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
    }
}

/* ── 中转 / 自定义接入：Base URL + Key + 协议 → POST /api/custom-providers ── */
@Composable
private fun CustomProviderForm(onDone: (Boolean, String) -> Unit) {
    val C = agateColors()
    val scope = rememberCoroutineScope()
    var name by remember { mutableStateOf("") }
    var baseUrl by remember { mutableStateOf("") }
    var key by remember { mutableStateOf("") }
    var protocol by remember { mutableStateOf(CustomProviderOps.Protocol.OPENAI) }
    var busy by remember { mutableStateOf(false) }
    var hint by remember { mutableStateOf<String?>(null) }

    val valid = name.isNotBlank() && baseUrl.startsWith("http") && key.isNotBlank()

    Column(
        Modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(16.dp))
            .border(1.dp, C.border, RoundedCornerShape(16.dp))
            .padding(14.dp),
    ) {
        Text("接入信息", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(10.dp))
        AgateFormInput(value = name, onValueChange = { name = it }, hint = "名称（如 OpenAI 兼容 / NewAPI）")
        Spacer(Modifier.height(8.dp))
        AgateFormInput(value = baseUrl, onValueChange = { baseUrl = it.trim() }, hint = "Base URL，如 https://api.example.com/v1")
        Spacer(Modifier.height(8.dp))
        AgateFormInput(value = key, onValueChange = { key = it.trim() }, hint = "API Key（可带 Bearer 前缀）")
        Spacer(Modifier.height(12.dp))
        Text("协议", color = C.textSub, fontSize = 12.sp)
        Spacer(Modifier.height(8.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            CustomProviderOps.Protocol.entries.forEach { p ->
                val active = protocol == p
                Text(
                    p.label,
                    color = if (active) C.onAccent else C.textSub,
                    fontSize = 12.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .background(if (active) C.accent else C.insetBg, RoundedCornerShape(999.dp))
                        .border(1.dp, if (active) Color.Transparent else C.border, RoundedCornerShape(999.dp))
                        .clickable { protocol = p }
                        .padding(horizontal = 14.dp, vertical = 8.dp),
                )
            }
        }
        hint?.let {
            Spacer(Modifier.height(8.dp))
            Text(it, color = C.red, fontSize = 12.sp)
        }
        Spacer(Modifier.height(12.dp))
        Button(
            onClick = {
                busy = true; hint = null
                scope.launch {
                    val r = CustomProviderOps.create(name, baseUrl, key, protocol)
                    busy = false
                    if (r.ok) onDone(true, "已添加 " + name)
                    else hint = r.error ?: "创建失败"
                }
            },
            enabled = !busy && valid,
            modifier = Modifier.fillMaxWidth(),
            colors = ButtonDefaults.buttonColors(containerColor = C.accent, contentColor = C.onAccent),
        ) { Text(if (busy) "创建中…" else "创建并接入", fontWeight = FontWeight.SemiBold) }
        Spacer(Modifier.height(6.dp))
        Text(
            "仅 LLM 请求 + 模型列表；兼容 OpenAI /v1/chat/completions 即可接入。",
            color = C.textDim, fontSize = 11.sp,
        )
    }
}
