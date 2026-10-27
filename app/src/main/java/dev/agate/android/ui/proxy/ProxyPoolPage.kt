package dev.agate.android.ui.proxy

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
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.ProxyOps
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateFormInput
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.AgateStatRow
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.StatCell
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import dev.agate.core.GatewayPrefs
import kotlinx.coroutines.launch

/**
 * 出网代理池 —— 对齐 v44 / OmniProxy 的 proxies 页。
 *
 * 自上而下：概览 → 代理条目列表 → 新建手动出口；页头另有「同步 Clash」与「设置」。
 *
 * ── 三种来源的能力不同，界面必须说清楚 ───────────────────────
 * 内核 `proxy_pool::normalize_item` 只接受**手动**条目：`clash` 来自
 * 「同步 Clash Verge」，`subscription` 来自订阅商节点。后两者是镜像记录，
 * 下次同步会按来源重写，所以：
 *   · 不给「编辑 / 删除」按钮（点了也只会 400，或者改了立刻被同步覆盖）；
 *   · 只给「本地禁用」—— 那是一个独立的本地位，同步不会抹掉它。
 * 这不是权限裁剪，是**能力事实**：改 Clash 出口要去 Clash Verge 改。
 *
 * ── 为什么「同步 Clash」要有反馈而进页面时没有 ────────────────
 * 列表接口自己会顺手同步一次（内核 `synced_payload`，幂等），所以进页面时
 * 不该弹任何提示——用户没点任何东西。而手动点按钮时，用户需要一个结论：
 * 「更新了 N 项」或「Clash 不可用」。
 *
 * ── 端口范围保存后为什么要提示重启 ───────────────────────────
 * 内核明确不回收已注册端口（见 `settings_save` 的注释）：新范围只约束**新分配**。
 * 界面若不讲这句，用户改完看到旧节点端口没变，会以为没保存成功。
 */
@Composable
fun ProxyPoolPage(
    onOpenDrawer: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    var pool by remember { mutableStateOf<ProxyOps.Pool?>(null) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    /** 正在测试的条目 id（按钮文案跟着变，避免重复点击）。 */
    var testingId by remember { mutableStateOf<String?>(null) }
    /** 非 null = 打开表单弹层（新建传 null 条目）。 */
    var showForm by remember { mutableStateOf(false) }
    var formTarget by remember { mutableStateOf<ProxyOps.Entry?>(null) }
    var showSettings by remember { mutableStateOf(false) }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    fun apply(r: ProxyOps.OpResult<ProxyOps.Pool>) {
        if (r.ok && r.data != null) {
            pool = r.data
            error = null
        } else {
            toast(r.error ?: "操作失败")
        }
    }

    fun reload() {
        if (loading) return
        loading = true
        scope.launch {
            val r = ProxyOps.pool()
            loading = false
            if (r.ok && r.data != null) {
                pool = r.data
                error = null
            } else {
                error = r.error ?: "读取失败"
            }
        }
    }

    DisposableEffect(Unit) {
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
        onDispose { }
    }
    LaunchedEffect(Unit) { reload() }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "出网代理池", onOpenDrawer = onOpenDrawer) {
            Text(
                "同步 Clash",
                color = C.accent,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clickable(enabled = !busy) {
                        busy = true
                        scope.launch {
                            val r = ProxyOps.syncClash()
                            busy = false
                            if (r.ok && r.data != null) {
                                pool = r.data.pool
                                val err = r.data.error
                                if (err != null) {
                                    toast("同步失败：$err")
                                } else if (r.data.changes == 0) {
                                    toast("已是最新，没有变更")
                                } else {
                                    toast("已更新 ${r.data.changes} 项")
                                }
                            } else {
                                toast(r.error ?: "同步失败")
                            }
                        }
                    }
                    .padding(horizontal = 8.dp, vertical = 5.dp),
            )
            Text(
                "设置",
                color = C.textSub,
                fontSize = 13.sp,
                modifier = Modifier
                    .clickable { showSettings = true }
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

            val p = pool
            if (error != null) {
                HintBox(error!!, error = true)
            } else if (p == null) {
                HintBox(if (loading) "读取中…" else "还没有代理条目")
            } else {
                AgateStatRow(
                    cells = listOf(
                        StatCell("条目", p.items.size.toString()),
                        StatCell("启用", p.enabledCount.toString(), if (p.enabledCount > 0) C.green else null),
                        StatCell("异常", p.brokenCount.toString(), if (p.brokenCount > 0) C.red else null),
                        StatCell("被引用", p.referencedCount.toString(), if (p.referencedCount > 0) C.accent else null),
                    ),
                )

                Spacer(Modifier.height(10.dp))

                Column(
                    Modifier
                        .fillMaxWidth()
                        .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
                        .border(1.dp, C.border, RoundedCornerShape(16.dp))
                        .padding(14.dp),
                ) {
                    Text(
                        "代理条目",
                        fontSize = 13.5.sp,
                        fontWeight = FontWeight.SemiBold,
                        color = C.textMain,
                    )
                    if (p.items.isEmpty()) {
                        Spacer(Modifier.height(8.dp))
                        Text(
                            "池里还没有条目。手动建一条，或在 Clash Verge 里加好出口后点「同步 Clash」",
                            fontSize = 11.5.sp,
                            color = C.textDim,
                            lineHeight = 16.sp,
                        )
                    }
                    p.items.forEachIndexed { index, entry ->
                        if (index > 0) {
                            Spacer(Modifier.height(9.dp))
                            Box(Modifier.fillMaxWidth().height(1.dp).background(C.border.copy(alpha = 0.5f)))
                        }
                        Spacer(Modifier.height(9.dp))
                        EntryRow(
                            entry = entry,
                            testing = testingId == entry.id,
                            busy = busy,
                            onTest = {
                                testingId = entry.id
                                scope.launch {
                                    val r = ProxyOps.test(entry.id)
                                    testingId = null
                                    if (r.ok && r.data != null) {
                                        r.data.pool?.let { pool = it }
                                        val t = r.data
                                        toast(
                                            if (t.success) {
                                                "出口 ${t.ip} · ${t.durationMs}ms"
                                            } else {
                                                "测试失败：${t.error ?: "未知原因"}"
                                            },
                                        )
                                        // 「结论出来了」与「没记住结论」是两件事
                                        t.saveError?.let { toast("测试结果未能保存：$it") }
                                    } else {
                                        toast(r.error ?: "测试失败")
                                    }
                                }
                            },
                            onToggle = {
                                busy = true
                                scope.launch {
                                    apply(ProxyOps.setEnabled(entry.id, !entry.enabled))
                                    busy = false
                                }
                            },
                            onEdit = {
                                formTarget = entry
                                showForm = true
                            },
                            onDelete = {
                                busy = true
                                scope.launch {
                                    val r = ProxyOps.remove(entry.id)
                                    busy = false
                                    if (r.ok) {
                                        apply(r)
                                        toast("已删除")
                                    } else {
                                        toast(r.error ?: "删除失败")
                                    }
                                }
                            },
                        )
                    }
                }

                Spacer(Modifier.height(10.dp))

                Column(
                    Modifier
                        .fillMaxWidth()
                        .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
                        .border(1.dp, C.border, RoundedCornerShape(16.dp))
                        .clickable {
                            formTarget = null
                            showForm = true
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
                            Text("新建手动出口", color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                            Text(
                                "填代理地址即可；Clash 与订阅节点走各自的同步，不能手工新建",
                                color = C.textSub,
                                fontSize = 11.sp,
                            )
                        }
                        Text("❯", color = C.textDim, fontSize = 13.sp)
                    }
                }
            }

            Spacer(Modifier.height(24.dp))
        }
    }

    if (showForm) {
        ProxyFormSheet(
            entry = formTarget,
            onDismiss = { showForm = false },
            onSubmit = { name, protocol, host, port, username, password ->
                busy = true
                scope.launch {
                    val target = formTarget
                    val r = if (target == null) {
                        ProxyOps.create(name, protocol, host, port, username, password)
                    } else {
                        ProxyOps.update(target.id, name, protocol, host, port, username, password)
                    }
                    busy = false
                    if (r.ok) {
                        apply(r)
                        showForm = false
                        toast(if (target == null) "已新建" else "已保存")
                    } else {
                        toast(r.error ?: "保存失败")
                    }
                }
            },
        )
    }

    if (showSettings) {
        ProxySettingsSheet(
            onDismiss = { showSettings = false },
            onSaved = { toast("已保存。端口范围需重启内核后完整生效") },
        )
    }
}

// ══════════════════════════════════════════════════════════════
//  条目行
// ══════════════════════════════════════════════════════════════

@Composable
private fun EntryRow(
    entry: ProxyOps.Entry,
    testing: Boolean,
    busy: Boolean,
    onTest: () -> Unit,
    onToggle: () -> Unit,
    onEdit: () -> Unit,
    onDelete: () -> Unit,
) {
    val C = agateColors()
    // 状态点分层：孤儿 / 解析失败 > 未启用 > 测试失败 > 正常
    val dot = when {
        entry.orphan || entry.resolveError != null -> C.red
        !entry.enabled -> C.textDim
        entry.lastTest?.success == false -> C.amber
        entry.lastTest?.success == true -> C.green
        else -> C.textDim
    }
    Column(Modifier.fillMaxWidth()) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(Modifier.size(6.dp).background(dot, CircleShape))
            Spacer(Modifier.width(7.dp))
            Text(
                entry.name,
                fontSize = 13.sp,
                fontWeight = FontWeight.Medium,
                color = C.textMain,
                maxLines = 1,
                modifier = Modifier.weight(1f, fill = false),
            )
            Spacer(Modifier.width(6.dp))
            AgateBadge(
                entry.source.label,
                when (entry.source) {
                    ProxyOps.Source.MANUAL -> BadgeTone.BLUE
                    else -> BadgeTone.GRAY
                },
            )
            if (entry.orphan) {
                Spacer(Modifier.width(4.dp))
                AgateBadge("孤儿", BadgeTone.RED)
            }
            Spacer(Modifier.weight(1f))
            Text(
                if (entry.usedBy.isEmpty()) "未引用" else "${entry.usedBy.size} 账号引用",
                fontSize = 10.5.sp,
                color = if (entry.usedBy.isEmpty()) C.textDim else C.accent,
            )
        }

        Spacer(Modifier.height(3.dp))
        Text(
            entry.endpoint().ifBlank { "—" },
            fontSize = 11.5.sp,
            color = C.textSub,
            style = AgateType.Num,
        )

        // 第二行状态：优先讲问题，没问题才讲最近一次测试
        val status = when {
            entry.orphan -> "所属订阅已不存在，配置无法解析"
            entry.resolveError != null -> entry.resolveError
            !entry.enabled && entry.localDisabled -> "本页已禁用，点开关可恢复"
            !entry.enabled -> "来源侧已关闭（需去来源处开启）"
            entry.lastTest == null -> "尚未测试"
            entry.lastTest.success -> "测试通过 · 出口 ${entry.lastTest.ip} · ${entry.lastTest.durationMs}ms · ${fmtAgo(entry.lastTest.at)}"
            else -> "测试失败 · ${entry.lastTest.error ?: "未知原因"} · ${fmtAgo(entry.lastTest.at)}"
        }
        val statusColor = when {
            entry.orphan || entry.resolveError != null -> C.red
            !entry.enabled -> C.textDim
            entry.lastTest?.success == true -> C.green
            entry.lastTest?.success == false -> C.amber
            else -> C.textDim
        }
        Spacer(Modifier.height(2.dp))
        Text(status, fontSize = 11.sp, color = statusColor, lineHeight = 15.sp)

        Spacer(Modifier.height(7.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
            ActionChip(
                text = if (testing) "测试中…" else "测试",
                enabled = !testing && !busy,
                onClick = onTest,
            )
            // 开关的可操作性：镜像条目点的是**本地禁用位**（内核支持），
            // 但「来源侧已经关着」时点了不会有任何可见变化 —— 那种情况不给按钮，
            // 只在状态行写明「需去来源处开启」。
            //   手动            → 始终可切
            //   镜像 + 生效中   → 可停用（写本地位）
            //   镜像 + 本地禁用 → 可恢复
            //   镜像 + 来源侧关 → 不给按钮
            if (entry.editable || entry.enabled || entry.localDisabled) {
                ActionChip(
                    text = if (entry.enabled) "停用" else "启用",
                    enabled = !busy,
                    onClick = onToggle,
                )
            }
            if (entry.editable) {
                ActionChip(text = "编辑", enabled = !busy, onClick = onEdit)
                ActionChip(text = "删除", enabled = !busy, danger = true, onClick = onDelete)
            }
        }
    }
}

@Composable
private fun ActionChip(
    text: String,
    enabled: Boolean,
    danger: Boolean = false,
    onClick: () -> Unit,
) {
    val C = agateColors()
    val color = when {
        !enabled -> C.textDim
        danger -> C.red
        else -> C.textSub
    }
    Text(
        text,
        fontSize = 11.sp,
        color = color,
        modifier = Modifier
            .border(
                1.dp,
                if (danger) C.red.copy(alpha = 0.35f) else C.border,
                RoundedCornerShape(7.dp),
            )
            .clickable(enabled = enabled, onClick = onClick)
            .padding(horizontal = 8.dp, vertical = 3.dp),
    )
}

@Composable
private fun HintBox(text: String, error: Boolean = false) {
    val C = agateColors()
    Box(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
            .border(1.dp, C.border, RoundedCornerShape(16.dp))
            .padding(20.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(text, fontSize = 13.sp, color = if (error) C.red else C.textDim)
    }
}

// ══════════════════════════════════════════════════════════════
//  表单弹层（新建 / 编辑共用）
// ══════════════════════════════════════════════════════════════

@Composable
private fun ProxyFormSheet(
    entry: ProxyOps.Entry?,
    onDismiss: () -> Unit,
    onSubmit: (name: String, protocol: String, host: String, port: Long, username: String, password: String) -> Unit,
) {
    val C = agateColors()
    var name by remember { mutableStateOf(entry?.name.orEmpty()) }
    var protocol by remember { mutableStateOf(entry?.protocol?.ifBlank { "http" } ?: "http") }
    var host by remember { mutableStateOf(entry?.host.orEmpty()) }
    var port by remember { mutableStateOf(entry?.port?.takeIf { it > 0 }?.toString() ?: "7890") }
    var username by remember { mutableStateOf(entry?.username.orEmpty()) }
    var password by remember { mutableStateOf(entry?.password.orEmpty()) }
    var formError by remember { mutableStateOf<String?>(null) }

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
                Text(
                    if (entry == null) "新建手动出口" else "编辑出口",
                    fontSize = 15.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = C.textMain,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    "✕",
                    fontSize = 14.sp,
                    color = C.textDim,
                    modifier = Modifier.clickable(onClick = onDismiss).padding(6.dp),
                )
            }

            Spacer(Modifier.height(10.dp))
            Column(Modifier.heightIn(max = 420.dp).verticalScroll(rememberScrollState())) {
                AgateFormInput(value = name, onValueChange = { name = it }, hint = "名称")
                Spacer(Modifier.height(8.dp))

                // 内核只认 http / socks5（见 `normalize_item` 的校验），没有 https
                Text("协议", fontSize = 11.sp, color = C.textDim)
                Spacer(Modifier.height(4.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    listOf("http", "socks5").forEach { item ->
                        val active = protocol == item
                        Text(
                            item,
                            fontSize = 12.sp,
                            color = if (active) C.accent else C.textSub,
                            modifier = Modifier
                                .background(
                                    if (active) C.accent.copy(alpha = 0.12f) else C.insetBg,
                                    RoundedCornerShape(8.dp),
                                )
                                .border(
                                    1.dp,
                                    if (active) C.accent.copy(alpha = 0.35f) else C.border,
                                    RoundedCornerShape(8.dp),
                                )
                                .clickable { protocol = item }
                                .padding(horizontal = 12.dp, vertical = 6.dp),
                        )
                    }
                }

                Spacer(Modifier.height(8.dp))
                AgateFormInput(value = host, onValueChange = { host = it }, hint = "主机地址（如 10.0.0.8）")
                Spacer(Modifier.height(8.dp))
                AgateFormInput(
                    value = port,
                    onValueChange = { port = it.filter(Char::isDigit).take(5) },
                    hint = "端口（1-65535）",
                    keyboardType = KeyboardType.Number,
                )
                Spacer(Modifier.height(8.dp))
                AgateFormInput(value = username, onValueChange = { username = it }, hint = "用户名（可选）")
                Spacer(Modifier.height(8.dp))
                AgateFormInput(value = password, onValueChange = { password = it }, hint = "密码（可选）")
                Spacer(Modifier.height(6.dp))
                Text(
                    "账号密码以明文存本机（与账号记录里的代理填写同一口径）；留空表示代理无需认证",
                    fontSize = 10.5.sp,
                    color = C.textDim,
                    lineHeight = 14.sp,
                )
            }

            formError?.let {
                Spacer(Modifier.height(8.dp))
                Text(it, fontSize = 11.5.sp, color = C.red)
            }

            Spacer(Modifier.height(12.dp))
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "取消",
                    fontSize = 13.sp,
                    color = C.textSub,
                    modifier = Modifier
                        .clickable(onClick = onDismiss)
                        .padding(horizontal = 12.dp, vertical = 7.dp),
                )
                Spacer(Modifier.weight(1f))
                Text(
                    "保存",
                    fontSize = 13.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = C.accent,
                    modifier = Modifier
                        .border(1.dp, C.accent.copy(alpha = 0.4f), RoundedCornerShape(8.dp))
                        .clickable {
                            // 本地先挡一遍：这些是内核同样会 400 的项，本地挡住能省一次往返，
                            // 但**不作为唯一防线** —— 内核仍是权威校验。
                            val p = port.toLongOrNull()
                            formError = when {
                                name.isBlank() -> "请填写名称"
                                host.isBlank() -> "请填写主机地址"
                                p == null || p !in 1..65535 -> "端口必须是 1-65535 的整数"
                                else -> null
                            }
                            if (formError == null) {
                                onSubmit(name.trim(), protocol, host.trim(), p!!, username.trim(), password)
                            }
                        }
                        .padding(horizontal = 14.dp, vertical = 8.dp),
                )
            }
        }
    }
}

// ══════════════════════════════════════════════════════════════
//  代理设置弹层
// ══════════════════════════════════════════════════════════════

@Composable
private fun ProxySettingsSheet(onDismiss: () -> Unit, onSaved: () -> Unit) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    var base by remember { mutableStateOf("") }
    var end by remember { mutableStateOf("") }
    var testUrl by remember { mutableStateOf("") }
    var defaults by remember { mutableStateOf<ProxyOps.Settings?>(null) }
    var formError by remember { mutableStateOf<String?>(null) }
    var loading by remember { mutableStateOf(true) }

    LaunchedEffect(Unit) {
        val r = ProxyOps.settings()
        loading = false
        if (r.ok && r.data != null) {
            defaults = r.data
            base = r.data.listenerPortBase.toString()
            end = r.data.listenerPortEnd.toString()
            testUrl = r.data.testUrl
        } else {
            formError = r.error ?: "读取设置失败"
        }
    }

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
                Text(
                    "代理设置",
                    fontSize = 15.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = C.textMain,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    "✕",
                    fontSize = 14.sp,
                    color = C.textDim,
                    modifier = Modifier.clickable(onClick = onDismiss).padding(6.dp),
                )
            }
            Spacer(Modifier.height(10.dp))

            if (loading) {
                Text("读取中…", fontSize = 12.sp, color = C.textDim)
            } else {
                Text("节点端口范围", fontSize = 11.sp, color = C.textDim)
                Spacer(Modifier.height(4.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(Modifier.weight(1f)) {
                        AgateFormInput(
                            value = base,
                            onValueChange = { base = it.filter(Char::isDigit).take(5) },
                            hint = "起始",
                            keyboardType = KeyboardType.Number,
                        )
                    }
                    Spacer(Modifier.width(8.dp))
                    Text("—", fontSize = 12.sp, color = C.textDim)
                    Spacer(Modifier.width(8.dp))
                    Box(Modifier.weight(1f)) {
                        AgateFormInput(
                            value = end,
                            onValueChange = { end = it.filter(Char::isDigit).take(5) },
                            hint = "结束",
                            keyboardType = KeyboardType.Number,
                        )
                    }
                }
                defaults?.let {
                    Spacer(Modifier.height(4.dp))
                    Text(
                        "默认 ${it.defaultPortBase}–${it.defaultPortEnd}",
                        fontSize = 10.5.sp,
                        color = C.textDim,
                    )
                }

                Spacer(Modifier.height(10.dp))
                Text("测试网址（留空 = 用默认）", fontSize = 11.sp, color = C.textDim)
                Spacer(Modifier.height(4.dp))
                AgateFormInput(
                    value = testUrl,
                    onValueChange = { testUrl = it },
                    hint = defaults?.defaultTestUrl ?: "https://…",
                    keyboardType = KeyboardType.Uri,
                )

                Spacer(Modifier.height(8.dp))
                Text(
                    "改端口范围不会自动重启内核：已注册的端口不回收，只有新分配的才受新范围约束。" +
                        "要让新范围完整生效，请手动重启内核。",
                    fontSize = 10.5.sp,
                    color = C.amber,
                    lineHeight = 14.sp,
                )
            }

            formError?.let {
                Spacer(Modifier.height(8.dp))
                Text(it, fontSize = 11.5.sp, color = C.red)
            }

            Spacer(Modifier.height(12.dp))
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "取消",
                    fontSize = 13.sp,
                    color = C.textSub,
                    modifier = Modifier
                        .clickable(onClick = onDismiss)
                        .padding(horizontal = 12.dp, vertical = 7.dp),
                )
                Spacer(Modifier.weight(1f))
                Text(
                    "保存",
                    fontSize = 13.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = C.accent,
                    modifier = Modifier
                        .border(1.dp, C.accent.copy(alpha = 0.4f), RoundedCornerShape(8.dp))
                        .clickable(enabled = !loading) {
                            val b = base.toLongOrNull()
                            val e = end.toLongOrNull()
                            formError = when {
                                b == null || b !in 1..65535 -> "起始端口必须是 1-65535"
                                e == null || e !in 1..65535 -> "结束端口必须是 1-65535"
                                b > e -> "起始端口不能大于结束端口"
                                else -> null
                            }
                            if (formError != null) return@clickable
                            scope.launch {
                                val r = ProxyOps.saveSettings(b!!, e!!, testUrl.trim())
                                if (r.ok) {
                                    onSaved()
                                    onDismiss()
                                } else {
                                    // 内核的校验消息是可读中文，原样显示比自造文案准
                                    formError = r.error ?: "保存失败"
                                }
                            }
                        }
                        .padding(horizontal = 14.dp, vertical = 8.dp),
                )
            }
        }
    }
}

/** 相对时间：`3 分钟前` / `2 小时前` / `5 天前`。 */
private fun fmtAgo(at: Long): String {
    if (at <= 0) return "时间未知"
    val diff = System.currentTimeMillis() - at
    if (diff < 0) return "刚刚"
    val minutes = diff / 60_000
    return when {
        minutes < 1 -> "刚刚"
        minutes < 60 -> "$minutes 分钟前"
        minutes < 60 * 24 -> "${minutes / 60} 小时前"
        else -> "${minutes / (60 * 24)} 天前"
    }
}
