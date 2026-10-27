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
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
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
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.ScheduledTaskOps
import dev.agate.android.data.PresetRegistry
import dev.agate.android.data.dto.AccountDto
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateProviderAvatar
import dev.agate.android.ui.components.AgateAvatar
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject

/**
 * 提供商详情（对齐 v44 sub-providerdetail）：
 * - 概览：账号统计 / 余额查询 / 登录通道 / 启用开关 / 队列位置
 * - 账号：逐账号余额与操作（上移 · 编辑备注 · 启停 · 删除）+ 添加账号 + 批量操作
 * - 设置：测试连通性 / 删除提供商
 *
 * 真实数据源：/api/session.accounts（列表）、/api/accounts/usage（余额/积分）、
 * /api/accounts/batch（启停/删除）、PATCH /api/accounts/{id}（改备注名）、
 * /api/accounts/{id}/move（调序）。倍率/续期/限流明细等依赖内核暂未暴露的端点，
 * 本页不臆造，统一在底部说明。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ProviderDetailPage(providerId: String, onBack: () -> Unit, onAddAccount: () -> Unit = {}) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val preset = PresetRegistry.ALL.find { it.id == providerId }

    val snap by GatewayRepository.snapshot.collectAsState()
    val accounts = snap.accountList.filter { it.provider == providerId }

    var tab by remember { mutableStateOf(0) }
    var busy by remember { mutableStateOf(false) }
    var usageLabel = remember { mutableStateMapOf<String, String>() }   // id → 展示文案
    var editing by remember { mutableStateOf<AccountDto?>(null) }
    var editName by remember { mutableStateOf("") }
    var editConc by remember { mutableStateOf("") }
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

    Column(Modifier.fillMaxSize().background(C.bg).statusBarsPadding().navigationBarsPadding()) {
        // ── 顶栏 ──
        Row(
            Modifier.fillMaxWidth().padding(start = 4.dp, end = 16.dp, top = 4.dp, bottom = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                Modifier.size(36.dp).clickable(onClick = onBack),
                contentAlignment = Alignment.Center,
            ) { Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "返回", tint = C.textMain) }
            AgateProviderAvatar(providerId = providerId, label = preset?.label ?: providerId, size = 32.dp, corner = 10.dp)
            Spacer(Modifier.width(10.dp))
            Text(
                preset?.label ?: providerId,
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
        }

        AgateSegmented(
            options = listOf("概览", "账号 · ${accounts.size}", "设置"),
            selected = tab,
            onSelect = { tab = it },
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
        )

        when (tab) {
            0 -> OverviewTab(
                accounts = accounts,
                usageLabel = usageLabel,
                busy = busy,
                presetLabel = preset?.desc ?: "自定义 / 中转上游",
                channel = preset?.let { channelLabel(it) } ?: "自定义",
                providerId = providerId,
                onQueryUsage = { loadUsage(force = true) },
                onToggleAll = { enable -> batchAction(if (enable) "enable" else "disable", done = if (enable) "已全部启用" else "已全部禁用") },
                onJumpToAccounts = { tab = 1 },
                credTask = credTask,
                checkinSupported = preset?.supportsCheckin == true,
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
            )
            1 -> AccountsTab(
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
            )
            else -> SettingsTab(
                accounts = accounts,
                providerId = providerId,
                busy = busy,
                onPatchAccount = { acc, patch ->
                    if (!busy) {
                        busy = true
                        scope.launch {
                            val r = AccountOps.update(acc.id, patch)
                            busy = false
                            toast(if (r.ok) "已保存（${acc.displayName}）" else (r.error ?: "保存失败"))
                            if (r.ok) GatewayRepository.refreshOnce()
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

    // ── 编辑备注名 ──
    editing?.let { acc ->
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
                }
            },
            confirmButton = {
                TextButton(onClick = {
                    val target = acc
                    editing = null
                    scope.launch {
                        val patch = JSONObject().put("name", editName.trim())
                        editConc.trim().toIntOrNull()?.let { patch.put("maxConcurrent", it) }
                        val r = AccountOps.update(target.id, patch)
                        toast(if (r.ok) "已保存" else (r.error ?: "保存失败"))
                        if (r.ok) GatewayRepository.refreshOnce()
                    }
                }) { Text("保存") }
            },
            dismissButton = { TextButton(onClick = { editing = null }) { Text("取消") } },
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
            title = { Text("删除提供商") },
            text = {
                Text(
                    "将移除「${preset?.label ?: providerId}」这一家：连同其 ${accounts.size} 个账号、凭证与设置一并删除，不可恢复。",
                    fontSize = 13.sp,
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    confirmDeleteProvider = false
                    if (accounts.isEmpty()) { onBack(); return@TextButton }
                    busy = true
                    scope.launch {
                        val ok = AccountOps.batch(accounts.map { it.id }, "remove")
                        busy = false
                        toast(if (ok) "已删除该提供商" else "删除失败")
                        if (ok) { GatewayRepository.refreshOnce(); onBack() }
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

/* ───────────── 概览 ───────────── */
@Composable
private fun OverviewTab(
    accounts: List<AccountDto>,
    usageLabel: Map<String, String>,
    busy: Boolean,
    presetLabel: String,
    channel: String,
    providerId: String,
    onQueryUsage: () -> Unit,
    onToggleAll: (Boolean) -> Unit,
    onJumpToAccounts: () -> Unit,
    credTask: JSONObject?,
    checkinSupported: Boolean,
    autoRefreshOn: Boolean?,
    onToggleAutoRefresh: (Boolean) -> Unit,
    onEditInterval: () -> Unit,
    onRefreshProvider: () -> Unit,
) {
    val C = agateColors()
    val allEnabled = accounts.isNotEmpty() && accounts.all { it.enabled }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp)) {
        Spacer(Modifier.height(10.dp))

        Card {
            Text("概览", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(6.dp))
            CfgRow(
                "账号",
                "${accounts.size} 个 · ${accounts.count { it.enabled }} 启用",
                if (accounts.isEmpty()) null else accounts.size.toString(),
            )
            CfgRow(
                "余额 / 积分",
                if (accounts.isEmpty()) "无账号" else if (usageLabel.isEmpty()) "点右侧查询" else "已查询 ${usageLabel.size}/${accounts.size}",
                if (accounts.isEmpty()) null else if (busy) "查询中…" else "查询",
                onValueClick = onQueryUsage,
            )
            CfgRow("登录通道", channel, null)
            CfgRow("接入说明", presetLabel, null)
            CfgRow("提供商 ID", providerId, null)
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
        Card {
            Text("自动化", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(8.dp))

            // 自动续期：能力/节奏来自全局任务；「立即续期」按该家账号逐个触发
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
                                else -> "已开启 · 每 $n 分钟检查临期凭证"
                            }
                        } ?: "节奏读取中…",
                        color = C.textSub, fontSize = 12.sp,
                    )
                }
                if (autoRefreshOn != null) {
                    androidx.compose.material3.Switch(
                        checked = autoRefreshOn,
                        onCheckedChange = onToggleAutoRefresh,
                    )
                }
                SmallAction("改间隔", enabled = !busy && credTask != null, onClick = onEditInterval)
            }
            Row(Modifier.fillMaxWidth().padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "转发时的按需续期始终生效，不受上面的开关影响。",
                    color = C.textDim, fontSize = 11.sp, modifier = Modifier.weight(1f),
                )
                SmallAction("立即续期该家", enabled = !busy && accounts.isNotEmpty(), onClick = onRefreshProvider, primary = true)
            }

            // 自动签到：能力来自内核签到矩阵（PresetRegistry.supportsCheckin）
            Row(Modifier.fillMaxWidth().padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("自动签到", color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                    Text(
                        if (checkinSupported) "该家支持 · 在任务中心统一配置签到时间与范围"
                        else "该家不支持签到（上游无签到能力）",
                        color = C.textSub, fontSize = 12.sp,
                    )
                }
                AgateBadge(
                    if (checkinSupported) "支持" else "不支持",
                    if (checkinSupported) BadgeTone.GREEN else BadgeTone.GRAY,
                )
            }
        }

        Spacer(Modifier.height(12.dp))
        Card {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("队列位置", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                Text(
                    "查看账号 ❯",
                    color = C.accent, fontSize = 12.sp, fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.clickable(onClick = onJumpToAccounts),
                )
            }
            Spacer(Modifier.height(6.dp))
            if (accounts.isEmpty()) {
                Text("该家还没有账号", color = C.textSub, fontSize = 12.sp)
            } else {
                accounts.take(6).forEachIndexed { i, acc ->
                    Row(Modifier.fillMaxWidth().padding(vertical = 5.dp), verticalAlignment = Alignment.CenterVertically) {
                        Text("#${i + 1}", color = C.textDim, fontSize = 12.sp, modifier = Modifier.width(34.dp))
                        Text(acc.displayName, color = C.textMain, fontSize = 13.sp, modifier = Modifier.weight(1f))
                        AgateBadge(if (acc.enabled) "出战" else "跳过", if (acc.enabled) BadgeTone.GREEN else BadgeTone.GRAY)
                    }
                }
            }
        }

        Spacer(Modifier.height(10.dp))
        Text(
            "倍率 / 凭证续期 / 策略覆盖 / 限流明细依赖内核暂未暴露的端点，本页不展示占位数据。",
            color = C.textDim, fontSize = 11.sp,
        )
        Spacer(Modifier.height(24.dp))
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
) {
    val C = agateColors()
    LazyColumn(
        Modifier.fillMaxSize().padding(horizontal = 16.dp),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = 10.dp),
    ) {
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
                    )
                }
                Spacer(Modifier.height(10.dp))
                SmallAction("+ 添加账号", primary = true, enabled = true, modifier = Modifier.fillMaxWidth(), onClick = onAdd)
            }
        }
        item {
            Spacer(Modifier.height(12.dp))
            Card {
                Text("批量操作", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.height(8.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
                    SmallAction("全部启用", enabled = !busy && accounts.isNotEmpty(), onClick = onEnableAll, modifier = Modifier.weight(1f))
                    SmallAction("全部禁用", enabled = !busy && accounts.isNotEmpty(), onClick = onDisableAll, modifier = Modifier.weight(1f))
                }
                Spacer(Modifier.height(8.dp))
                SmallAction("删除全部账号", enabled = !busy && accounts.isNotEmpty(), onClick = onDeleteAll, modifier = Modifier.fillMaxWidth(), danger = true)
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
        // 凭证有效期（与列表页同口径：双字段 + 秒毫秒归一；没有该字段的家不渲染）
        credentialExpiryMs(acc)?.let { at ->
            ExpiryLine(at)
        }
        // 额度设限：只对支持余额查询的家显示（该家没接余额时填了阈值也看不到效果）
        if (usageCapable) {
            LimiterEntryRow(
                summary = limiterRulesOf(acc.raw).firstOrNull()?.summary() ?: "未设限",
                onOpen = { onOpenLimiter(acc) },
            )
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

/* ───────────── 设置 ───────────── */
@Composable
private fun SettingsTab(
    accounts: List<AccountDto>,
    providerId: String,
    busy: Boolean,
    onPatchAccount: (AccountDto, JSONObject) -> Unit,
    onTest: () -> Unit,
    onDeleteProvider: () -> Unit,
    onRefreshExpiring: () -> Unit,
) {
    val C = agateColors()
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp)) {
        Spacer(Modifier.height(10.dp))
        // ── 家专属设置（只在这几家出现；字段来自内核的按家白名单）──
        val firstAccount = accounts.firstOrNull()
        when (providerId) {
            "catpaw" -> {
                var token by remember { mutableStateOf("") }
                Card {
                    Text("余额查询凭证", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                    Spacer(Modifier.height(4.dp))
                    Text(
                        "CatPaw 的余额要带 passport cookie（token2）才能查。留空 = 不修改。",
                        color = C.textSub, fontSize = 12.sp,
                    )
                    Spacer(Modifier.height(8.dp))
                    OutlinedTextField(
                        value = token,
                        onValueChange = { token = it.trim() },
                        singleLine = true,
                        placeholder = { Text("粘贴 token2 / balanceToken", fontSize = 12.sp) },
                        modifier = Modifier.fillMaxWidth(),
                    )
                    Spacer(Modifier.height(8.dp))
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            if (firstAccount == null) "该家还没有账号" else "应用到：${firstAccount.displayName}",
                            color = C.textDim, fontSize = 11.sp, modifier = Modifier.weight(1f),
                        )
                        SmallAction(
                            "保存",
                            false,
                            !busy && firstAccount != null && token.isNotBlank(),
                            onClick = {
                                val acc = firstAccount ?: return@SmallAction
                                onPatchAccount(acc, JSONObject().put("balanceToken", token))
                                token = ""
                            },
                        )
                    }
                }
                Spacer(Modifier.height(12.dp))
            }
            "zcode", "zcode-intl" -> {
                val current = firstAccount?.raw?.optString("zcodePlan").orEmpty()
                    .ifBlank { "coding-plan" }
                Card {
                    Text("套餐通道", color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                    Spacer(Modifier.height(4.dp))
                    Text(
                        "编码套餐与活动套餐走不同的上游通道；切换后新请求立即生效。",
                        color = C.textSub, fontSize = 12.sp,
                    )
                    Spacer(Modifier.height(10.dp))
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        listOf("coding-plan" to "编码套餐", "start-plan" to "活动套餐").forEach { (value, label) ->
                            val active = current == value
                            Text(
                                label,
                                color = if (active) C.onAccent else C.textSub,
                                fontSize = 12.sp,
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
                                    .clickable {
                                        val acc = firstAccount ?: return@clickable
                                        if (!active) onPatchAccount(acc, JSONObject().put("zcodePlan", value))
                                    }
                                    .padding(horizontal = 14.dp, vertical = 8.dp),
                            )
                        }
                    }
                    if (firstAccount == null) {
                        Spacer(Modifier.height(6.dp))
                        Text("该家还没有账号", color = C.textDim, fontSize = 11.sp)
                    }
                }
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
            "本家专有设置（伪装头 / 设备指纹 / 免费池等）按家差异较大，后续按提供商逐个接入。",
            color = C.textDim, fontSize = 11.sp,
        )
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
private fun CfgRow(
    name: String,
    desc: String,
    value: String?,
    onValueClick: (() -> Unit)? = null,
) {
    val C = agateColors()
    Row(Modifier.fillMaxWidth().padding(vertical = 9.dp), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(name, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            if (desc.isNotBlank()) Text(desc, color = C.textSub, fontSize = 12.sp)
        }
        if (value != null) {
            Text(
                value,
                color = C.accent, fontSize = 12.sp, fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(C.accent.copy(alpha = 0.10f), RoundedCornerShape(999.dp))
                    .then(if (onValueClick != null) Modifier.clickable(onClick = onValueClick) else Modifier)
                    .padding(horizontal = 12.dp, vertical = 5.dp),
            )
        }
    }
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