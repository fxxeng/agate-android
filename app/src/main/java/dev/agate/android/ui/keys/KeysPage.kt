package dev.agate.android.ui.keys

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
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.KeyOps
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

    var rows by remember { mutableStateOf<List<KeyOps.KeyRow>>(emptyList()) }
    var loading by remember { mutableStateOf(false) }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    fun reload() {
        if (loading) return
        loading = true
        scope.launch {
            val r = KeyOps.list()
            loading = false
            rows = KeyOps.parseRows(r.data, localKey)
            if (!r.ok) toast(r.error ?: "读取失败")
        }
    }

    DisposableEffect(Unit) {
        KeyOps.configure(localKey)
        GatewayRepository.configure(localKey)
        onDispose { }
    }
    LaunchedEffect(Unit) { reload() }

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
                    listOf("Base URL" to baseUrl, "API Key" to localKey)
                } else {
                    // 本机不需要 Key：只给地址，别让用户去填一个用不上的字段
                    listOf("Base URL" to baseUrl)
                },
                hint = if (lanMode) {
                    "已开局域网监听，客户端必须带 Key（不匹配会拿到 401 invalid or missing API key）"
                } else {
                    "本机访问**无需填 Key**：端口只监听 127.0.0.1，客户端只填 Base URL 即可。" +
                        "开启「局域网监听」后才需要 Key"
                },
                onCopy = { text ->
                    clipboard.setText(AnnotatedString(text))
                    toast("已复制")
                },
                onCopyAll = {
                    clipboard.setText(AnnotatedString("Base URL: $baseUrl\nAPI Key: $localKey"))
                    toast("已复制 Base URL 与 Key")
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
                    if (row.restricted) {
                        append(" · 限 ${row.allowedProviders.size} 家")
                        if (row.allowedModels.isNotEmpty()) append(" / ${row.allowedModels.size} 模型")
                    }
                },
                color = C.textDim,
                fontSize = 11.sp,
            )
        }
        if (!row.fromEnv) {
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
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
