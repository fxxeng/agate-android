package dev.agate.android.ui.settings

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.ModelAccessOps
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateSubPageHeader
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch

/**
 * 模型 Key 权限查询：选一把 Key，问「这把 Key 实际能调哪些模型」。
 *
 * ── 为什么不是 /v1/models ────────────────────────────────────
 * `/v1/models` 是免鉴权探针，拿不到 Key 限制就给全量，看着像能用其实会被 403。
 * 这里走管理面 `/api/model-access/models`，按次把选中的 Key 明文注入请求，
 * 问的是那一把的真实权限。所以「选 A 却按 B 的权限算」是这页唯一要防的坑 ——
 * 每次查询都传当前选中 Key，绝不复用面板那把全局凭证。
 */
@Composable
fun ModelAccessPage(onBack: () -> Unit) {
    val C = agateColors()
    val scope = rememberCoroutineScope()

    var keys by remember { mutableStateOf<List<ModelAccessOps.KeyChoice>>(emptyList()) }
    var selected by remember { mutableStateOf<Int?>(null) }
    var models by remember { mutableStateOf<List<ModelAccessOps.ModelItem>?>(null) }
    var queryKey by remember { mutableStateOf<String?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var loading by remember { mutableStateOf(true) }
    var querying by remember { mutableStateOf(false) }

    fun loadKeys() {
        scope.launch {
            loading = true
            val r = ModelAccessOps.keys()
            if (r.ok) {
                keys = r.data.orEmpty()
                error = null
            } else {
                error = r.error ?: "读取 Key 清单失败"
            }
            loading = false
        }
    }

    androidx.compose.runtime.LaunchedEffect(Unit) { loadKeys() }

    fun query() {
        val idx = selected
        if (idx == null || idx !in keys.indices) {
            error = "请先选择一把 API Key"
            return
        }
        val key = keys[idx]
        querying = true
        error = null
        models = null
        queryKey = key.masked.ifBlank { key.name }
        scope.launch {
            val r = ModelAccessOps.models(key.key)
            if (r.ok) {
                models = r.data.orEmpty()
            } else {
                error = r.error ?: "查询失败"
            }
            querying = false
        }
    }

    Column(Modifier.fillMaxSize().background(C.bg)) {
        AgateSubPageHeader("模型 Key 权限", onBack = onBack) {
            if (keys.isNotEmpty()) AgateBadge("${keys.size} 把 Key", BadgeTone.BLUE)
        }

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))

            Text(
                "按选中的那把 Key 查询它实际可用的模型。Key 明文只在本机管理面使用，不会显示在这里。",
                style = AgateType.Caption,
                color = C.textSub,
            )
            Spacer(Modifier.height(12.dp))

            if (error != null) {
                MacBanner(error!!, C.red, C.redBg)
                Spacer(Modifier.height(10.dp))
            }

            Text("① 选择 Key", style = AgateType.SectionTitle, color = C.textMain, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(8.dp))
            if (loading) {
                MacCard { Text("读取 Key 清单…", style = AgateType.BodyS, color = C.textDim) }
            } else if (keys.isEmpty()) {
                MacCard { Text("没有可用的 Key —— 请先在「访问密钥」里添加。", style = AgateType.BodyS, color = C.textDim) }
            } else {
                MacCard {
                    keys.forEachIndexed { index, k ->
                        if (index > 0) MacDivider(C)
                        val active = index == selected
                        Row(
                            Modifier
                                .fillMaxWidth()
                                .clickable { selected = index; models = null; queryKey = null }
                                .background(if (active) C.accent.copy(alpha = 0.10f) else androidx.compose.ui.graphics.Color.Transparent, RoundedCornerShape(10.dp))
                                .padding(horizontal = 10.dp, vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Box(
                                Modifier
                                    .height(16.dp)
                                    .padding(end = 8.dp),
                            ) {
                                Text(if (active) "●" else "○", fontSize = 14.sp, color = if (active) C.accent else C.textDim)
                            }
                            Column(Modifier.weight(1f)) {
                                Text(k.name.ifBlank { k.masked }, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.Medium)
                                Text(
                                    buildString {
                                        append(k.masked)
                                        if (k.isEnv) append(" · 环境变量")
                                        if (k.allowedProviders.isNotEmpty()) append(" · ${k.allowedProviders.size} 家")
                                        if (k.allowedModels.isNotEmpty()) append(" · ${k.allowedModels.size} 模型")
                                    },
                                    style = AgateType.Caption,
                                    color = C.textDim,
                                )
                            }
                            if (k.enabled) AgateBadge("启用", BadgeTone.GREEN) else AgateBadge("停用", BadgeTone.GRAY)
                        }
                    }
                }
            }

            Spacer(Modifier.height(14.dp))
            MacBtn("② 查询这把 Key 的模型权限", primary = true, enabled = selected != null && !querying) { query() }
            Spacer(Modifier.height(14.dp))

            if (queryKey != null || models != null) {
                Text("③ 可用模型${if (queryKey != null) " · $queryKey" else ""}",
                    style = AgateType.SectionTitle, color = C.textMain, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.height(8.dp))
                if (querying) {
                    MacCard { Text("查询中…", style = AgateType.BodyS, color = C.textDim) }
                } else if (models != null) {
                    if (models!!.isEmpty()) {
                        MacCard { Text("这把 Key 没有可用模型（或权限为空）。", style = AgateType.BodyS, color = C.textDim) }
                    } else {
                        MacCard {
                            Text("共 ${models!!.size} 个", style = AgateType.Caption, color = C.textSub)
                            Spacer(Modifier.height(6.dp))
                            models!!.forEachIndexed { index, m ->
                                if (index > 0) MacDivider(C)
                                Row(Modifier.fillMaxWidth().padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                                    Column(Modifier.weight(1f)) {
                                        Text(m.name, color = C.textMain, fontSize = 13.sp)
                                        if (m.id != m.name) Text(m.id, style = AgateType.Caption, color = C.textDim)
                                    }
                                    if (m.ownedBy.isNotBlank()) AgateBadge(m.ownedBy, BadgeTone.GRAY)
                                }
                            }
                        }
                    }
                }
            }
            Spacer(Modifier.height(24.dp))
        }
    }
}

@Composable
private fun MacCard(content: @Composable ColumnScope.() -> Unit) {
    val C = agateColors()
    Column(
        Modifier.fillMaxWidth().agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp)).padding(horizontal = 14.dp, vertical = 13.dp),
        content = content,
    )
}

@Composable
private fun MacDivider(C: dev.agate.android.ui.theme.AgateColors) {
    Box(Modifier.fillMaxWidth().height(1.dp).background(C.border.copy(alpha = 0.55f)))
}

@Composable
private fun MacBanner(text: String, fg: androidx.compose.ui.graphics.Color, bg: androidx.compose.ui.graphics.Color) {
    Row(Modifier.fillMaxWidth().background(bg, RoundedCornerShape(10.dp)).padding(horizontal = 12.dp, vertical = 8.dp)) {
        Text(text, style = AgateType.Caption, color = fg)
    }
}

@Composable
private fun MacBtn(text: String, primary: Boolean, enabled: Boolean, onClick: () -> Unit) {
    val C = agateColors()
    Box(
        Modifier.fillMaxWidth()
            .background(if (enabled) if (primary) C.accent else C.btnBg else C.btnBg.copy(alpha = 0.5f), RoundedCornerShape(11.dp))
            .then(if (enabled) Modifier.clickable(onClick = onClick) else Modifier)
            .padding(vertical = 12.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(text, style = AgateType.BodyS, fontWeight = FontWeight.SemiBold,
            color = if (!enabled) C.textDim else if (primary) C.onAccent else C.textMain)
    }
}
