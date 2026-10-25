package dev.agate.android.ui.provider

import android.widget.Toast
import androidx.compose.foundation.background
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
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.collectAsState
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
import dev.agate.android.auth.AuthWebViewSheet
import dev.agate.android.auth.LoginSession
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.LoginClient
import dev.agate.android.data.PresetRegistry
import dev.agate.android.data.dto.AccountDto
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateAvatar
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch

/**
 * 提供商页（M3-3）：账号按家分组总览 + 添加提供商（24 官方预置）。
 *
 * 数据源：GatewayRepository.snapshot（5s 轮询 /api/session，单一数据源）；
 * ★＝该家队首（currentAccountIds，与内核选路口径一致），不是全局 currentAccountId。
 * 编辑（启停/删除/拖拽排序）进 provider 详情页，本页只读 + 添加。
 */
@Composable
fun ProviderPage(modifier: Modifier = Modifier) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val snap by GatewayRepository.snapshot.collectAsState()
    val snapError by GatewayRepository.snapshotError.collectAsState()

    var showAdd by remember { mutableStateOf(false) }
    var loginSession by remember { mutableStateOf<LoginSession?>(null) }

    DisposableEffect(Unit) {
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
        GatewayRepository.startPolling()
        onDispose { GatewayRepository.stopPolling() }
    }

    // 分组：预置注册表顺序优先（同产品线相邻），未知家排最后。
    val groups = remember(snap.accountList) {
        val byProvider = snap.accountList.groupBy { it.provider }
        val ordered = ArrayList<ProviderGroupView>(byProvider.size)
        for (preset in PresetRegistry.ALL) {
            val list = byProvider[preset.id]
            if (!list.isNullOrEmpty()) {
                ordered += ProviderGroupView(preset.id, preset.label, preset, list)
            }
        }
        for ((pid, list) in byProvider) {
            if (ordered.none { it.providerId == pid }) {
                ordered += ProviderGroupView(pid, PresetRegistry.labelOf(pid), null, list)
            }
        }
        ordered
    }

    val enabled = snap.accountList.count { it.enabled }
    val activeHomes = groups.size

    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(horizontal = 16.dp),
    ) {
        Spacer(Modifier.height(16.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("提供商", color = C.textMain, fontSize = 22.sp, fontWeight = FontWeight.Bold)
                Text(
                    "活跃账号 $enabled/${snap.accountList.size} · $activeHomes 家",
                    color = C.textSub,
                    fontSize = 12.sp,
                )
            }
            Button(onClick = { showAdd = true }) { Text("添加") }
        }
        Spacer(Modifier.height(12.dp))

        snapError?.let {
            Text(it, color = C.amber, fontSize = 12.sp)
            Spacer(Modifier.height(8.dp))
        }

        if (snap.accountList.isEmpty()) {
            EmptyHint(
                hasError = snapError != null,
                onAdd = { showAdd = true },
            )
        } else {
            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                items(groups.size) { gi ->
                    val g = groups[gi]
                    GroupCard(g, heads = snap.currentAccountIds)
                }
                item { Spacer(Modifier.height(24.dp)) }
            }
        }
    }

    if (showAdd) {
        AddProviderSheet(
            providerCounts = snap.providerCounts,
            onDismiss = { showAdd = false },
            onPick = { preset ->
                if (preset.kind != dev.agate.android.data.LoginKind.WEBVIEW) {
                    Toast.makeText(ctx, "${preset.label}：该登录通道将在后续版本提供", Toast.LENGTH_SHORT).show()
                    return@AddProviderSheet
                }
                scope.launch {
                    when (val r = LoginClient.start(preset.id)) {
                        is LoginClient.StartResult.Started -> {
                            showAdd = false
                            loginSession = LoginSession(r.provider, r.state, r.authUrl)
                        }
                        is LoginClient.StartResult.Failed ->
                            Toast.makeText(ctx, r.reason, Toast.LENGTH_LONG).show()
                    }
                }
            },
        )
    }

    loginSession?.let { session ->
        AuthWebViewSheet(
            session = session,
            onDismiss = { loginSession = null },
            onResult = { success, message ->
                loginSession = null
                Toast.makeText(ctx, message, Toast.LENGTH_LONG).show()
                if (success) GatewayRepository.refreshOnce()
            },
        )
    }
}

/** 展示用分组（preset 可空：手改数据塞进来的未知 provider id 也能显示）。 */
private data class ProviderGroupView(
    val providerId: String,
    val label: String,
    val preset: dev.agate.android.data.ProviderPreset?,
    val accounts: List<AccountDto>,
)

@Composable
private fun EmptyHint(hasError: Boolean, onAdd: () -> Unit) {
    val C = agateColors()
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(16.dp))
            .padding(vertical = 28.dp, horizontal = 16.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            if (hasError) "内核未运行，启动网关后账号会出现在这里" else "还没有���号",
            color = C.textMain,
            fontSize = 15.sp,
            fontWeight = FontWeight.Medium,
        )
        Spacer(Modifier.height(4.dp))
        Text(
            if (hasError) "下方「首页」页签可启动网关" else "添加一家提供商并登录，即可开始聚合调用",
            color = C.textDim,
            fontSize = 12.sp,
        )
        Spacer(Modifier.height(14.dp))
        Button(onClick = onAdd) { Text("添加提供商") }
    }
}

@Composable
private fun GroupCard(
    g: ProviderGroupView,
    heads: Map<String, String>,
) {
    val C = agateColors()
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(16.dp))
            .padding(14.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            AgateAvatar(name = g.label, size = 34.dp, corner = 10.dp)
            Spacer(Modifier.width(10.dp))
            Column(Modifier.weight(1f)) {
                Text(g.label, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                Text(g.preset?.desc ?: "未知提供商", color = C.textDim, fontSize = 11.sp)
            }
            AgateBadge(text = "${g.accounts.size} 个账号", tone = BadgeTone.GRAY)
        }
        Spacer(Modifier.height(10.dp))
        g.accounts.forEach { acc ->
            AccountRow(acc, isHead = heads[g.providerId] == acc.id)
            Spacer(Modifier.height(6.dp))
        }
    }
}

@Composable
private fun AccountRow(acc: AccountDto, isHead: Boolean) {
    val C = agateColors()
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(C.insetBg, RoundedCornerShape(10.dp))
            .clickable(enabled = false) { }
            .padding(horizontal = 10.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        AgateAvatar(name = acc.displayName, size = 28.dp, corner = 9.dp)
        Spacer(Modifier.width(8.dp))
        Column(Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    acc.displayName,
                    color = if (acc.enabled) C.textMain else C.textDim,
                    fontSize = 13.sp,
                )
                if (isHead) {
                    Spacer(Modifier.width(6.dp))
                    Text("★ 首选", color = C.accent, fontSize = 11.sp)
                }
            }
            Text(
                buildString {
                    append(if (acc.enabled) "已启用" else "已禁用")
                    if (!acc.hasCredentials) append(" · 无凭证")
                    else if (!acc.chatSupported) append(" · 不参与转发")
                    acc.edition?.let { append(" · $it") }
                },
                color = C.textDim,
                fontSize = 11.sp,
            )
        }
        when {
            !acc.hasCredentials -> AgateBadge("缺凭证", BadgeTone.RED)
            acc.lowBalance -> AgateBadge("余额低", BadgeTone.AMBER)
            acc.enabled -> AgateBadge("健康", BadgeTone.GREEN, withDot = true)
            else -> AgateBadge("停用", BadgeTone.GRAY)
        }
    }
}
