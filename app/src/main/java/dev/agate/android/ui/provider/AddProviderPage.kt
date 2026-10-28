package dev.agate.android.ui.provider

import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.Image
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
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
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import android.util.Base64
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import android.content.Intent
import android.net.Uri
import dev.agate.android.data.AuthChannel
import dev.agate.android.data.AccountOps
import dev.agate.android.data.CustomProviderOps
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.KernelOps
import dev.agate.android.data.LoginClient
import dev.agate.android.data.LoginKind
import dev.agate.android.data.ProviderPreset
import dev.agate.android.data.PresetApiRegistry
import dev.agate.android.data.PresetRegistry
import dev.agate.core.AgateForegroundService
import dev.agate.core.GatewayPrefs
import dev.agate.android.auth.AuthWebViewSheet
import dev.agate.android.auth.LoginSession
import dev.agate.android.ui.components.AgateSearchBar
import dev.agate.android.ui.components.AgateProviderAvatar
import dev.agate.android.ui.components.AgateAvatar
import dev.agate.android.ui.components.AgateChip
import dev.agate.android.ui.components.AgateFormInput
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
    // 内核探活：登录/落账号都由网关执行，没起来时先给「启动网关」入口
    var kernelUp by remember { mutableStateOf<Boolean?>(null) }
    var loginSession by remember { mutableStateOf<LoginSession?>(null) }
    /**
     * 已建的自定义提供商（含预置卡建出来的）。预置/自定义两段都读它：
     *   · 预置卡「这张建过了吗」= 有同名的记录（上游 `matchesPresetName` 口径）
     *   · 自定义段列出「非预置同名」的那些，作为「给这家再加账号」的入口
     */
    var customProviders by remember { mutableStateOf<List<CustomProviderOps.Record>>(emptyList()) }
    var customLoaded by remember { mutableStateOf(false) }

    /** 拉一次自定义提供商目录（建/删/改之后都要重拉，界面据此重判「建过没」）。 */
    fun reloadCustom() {
        scope.launch {
            customProviders = CustomProviderOps.Record.listOf(CustomProviderOps.list())
            customLoaded = true
        }
    }

    LaunchedEffect(Unit) {
        while (isActive) {
            val up = KernelOps.isUp()
            kernelUp = up
            if (up) break
            delay(2_000L)
        }
    }

    DisposableEffect(Unit) {
        LoginClient.configure(GatewayPrefs.apiKey(ctx))
        AccountOps.configure(GatewayPrefs.apiKey(ctx))
        CustomProviderOps.configure(GatewayPrefs.apiKey(ctx))
        onDispose { }
    }
    // 自定义提供商目录：进页面拉一次（预置/自定义两段都要用它判「建过没」）
    LaunchedEffect(Unit) { reloadCustom() }

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
            // ptabs：v44 胶囊页签（官方 / 中转 / 自定义），中转与自定义为占位
            var tab by remember { mutableStateOf(0) }
            Row(
                modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                // 三个页签：官方 AGENT / 中转 API / 自定义。
                //
                // 为什么没有上游那个「导入 · 更多」：
                // 上游的 `data-for` 只有 official / relay / custom 三个值，
                // 那个页签点进去**是空的**（见 docs/Agate-UI设计基线-v44.html
                // 的 apGroup 分组标记）。而在本仓，跨端导入能力已经在
                // AGENT 页签里按家渲染 —— 选中支持文件导入的家（小浣熊 /
                // CatPaw / Cline / KukuAI）时，通道 chips 里就有「导入凭证文件」，
                // 走 ImportFileForm；需要粘贴的家有「粘贴凭证」。再开一个页签
                // 只会让人多选一次提供商，做的是同一件事。
                listOf(
                    "Agent · ${PresetRegistry.ALL.size}",
                    "预置 API · ${PresetApiRegistry.ALL.size}",
                    "自定义",
                ).forEachIndexed { i, label ->
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
                                Text("24 家 Agent · 点此打开选择器", color = C.textSub, fontSize = 12.sp)
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
                        .filter { it.disabledNote == null }
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
                        // 这五家的回调落在网关端口上：Core 态（网关未运行）收不到回调
                        if (picked.needsGateway && kernelUp != true) {
                            Column(
                                Modifier
                                    .fillMaxWidth()
                                    .background(C.amber.copy(alpha = 0.10f), RoundedCornerShape(12.dp))
                                    .border(1.dp, C.amber.copy(alpha = 0.45f), RoundedCornerShape(12.dp))
                                    .padding(12.dp),
                            ) {
                                Text(
                                    "该家的登录回调落在本机端口，需要先运行网关（粘贴/导入通道不受影响）。",
                                    color = C.textMain, fontSize = 12.sp,
                                )
                                Spacer(Modifier.height(8.dp))
                                Button(
                                    onClick = { AgateForegroundService.start(ctx) },
                                    colors = ButtonDefaults.buttonColors(
                                        containerColor = C.accent, contentColor = C.onAccent,
                                    ),
                                ) { Text("运行网关并继续", fontWeight = FontWeight.SemiBold) }
                            }
                            Spacer(Modifier.height(12.dp))
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
                            AuthChannel.IMPORT_FILE -> ImportFileForm(preset = picked, onDone = onAdded)
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
                // 建完/加完账号后的收尾：刷新内置快照（账号列表）+ 重拉自定义目录
                // （预置卡要靠它判「这张建过没」），成功才退回上一页。
                val done: (Boolean, String) -> Unit = { ok, msg ->
                    Toast.makeText(ctx, msg, Toast.LENGTH_LONG).show()
                    if (ok) {
                        GatewayRepository.refreshOnce()
                        reloadCustom()
                        onBack()
                    }
                }
                // tab 0 是 Agent（上面那一支），这里只剩 1（预置 API）/ 2（自定义）。
                // 用 else 兜底而不是穷举：tab 来自上面的 forEachIndexed，
                // 万一以后加页签却忘了这里，宁可不渲染也不要崩溃。
                when (tab) {
                    1 -> PresetApiTab(
                        custom = customProviders,
                        onDone = done,
                    )
                    2 -> CustomProviderTab(
                        custom = customProviders,
                        loaded = customLoaded,
                        onDone = done,
                    )
                    else -> Unit
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
                    "选择 AGENT",
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
                    val disabled = p.disabledNote != null
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .alpha(if (disabled) 0.45f else 1f)
                            .clickable(enabled = !disabled) { onPick(p) }
                            .padding(vertical = 10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        AgateProviderAvatar(providerId = p.id, label = p.label, size = 38.dp, corner = 12.dp)
                        Spacer(Modifier.width(12.dp))
                        Column(Modifier.weight(1f)) {
                            Text(p.label, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                            Text(p.desc, color = C.textSub, fontSize = 12.sp)
                            if (disabled) {
                                Text(p.disabledNote!!, color = C.textSub, fontSize = 11.sp)
                            }
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

/* ══════════ 预置 API（对齐上游 3.0.8 的 preset 段）══════════ */

/** 余额方案 → 需要用户补的一个凭证字段（对齐上游 `balance-query.tsx` 的 fields）。 */
private data class BalanceField(
    val key: String,
    val label: String,
    val secret: Boolean,
    val hint: String,
)

/**
 * 方案 → 凭证字段表。
 *
 * 只有三条方案要额外凭证（NewAPI 的个人令牌、商汤控制台的账号密码、智谱的
 * 控制台令牌）；其余四条（sub2api / deepseek / opencode / zhipu_zai）只用
 * **账号自己的 API Key**，因此返回空表 —— 界面只显示一句方案说明，不渲染输入框。
 * 与上游「`fields: []` 的家不要多问」同一口径。
 */
private fun balanceFieldsOf(scheme: String): List<BalanceField> = when (scheme) {
    "new_api" -> listOf(
        BalanceField("accessToken", "NewAPI 个人访问令牌", true, "站点「个人设置」里生成；与账号的 API Key 不是同一个"),
        BalanceField("userId", "NewAPI 用户 ID", false, "旧版 / 定制站点才需要，新版可留空"),
    )
    "sensenova" -> listOf(
        BalanceField("username", "商汤控制台账号", false, "必填"),
        BalanceField("password", "商汤控制台密码", true, "与推理用的 API Key 不是同一套"),
    )
    "zhipu" -> listOf(
        BalanceField("token", "bigmodel 访问令牌", true, "登录后 Cookie bigmodel_token_production 的值"),
    )
    else -> emptyList()
}

/** 方案 → 一句说明（要填什么 / 去哪拿）。空串 = 不显示这一段。 */
private fun balanceNoteOf(scheme: String): String = when (scheme) {
    "sub2api" -> "余额查询用这条账号的 API Key 打 {base}/v1/usage，不需要额外配置"
    "new_api" -> "用站点的个人访问令牌查询账户余额（不是这条账号的上游 API Key）"
    "deepseek" -> "余额查询用这条账号的 API Key，不需要额外配置"
    "opencode" -> "读订阅用量（5 小时 / 每周 / 每月三个窗口），不需要额外配置"
    "sensenova" -> "用商汤控制台账号密码查询（与推理用的 API Key 不是同一套）"
    "zhipu" -> "用 bigmodel.cn 控制台的访问令牌查询（人民币元）"
    "zhipu_zai" -> "用这条账号的 API Key 铸签名后查询（美元），不需要额外配置"
    else -> ""
}

/**
 * 预置 API 段：一张名片式的模板卡网格。
 *
 * ── 为什么不叫「中转」──────────────────────────────────────
 * 上游 3.0.8 的四类账号是 **Agent / 预置 API / 自定义 / 导入**（见
 * `add-provider-pick.tsx` 的 `AccountType`）。「预置 API」不是中转的别名，
 * 而是**模板卡**：点一张卡把这一家的名称 / 协议 / Base URL / 余额方案 / 上游
 * 特判预填进新建表单，建出来的仍是一条普通 `custom-` 记录。
 *
 * ── 卡片为什么建过了也不消失 ──────────────────────────────
 * 上游注释写得很清楚：模板从列表里消失会让用户以为「这张只能用一次」。
 * 所以卡片常驻，只是第二行变成「已建 · N 个账号」，再点它就是**给那家加账号**
 * （而不是重复建一家同名的）。
 */
@Composable
private fun PresetApiTab(
    custom: List<CustomProviderOps.Record>,
    onDone: (Boolean, String) -> Unit,
) {
    val C = agateColors()
    var selected by remember { mutableStateOf<PresetApiRegistry.Preset?>(null) }

    val picked = selected
    if (picked != null) {
        val built = custom.firstOrNull { it.name == picked.name }
        CustomProviderForm(
            preset = picked,
            existing = built,
            onBack = { selected = null },
            onDone = onDone,
        )
        return
    }

    Text(
        "点一张卡 = 用它的名称 / 协议 / 地址 / 余额方案预填新建；" +
            "这家已经建过时，同一张卡改成「给它再加一个账号」",
        color = C.textSub,
        fontSize = 12.sp,
    )
    Spacer(Modifier.height(10.dp))

    PresetApiRegistry.ALL.chunked(2).forEach { row ->
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            row.forEach { preset ->
                Box(Modifier.weight(1f)) {
                    PresetApiCard(
                        preset = preset,
                        builtAccounts = custom.firstOrNull { it.name == preset.name },
                        onClick = { selected = preset },
                    )
                }
            }
            // 单数行补一格，避免最后一张卡被拉成整行宽
            if (row.size == 1) Spacer(Modifier.weight(1f))
        }
        Spacer(Modifier.height(8.dp))
    }
}

/** 一张预置模板卡：图标 + 名称 + 协议/域名 + 「已建」状态。 */
@Composable
private fun PresetApiCard(
    preset: PresetApiRegistry.Preset,
    builtAccounts: CustomProviderOps.Record?,
    onClick: () -> Unit,
) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(14.dp))
            .border(
                1.dp,
                if (builtAccounts != null) C.accent.copy(alpha = 0.35f) else C.border,
                RoundedCornerShape(14.dp),
            )
            .clickable(onClick = onClick)
            .padding(10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Image(
                painter = painterResource(preset.iconRes),
                contentDescription = preset.name,
                modifier = Modifier.size(26.dp).clip(RoundedCornerShape(8.dp)),
            )
            Spacer(Modifier.width(8.dp))
            Text(
                preset.name,
                color = C.textMain,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                maxLines = 2,
                modifier = Modifier.weight(1f),
            )
        }
        Spacer(Modifier.height(6.dp))
        Text(
            if (preset.selfHosted) "自建 · 地址自己填"
            else preset.baseUrl.removePrefix("https://").removePrefix("http://").substringBefore('/'),
            color = C.textDim,
            fontSize = 10.sp,
            maxLines = 1,
        )
        Spacer(Modifier.height(2.dp))
        Text(
            when {
                builtAccounts != null -> "已建 · 点此再加账号"
                preset.balanceScheme.isNotBlank() -> "可查余额 · 点此新建"
                else -> "点此新建"
            },
            color = if (builtAccounts != null) C.accent else C.textSub,
            fontSize = 10.sp,
            maxLines = 1,
        )
    }
}

/* ══════════ 自定义（对齐上游 custom 段）══════════ */

/**
 * 自定义段：「新建」入口 + 已建的自定义家（点它 = 给这家再加账号）。
 *
 * 预置模板同名建出来的家**不进这一段** —— 那是上游的刻意口径：它的「加账号」
 * 入口就是预置段那张常驻模板卡，再出现一份只会让人以为建了两个家。
 * 用户给那家改名后名字不再匹配任何模板，它就自动回到这一段。
 */
@Composable
private fun CustomProviderTab(
    custom: List<CustomProviderOps.Record>,
    loaded: Boolean,
    onDone: (Boolean, String) -> Unit,
) {
    val C = agateColors()
    var creating by remember { mutableStateOf(false) }
    var pendingExisting by remember { mutableStateOf<CustomProviderOps.Record?>(null) }

    // 两种表单态优先：加账号（目标已知）→ 新建（空白表单）
    val target = pendingExisting
    if (target != null) {
        CustomProviderForm(
            preset = null,
            existing = target,
            onBack = { pendingExisting = null },
            onDone = onDone,
        )
        return
    }
    if (creating) {
        CustomProviderForm(
            preset = null,
            existing = null,
            onBack = { creating = false },
            onDone = onDone,
        )
        return
    }

    Column(
        Modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(14.dp))
            .border(1.dp, C.accent.copy(alpha = 0.4f), RoundedCornerShape(14.dp))
            .clickable { creating = true }
            .padding(14.dp),
    ) {
        Text("新建自定义提供商", color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(2.dp))
        Text(
            "自己填名称 / 协议 / Base URL / Key，创建时同时建立第一家账号",
            color = C.textSub, fontSize = 11.sp,
        )
    }

    val manual = custom.filter { !PresetApiRegistry.matchesPresetName(it.name) }
    if (manual.isNotEmpty()) {
        Spacer(Modifier.height(14.dp))
        Text("已建的自定义提供商", color = C.textSub, fontSize = 12.sp)
        Spacer(Modifier.height(8.dp))
        manual.forEach { record ->
            Row(
                Modifier
                    .fillMaxWidth()
                    .background(C.cardBg, RoundedCornerShape(14.dp))
                    .border(1.dp, C.border, RoundedCornerShape(14.dp))
                    .padding(horizontal = 12.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text(record.name, color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                    Text(
                        "${record.protocol.label} · ${record.baseUrl.ifBlank { "未填地址" }}",
                        color = C.textDim, fontSize = 10.sp, maxLines = 1,
                    )
                }
                Spacer(Modifier.width(8.dp))
                SmallAction("加账号") { pendingExisting = record }
            }
            Spacer(Modifier.height(8.dp))
        }
    } else if (!loaded) {
        Spacer(Modifier.height(12.dp))
        Text("读取中…", color = C.textDim, fontSize = 12.sp)
    }
}

/** 行内小动作按钮（本页自用；组件库里没有这种胶囊小按钮）。 */
@Composable
private fun SmallAction(text: String, onClick: () -> Unit) {
    val C = agateColors()
    Text(
        text,
        color = C.accent,
        fontSize = 11.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier
            .background(C.accent.copy(alpha = 0.10f), RoundedCornerShape(8.dp))
            .clickable(onClick = onClick)
            .padding(horizontal = 9.dp, vertical = 4.dp),
    )
}

/**
 * 新建 / 加账号共用的表单。
 *
 * `existing` 非空 = **往已有家加账号**（不新建记录，只 `POST /api/accounts`）；
 * 否则按 `preset` 预填（可为 null = 完全手工）。两者字段集的差异只有
 * 「名称 / 协议 / Base URL 不可改」（它们属于提供商记录，不属于账号）。
 */
@Composable
private fun CustomProviderForm(
    preset: PresetApiRegistry.Preset?,
    existing: CustomProviderOps.Record?,
    onBack: () -> Unit,
    onDone: (Boolean, String) -> Unit,
) {
    val C = agateColors()
    val scope = rememberCoroutineScope()
    val adding = existing != null

    var name by remember { mutableStateOf(preset?.name ?: "") }
    var baseUrl by remember { mutableStateOf(preset?.baseUrl ?: "") }
    var key by remember { mutableStateOf("") }
    var noAuth by remember { mutableStateOf(preset?.noAuth ?: false) }
    var protocol by remember {
        mutableStateOf(existing?.protocol ?: preset?.protocol ?: CustomProviderOps.Protocol.OPENAI)
    }
    // 余额查询要补的凭证：键 → 输入值
    val balanceScheme = existing?.balanceScheme ?: preset?.balanceScheme ?: ""
    val balanceFields = balanceFieldsOf(balanceScheme)
    val balanceValues = remember(balanceScheme) { mutableStateMapOf<String, String>() }
    var busy by remember { mutableStateOf(false) }
    var hint by remember { mutableStateOf<String?>(null) }

    // 加账号时只校验 Key：名称/协议/Base URL 属于**提供商记录**，不在这张表单上
    // （从「自定义 → 加账号」进来时 baseUrl 状态是空的，按新建那套判会永远不可提交）
    val valid = (adding || baseUrl.startsWith("http")) && (noAuth || key.isNotBlank())

    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(
            when {
                adding -> "给「${existing?.name}」加账号"
                preset != null -> "使用预置：${preset.name}"
                else -> "新建自定义提供商"
            },
            color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold,
            modifier = Modifier.weight(1f),
        )
        SmallAction("返回") { onBack() }
    }
    Spacer(Modifier.height(10.dp))

    FormCard(if (adding) "账号信息" else "提供商信息") {
        if (!adding) {
            AgateFormInput(value = name, onValueChange = { name = it }, hint = "名称 · 1~64 字符，账号列表按它分组")
            Spacer(Modifier.height(8.dp))
            AgateFormInput(
                value = baseUrl,
                onValueChange = { baseUrl = it.trim() },
                hint = preset?.placeholder?.ifBlank { null } ?: "Base URL · 如 https://api.example.com/v1",
            )
            preset?.hint?.takeIf { it.isNotBlank() }?.let {
                Spacer(Modifier.height(4.dp))
                Text(it, color = C.textDim, fontSize = 10.sp)
            }
            Spacer(Modifier.height(12.dp))
            Text("协议", color = C.textSub, fontSize = 12.sp)
            Spacer(Modifier.height(8.dp))
            ProtocolChips(protocol) { protocol = it }
            Spacer(Modifier.height(4.dp))
            Text(
                if (protocol == CustomProviderOps.Protocol.ANTHROPIC)
                    "Anthropic 填根地址，不要带 /v1（内核按协议拼 /v1/messages）"
                else "OpenAI 兼容填到 /v1（内核按协议拼 /chat/completions）",
                color = C.textDim, fontSize = 10.sp,
            )
            Spacer(Modifier.height(12.dp))
        } else {
            Text(
                existing?.baseUrl?.ifBlank { "（未填地址）" } ?: "",
                color = C.textDim, fontSize = 11.sp,
            )
            Spacer(Modifier.height(10.dp))
        }

        AgateFormInput(
            value = if (noAuth) "" else key,
            onValueChange = { key = it.trim() },
            hint = if (noAuth) "已勾选无需鉴权" else "API Key · sk-…",
        )
        Spacer(Modifier.height(10.dp))
        ToggleRow(
            title = "该上游无需鉴权",
            desc = "本地 Ollama / 免费档：转发与拉取模型都不发鉴权头",
            checked = noAuth,
            onChange = { on -> noAuth = on; if (on) key = "" },
        )

        // ── 余额查询：只有带方案的家才出现 ──
        if (balanceScheme.isNotBlank()) {
            Spacer(Modifier.height(12.dp))
            Text("余额查询", color = C.textSub, fontSize = 12.sp)
            Spacer(Modifier.height(4.dp))
            Text(
                "方案：" + (CustomProviderOps.balanceSchemes.firstOrNull { it.first == balanceScheme }?.second
                    ?: balanceScheme) + " · " + balanceNoteOf(balanceScheme),
                color = C.textDim, fontSize = 10.sp,
            )
            balanceFields.forEach { field ->
                Spacer(Modifier.height(8.dp))
                AgateFormInput(
                    value = balanceValues[field.key].orEmpty(),
                    onValueChange = { balanceValues[field.key] = it.trim() },
                    hint = field.label + " · " + field.hint,
                )
            }
        }
    }

    hint?.let {
        Spacer(Modifier.height(8.dp))
        Text(it, color = C.red, fontSize = 12.sp)
    }
    Spacer(Modifier.height(12.dp))
    SubmitButton(
        when {
            busy -> "保存中…"
            adding -> "添加账号"
            else -> "创建并接入"
        },
        valid && !busy,
    ) {
        busy = true; hint = null
        scope.launch {
            val query = balanceFields
                .mapNotNull { f -> balanceValues[f.key]?.takeIf { it.isNotBlank() }?.let { f.key to it } }
                .toMap()
            val r = if (adding && existing != null) {
                CustomProviderOps.addAccount(
                    providerId = existing.id,
                    apiKeyValue = key,
                    noAuth = noAuth,
                    balanceQuery = query.takeIf { it.isNotEmpty() },
                )
            } else {
                CustomProviderOps.create(
                    name = name.trim(),
                    baseUrl = baseUrl.trim(),
                    apiKeyValue = key,
                    protocol = protocol,
                    noAuth = noAuth,
                    balanceScheme = balanceScheme.takeIf { it.isNotBlank() },
                    urlSuffix = preset?.urlSuffix,
                    headers = preset?.headers?.takeIf { it.isNotEmpty() },
                    anthropicToolType = preset?.anthropicToolType?.takeIf { it.isNotBlank() },
                    clientEmulation = preset?.clientEmulation?.takeIf { it.isNotBlank() },
                    balanceQuery = query.takeIf { it.isNotEmpty() },
                )
            }
            busy = false
            if (r.ok) {
                onDone(
                    true,
                    if (adding) "已给「${existing?.name}」添加账号"
                    else "已创建「${name.trim()}」并添加账号",
                )
            } else {
                hint = r.error ?: "保存失败"
            }
        }
    }
    Spacer(Modifier.height(6.dp))
    Text(
        "保存后立即参与选号 · 额度信息在首次请求后回填",
        color = C.textDim, fontSize = 11.sp,
    )
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

    // 设备码：只有上游给了 user_code 的（Cline）才有；从 authUrl 的 query 里取
    val userCode = remember(authUrl) {
        authUrl?.let { u -> runCatching { Uri.parse(u).getQueryParameter("user_code") }.getOrNull() }
    }
    val clipboard = LocalClipboardManager.current

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

    if (userCode != null) {
        Text("设备码", color = C.textSub, fontSize = 12.sp)
        Spacer(Modifier.height(4.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                userCode,
                color = C.accent, fontSize = 20.sp, fontWeight = FontWeight.Bold,
                modifier = Modifier.weight(1f),
            )
            Text(
                "复制",
                color = C.accent, fontSize = 12.sp, fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(C.accent.copy(alpha = 0.10f), RoundedCornerShape(9.dp))
                    .clickable { clipboard.setText(AnnotatedString(userCode)) }
                    .padding(horizontal = 10.dp, vertical = 6.dp),
            )
        }
        Spacer(Modifier.height(10.dp))
    }

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

/* ── 导入凭证文件：SAF 选文件 → 文本走 authFileContent / 二进制走 authFileB64 ── */
@Composable
private fun ImportFileForm(preset: ProviderPreset, onDone: (Boolean, String) -> Unit) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    var busy by remember { mutableStateOf(false) }
    var hint by remember { mutableStateOf<String?>(null) }
    var pickedName by remember { mutableStateOf<String?>(null) }

    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) {
            busy = true
            hint = null
            scope.launch {
                val name = uri.lastPathSegment?.substringAfterLast('/')?.takeIf { it.isNotBlank() } ?: "credential"
                pickedName = name
                val bytes = runCatching {
                    ctx.contentResolver.openInputStream(uri)?.use { it.readBytes() }
                }.getOrNull()
                if (bytes == null) {
                    busy = false
                    hint = "读取文件失败（权限或路径不可用）"
                    return@launch
                }
                if (bytes.size > 4 * 1024 * 1024) {
                    busy = false
                    hint = "文件过大（超过 4MB），请改用文本 JSON"
                    return@launch
                }
                val asText = runCatching { String(bytes, Charsets.UTF_8) }.getOrNull()
                val looksJson = asText != null && (asText.trimStart().startsWith("{") || asText.trimStart().startsWith("["))
                val binary = !looksJson && (
                    preset.id == "kuku" ||
                        name.endsWith(".db", true) || name.endsWith(".sqlite", true)
                    )
                val r = if (binary) {
                    AccountOps.importCredential(
                        preset.id, name,
                        b64 = Base64.encodeToString(bytes, Base64.NO_WRAP),
                    )
                } else {
                    AccountOps.importCredential(
                        preset.id, name,
                        text = asText ?: String(bytes, Charsets.UTF_8),
                    )
                }
                busy = false
                if (r.ok) onDone(true, "已从 $name 导入 ${preset.label} 账号")
                else hint = r.error ?: "导入失败"
            }
        }
    }

    Text(
        "选择桌面客户端导出的凭证文件（auth.json / providers.json / Cookie 库）。" +
            "文本 JSON 直接解析，二进制（KukuAI 的 Cookies SQLite）自动转 base64。",
        color = C.textSub, fontSize = 12.sp,
    )
    Spacer(Modifier.height(10.dp))
    Button(
        onClick = { launcher.launch(arrayOf("*/*")) },
        enabled = !busy,
        modifier = Modifier.fillMaxWidth(),
        colors = ButtonDefaults.buttonColors(containerColor = C.accent, contentColor = C.onAccent),
    ) { Text(if (busy) "导入中…" else "选择文件", fontWeight = FontWeight.SemiBold) }
    pickedName?.let {
        Spacer(Modifier.height(6.dp))
        Text("已选：$it", color = C.textDim, fontSize = 11.sp)
    }
    hint?.let {
        Spacer(Modifier.height(8.dp))
        Text(it, color = C.red, fontSize = 12.sp)
    }
}
