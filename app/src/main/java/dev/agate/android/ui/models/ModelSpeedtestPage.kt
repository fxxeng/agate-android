package dev.agate.android.ui.models

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
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
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
import dev.agate.android.data.ModelOps
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.launch

/**
 * 模型测速页（替换原来的占位骨架页）。
 *
 * ── 它测的是什么 ────────────────────────────────────────────
 * `POST /api/models/test`：走**生产转发链路**真打一次上游，钉住这一家与
 * 一个账号、不顺延换号、跳过启停门禁。所以读数就是「这条模型此刻能不能用、
 * 多快」，不是另开一条内部通道估出来的。
 *
 * ── 会花钱，所以先说清楚 ────────────────────────────────────
 * 每测一条 = 一次真实请求（页面顶部常驻这句提示）。「全部测速」只对**已启用**
 * 的模型跑，且并发 3 —— 并发再高会自己把自己的限流打出来，读数就不可信了。
 */
@Composable
fun ModelSpeedtestPage(onBack: () -> Unit) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    var rows by remember { mutableStateOf<List<ModelOps.ManageModelRow>>(emptyList()) }
    var loading by remember { mutableStateOf(true) }
    var runningAll by remember { mutableStateOf(false) }
    val results by ModelTestStore.results.collectAsState()
    val running by ModelTestStore.running.collectAsState()

    LaunchedEffect(Unit) {
        ModelOps.configure(GatewayPrefs.apiKey(ctx))
        rows = ModelOps.ManageModelRow.parse(ModelOps.fetchManage().data).filter { it.enabled }
        loading = false
    }

    /** 全部测速：按 3 条一组顺序推进，组内并发。 */
    fun runAll() {
        if (runningAll) return
        runningAll = true
        scope.launch {
            try {
                rows.chunked(3).forEach { group ->
                    coroutineScope {
                        group.map { row ->
                            async { ModelTestStore.run(row.provider, row.id) }
                        }.awaitAll()
                    }
                }
            } finally {
                runningAll = false
            }
        }
    }

    Column(Modifier.fillMaxSize().background(C.bg).statusBarsPadding().navigationBarsPadding()) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(onClick = onBack) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "返回", tint = C.textMain)
            }
            Text("模型测速", color = C.textMain, fontSize = 18.sp, fontWeight = FontWeight.Bold)
            Spacer(Modifier.weight(1f))
            Text(
                if (runningAll) "测速中…" else "全部测速",
                color = if (runningAll) C.textDim else C.accent,
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(
                        if (runningAll) C.insetBg else C.accent.copy(alpha = 0.12f),
                        RoundedCornerShape(9.dp),
                    )
                    .clickable(enabled = !runningAll && rows.isNotEmpty()) { runAll() }
                    .padding(horizontal = 10.dp, vertical = 6.dp),
            )
            Spacer(Modifier.width(10.dp))
        }

        Text(
            "每条测试都是一次真实上游请求（会消耗额度）· 钉住单一账号、不顺延换号",
            color = C.textDim,
            fontSize = 11.sp,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
        )

        if (loading) {
            Spacer(Modifier.height(12.dp))
            Text("读取模型清单…", color = C.textDim, fontSize = 12.sp, modifier = Modifier.padding(horizontal = 16.dp))
            return@Column
        }
        if (rows.isEmpty()) {
            Spacer(Modifier.height(12.dp))
            Text(
                "没有已启用的模型 · 先到「模型」页打开几个再回来测",
                color = C.textSub, fontSize = 12.sp,
                modifier = Modifier.padding(horizontal = 16.dp),
            )
            return@Column
        }

        val byProvider = rows.groupBy { it.providerLabel }
        LazyColumn(
            Modifier.fillMaxSize().padding(horizontal = 16.dp),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = 10.dp),
        ) {
            byProvider.forEach { (label, list) ->
                item {
                    Text(
                        label,
                        color = C.textSub,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier.padding(top = 8.dp, bottom = 6.dp),
                    )
                }
                items(list.size) { index ->
                    val row = list[index]
                    val key = ModelTestStore.key(row.provider, row.id)
                    val busy = key in running
                    val result = results[key]
                    Column(
                        Modifier
                            .fillMaxWidth()
                            .background(C.cardBg, RoundedCornerShape(14.dp))
                            .border(1.dp, C.border, RoundedCornerShape(14.dp))
                            .padding(horizontal = 12.dp, vertical = 10.dp),
                    ) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(
                                row.id,
                                color = C.textMain,
                                fontSize = 13.sp,
                                fontWeight = FontWeight.SemiBold,
                                modifier = Modifier.weight(1f),
                            )
                            row.credits?.let {
                                Text(it, color = C.textDim, fontSize = 10.sp, modifier = Modifier.padding(end = 8.dp))
                            }
                            Text(
                                if (busy) "测试中…" else "测速",
                                color = if (busy) C.textDim else C.accent,
                                fontSize = 11.sp,
                                fontWeight = FontWeight.SemiBold,
                                modifier = Modifier
                                    .background(C.accent.copy(alpha = 0.10f), RoundedCornerShape(8.dp))
                                    .clickable(enabled = !busy) {
                                        scope.launch { ModelTestStore.run(row.provider, row.id) }
                                    }
                                    .padding(horizontal = 9.dp, vertical = 4.dp),
                            )
                        }
                        if (result != null) {
                            Spacer(Modifier.height(5.dp))
                            Text(
                                result.summary(),
                                // 失败用红、成功用绿：这一页就是拿来看「哪家挂了」
                                color = if (result.ok) C.green else C.red,
                                fontSize = 11.sp,
                                style = AgateType.Caption,
                            )
                        }
                    }
                    Spacer(Modifier.height(8.dp))
                }
            }
        }
    }
}
