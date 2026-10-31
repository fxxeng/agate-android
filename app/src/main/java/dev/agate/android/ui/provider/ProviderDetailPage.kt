package dev.agate.android.ui.provider

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
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.AccountOps
import dev.agate.android.data.CustomProviderOps
import dev.agate.android.data.ProviderIdentityOps
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.ScheduledTaskOps
import dev.agate.android.data.PresetRegistry
import dev.agate.android.data.dto.AccountDto
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateProviderAvatar
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject

/**
 * 提供商详情（对齐 v44 sub-providerdetail，**两 tab**）：
 * - 账号：逐账号余额与操作（上移 · 编辑备注 · 启停 · 删除）+ 添加账号 + 批量操作
 * - 设置：启用开关 / 自动化 / 家专属设置 / 测试连通性 / 删除提供商
 *
 * ── 为什么没有「概览」tab（原三 tab 的第一段）─────────────────────
 * 那一段 9 项里只有 2 项能改（启用提供商、自动续期），其余 7 项全是别处已经
 * 写过的字：账号数顶栏徽章有、余额账号 tab 有、登录通道与接入说明是添加页的
 * 内容、提供商 ID 是报障才要的技术细节、队列位置在路由页有完整版（还带跳过
 * 原因）。一个「只能看、且看到的都是别处删节版」的 tab 只会让人多点一次。
 * 所以把能改的两项并进「设置」（它本来就是设置语义），其余删掉 —— 队列位置
 * 改成账号 tab 底部一行指路，提供商 ID 降级成设置页脚注。
 *
 * 真实数据源：/api/session.accounts（列表）、/api/accounts/usage（余额/积分）、
 * /api/accounts/batch（启停/删除）、PATCH /api/accounts/{id}（改备注名）、
 * /api/accounts/{id}/move（调序）。倍率/续期/限流明细等依赖内核暂未暴露的端点，
 * 本页不臆造，统一在底部说明。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ProviderDetailPage(
    providerId: String,
    onBack: () -> Unit,
    onAddAccount: () -> Unit = {},
    /** 跳「路由预演」页（账号 tab 底部指路行用）。空实现时那一行不渲染。 */
    onOpenRouting: (() -> Unit)? = null,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val preset = PresetRegistry.ALL.find { it.id == providerId }

    val snap by GatewayRepository.snapshot.collectAsState()
    val accounts = snap.accountList.filter { it.provider == providerId }

    // 两个 tab：0 = 账号（默认）、1 = 设置。进这页十次有九次是加账号或看余额。
    var tab by remember { mutableStateOf(TAB_ACCOUNTS) }
    // ── 自定义提供商记录（名称 / 协议 / BaseURL / 余额方案）──
    // 内置 24 家没有这份记录，它们的配置在 PresetRegistry 里；自定义家的配置
    // 只在内核 `/api/custom-providers` 里，不拉就什么都改不了（上一版就是这样：
    // 详情页对 custom- 家没有任何可编辑项）。
    val isCustom = CustomProviderOps.isCustomProviderId(providerId)
    var customRecord by remember { mutableStateOf<CustomProviderOps.Record?>(null) }
    var customReload by remember { mutableIntStateOf(0) }
    LaunchedEffect(providerId, customReload) {
        if (isCustom) {
            customRecord = CustomProviderOps.Record
                .listOf(CustomProviderOps.list())
                .firstOrNull { it.id == providerId }
        }
    }
    // 批量工具条是否展开（页头「批量」按钮切换），切 tab 时自动收起
    var bulkOpen by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var usageLabel = remember { mutableStateMapOf<String, String>() }   // id → 展示文案
    var editing by remember { mutableStateOf<AccountDto?>(null) }
    var editName by remember { mutableStateOf("") }
    var editConc by remember { mutableStateOf("") }
    // CatPaw 账号的余额查询凭证草稿（编辑弹窗用；留空 = 不修改）
    var editBalanceToken by remember { mutableStateOf("") }
    var confirmRemove by remember { mutableStateOf<AccountDto?>(null) }
    var confirmRemoveAll by remember { mutableStateOf(false) }
    // 「额度设限」的编辑目标（null = 弹窗关闭）
    var limiterTarget by remember { mutableStateOf<AccountDto?>(null) }
    // 「删除提供商」与「删除全部账号」是两件事（前者连配置一起没），
    // 共用一个状态会让文案互相冒充 —— 分开。
    var confirmDeleteProvider by remember { mutableStateOf(false) }
    // 自动化：凭证自动维护（自动续期）是**全局**定时任务，这里只做该家的入口与节奏调整
    var credTask by remember { mutableStateOf<JSONObject?>(null) }
    // 「自动续期」按家关闭清单（黑名单；空 = 全家开启）。null = 还没读到
    var disabledProviders by remember { mutableStateOf<Set<String>?>(null) }
    var editingInterval by remember { mutableStateOf(false) }
    var intervalText by remember { mutableStateOf("") }

    // 这一家有没有余额能力（决定账号卡上要不要显示「额度设限」入口）——
    // 判据与列表页同源：内核 `ProviderAdapter::supports_usage` 经
    // /api/session 的 `providers[].usage` 带下来。
    val usageCapable = snap.providerUsage[providerId] ?: false

    DisposableEffect(Unit) {
        AccountOps.configure(GatewayPrefs.apiKey(ctx))
        onDispose { }
    }

    fun toast(m: String) = Toast.makeText(ctx, m, Toast.LENGTH_SHORT).show()

    fun loadUsage(id: String? = null, force: Boolean = false) {
        if (busy) return
        busy = true
        scope.launch {
            // force=false 时由 UsageCache 决定：新鲜缓存直接返回，过期缓存也先用
            // 旧的（进详情页不再自动打上游，避免频繁请求被官方风控）
            val data = AccountOps.usage(id, force = force)
            busy = false
            if (data == null) { toast("余额查询失败"); return@launch }
            val arr = data.optJSONArray("results")
            if (arr != null) {
                for (i in 0 until arr.length()) {
                    val row = arr.optJSONObject(i) ?: continue
                    val aid = row.optString("id")
                    if (aid.isBlank()) continue
                    // ⚠️ `error` 为 JSON null 时不能用 optString（会得到字符串 "null"，
                    // 界面变成「查询失败：null」—— 真机踩到过）
                    val err = (row.opt("error") as? String)?.trim().orEmpty()
                    val usage = row.optJSONObject("usage")
                    usageLabel[aid] = when {
                        err.isNotEmpty() -> "查询失败：$err"
                        usage == null -> "未返回余额数据"
                        else -> formatUsage(usage) ?: "已查询"
                    }
                }
            }
            toast(if (force) "余额已更新" else "余额已载入（缓存）")
        }
    }

    LaunchedEffect(Unit) {
        ScheduledTaskOps.configure(GatewayPrefs.apiKey(ctx))
        disabledProviders = ScheduledTaskOps.disabledProviders()
        val arr = ScheduledTaskOps.list()
        if (arr != null) {
            for (i in 0 until arr.length()) {
                val t = arr.optJSONObject(i) ?: continue
                if (t.optString("id") == ScheduledTaskOps.TASK_CREDENTIAL_MAINTENANCE) {
                    credTask = t
                    break
                }
            }
        }
    }

    LaunchedEffect(accounts.map { it.id }) {
        if (accounts.isNotEmpty()) loadUsage(force = false)
    }

    fun batchAction(action: String, ids: List<String> = accounts.map { it.id }, done: String) {
        if (ids.isEmpty() || busy) return
        busy = true
        scope.launch {
            val ok = AccountOps.batch(ids, action)
            busy = false
            toast(if (ok) done else "操作失败")
            if (ok) GatewayRepository.refreshOnce()
        }
    }

    // 底部安全区由 AgateNavHost 统一加（见那里的说明），这里只管状态栏
    Column(Modifier.fillMaxSize().background(C.bg).statusBarsPadding()) {
        // ── 顶栏 ──
        Row(
            Modifier.fillMaxWidth().padding(start = 4.dp, end = 16.dp, top = 4.dp, bottom = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                Modifier.size(36.dp).clickable(onClick = onBack),
                contentAlignment = Alignment.Center,
            ) { Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "返回", tint = C.textMain) }
            // 自定义家的名字在 `customRecord` 里（注册表没有它）：不接这一层的话
            // 标题会显示 `custom-7a3f9c21`，与列表页的名称对不上。
            val displayLabel = customRecord?.name ?: preset?.label ?: providerId
            AgateProviderAvatar(providerId = providerId, label = displayLabel, size = 32.dp, corner = 10.dp)
            Spacer(Modifier.width(10.dp))
            Text(
                displayLabel,
                color = C.textMain, fontSize = 18.sp, fontWeight = FontWeight.Bold,
                modifier = Modifier.weight(1f),
            )
            val enabledCount = accounts.count { it.enabled }
            when {
                accounts.isEmpty() -> AgateBadge("无账号", BadgeTone.GRAY)
                enabledCount == accounts.size -> AgateBadge("就绪", BadgeTone.GREEN)
                enabledCount == 0 -> AgateBadge("全部停用", BadgeTone.RED)
                else -> AgateBadge("$enabledCount/${accounts.size} 启用", BadgeTone.AMBER)
            }
            // 「批量」只在账号 tab 出现：设置 tab 上没有可批量作用的对象。
            // 无账号时也不出现 —— 三颗点了没反应的按钮比没有更糟。
            if (tab == TAB_ACCOUNTS && accounts.isNotEmpty()) {
                Spacer(Modifier.width(8.dp))
                val on = bulkOpen
                Text(
                    "批量",
                    color = if (on) C.accent else C.textSub,
                    fontSize = 12.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .background(
                            if (on) C.accent.copy(alpha = 0.14f) else C.btnBg,
                            RoundedCornerShape(9.dp),
                        )
                        .clickable { bulkOpen = !bulkOpen }
                        .padding(horizontal = 10.dp, vertical = 6.dp),
                )
            }
        }

        AgateSegmented(
            options = listOf("账号 · ${accounts.size}", "设置"),
            selected = tab,
            onSelect = {
                tab = it
                // 切走时收起：否则下次回账号页会看到上次留下的工具条，
                // 像是「批量模式一直开着」，而用户已不记得自己开过。
                if (it != TAB_ACCOUNTS) bulkOpen = false
            },
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
        )

        // 账号级字段写入（zcodePlan / balanceToken 等）：两处调用共用
        // （「账号」tab 的账号卡与「设置」tab 的专有设置），避免两份实现漂移
        // 用 val lambda 而不是 fun：要作为参数往子组件传（局部 fun 不能直接当函数值用）
        val patchAccount: (AccountDto, JSONObject) -> Unit = { acc, patch ->
            if (!busy) {
                busy = true
                scope.launch {
                    val r = AccountOps.update(acc.id, patch)
                    busy = false
                    toast(if (r.ok) "已保存（${acc.displayName}）" else (r.error ?: "保存失败"))
                    if (r.ok) GatewayRepository.refreshOnce()
                }
            }
        }

        when (tab) {
            TAB_ACCOUNTS -> AccountsTab(
                accounts = accounts,
                usageLabel = usageLabel,
                busy = busy,
                usageCapable = usageCapable,
                onOpenLimiter = { acc -> limiterTarget = acc },
                onQueryUsage = { loadUsage(force = true) },
                onEdit = { acc ->
                    editing = acc
                    editName = acc.displayName
                    editConc = acc.raw.optInt("maxConcurrent", 0).takeIf { it > 0 }?.toString() ?: ""
                    // 凭证值拿不到（内核只回 hasBalanceToken），草稿每次都从空开始
                    editBalanceToken = ""
                },
                onPin = { acc ->
                    if (!busy) {
                        busy = true
                        scope.launch {
                            // 置顶 = 连续上移到本家队首（内核只有单向 move）
                            repeat(accounts.size) { runCatching { AccountOps.move(acc.id, up = true) } }
                            busy = false
                            toast("已置顶")
                            GatewayRepository.refreshOnce()
                        }
                    }
                },
                onToggle = { acc -> batchAction(if (acc.enabled) "disable" else "enable", listOf(acc.id), if (acc.enabled) "已禁用" else "已启用") },
                onDelete = { acc -> confirmRemove = acc },
                onAdd = onAddAccount,
                onEnableAll = { batchAction("enable", done = "已全部启用") },
                onDisableAll = { batchAction("disable", done = "已全部禁用") },
                onDeleteAll = { confirmRemoveAll = true },
                onPatchAccount = patchAccount,
                bulkOpen = bulkOpen,
                onOpenRouting = onOpenRouting,
            )
            else -> SettingsTab(
                accounts = accounts,
                providerId = providerId,
                custom = customRecord,
                onCustomSaved = { customReload++ },
                busy = busy,
                onPatchAccount = patchAccount,
                allEnabled = accounts.isNotEmpty() && accounts.all { it.enabled },
                onToggleAll = { enable ->
                    batchAction(
                        if (enable) "enable" else "disable",
                        done = if (enable) "已全部启用" else "已全部禁用",
                    )
                },
                credTask = credTask,
                autoRefreshOn = disabledProviders?.let { providerId !in it },
                onToggleAutoRefresh = { enabled ->
                    val current = disabledProviders ?: emptySet()
                    val next = if (enabled) current - providerId else current + providerId
                    scope.launch {
                        val r = ScheduledTaskOps.setDisabledProviders(next.toList().sorted())
                        toast(
                            if (r.ok) (if (enabled) "已开启该家自动续期" else "已关闭该家自动续期")
                            else (r.error ?: "保存失败")
                        )
                        if (r.ok) disabledProviders = next
                    }
                },
                onEditInterval = {
                    intervalText = (credTask?.optInt("interval", 10) ?: 10).toString()
                    editingInterval = true
                },
                onRefreshProvider = {
                    if (!busy) {
                        busy = true
                        scope.launch {
                            var okCount = 0
                            var lastErr: String? = null
                            accounts.forEach { acc ->
                                val r = AccountOps.refreshAccount(acc.id)
                                if (r.ok) okCount++ else lastErr = r.error
                            }
                            busy = false
                            toast(
                                if (okCount == accounts.size) "已续期 $okCount 个账号"
                                else "成功 $okCount/${accounts.size}" + (lastErr?.let { " · $it" } ?: "")
                            )
                            GatewayRepository.refreshOnce()
                        }
                    }
                },
                onTest = {
                    val first = accounts.firstOrNull()
                    if (first == null) {
                        toast("该家没有账号可测")
                    } else if (!busy) {
                        busy = true
                        scope.launch {
                            val data = AccountOps.usage(first.id, force = true)
                            busy = false
                            val row = data?.optJSONArray("results")?.optJSONObject(0)
                            val err = row?.optString("error").orEmpty()
                            toast(
                                if (row != null && err.isBlank())
                                    "连通正常 · ${formatUsage(row.optJSONObject("usage")) ?: "已响应"}"
                                else "连通失败：${err.ifBlank { "无响应" }}"
                            )
                        }
                    }
                },
                onDeleteProvider = { confirmDeleteProvider = true },
                onRefreshExpiring = {
                    if (!busy) {
                        busy = true
                        scope.launch {
                            val r = AccountOps.refreshExpiring()
                            busy = false
                            toast(if (r.ok) "已触发凭证刷新" else (r.error ?: "刷新失败"))
                            GatewayRepository.refreshOnce()
                        }
                    }
                },
            )
        }
    }

    // ── 调整自动续期间隔（全局任务，就地改，不用去任务中心）──
    if (editingInterval) {
        AlertDialog(
            onDismissRequest = { editingInterval = false },
            title = { Text("自动续期间隔") },
            text = {
                Column {
                    Text("单位：分钟（该任务为全局节奏，对所有提供商生效）", fontSize = 12.sp, color = agateColors().textSub)
                    Spacer(Modifier.height(6.dp))
                    OutlinedTextField(
                        value = intervalText,
                        onValueChange = { v -> intervalText = v.filter { it.isDigit() }.take(4) },
                        singleLine = true,
                    )
                }
            },
            confirmButton = {
                TextButton(onClick = {
                    val minutes = intervalText.toIntOrNull()
                    editingInterval = false
                    if (minutes == null || minutes <= 0) {
                        toast("请输入正整数分钟")
                        return@TextButton
                    }
                    scope.launch {
                        val r = ScheduledTaskOps.configure(
                            ScheduledTaskOps.TASK_CREDENTIAL_MAINTENANCE,
                            interval = minutes,
                        )
                        toast(if (r.ok) "已改为每 $minutes 分钟" else (r.error ?: "保存失败"))
                        if (r.ok) {
                            val arr = ScheduledTaskOps.list()
                            if (arr != null) for (i in 0 until arr.length()) {
                                val t = arr.optJSONObject(i) ?: continue
                                if (t.optString("id") == ScheduledTaskOps.TASK_CREDENTIAL_MAINTENANCE) {
                                    credTask = t
                                    break
                                }
                            }
                        }
                    }
                }) { Text("保存") }
            },
            dismissButton = { TextButton(onClick = { editingInterval = false }) { Text("取消") } },
        )
    }

    // ── 编辑账号（备注名 / 并发 / CatPaw 余额凭证）──
    editing?.let { acc ->
        // 该账号已配的余额凭证值拿不到（内核只回 hasBalanceToken），所以这里是
        // 「留空 = 不修改」而不是「回填现值」：用户输入新值才写，留空不动。
        val hasToken = acc.raw.optBoolean("hasBalanceToken", false)
        AlertDialog(
            onDismissRequest = { editing = null },
            title = { Text("编辑账号") },
            text = {
                Column {
                    Text("备注名", fontSize = 12.sp, color = agateColors().textSub)
                    Spacer(Modifier.height(6.dp))
                    OutlinedTextField(value = editName, onValueChange = { editName = it }, singleLine = true)
                    Spacer(Modifier.height(10.dp))
                    Text("并发上限（留空 = 不限）", fontSize = 12.sp, color = agateColors().textSub)
                    Spacer(Modifier.height(6.dp))
                    OutlinedTextField(
                        value = editConc,
                        onValueChange = { v -> editConc = v.filter { it.isDigit() }.take(3) },
                        singleLine = true,
                    )
                    // CatPaw 专属：余额查询凭证按账号填（见账号卡上的说明）
                    if (acc.provider == "catpaw") {
                        Spacer(Modifier.height(14.dp))
                        Text(
                            "余额查询凭证",
                            fontSize = 12.sp, color = agateColors().textSub,
                        )
                        Spacer(Modifier.height(6.dp))
                        OutlinedTextField(
                            value = editBalanceToken,
                            onValueChange = { editBalanceToken = it.trim() },
                            singleLine = true,
                            placeholder = {
                                Text(
                                    if (hasToken) "已配置 · 留空 = 不修改" else "粘贴 token2 / balanceToken",
                                    fontSize = 12.sp,
                                )
                            },
                            modifier = Modifier.fillMaxWidth(),
                        )
                        Spacer(Modifier.height(4.dp))
                        Text(
                            "余额要带 passport cookie（token2）才能查。",
                            fontSize = 11.sp, color = agateColors().textDim,
                        )
                    }
                }
            },
            confirmButton = {
                TextButton(onClick = {
                    val target = acc
                    val tokenDraft = editBalanceToken
                    editing = null
                    editBalanceToken = ""
                    scope.launch {
                        val patch = JSONObject().put("name", editName.trim())
                        editConc.trim().toIntOrNull()?.let { patch.put("maxConcurrent", it) }
                        // 留空 = 不提交这个键（内核语义里空串是「清除」，与用户
                        // 留空的意思「别动」不同，所以不能传空串进去）
                        if (tokenDraft.isNotBlank()) patch.put("balanceToken", tokenDraft)
                        val r = AccountOps.update(target.id, patch)
                        toast(if (r.ok) "已保存" else (r.error ?: "保存失败"))
                        if (r.ok) GatewayRepository.refreshOnce()
                    }
                }) { Text("保存") }
            },
            dismissButton = { TextButton(onClick = { editing = null; editBalanceToken = "" }) { Text("取消") } },
        )
    }

    // ── 删除单账号 ──
    confirmRemove?.let { acc ->
        AlertDialog(
            onDismissRequest = { confirmRemove = null },
            title = { Text("删除账号") },
            text = { Text("删除「${acc.displayName}」？登录态一并移除，不可恢复。", fontSize = 13.sp) },
            confirmButton = {
                TextButton(onClick = {
                    val target = acc
                    confirmRemove = null
                    batchAction("remove", listOf(target.id), "已删除账号")
                }) { Text("删除", color = C.red) }
            },
            dismissButton = { TextButton(onClick = { confirmRemove = null }) { Text("取消") } },
        )
    }

    // ── 删除提供商（全部账号）──
    if (confirmRemoveAll) {
        AlertDialog(
            onDismissRequest = { confirmRemoveAll = false },
            title = { Text("删除全部账号") },
            text = { Text("将删除「${preset?.label ?: providerId}」下全部 ${accounts.size} 个账号，凭证一并清除，不可恢复。", fontSize = 13.sp) },
            confirmButton = {
                TextButton(onClick = {
                    confirmRemoveAll = false
                    if (accounts.isEmpty()) { onBack(); return@TextButton }
                    busy = true
                    scope.launch {
                        val ok = AccountOps.batch(accounts.map { it.id }, "remove")
                        busy = false
                        toast(if (ok) "已删除该提供商全部账号" else "删除失败")
                        if (ok) { GatewayRepository.refreshOnce(); onBack() }
                    }
                }) { Text("删除", color = C.red) }
            },
            dismissButton = { TextButton(onClick = { confirmRemoveAll = false }) { Text("取消") } },
        )
    }

    if (confirmDeleteProvider) {
        AlertDialog(
            onDismissRequest = { confirmDeleteProvider = false },
            title = { Text(if (isCustom) "删除自定义提供商" else "删除提供商") },
            text = {
                Text(
                    if (isCustom) {
                        // 自定义家的删除是**级联**的：内核一次删掉提供商记录 +
                        // 名下账号 + 各 Key 白名单里的这一家。文案必须把「连带
                        // 删几个账号」写在问句里，那是可逆与不可逆的分界。
                        "将删除自定义提供商「${customRecord?.name ?: providerId}」：连同其 " +
                            "${accounts.size} 个账号与凭证一并删除，并从各 Key 的可用提供商里移除这家，不可恢复。"
                    } else {
                        "将移除「${preset?.label ?: providerId}」这一家：连同其 ${accounts.size} 个账号、凭证与设置一并删除，不可恢复。"
                    },
                    fontSize = 13.sp,
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    confirmDeleteProvider = false
                    busy = true
                    scope.launch {
                        if (isCustom) {
                            // 走内核级联接口。**不能**只批量删账号 —— 那样会留下
                            // 一条没有账号的提供商记录：它从列表里消失（列表只列
                            // 有账号的家），却再也没入口改或删，成为孤儿。
                            // 上游 3.0.8 把这个坑写成了「已知可达性缺口」，这里修掉。
                            val r = CustomProviderOps.remove(providerId)
                            busy = false
                            val removed = r.data?.optInt("accountsRemoved", 0) ?: 0
                            val keys = r.data?.optInt("keysUpdated", 0) ?: 0
                            val free = r.data?.optInt("keysUnrestricted", 0) ?: 0
                            toast(
                                if (r.ok) buildString {
                                    append("已删除提供商")
                                    if (removed > 0) append("及 $removed 个账号")
                                    if (keys > 0) append("，并从 $keys 把 Key 的可用范围里移除")
                                    if (free > 0) append("（其中 $free 把恢复为不限制）")
                                } else {
                                    r.error ?: "删除失败"
                                }
                            )
                            if (r.ok) { GatewayRepository.refreshOnce(); onBack() }
                        } else {
                            if (accounts.isEmpty()) { busy = false; onBack(); return@launch }
                            val ok = AccountOps.batch(accounts.map { it.id }, "remove")
                            busy = false
                            toast(if (ok) "已删除该提供商" else "删除失败")
                            if (ok) { GatewayRepository.refreshOnce(); onBack() }
                        }
                    }
                }) { Text("删除", color = C.red) }
            },
            dismissButton = { TextButton(onClick = { confirmDeleteProvider = false }) { Text("取消") } },
        )
    }
    // ── 额度设限弹窗（与列表页同款）──
    limiterTarget?.let { acc ->
        AccountLimiterDialog(
            accountName = acc.displayName,
            current = limiterRulesOf(acc.raw),
            onDismiss = { limiterTarget = null },
            onSave = { result ->
                limiterTarget = null
                busy = true
                scope.launch {
                    val body = JSONObject()
                    body.put("limiters", JSONArray().apply { result.forEach { put(it.toJson()) } })
                    val r = AccountOps.update(acc.id, body)
                    busy = false
                    toast(if (r.ok) (if (result.isEmpty()) "已取消额度限制" else "已设置额度限制") else (r.error ?: "保存失败"))
                    if (r.ok) GatewayRepository.refreshOnce()
                }
            },
        )
    }
}

/* ───────────── 账号 ───────────── */
@Composable
private fun AccountsTab(
    accounts: List<AccountDto>,
    usageLabel: Map<String, String>,
    busy: Boolean,
    /** 该家是否支持余额查询（透传到账号卡，决定「额度设限」入口是否渲染）。 */
    usageCapable: Boolean,
    onOpenLimiter: (AccountDto) -> Unit,
    onQueryUsage: () -> Unit,
    onPin: (AccountDto) -> Unit,
    onEdit: (AccountDto) -> Unit,
    onToggle: (AccountDto) -> Unit,
    onDelete: (AccountDto) -> Unit,
    onAdd: () -> Unit,
    onEnableAll: () -> Unit,
    onDisableAll: () -> Unit,
    onDeleteAll: () -> Unit,
    /** 账号级字段的写入（如 ZCode 的 zcodePlan / balanceToken）。 */
    onPatchAccount: (AccountDto, JSONObject) -> Unit,
    /** 批量工具条是否展开（由页头「批量」按钮控制）。 */
    bulkOpen: Boolean,
    /** 跳路由页；null = 不渲染底部指路行。 */
    onOpenRouting: (() -> Unit)?,
) {
    val C = agateColors()
    LazyColumn(
        Modifier.fillMaxSize().padding(horizontal = 16.dp),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = 10.dp),
    ) {
        // ── 批量工具条：由页头「批量」按钮切换，浮在账号列表**上方** ──
        // 放在列表上方而不是下方，是因为它作用的对象（账号）就在下面 ——
        // 原来放在列表底部的常驻卡片要滚到底才点得到，而账号只有 1 个时
        // 那三颗按钮常年无用却一直占位。
        if (bulkOpen) {
            item {
                Row(
                    Modifier.fillMaxWidth()
                        .background(C.accent.copy(alpha = 0.07f), RoundedCornerShape(12.dp))
                        .border(1.dp, C.accent.copy(alpha = 0.30f), RoundedCornerShape(12.dp))
                        .padding(8.dp),
                    horizontalArrangement = Arrangement.spacedBy(7.dp),
                ) {
                    SmallAction("全部启用", enabled = !busy && accounts.isNotEmpty(), onClick = onEnableAll, modifier = Modifier.weight(1f))
                    SmallAction("全部禁用", enabled = !busy && accounts.isNotEmpty(), onClick = onDisableAll, modifier = Modifier.weight(1f))
                    // 删除放最右且标红：一行里混着危险操作时，靠颜色 + 位置区分，
                    // 比隔行摆放更不容易误点相邻的普通按钮。
                    SmallAction("删除全部", enabled = !busy && accounts.isNotEmpty(), onClick = onDeleteAll, modifier = Modifier.weight(1f), danger = true)
                }
                Spacer(Modifier.height(10.dp))
            }
        }
        item {
            Card {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("账号列表", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                    SmallAction(if (busy) "查询中…" else "刷新余额", primary = false, enabled = !busy && accounts.isNotEmpty(), onClick = onQueryUsage)
                }
                Spacer(Modifier.height(8.dp))
                if (accounts.isEmpty()) {
                    Text("该家还没有账号 · 点下方「+ 添加账号」", color = C.textSub, fontSize = 13.sp)
                }
                accounts.forEachIndexed { i, acc ->
                    Spacer(Modifier.height(8.dp))
                    AccountBlock(
                        acc = acc,
                        rank = i + 1,
                        usage = usageLabel[acc.id],
                        usageCapable = usageCapable,
                        onOpenLimiter = onOpenLimiter,
                        busy = busy,
                        onPin = { onPin(acc) },
                        onEdit = { onEdit(acc) },
                        onToggle = { onToggle(acc) },
                        onDelete = { onDelete(acc) },
                        onPatchAccount = onPatchAccount,
                    )
                }
                Spacer(Modifier.height(10.dp))
                SmallAction("+ 添加账号", primary = true, enabled = true, modifier = Modifier.fillMaxWidth(), onClick = onAdd)
            }
        }
        // ── 队列位置：改成一行指路，不再重复渲染前 6 个账号 ──
        // 路由页的「候选队列」是全量版，还写明每个账号被跳过的原因；
        // 这里的删节版既不全也不解释，属于重复且更差的表达。
        if (onOpenRouting != null) {
            item {
                Spacer(Modifier.height(12.dp))
                Row(
                    Modifier.fillMaxWidth()
                        .clickable(onClick = onOpenRouting)
                        .padding(vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        "本家账号在全局队列里的先后，在路由页统一调整",
                        color = C.accent, fontSize = 12.sp, fontWeight = FontWeight.SemiBold,
                        modifier = Modifier.weight(1f),
                    )
                    Text("›", color = C.accent, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
                }
            }
        }
    }
}

@Composable
private fun AccountBlock(
    acc: AccountDto,
    rank: Int,
    usage: String?,
    busy: Boolean,
    /** 该家是否支持余额查询（决定要不要显示「额度设限」入口）。 */
    usageCapable: Boolean,
    onPin: () -> Unit,
    onEdit: () -> Unit,
    onToggle: () -> Unit,
    onDelete: () -> Unit,
    onOpenLimiter: (AccountDto) -> Unit,
    onPatchAccount: (AccountDto, JSONObject) -> Unit,
) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .background(C.insetBg, RoundedCornerShape(12.dp))
            .padding(horizontal = 12.dp, vertical = 10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("#$rank", color = C.textDim, fontSize = 12.sp)
            Spacer(Modifier.width(8.dp))
            Text(acc.displayName, color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
            when {
                !acc.enabled -> AgateBadge("停用", BadgeTone.GRAY)
                !acc.hasCredentials -> AgateBadge("缺凭证", BadgeTone.RED)
                acc.lowBalance -> AgateBadge("余额低", BadgeTone.AMBER)
                else -> AgateBadge("就绪", BadgeTone.GREEN)
            }
        }
        Spacer(Modifier.height(4.dp))
        Text(
            usage ?: "余额未查询",
            color = if (usage?.startsWith("查询失败") == true) C.red else C.textSub,
            fontSize = 12.sp,
        )
        // 凭证（与列表页账号卡同口径：双字段 + 秒毫秒归一 + 统一到期文案）
        credentialExpiryMs(acc)?.let { at ->
            ExpiryLine(at)
        }
        // ── 余额查询凭证（CatPaw 专属，**账号级**）────────────────
        //
        // 内核把它存在账号自己那条记录上（`update_catpaw_balance_token(id, …)`
        // 按 id + provider 定位单条），视图只透出 `hasBalanceToken`（配没配，
        // 值本身不下发前端 —— 它是完整会话凭证，而账号列表会被截图排障）。
        // 之前这一项在「设置」tab 按 `accounts.first()` 取值：多账号时其余账号
        // 永远查不到余额，界面上还看不出哪一个是缺的。按账号显示才说得清。
        if (acc.provider == "catpaw") {
            val hasToken = acc.raw.optBoolean("hasBalanceToken", false)
            Row(Modifier.fillMaxWidth().padding(top = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                Text("余额凭证", color = C.textSub, fontSize = 12.sp, modifier = Modifier.weight(1f))
                Text(
                    if (hasToken) "已配置" else "未配置 · 点「编辑」补",
                    color = if (hasToken) C.textSub else C.amber,
                    fontSize = 12.sp,
                )
            }
        }
        // 额度设限：只对支持余额查询的家显示（该家没接余额时填了阈值也看不到效果）
        if (usageCapable) {
            LimiterEntryRow(
                summary = limiterRulesOf(acc.raw).firstOrNull()?.summary() ?: "未设限",
                onOpen = { onOpenLimiter(acc) },
            )
        }
        // ── 套餐通道（ZCode 专属，**按账号**而不是按家）───────────────
        //
        // 为什么必须按账号：`zcodePlan` 存在账号自己的 raw 里（内核
        // `zcode::plan_of` 读的就是它），而两条通道是**两份独立的额度**。
        // 同一家的两个账号完全可能一个走编码套餐、一个走活动套餐 ——
        // 放在「设置」tab 里只能改第一个账号，第二个账号无处可改。
        if (acc.provider == "zcode" || acc.provider == "zcode-intl") {
            ZcodePlanRow(acc = acc, busy = busy, onPatch = onPatchAccount)
        }
        Spacer(Modifier.height(8.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            SmallAction("置顶", enabled = !busy, onClick = onPin)
            SmallAction("编辑", false, !busy, onClick = onEdit)
            SmallAction(if (acc.enabled) "停用" else "启用", false, !busy, onClick = onToggle)
            SmallAction("删除", false, !busy, onClick = onDelete, danger = true)
        }
    }

}

/**
 * ZCode「使用套餐」行（**账号级**）。
 *
 * ── 为什么按账号、而不是按家 ───────────────────────────────
 * `zcodePlan` 存在账号自己的 raw 里（内核 `zcode::plan_of` 读它），而两条通道
 * 对应**两份独立的额度**：同一家的两个账号完全可以一个走编码套餐、一个走活动
 * 套餐。此前这一项放在「设置」tab 并按 `firstAccount` 取值 —— 多个账号时
 * 只能改第一个，其余账号无处可改，且看起来像「这家的设置」。
 *
 * ── 四条提示各自回答什么（照抄 3.0.8 的文案）────────────────
 *   ① 两条通道的**区别**（走哪个端点、消耗哪份额度）—— 选之前必须知道；
 *   ② 活动套餐缺 jwt 时**仍可选中**但红字说明「会失败，请重新登录」——
 *      灰掉选项会让用户以为「这个功能不存在」，实际问题是他得重登一次；
 *   ③ 活动套餐要**验证码令牌**，给出池内库存（0 时要明说会失败）；
 *   ④ 令牌池由本应用**后台静默铸造**，界面关掉就铸不了 —— 这条决定了
 *      用户该不该把界面留着。
 */
@Composable
private fun ZcodePlanRow(
    acc: AccountDto,
    busy: Boolean,
    onPatch: (AccountDto, JSONObject) -> Unit,
) {
    val C = agateColors()
    val current = acc.raw.optString("zcodePlan").ifBlank { CODING_PLAN }
    val hasJwt = acc.raw.optString("jwt").isNotBlank()
    // 令牌池是网关全局的一份：这里只触发「过期就刷新」，真正的数字从缓存读
    // （每个账号块各发一次请求会把列表刷成 N 次往返，见 CaptchaStatsCache）
    if (current == START_PLAN) {
        androidx.compose.runtime.LaunchedEffect(Unit) {
            dev.agate.android.data.CaptchaStatsCache.refreshIfStale()
        }
    }

    Column(Modifier.fillMaxWidth().padding(top = 8.dp)) {
        Text("使用套餐", color = C.textSub, fontSize = 12.sp)
        Spacer(Modifier.height(6.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(7.dp)) {
            listOf(
                CODING_PLAN to "编码套餐",
                START_PLAN to "活动套餐",
            ).forEach { (value, label) ->
                val active = current == value
                Text(
                    label,
                    color = if (active) C.onAccent else C.textSub,
                    fontSize = 11.sp,
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
                        .clickable(enabled = !busy) {
                            if (!active) onPatch(acc, JSONObject().put("zcodePlan", value))
                        }
                        .padding(horizontal = 11.dp, vertical = 5.dp),
                )
            }
        }

        // ① 两条通道的区别（常显：选之前就得知道）
        Spacer(Modifier.height(6.dp))
        Text(
            if (current == START_PLAN) {
                "走 zcode.z.ai 端点，消耗 ZCode 套餐池的额度（新用户体验额度 / 活动领取）。"
            } else {
                "走开放平台端点，消耗开放平台的额度（订阅套餐 / 资源包 / 充值余额）。"
            },
            color = C.textDim,
            fontSize = 10.sp,
        )

        // ② 活动套餐但缺 jwt：可选中，但必须说清会失败
        if (current == START_PLAN && !hasJwt) {
            Spacer(Modifier.height(3.dp))
            Text(
                "该账号没有套餐登录态（jwt），活动套餐通道会失败：请重新登录该账号",
                color = C.red,
                fontSize = 10.sp,
            )
        }

        // ③ 活动套餐要验证码令牌：给池内库存
        if (current == START_PLAN) {
            Spacer(Modifier.height(3.dp))
            Text(
                "需要阿里云验证码令牌 · " + zcodePoolHint(),
                color = C.textDim,
                fontSize = 10.sp,
            )
            // ④ 令牌由后台铸造，界面关掉就铸不了
            Spacer(Modifier.height(2.dp))
            Text(
                "令牌由本应用后台静默铸造；界面关闭时无法铸造",
                color = C.textDim,
                fontSize = 10.sp,
            )
        }
    }
}

/** 详情页默认 tab：0 = 账号（第二个 tab「设置」由 `else` 分支兜住）。 */
private const val TAB_ACCOUNTS = 0

/** 套餐通道取值（与内核 `zcode::normalize_plan` 逐字一致）。 */
private const val CODING_PLAN = "coding-plan"
private const val START_PLAN = "start-plan"

/**
 * 令牌池库存文案。
 *
 * 池统计由一个**模块级缓存**提供（5 秒内复用上次结果），因为这一段在每个
 * ZCode 账号块里都会渲染 —— 每个账号各发一次请求会把列表刷成 N 次往返。
 * 拿不到时说「读取中…」，不编数字。
 */
private fun zcodePoolHint(): String {
    val stats = dev.agate.android.data.CaptchaStatsCache.get()
    return if (stats == null) {
        "读取中…"
    } else {
        val text = "令牌 ${stats.first} / ${stats.second}"
        if (stats.first == 0) "$text（库存为空，正在补；补不上时转发会失败）" else text
    }
}

/**
 * ZCode「客户端版本号」设置卡。
 *
 * ── 这个值为什么值得单独做一个输入框 ─────────────────────────
 * 它进推理请求的 `User-Agent: ZCode/{version}` 与 `X-ZCode-App-Version`，上游
 * 按客户端形态识别请求、**版本过旧会被拒**（这正是「账号有额度却调不通」的
 * 一类原因）。内置默认值会随版本更新，但上游抬门槛比发版快 —— 所以在界面上
 * 留一个能自己改的口子。
 */
@Composable
private fun ZcodeVersionCard() {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    var identity by remember { mutableStateOf<ProviderIdentityOps.ZcodeIdentity?>(null) }
    var editing by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }

    LaunchedEffect(Unit) { identity = ProviderIdentityOps.getZcode() }

    Card {
        Text("客户端版本号", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(4.dp))
        Text(
            "随请求头发给上游（UA 与 X-ZCode-App-Version）。上游按客户端形态识别请求，版本过旧会被拒；" +
                "调不通时可先试填与官方客户端一致的版本。",
            color = C.textSub, fontSize = 12.sp,
        )
        Spacer(Modifier.height(10.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            val current = identity
            Text(
                when {
                    current == null -> "读取中…"
                    current.overridden.isBlank() -> "当前生效 ${current.default}（默认）"
                    else -> "当前生效 ${current.overridden}（已覆盖，默认 ${current.default}）"
                },
                color = C.textSub,
                fontSize = 12.sp,
                modifier = Modifier.weight(1f),
            )
            SmallAction(
                if (editing == null) "修改" else "取消",
                primary = false,
                enabled = !busy && identity != null,
                onClick = { editing = if (editing == null) identity?.overridden.orEmpty() else null },
            )
        }
        editing?.let { draft ->
            Spacer(Modifier.height(8.dp))
            OutlinedTextField(
                value = draft,
                onValueChange = { editing = it },
                singleLine = true,
                textStyle = androidx.compose.ui.text.TextStyle(fontSize = 13.sp),
                placeholder = { Text(identity?.default ?: "3.16.2", fontSize = 12.sp) },
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(6.dp))
            Text(
                "留空 = 回到内置默认（${identity?.default ?: "—"}）",
                color = C.textDim, fontSize = 11.sp,
            )
            Spacer(Modifier.height(8.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                SmallAction("保存", primary = true, enabled = !busy, onClick = {
                    busy = true
                    scope.launch {
                        val r = ProviderIdentityOps.setZcode(draft.trim())
                        busy = false
                        Toast.makeText(
                            ctx,
                            if (r.ok) "已保存，下一个请求生效" else (r.error ?: "保存失败"),
                            Toast.LENGTH_SHORT,
                        ).show()
                        if (r.ok) {
                            identity = ProviderIdentityOps.getZcode()
                            editing = null
                        }
                    }
                })
            }
        }
    }
}

/**
 * Cline「伪装头」设置卡。
 *
 * ── 只列「默认清单里那几个」+ 允许改值，不做整表编辑器 ──────────
 * 用户的实际需求是「上游收紧了，把版本号/平台改一下」，而不是从零设计一套头。
 * 所以按默认清单逐项给输入框，**留空 = 发默认值**；这比自由增删行更不容易
 * 配坏（少一个硬门槛头 `X-CLIENT-TYPE`，整家就 403）。
 */
@Composable
private fun ClineHeadersCard() {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    var headers by remember { mutableStateOf<ProviderIdentityOps.ClineHeaders?>(null) }
    var draft by remember { mutableStateOf<Map<String, String>>(emptyMap()) }
    var expanded by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }

    LaunchedEffect(Unit) {
        headers = ProviderIdentityOps.getCline()
        draft = headers?.overrides ?: emptyMap()
    }

    Card {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("伪装头", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.height(4.dp))
                Text(
                    "上游按请求头判定「是不是 Cline 自家客户端」。X-CLIENT-TYPE 是免费池的硬门槛，" +
                        "其余头让请求更贴近官方 CLI；上游收紧时改这里",
                    color = C.textSub, fontSize = 12.sp,
                )
            }
            SmallAction(
                if (expanded) "收起" else "编辑",
                primary = false,
                enabled = headers != null,
                onClick = { expanded = !expanded },
            )
        }

        if (expanded) {
            val defaults = headers?.defaults ?: emptyMap()
            Spacer(Modifier.height(10.dp))
            defaults.keys.sorted().forEach { name ->
                val isFixed = name.equals("X-CLIENT-TYPE", ignoreCase = true)
                Spacer(Modifier.height(7.dp))
                Text(
                    if (isFixed) "$name（硬门槛，不可改）" else name,
                    color = if (isFixed) C.textDim else C.textSub,
                    fontSize = 11.sp,
                )
                Spacer(Modifier.height(3.dp))
                OutlinedTextField(
                    value = if (isFixed) (defaults[name] ?: "") else (draft[name] ?: ""),
                    onValueChange = { if (!isFixed) draft = draft + (name to it) },
                    enabled = !isFixed && !busy,
                    singleLine = true,
                    textStyle = androidx.compose.ui.text.TextStyle(fontSize = 12.sp),
                    placeholder = { Text(defaults[name] ?: "", fontSize = 11.sp) },
                    modifier = Modifier.fillMaxWidth(),
                )
            }
            Spacer(Modifier.height(8.dp))
            Text("留空 = 发内置默认值；输入框里的灰字就是当前默认", color = C.textDim, fontSize = 11.sp)
            Spacer(Modifier.height(8.dp))
            SmallAction("保存", primary = true, enabled = !busy, onClick = {
                busy = true
                scope.launch {
                    // 只提交非空的覆盖（空串在内核语义里是「不发这个头」，
                    // 而用户在这里清空输入框的意思是「用默认」——两者不同，
                    // 所以此处把空值直接从表里去掉）
                    val cleaned = draft.filterValues { it.isNotBlank() }
                    val r = ProviderIdentityOps.setCline(cleaned)
                    busy = false
                    Toast.makeText(
                        ctx,
                        if (r.ok) "已保存，下一个请求生效" else (r.error ?: "保存失败"),
                        Toast.LENGTH_SHORT,
                    ).show()
                    if (r.ok) {
                        headers = ProviderIdentityOps.getCline()
                        draft = headers?.overrides ?: emptyMap()
                    }
                }
            })
        }
    }
}

/* ───────────── 设置 ───────────── */
/**
 * 自定义提供商的「提供商设置」卡（只有 `custom-` 家有）。
 *
 * ── 为什么放在账号详情页而不是单开一个管理页 ────────────────
 * 上游 3.0.8 的 `custom-provider-ui.js` 头注释解释了同一件事：改 Base URL 的
 * 动机通常正是「这个账号连不上了」，就地改比先退出去找一个管理入口近一步。
 * 提供商级可改的只有三件事：**名称 / 协议 / Base URL**（外加伪装开关），
 * 与上游 `update()` 提交的字段集一致。
 *
 * ── 协议改动会立刻影响转发 ──────────────────────────────────
 * 内核按协议拼出站路径（`chat_completions` → `{base}/chat/completions`，
 * `anthropic` → `{base}/v1/messages`），所以两边的地址语义不同：Anthropic 要
 * 根地址，OpenAI 兼容要带 `/v1`。提示文案随所选协议换，别让人两头都填错。
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun CustomProviderCard(custom: CustomProviderOps.Record, onSaved: () -> Unit) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    var name by remember(custom.id) { mutableStateOf(custom.name) }
    var baseUrl by remember(custom.id) { mutableStateOf(custom.baseUrl) }
    var protocol by remember(custom.id) { mutableStateOf(custom.protocol) }
    var busy by remember { mutableStateOf(false) }

    val dirty = name != custom.name || baseUrl != custom.baseUrl || protocol != custom.protocol

    Card {
        Text("自定义提供商", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(4.dp))
        Text(
            "这一家的名称 / 协议 / Base URL · 改完立即生效，不用重建账号",
            color = C.textSub, fontSize = 11.sp,
        )
        Spacer(Modifier.height(10.dp))

        Text("名称", color = C.textSub, fontSize = 11.sp)
        Spacer(Modifier.height(3.dp))
        OutlinedTextField(
            value = name,
            onValueChange = { name = it.take(64) },
            enabled = !busy,
            singleLine = true,
            textStyle = androidx.compose.ui.text.TextStyle(fontSize = 12.sp),
            modifier = Modifier.fillMaxWidth(),
        )

        Spacer(Modifier.height(10.dp))
        Text("协议", color = C.textSub, fontSize = 11.sp)
        Spacer(Modifier.height(5.dp))
        // FlowRow 而非 Row：三个协议标签都很长，一行放不下时普通 Row 会给最后
        // 一个 0 宽度 —— 文字逐字折行成一根看不见的竖条，把卡片撑出大片空白
        //（新增提供商页踩过同一个坑，见那里 ProtocolChips 的注释）。
        FlowRow(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            CustomProviderOps.Protocol.entries.forEach { p ->
                val active = p == protocol
                Text(
                    p.label,
                    maxLines = 1,
                    softWrap = false,
                    color = if (active) C.onAccent else C.textSub,
                    fontSize = 11.sp,
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
                        .clickable(enabled = !busy) { protocol = p }
                        .padding(horizontal = 10.dp, vertical = 6.dp),
                )
            }
        }

        Spacer(Modifier.height(10.dp))
        Text("Base URL", color = C.textSub, fontSize = 11.sp)
        Spacer(Modifier.height(3.dp))
        OutlinedTextField(
            value = baseUrl,
            onValueChange = { baseUrl = it.trim() },
            enabled = !busy,
            singleLine = true,
            textStyle = androidx.compose.ui.text.TextStyle(fontSize = 12.sp),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(4.dp))
        Text(
            if (protocol == CustomProviderOps.Protocol.ANTHROPIC)
                "Anthropic 填根地址，不要带 /v1（内核按协议拼 /v1/messages）"
            else "OpenAI 兼容 / Responses 填到 /v1（内核按协议拼 /chat/completions 或 /responses）",
            color = C.textDim, fontSize = 10.sp,
        )

        // 余额方案与上游特判是「这一家是谁」的派生事实，不在界面改（上游同样
        // 只读），但要看得到 —— 否则用户不知道「为什么它有余额读数」。
        if (custom.balanceScheme.isNotBlank() || custom.clientEmulation.isNotBlank() ||
            custom.urlSuffix.isNotBlank() || custom.headerCount > 0
        ) {
            Spacer(Modifier.height(10.dp))
            Text(
                buildString {
                    if (custom.balanceScheme.isNotBlank()) {
                        append("余额方案 ")
                        append(CustomProviderOps.balanceSchemes.firstOrNull { it.first == custom.balanceScheme }?.second ?: custom.balanceScheme)
                    }
                    if (custom.urlSuffix.isNotBlank()) {
                        if (isNotEmpty()) append(" · ")
                        append("地址后缀 ").append(custom.urlSuffix)
                    }
                    if (custom.headerCount > 0) {
                        if (isNotEmpty()) append(" · ")
                        append("额外头 ").append(custom.headerCount).append(" 条")
                    }
                    if (custom.clientEmulation.isNotBlank()) {
                        if (isNotEmpty()) append(" · ")
                        append("客户端伪装 ").append(custom.clientEmulation)
                    }
                },
                color = C.textDim, fontSize = 10.sp,
            )
        }

        Spacer(Modifier.height(10.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            SmallAction(
                text = "保存",
                primary = true,
                enabled = dirty && !busy && name.isNotBlank(),
                onClick = {
                    if (!busy) {
                        busy = true
                        scope.launch {
                            val r = CustomProviderOps.update(
                                id = custom.id,
                                name = name.trim(),
                                baseUrl = baseUrl.trim(),
                                protocol = protocol,
                            )
                            busy = false
                            Toast.makeText(
                                ctx,
                                if (r.ok) "已保存" else (r.error ?: "保存失败"),
                                Toast.LENGTH_SHORT,
                            ).show()
                            if (r.ok) {
                                GatewayRepository.refreshOnce()
                                onSaved()
                            }
                        }
                    }
                },
            )
            Spacer(Modifier.width(10.dp))
            if (!dirty) {
                Text("没有改动", color = C.textDim, fontSize = 10.sp)
            }
        }
    }
}

@Composable
private fun SettingsTab(
    accounts: List<AccountDto>,
    providerId: String,
    /** 自定义提供商的记录（内置家为 null）。非空时本页顶部多一段「提供商设置」。 */
    custom: CustomProviderOps.Record?,
    onCustomSaved: () -> Unit,
    busy: Boolean,
    onPatchAccount: (AccountDto, JSONObject) -> Unit,
    onTest: () -> Unit,
    onDeleteProvider: () -> Unit,
    onRefreshExpiring: () -> Unit,
    /** 全部账号是否启用（启用提供商开关的读数）。 */
    allEnabled: Boolean,
    onToggleAll: (Boolean) -> Unit,
    /** 凭证自动维护任务（全局；节奏对所有家生效）。null = 读取中。 */
    credTask: JSONObject?,
    /** 该家是否被排除在自动续期之外（null = 还没读到）。 */
    autoRefreshOn: Boolean?,
    onToggleAutoRefresh: (Boolean) -> Unit,
    onEditInterval: () -> Unit,
    onRefreshProvider: () -> Unit,
) {
    val C = agateColors()
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp)) {
        Spacer(Modifier.height(10.dp))

        // ── 自定义提供商设置（只有 custom- 家有这一段）──────────────
        // 对齐上游 3.0.8 的口径：自定义家的「改名称 / 改协议 / 改 Base URL」
        // 落在账号自己的设置里就地编辑，而不是另开一个管理页 —— 改 Base URL 的
        // 动机通常正是「这个账号连不上了」，就地改比退出去找入口近一步。
        if (custom != null) {
            CustomProviderCard(custom = custom, onSaved = onCustomSaved)
            Spacer(Modifier.height(12.dp))
        }

        // ── 启用与选号 ──────────────────────────────────────────
        // 这两段原本在「概览」tab 里。删掉概览后它们必须有个去处，而「启用 /
        // 自动化」本来就是设置语义 —— 比跟一堆只读统计混在一张「概览」卡里合理。
        Card {
            Text("启用与选号", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(4.dp))
            Row(
                Modifier.fillMaxWidth().padding(vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text("启用提供商", color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                    Text(
                        if (allEnabled) "参与选号 · 关闭后本家账号全部停用" else "当前有账号停用 · 打开即全部启用",
                        color = C.textSub, fontSize = 12.sp,
                    )
                }
                Switch(checked = allEnabled, enabled = accounts.isNotEmpty(), onCheckedChange = onToggleAll)
            }
        }
        Spacer(Modifier.height(12.dp))

        // ── 自动化 ──────────────────────────────────────────────
        // 三行各只做一件事：开关 / 间隔 / 手动触发。
        // 旧布局把「开关」和「改间隔」并排放在同一行右侧 —— 两者都有色块边框，
        // 视觉重量相当，读起来像两个平级动作，实际却是「总开关」与「调一个参数」。
        // 现在间隔做成可点的值胶囊（与设置页 ValueRow 同一种交互）：胶囊同时回答
        // 「现在是多少」和「这里能点」，按钮只能回答后者。
        Card {
            Text("自动化", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(4.dp))

            // 行 1：自动续期总开关（按家）
            Row(Modifier.fillMaxWidth().padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("自动续期", color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                    Text(
                        credTask?.let { t ->
                            val on = t.optBoolean("enabled", true)
                            val n = t.optInt("interval", 10)
                            when {
                                !on -> "全局已关闭 · 转发时仍按需续期"
                                autoRefreshOn == false -> "该家已关闭 · 全局每 $n 分钟检查（不含本家）"
                                else -> "定期检查本家账号的临期凭证并换新"
                            }
                        } ?: "节奏读取中…",
                        color = C.textSub, fontSize = 12.sp,
                    )
                }
                if (autoRefreshOn != null) {
                    Switch(checked = autoRefreshOn, onCheckedChange = onToggleAutoRefresh)
                }
            }

            // 行 2：检查间隔（可点的值胶囊）
            Row(Modifier.fillMaxWidth().padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("检查间隔", color = C.textMain, fontSize = 13.sp)
                    Text("该节奏对所有提供商生效 · 点右侧修改", color = C.textSub, fontSize = 12.sp)
                }
                val minutes = credTask?.optInt("interval", 10)
                Text(
                    if (minutes == null) "读取中…" else "每 ${minutes} 分钟 ›",
                    color = if (minutes == null) C.textDim else C.accent,
                    fontSize = 12.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .background(C.accent.copy(alpha = 0.10f), RoundedCornerShape(999.dp))
                        .then(if (minutes == null) Modifier else Modifier.clickable(enabled = !busy, onClick = onEditInterval))
                        .padding(horizontal = 12.dp, vertical = 5.dp),
                )
            }

            // 行 3：按需续期说明 + 手动触发该家
            Row(Modifier.fillMaxWidth().padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "转发时的按需续期始终生效，不受上面的开关影响。",
                    color = C.textDim, fontSize = 11.sp, modifier = Modifier.weight(1f),
                )
                SmallAction("立即续期该家", enabled = !busy && accounts.isNotEmpty(), onClick = onRefreshProvider, primary = true)
            }
        }
        Spacer(Modifier.height(12.dp))

        // ── 家专属设置：出站身份（ZCode 版本号 / Cline 伪装头）────────
        // 两者都进上游请求头，上游按「像不像官方客户端」决定放不放行。
        // 放在「设置」tab 是因为它们是**这一家的接入形态**，不是某个账号的属性。
        //
        // ⚠️ CatPaw 的余额查询凭证曾经也在这里，已移走：它是**账号级**字段
        // （内核 `update_catpaw_balance_token(id, …)` 按 id + provider 定位单条
        // 记录），而这里原先只写 `accounts.first()` —— 多账号时其余账号的余额
        // 永远查不到，界面上还看不出缺的是哪个。现在挂在「编辑账号」弹窗里。
        when (providerId) {
            "zcode", "zcode-intl" -> {
                ZcodeVersionCard()
                Spacer(Modifier.height(12.dp))
            }
            "cline-free", "cline-pass" -> {
                ClineHeadersCard()
                Spacer(Modifier.height(12.dp))
            }
        }

        Card {
            Text("操作", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(6.dp))
            Row(Modifier.fillMaxWidth().padding(vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Text("测试连通性", color = C.textMain, fontSize = 14.sp, modifier = Modifier.weight(1f))
                SmallAction(if (busy) "测试中…" else "测试", false, !busy && accounts.isNotEmpty(), onClick = onTest)
            }
            Row(Modifier.fillMaxWidth().padding(vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Text("刷新临期凭证", color = C.textMain, fontSize = 14.sp, modifier = Modifier.weight(1f))
                SmallAction("刷新", false, !busy, onClick = onRefreshExpiring)
            }
            Row(Modifier.fillMaxWidth().padding(vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("删除提供商", color = C.textMain, fontSize = 14.sp)
                    Text("删除该家全部账号，不可恢复", color = C.textSub, fontSize = 12.sp)
                }
                SmallAction("删除", false, !busy, onClick = onDeleteProvider, danger = true)
            }
        }
        Spacer(Modifier.height(12.dp))
        Text(
            "本家专有设置（伪装头 / 设备指纹 / 免费池等）按家差异较大，后续按提供商逐个接入。" +
                "倍率 / 策略覆盖 / 限流明细依赖内核暂未暴露的端点，本页不展示占位数据。",
            color = C.textDim, fontSize = 11.sp,
        )
        Spacer(Modifier.height(6.dp))
        // 提供商 ID 原来在「概览」占一整行 —— 它只有报障时要，降级成脚注足够，
        // 但选中可复制（贴到工单里比手抄准）。
        val ctx = LocalContext.current
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("提供商 ID ", color = C.textDim, fontSize = 11.sp)
            Text(
                providerId,
                color = C.textDim, fontSize = 11.sp,
                modifier = Modifier
                    .clickable {
                        val cm = ctx.getSystemService(android.content.Context.CLIPBOARD_SERVICE)
                                as android.content.ClipboardManager
                        cm.setPrimaryClip(android.content.ClipData.newPlainText("providerId", providerId))
                        android.widget.Toast.makeText(ctx, "已复制 $providerId", android.widget.Toast.LENGTH_SHORT).show()
                    }
                    .padding(vertical = 2.dp),
                style = androidx.compose.ui.text.TextStyle(fontFamily = androidx.compose.ui.text.font.FontFamily.Monospace),
            )
        }
        Spacer(Modifier.height(24.dp))
    }
}

/* ───────────── 小件 ───────────── */
@Composable
private fun Card(content: @Composable androidx.compose.foundation.layout.ColumnScope.() -> Unit) {
    val C = agateColors()
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(18.dp))
            .border(1.dp, C.border, RoundedCornerShape(18.dp))
            .padding(14.dp),
        content = content,
    )
}

@Composable
private fun SmallAction(
    text: String,
    primary: Boolean = false,
    enabled: Boolean = true,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    danger: Boolean = false,
) {
    val C = agateColors()
    val fg = when {
        !enabled -> C.textDim
        danger -> C.red
        primary -> C.onAccent
        else -> C.textMain
    }
    val bg = when {
        primary && enabled -> C.accent
        danger && enabled -> C.redBg
        else -> C.btnBg
    }
    Box(
        modifier
            .background(bg, RoundedCornerShape(9.dp))
            .then(if (enabled) Modifier.clickable(onClick = onClick) else Modifier)
            .padding(horizontal = 12.dp, vertical = 8.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(text, color = fg, fontSize = 12.sp, fontWeight = FontWeight.SemiBold, style = AgateType.BodyS)
    }
}

/**
 * 把 /api/accounts/usage 的 usage 对象格式化成一行文案。
 * 各家字段不同，按候选键取值；全无则返回 null（调用方显示「已查询」）。
 */
private fun formatUsage(usage: JSONObject?): String? {
    if (usage == null) return null
    fun d(vararg keys: String): Double? =
        keys.firstNotNullOfOrNull { k ->
            // 只认数字与数字串：布尔不能当数字（`optDouble` 在旧实现在这里会 toString）
            when (val raw = usage.opt(k)) {
                is Number -> raw.toDouble().takeIf { it.isFinite() }
                is String -> raw.trim().toDoubleOrNull()?.takeIf { it.isFinite() }
                else -> null
            }
        }

    // WorkBuddy 的既有契约 `{totalLeft, planLeft, bonusLeft}`（无 total/used）
    if (usage.has("totalLeft")) {
        if (usage.optBoolean("unlimited", false)) return "不限量"
        val parts = ArrayList<String>(3)
        d("totalLeft")?.let { parts += "共${fmtNum(it)}" }
        d("planLeft")?.let { parts += "套餐${fmtNum(it)}" }
        d("bonusLeft")?.let { parts += "奖励${fmtNum(it)}" }
        return parts.takeIf { it.isNotEmpty() }?.joinToString(" · ")
    }

    val left = d("left", "remain", "remaining", "balance", "available", "total_granted")
    val total = d("total", "quota", "granted")
    val used = d("used", "usage", "consumed")
    return when {
        total != null && used != null && total > 0 ->
            "剩余 ${fmtNum(total - used)} / ${fmtNum(total)}（${((total - used) / total * 100).toInt()}%）"
        left != null && total != null && total > 0 ->
            "剩余 ${fmtNum(left)} / ${fmtNum(total)}（${(left / total * 100).toInt()}%）"
        left != null -> "剩余 ${fmtNum(left)}"
        used != null -> "已用 ${fmtNum(used)}"
        else -> null
    }
}

private fun fmtNum(v: Double): String =
    if (v >= 1000) String.format("%,.0f", v) else String.format("%.2f", v).trimEnd('0').trimEnd('.')