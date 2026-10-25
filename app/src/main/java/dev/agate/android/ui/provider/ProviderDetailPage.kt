package dev.agate.android.ui.provider

import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.AccountOps
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.ProviderPreset
import dev.agate.android.data.PresetRegistry
import dev.agate.android.data.dto.AccountDto
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateAvatar
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch

/**
 * 提供商详情子页（M3-3R，对齐 v44 sub-providerdetail）：
 * 三档 seg —— 概览（家档案）/ 账号 · N（acct 块 + 启停/删除）/ 设置（危险区）。
 * v44 概览里的倍率/续期/策略覆盖等项依赖内核暂无的端点，本轮只列真实数据。
 */
@Composable
fun ProviderDetailPage(providerId: String, onBack: () -> Unit) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val preset = PresetRegistry.ALL.find { it.id == providerId }

    val snap by GatewayRepository.snapshot.collectAsState()
    val accounts = snap.accountList.filter { it.provider == providerId }

    var tab by remember { mutableStateOf(0) }
    var opsAccount by remember { mutableStateOf<AccountDto?>(null) }
    var confirmDeleteAll by remember { mutableStateOf(false) }

    DisposableEffect(Unit) {
        AccountOps.configure(GatewayPrefs.apiKey(ctx))
        onDispose { }
    }

    Column(Modifier.fillMaxSize().background(C.bg).statusBarsPadding()) {
        // 顶栏：返回 + 家名（头像 + 名称）+ 右侧状态徽标
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(onClick = onBack) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "返回", tint = C.textMain)
            }
            preset?.let {
                AgateAvatar(name = it.label, size = 32.dp, corner = 10.dp, letters = initialsOf(it.label))
                Spacer(Modifier.width(10.dp))
            }
            Text(
                preset?.label ?: providerId,
                color = C.textMain,
                fontSize = 18.sp,
                fontWeight = FontWeight.Bold,
                modifier = Modifier.weight(1f),
            )
            if (accounts.isNotEmpty()) {
                val anyDown = accounts.any { !it.enabled }
                if (!anyDown) AgateBadge("就绪", BadgeTone.GREEN) else AgateBadge("有停用", BadgeTone.AMBER)
            }
        }

        AgateSegmented(
            options = listOf("概览", "账号 · ${accounts.size}", "设置"),
            selected = tab,
            onSelect = { tab = it },
            modifier = Modifier.padding(horizontal = 16.dp),
        )

        when (tab) {
            0 -> LazyColumn(
                Modifier.fillMaxSize().padding(horizontal = 16.dp),
                contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = 12.dp),
            ) {
                item {
                    Column(
                        Modifier
                            .fillMaxWidth()
                            .background(C.cardBg, RoundedCornerShape(16.dp))
                            .border(1.dp, C.border, RoundedCornerShape(16.dp))
                            .padding(16.dp),
                    ) {
                        CfgRow("账号", "${accounts.size} 个 · ${accounts.count { it.enabled }} 启用", if (accounts.isEmpty()) null else accounts.size.toString())
                        CfgRow("登录通道", preset?.let { channelLabel(it) } ?: "自定义 / 中转上游", null)
                        CfgRow("接入说明", preset?.desc ?: "", null)
                        CfgRow("提供商 ID", providerId, null)
                    }
                }
                item {
                    Spacer(Modifier.height(8.dp))
                    Text(
                        "倍率 / 凭证续期 / 策略覆盖等待内核对应端点接入后开放",
                        color = C.textDim,
                        fontSize = 12.sp,
                    )
                }
            }
            1 -> LazyColumn(
                Modifier.fillMaxSize().padding(horizontal = 16.dp),
                contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = 12.dp),
            ) {
                if (accounts.isEmpty()) {
                    item {
                        Text("该家还没有账号 · 回提供商页点「+ 添加」", color = C.textSub, fontSize = 13.sp)
                    }
                }
                items(accounts, key = { it.id }) { acc ->
                    DetailAcctRow(
                        acc = acc,
                        rank = accounts.indexOf(acc) + 1,
                        onOpen = { opsAccount = acc },
                    )
                    Spacer(Modifier.height(8.dp))
                }
            }
            else -> LazyColumn(
                Modifier.fillMaxSize().padding(horizontal = 16.dp),
                contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = 12.dp),
            ) {
                item {
                    Column(
                        Modifier
                            .fillMaxWidth()
                            .background(C.cardBg, RoundedCornerShape(16.dp))
                            .border(1.dp, C.border, RoundedCornerShape(16.dp))
                            .padding(16.dp),
                    ) {
                        Text("危险区", color = C.red, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                        Spacer(Modifier.height(6.dp))
                        Text(
                            "删除该提供商下全部 ${accounts.size} 个账号（凭证一并清除，不可恢复）",
                            color = C.textSub,
                            fontSize = 12.sp,
                        )
                        Spacer(Modifier.height(10.dp))
                        Text(
                            "删除全部账号",
                            color = C.red,
                            fontSize = 13.sp,
                            fontWeight = FontWeight.SemiBold,
                            modifier = Modifier
                                .background(C.redBg, RoundedCornerShape(10.dp))
                                .clickable(enabled = accounts.isNotEmpty()) { confirmDeleteAll = true }
                                .padding(horizontal = 14.dp, vertical = 10.dp),
                        )
                    }
                }
            }
        }
    }

    // 单账号操作（启停 / 删除）
    opsAccount?.let { acc ->
        AlertDialog(
            onDismissRequest = { opsAccount = null },
            title = { Text(acc.displayName) },
            text = {
                Text(
                    if (acc.enabled) "已启用 · 转发选路参与排队" else "已禁用 · 不参与转发",
                    fontSize = 13.sp,
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    val target = acc
                    opsAccount = null
                    scope.launch {
                        if (AccountOps.batch(listOf(target.id), if (target.enabled) "disable" else "enable")) {
                            Toast.makeText(ctx, if (target.enabled) "已禁用" else "已启用", Toast.LENGTH_SHORT).show()
                            GatewayRepository.refreshOnce()
                        } else Toast.makeText(ctx, "操作失败", Toast.LENGTH_SHORT).show()
                    }
                }) { Text(if (acc.enabled) "禁用" else "启用") }
            },
            dismissButton = {
                Row {
                    TextButton(onClick = { opsAccount = null }) { Text("取消") }
                    TextButton(onClick = {
                        val target = acc
                        opsAccount = null
                        scope.launch {
                            if (AccountOps.batch(listOf(target.id), "remove")) {
                                Toast.makeText(ctx, "已删除账号", Toast.LENGTH_SHORT).show()
                                GatewayRepository.refreshOnce()
                            } else Toast.makeText(ctx, "删除失败", Toast.LENGTH_SHORT).show()
                        }
                    }) { Text("删除", color = C.red) }
                }
            },
        )
    }

    if (confirmDeleteAll) {
        AlertDialog(
            onDismissRequest = { confirmDeleteAll = false },
            title = { Text("删除全部账号") },
            text = { Text("将删除 ${preset?.label} 下 ${accounts.size} 个账号，不可恢复。", fontSize = 13.sp) },
            confirmButton = {
                TextButton(onClick = {
                    confirmDeleteAll = false
                    scope.launch {
                        if (AccountOps.batch(accounts.map { it.id }, "remove")) {
                            Toast.makeText(ctx, "已删除全部账号", Toast.LENGTH_SHORT).show()
                            GatewayRepository.refreshOnce()
                            onBack()
                        } else Toast.makeText(ctx, "删除失败", Toast.LENGTH_SHORT).show()
                    }
                }) { Text("删除", color = C.red) }
            },
            dismissButton = { TextButton(onClick = { confirmDeleteAll = false }) { Text("取消") } },
        )
    }
}

/** v44 .cfg-row：左 name + desc，右 value（chip 风）。 */
@Composable
private fun CfgRow(name: String, desc: String, value: String?) {
    val C = agateColors()
    Row(
        Modifier.fillMaxWidth().padding(vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(name, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            Text(desc, color = C.textSub, fontSize = 12.sp)
        }
        if (value != null) {
            Box(
                Modifier
                    .background(C.insetBg, RoundedCornerShape(999.dp))
                    .padding(horizontal = 12.dp, vertical = 5.dp),
            ) {
                Text(value, color = C.accent, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
            }
        }
    }
}

/** v44 .acct 块（详情页用：序号 + 名称 + 状态徽标，点击弹操作）。 */
@Composable
private fun DetailAcctRow(acc: AccountDto, rank: Int, onOpen: () -> Unit) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .background(C.insetBg, RoundedCornerShape(12.dp))
            .clickable(onClick = onOpen)
            .padding(horizontal = 12.dp, vertical = 10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("#$rank", color = C.textDim, fontSize = 12.sp)
            Spacer(Modifier.width(8.dp))
            Text(acc.displayName, color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
            if (acc.enabled) AgateBadge("就绪", BadgeTone.GREEN) else AgateBadge("停用", BadgeTone.GRAY)
        }
        Spacer(Modifier.height(6.dp))
        Text("余额与用量待额度端点接入 · 点此启停或删除", color = C.textDim, fontSize = 11.sp)
    }
}
