package dev.agate.android.ui.keys

import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
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
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.KeyOps
import dev.agate.android.data.LanAddress
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import dev.agate.core.GatewayPrefs
import kotlinx.coroutines.launch

/**
 * 访问密钥页（原为 M3 骨架占位，本次补实）。
 *
 * ── 为什么这一页必须先修 ────────────────────────────────────
 * 用户反馈「登录了账号，客户端调模型报 `401 Unauthorized: invalid or missing
 * API key`」。内核闸门会校验 `Authorization: Bearer <key>` 或 `x-api-key`，
 * 而壳层那把 Key 是**本机自动生成**（`agate-` + 32 hex）并注入内核的 ——
 * 它此前只在一个骨架页里「提到过」，用户拿不到也复制不了。
 *
 * 所以这一页的三件事按「能不能调通」排优先级：
 *   ① **本机 Key 明文可见 + 一键复制**（最常需要）；
 *   ② **Base URL 一并复制**（客户端要同时配这两样）；
 *   ③ 自建更多 Key（可限制可用提供商 / 模型，给不同的下游分开用）。
 */
@Composable
fun KeysPage(
    onOpenDrawer: () -> Unit = {},
    onBack: (() -> Unit)? = null,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val clipboard = LocalClipboardManager.current

    val localKey = remember { GatewayPrefs.apiKey(ctx) }
    val port = remember { GatewayPrefs.port(ctx) }
    val baseUrl = remember { "http://127.0.0.1:$port/v1" }
    // 本机模式（未开局域网监听）**不需要 Key**：内核只在 LAN 模式注入环境变量 Key，
    // 本机时 Key 列表为空 → 鉴权走免鉴权分支。文案必须跟着说清楚，否则用户会
    // 以为「必须填」而卡在这一步（这正是之前的实际反馈）。
    val lanMode = remember { GatewayPrefs.bindLan(ctx) }
    // 局域网地址只在真的开了监听时才算：没开时内核只绑 127.0.0.1，
    // 给出一个 192.168.x.x 的地址会让人复制到一个连不上的端口。
    val lanUrl = remember(lanMode) {
        if (!lanMode) null else LanAddress.ipv4()?.let { "http://$it:$port/v1" }
    }

    var rows by remember { mutableStateOf<List<KeyOps.KeyRow>>(emptyList()) }
    // 两张候选表由后端拼整（`providers` 含注册表 + 自定义家，`modelsByProvider`
    // 是各家 → 对外名），前端不烤第二份清单。随主键列表一起读回来即可。
    var catalog by remember { mutableStateOf(KeyOps.Catalog(emptyList(), emptyMap())) }
    var scopeTarget by remember { mutableStateOf<KeyOps.KeyRow?>(null) }
    var loading by remember { mutableStateOf(false) }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    fun reload() {
        if (loading) return
        loading = true
        scope.launch {
            val r = KeyOps.list()
            loading = false
            rows = KeyOps.parseRows(r.data, localKey)
            catalog = KeyOps.parseCatalog(r.data)
            if (!r.ok) toast(r.error ?: "读取失败")
        }
    }

    DisposableEffect(Unit) {
        KeyOps.configure(localKey)
        GatewayRepository.configure(localKey)
        onDispose { }
    }
    LaunchedEffect(Unit) { reload() }

    // 键盘避让由 `AgateNavHost` 统一处理（见那里的说明），页面不再各加一层
    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "访问密钥", onOpenDrawer = onOpenDrawer, onBack = onBack) {
            Text(
                "刷新",
                color = C.accent,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clickable(enabled = !loading) { reload() }
                    .padding(horizontal = 8.dp, vertical = 5.dp),
            )
        }

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))

            // ── ① 怎么连（最重要的一条，放最上）──
            CopyCard(
                title = "客户端连接信息",
                rows = if (lanMode) {
                    buildList {
                        add("Base URL" to baseUrl)
                        // 同网段的电脑/平板要用的就是这一条（本机那条它们连不上）
                        lanUrl?.let { add("局域网 Base URL" to it) }
                        add("API Key" to localKey)
                    }
                } else {
                    // 本机不需要 Key：只给地址，别让用户去填一个用不上的字段
                    listOf("Base URL" to baseUrl)
                },
                hint = if (lanMode) {
                    "已开局域网监听，客户端必须带 Key（不匹配会拿到 401 invalid or missing API key）" +
                            if (lanUrl == null) "\n（没取到局域网 IP：确认手机已连 Wi-Fi）" else ""
                } else {
                    "本机访问**无需填 Key**：端口只监听 127.0.0.1，客户端只填 Base URL 即可。" +
                        "开启「局域网监听」后才需要 Key"
                },
                onCopy = { text ->
                    clipboard.setText(AnnotatedString(text))
                    toast("已复制")
                },
                onCopyAll = {
                    // 复制的内容按「对方能不能直接用」组织：开了局域网监听时
                    // 优先给局域网地址，并附上终端可直接粘贴的两行 export ——
                    // 电脑上接网关基本都要手打这两个变量，抄地址再抄 Key 很容易错。
                    val target = lanUrl ?: baseUrl
                    val text = if (lanMode) {
                        "Base URL: $target\nAPI Key: $localKey\n\n" +
                            "export ANTHROPIC_BASE_URL=$target\n" +
                            "export ANTHROPIC_API_KEY=$localKey"
                    } else {
                        "Base URL: $baseUrl\nAPI Key: $localKey"
                    }
                    clipboard.setText(AnnotatedString(text))
                    toast(if (lanMode) "已复制局域网地址与启动命令" else "已复制 Base URL 与 Key")
                },
            )

            Spacer(Modifier.height(10.dp))

            // ── ② 现有密钥 ──
            Column(
                Modifier
                    .fillMaxWidth()
                    .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
                    .border(1.dp, C.border, RoundedCornerShape(20.dp))
                    .padding(14.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        "密钥列表",
                        style = AgateType.SectionTitle,
                        fontWeight = FontWeight.SemiBold,
                        color = C.textMain,
                        modifier = Modifier.weight(1f),
                    )
                    AgateBadge("${rows.size} 把", BadgeTone.GRAY)
                }
                if (rows.isEmpty()) {
                    Spacer(Modifier.height(8.dp))
                    Text(
                        if (loading) "读取中…" else "还没有自建密钥（上面的本机默认已可用于 /v1）",
                        color = C.textDim,
                        fontSize = 12.sp,
                    )
                }
                rows.forEach { row ->
                    Spacer(Modifier.height(9.dp))
                    KeyRowView(
                        row = row,
                        onEditScope = { scopeTarget = row },
                        onToggle = {
                            if (row.fromEnv) {
                                toast("本机默认 Key 由应用生成，不可停用")
                            } else {
                                scope.launch {
                                    val r = KeyOps.setEnabled(row.id, !row.enabled)
                                    if (r.ok) reload() else toast(r.error ?: "操作失败")
                                }
                            }
                        },
                        onDelete = {
                            scope.launch {
                                val r = KeyOps.remove(row.id)
                                if (r.ok) {
                                    toast("已吊销")
                                    reload()
                                } else {
                                    toast(r.error ?: "吊销失败")
                                }
                            }
                        },
                    )
                }
            }

            Spacer(Modifier.height(10.dp))

            // ── ③ 新建 ──
            Column(
                Modifier
                    .fillMaxWidth()
                    .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
                    .border(1.dp, C.border, RoundedCornerShape(20.dp))
                    .clickable {
                        scope.launch {
                            val r = KeyOps.create("Key ${rows.count { !it.fromEnv } + 1}")
                            if (r.ok) {
                                toast("已新建密钥")
                                reload()
                            } else {
                                toast(r.error ?: "新建失败")
                            }
                        }
                    }
                    .padding(14.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(
                        Modifier
                            .width(30.dp)
                            .height(30.dp)
                            .background(C.accent.copy(alpha = 0.13f), RoundedCornerShape(10.dp)),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text("＋", color = C.accent, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
                    }
                    Spacer(Modifier.width(10.dp))
                    Column(Modifier.weight(1f)) {
                        Text(
                            "新建密钥",
                            color = C.textMain,
                            fontSize = 14.sp,
                            fontWeight = FontWeight.SemiBold,
                        )
                        Text(
                            "给不同的下游分开用；可限制它只能用哪些提供商 / 模型",
                            color = C.textSub,
                            fontSize = 11.sp,
                        )
                    }
                    Text("❯", color = C.textDim, fontSize = 13.sp)
                }
            }

            Spacer(Modifier.height(28.dp))
        }
    }

    scopeTarget?.let { row ->
        ScopeSheet(
            row = row,
            catalog = catalog,
            onDismiss = { scopeTarget = null },
            onSave = { providers, models ->
                scope.launch {
                    val r = KeyOps.setScope(row.id, providers, models)
                    if (r.ok) {
                        toast("已更新可用范围")
                        scopeTarget = null
                        reload()
                    } else {
                        toast(r.error ?: "保存失败")
                    }
                }
            },
        )
    }
}

/** 可复制的信息卡（Base URL / Key 各一行，带整块复制）。 */
@Composable
private fun CopyCard(
    title: String,
    rows: List<Pair<String, String>>,
    hint: String,
    onCopy: (String) -> Unit,
    onCopyAll: () -> Unit,
) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(14.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                title,
                style = AgateType.SectionTitle,
                fontWeight = FontWeight.SemiBold,
                color = C.textMain,
                modifier = Modifier.weight(1f),
            )
            Text(
                "全部复制",
                color = C.accent,
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(C.accent.copy(alpha = 0.11f), RoundedCornerShape(8.dp))
                    .clickable(onClick = onCopyAll)
                    .padding(horizontal = 9.dp, vertical = 4.dp),
            )
        }
        rows.forEach { (label, value) ->
            Spacer(Modifier.height(9.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(label, color = C.textDim, fontSize = 11.sp)
                    // 可选中的原文：Key 要能长按复制，也方便核对
                    SelectionContainer {
                        Text(
                            value,
                            color = C.textMain,
                            fontSize = 12.sp,
                            fontWeight = FontWeight.Medium,
                        )
                    }
                }
                Text(
                    "复制",
                    color = C.accent,
                    fontSize = 11.sp,
                    modifier = Modifier
                        .background(C.accent.copy(alpha = 0.10f), RoundedCornerShape(7.dp))
                        .clickable { onCopy(value) }
                        .padding(horizontal = 8.dp, vertical = 3.dp),
                )
            }
        }
        Spacer(Modifier.height(9.dp))
        Text(hint, color = C.textDim, fontSize = 11.sp)
    }
}

@Composable
private fun KeyRowView(
    row: KeyOps.KeyRow,
    onEditScope: () -> Unit,
    onToggle: () -> Unit,
    onDelete: () -> Unit,
) {
    val C = agateColors()
    Row(
        Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(row.name, color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.Medium)
                Spacer(Modifier.width(6.dp))
                when {
                    row.fromEnv -> AgateBadge("环境变量", BadgeTone.BLUE)
                    row.enabled -> AgateBadge("启用", BadgeTone.GREEN)
                    else -> AgateBadge("停用", BadgeTone.GRAY)
                }
            }
            Text(
                buildString {
                    append(row.keyTail)
                    append(" · ")
                    if (row.restricted) {
                        if (row.allowedProviders.isNotEmpty()) append("限 ${row.allowedProviders.size} 家")
                        if (row.allowedModels.isNotEmpty()) {
                            if (row.allowedProviders.isNotEmpty()) append(" / ")
                            append("${row.allowedModels.size} 模型")
                        }
                    } else {
                        append("不限")
                    }
                },
                color = C.textDim,
                fontSize = 11.sp,
            )
        }
        Row(
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            // 本机默认那把走环境变量，不在 SQLite 列表里，没法 PATCH —— 索性不显示入口，
            // 而不是给个点了会 404 的按钮。
            if (!row.fromEnv) {
                Text(
                    "范围",
                    color = C.textSub,
                    fontSize = 11.sp,
                    modifier = Modifier
                        .border(1.dp, C.border, RoundedCornerShape(7.dp))
                        .clickable(onClick = onEditScope)
                        .padding(horizontal = 8.dp, vertical = 3.dp),
                )
            }
            if (!row.fromEnv) {
                Text(
                    if (row.enabled) "停用" else "启用",
                    color = C.textSub,
                    fontSize = 11.sp,
                    modifier = Modifier
                        .border(1.dp, C.border, RoundedCornerShape(7.dp))
                        .clickable(onClick = onToggle)
                        .padding(horizontal = 8.dp, vertical = 3.dp),
                )
                Text(
                    "吊销",
                    color = C.red,
                    fontSize = 11.sp,
                    modifier = Modifier
                        .border(1.dp, C.red.copy(alpha = 0.35f), RoundedCornerShape(7.dp))
                        .clickable(onClick = onDelete)
                        .padding(horizontal = 8.dp, vertical = 3.dp),
                )
            }
        }
    }
}

/**
 * 「可用范围」编辑弹层。
 *
 * ── 两个白名单的关系是「各自独立判定」，界面要讲的是交集 ────────────
 * 内核 `KeyScope::allows_model` / `allows_provider` 各判各的：两者都非空时，
 * 请求必须**同时**落在两个白名单里才放行。所以 UI 上把它们摆成上下两组、
 * 中间用文案点明「交集生效」，而不是让用户以为下面一组是上一组的下钻。
 *
 * ── 候选模型为什么不实时等于并集 ───────────────────────────
 * 候选 = **已勾几家的并集 ∪ 这把 Key 当前已勾选的模型**。多出来的后者是有意的：
 * 内核在读取侧一律宽容（「升级绝不能让已有 Key 失效」，见 `core::api_keys`
 * 模块头那条硬不变量），如果某个模型已从某家清单里消失但白名单里还留着它，
 * 界面把它藏起来会让用户「看不见也改不掉」，等于由 UI 静默改了他的配置。
 * 显式列出来，删不删由他决定。
 *
 * ── 取消勾选某家为什么要连带清模型 ─────────────────────────
 * 反过来不留悬空：那家被取消后，只属于它的模型在候选里已经无处可依，留着会让
 * 「模型白名单非空」这一事实与「看不见任何候选」自相矛盾。取消勾选 = 收窄范围，
 * 收窄不该让看不见的约束留在生效状态。
 */
@Composable
private fun ScopeSheet(
    row: KeyOps.KeyRow,
    catalog: KeyOps.Catalog,
    onDismiss: () -> Unit,
    onSave: (providers: List<String>, models: List<String>) -> Unit,
) {
    val C = agateColors()

    // 每次切换到另一把 Key 都重算初值；同一把内用户的勾选是唯一真值。
    var pickedProviders by remember(row.id) {
        mutableStateOf(row.allowedProviders.toCollection(LinkedHashSet()))
    }
    var pickedModels by remember(row.id) {
        mutableStateOf(row.allowedModels.toCollection(LinkedHashSet()))
    }

    fun currentCandidates(): List<String> {
        val set = LinkedHashSet<String>()
        pickedProviders.forEach { set.addAll(catalog.modelsFor(it)) }
        set.addAll(pickedModels)
        return set.sorted()
    }

    fun toggleProvider(id: String) {
        val next = LinkedHashSet(pickedProviders)
        if (!next.remove(id)) next.add(id)
        pickedProviders = next
        val available = HashSet<String>()
        next.forEach { available.addAll(catalog.modelsFor(it).map { it.lowercase() }) }
        pickedModels = pickedModels.filterTo(LinkedHashSet()) { it.lowercase() in available }
    }

    val candidates = currentCandidates()

    Box(
        Modifier
            .fillMaxSize()
            .background(Color(0x99000000))
            .clickable(onClick = onDismiss),
        contentAlignment = Alignment.BottomCenter,
    ) {
        Column(
            Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(topStart = 18.dp, topEnd = 18.dp))
                .background(C.cardBg)
                .clickable(enabled = false, onClick = {})
                .padding(16.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("可用范围", fontSize = 15.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
                    Text(row.name, fontSize = 11.sp, color = C.textDim)
                }
                Text("✕", fontSize = 14.sp, color = C.textDim, modifier = Modifier.clickable(onClick = onDismiss).padding(6.dp))
            }

            Spacer(Modifier.height(6.dp))
            Column(Modifier.heightIn(max = 400.dp).verticalScroll(rememberScrollState())) {
                ChipSection(
                    title = "可用提供商",
                    note = "留空 = 不限制",
                    emptyHint = "还没有任何家（候选由后端给出，含自定义家）",
                    items = catalog.providers,
                    itemKey = { it.id },
                    itemLabel = { it.label },
                    itemBadge = { it.count.toString() },
                    selected = pickedProviders,
                    onToggle = { toggleProvider(it) },
                )

                Spacer(Modifier.height(12.dp))

                if (pickedProviders.isEmpty() && pickedModels.isEmpty()) {
                    ChipSection(
                        title = "可用模型",
                        note = "先勾选提供商",
                        emptyHint = "候选 = 已勾几家的模型并集；一家都没勾时没有候选",
                        items = emptyList<String>(),
                        itemKey = { it },
                        itemLabel = { it },
                        itemBadge = { null },
                        selected = pickedModels,
                        onToggle = {},
                    )
                } else {
                    ChipSection(
                        title = "可用模型",
                        note = "候选跟着上面走",
                        emptyHint = "这几家当前没有对外模型",
                        items = candidates,
                        itemKey = { it },
                        itemLabel = { it },
                        itemBadge = { null },
                        selected = pickedModels,
                        onToggle = { id ->
                            val next = LinkedHashSet(pickedModels)
                            if (!next.remove(id)) next.add(id)
                            pickedModels = next
                        },
                    )
                }

                Spacer(Modifier.height(10.dp))
                Text(
                    "两个都设时按交集生效：模型不在白名单、或提供它的家不在上面列表里，请求都会被拒。" +
                        "被限制掉的模型对这把 Key 表现为「不存在」（/v1/models 也看不到）。保存后立即生效。",
                    fontSize = 11.sp,
                    color = C.textDim,
                    lineHeight = 15.sp,
                )
            }

            Spacer(Modifier.height(12.dp))
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "清空",
                    fontSize = 13.sp,
                    color = C.textSub,
                    modifier = Modifier
                        .border(1.dp, C.border, RoundedCornerShape(8.dp))
                        .clickable {
                            pickedProviders = LinkedHashSet()
                            pickedModels = LinkedHashSet()
                        }
                        .padding(horizontal = 14.dp, vertical = 8.dp),
                )
                Spacer(Modifier.width(8.dp))
                Box(Modifier.weight(1f))
                Text(
                    "保存",
                    fontSize = 13.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = C.accent,
                    modifier = Modifier
                        .border(1.dp, C.accent.copy(alpha = 0.4f), RoundedCornerShape(8.dp))
                        .clickable { onSave(pickedProviders.toList(), pickedModels.toList()) }
                        .padding(horizontal = 14.dp, vertical = 8.dp),
                )
            }
        }
    }
}

/** 一组多选 chip；泛型只是为了让「对象」与「模型名」两种形态共用同一份样式与布局。 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun <T> ChipSection(
    title: String,
    note: String,
    emptyHint: String,
    items: List<T>,
    itemKey: (T) -> String,
    itemLabel: (T) -> String,
    itemBadge: (T) -> String?,
    selected: Set<String>,
    onToggle: (String) -> Unit,
) {
    val C = agateColors()
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(title, fontSize = 13.5.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
        Spacer(Modifier.width(6.dp))
        Text(note, fontSize = 10.5.sp, color = C.textDim)
    }
    Spacer(Modifier.height(8.dp))
    if (items.isEmpty()) {
        Box(
            Modifier
                .fillMaxWidth()
                .background(C.insetBg, RoundedCornerShape(10.dp))
                .padding(horizontal = 10.dp, vertical = 12.dp),
        ) {
            Text(emptyHint, fontSize = 11.5.sp, color = C.textDim, lineHeight = 15.sp)
        }
    } else {
        FlowRow(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            items.forEach { item ->
                val key = itemKey(item)
                val on = key in selected
                Row(
                    Modifier
                        .clip(RoundedCornerShape(9.dp))
                        .background(if (on) C.accent.copy(alpha = 0.13f) else C.insetBg)
                        .border(
                            1.dp,
                            if (on) C.accent.copy(alpha = 0.38f) else C.border,
                            RoundedCornerShape(9.dp),
                        )
                        .clickable { onToggle(key) }
                        .padding(horizontal = 10.dp, vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    if (on) {
                        Text("✓ ", fontSize = 11.sp, color = C.accent)
                    }
                    Text(
                        itemLabel(item),
                        fontSize = 11.5.sp,
                        color = if (on) C.accent else C.textSub,
                        maxLines = 1,
                    )
                    itemBadge(item)?.let { badge ->
                        Spacer(Modifier.width(5.dp))
                        Text(
                            badge,
                            fontSize = 10.sp,
                            style = AgateType.Num,
                            color = if (on) C.accent else C.textDim,
                        )
                    }
                }
            }
        }
    }
}
