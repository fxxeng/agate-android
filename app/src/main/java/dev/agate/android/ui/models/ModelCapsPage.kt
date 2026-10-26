package dev.agate.android.ui.models

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
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
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
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
import android.widget.Toast
import dev.agate.android.data.GatewayPrefs
import dev.agate.android.data.ModelOps
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateFormInput
import dev.agate.android.ui.components.AgateSearchBar
import dev.agate.android.ui.components.AgateShape
import dev.agate.android.ui.components.AgateType
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.components.agateColors
import dev.agate.android.ui.components.AgatePageHeader
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject

/**
 * 能力位（M3 · MODEL_CAPS 子页）。
 *
 * 数据源 GET /api/models/manage 的 models[]：每行带 capabilities（六键出口
 * 生效值）、capSources（每键来源 upstream=上游实测 / modelsdev=社区库 /
 * user=用户改；缺键=未声明）与 capOverrides（已覆盖键列表）。
 *
 * 编辑走 POST /api/models/capabilities，patch 语义（model_manage.rs）：
 * 只提交改动键；值 null = 恢复上游值；token 键 1~1 亿正整数、开关键布尔；
 * 仅内置提供商（自定义提供商本端点 400，列表里直接排除）。
 * 覆盖只影响对外声明（/v1/models、Anthropic 列表与管理页），不改路由/启停/转发。
 */
private val CAP_KEYS = listOf(
    "maxInputTokens", "maxOutputTokens",
    "supportsToolCall", "supportsImages", "supportsVideo", "supportsReasoning",
)
private val BOOL_KEYS = setOf(
    "supportsToolCall", "supportsImages", "supportsVideo", "supportsReasoning",
)

@Composable
fun ModelCapsPage(
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    var view by remember { mutableStateOf<JSONObject?>(null) }
    var loading by remember { mutableStateOf(false) }
    var query by remember { mutableStateOf("") }
    var picked by remember { mutableStateOf<JSONObject?>(null) }
    // pending patch：键 → 值（JSONObject.NULL = 恢复上游）
    var patch by remember { mutableStateOf<JSONObject?>(null) }

    fun load() {
        if (loading) return
        loading = true
        scope.launch {
            val r = ModelOps.fetchManage()
            loading = false
            if (r.ok) {
                view = r.data
                // 就地更新选中行（保存后响应里的 capabilities 已是最新）
                picked?.let { p ->
                    val id = p.optString("id"); val pv = p.optString("provider")
                    r.data?.optJSONArray("models")?.let { arr ->
                        (0 until arr.length()).firstOrNull { i ->
                            val m = arr.optJSONObject(i)
                            m != null && m.optString("id") == id && m.optString("provider") == pv
                        }?.let { picked = arr.optJSONObject(it) }
                    }
                }
            } else {
                Toast.makeText(ctx, "加载失败：${r.error ?: "未知"}", Toast.LENGTH_SHORT).show()
            }
        }
    }

    DisposableEffect(Unit) {
        ModelOps.configure(GatewayPrefs.apiKey(ctx))
        load()
        onDispose { }
    }

    val models = remember(view, query) {
        val arr = view?.optJSONArray("models") ?: JSONArray()
        val q = query.trim()
        (0 until arr.length()).mapNotNull { i -> arr.optJSONObject(i) }
            .filter { it.optString("id").isNotBlank() && it.optString("provider").isNotBlank() }
            .filter { q.isEmpty() ||
                it.optString("id").contains(q, ignoreCase = true) ||
                it.optString("providerLabel").contains(q, ignoreCase = true) }
            .take(60)
    }

    fun putPatch(key: String, value: Any?) {
        val p = patch ?: JSONObject()
        p.put(key, value ?: JSONObject.NULL)
        patch = p
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "能力位", onOpenDrawer = {}, onBack = onBack)

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))
            Text(
                "覆盖只改「对下游怎么声明」（/v1/models 与管理页），不动路由、启停与转发行为；仅内置提供商可覆盖",
                style = AgateType.Caption, color = C.textDim,
            )
            Spacer(Modifier.height(10.dp))

            // ── 模型选择（搜索 + 单选展开） ──
            AgateSearchBar(value = query, onValueChange = { query = it }, hint = "搜索模型 id / 家")
            Spacer(Modifier.height(8.dp))
            if (models.isEmpty()) {
                Text(
                    if (loading) "加载中…" else "清单为空（先到「模型」页刷新目录）",
                    style = AgateType.Caption, color = C.textDim,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
            models.forEach { m ->
                val key = "${m.optString("provider")}|${m.optString("id")}"
                val selected = picked?.optString("provider") == m.optString("provider") &&
                    picked?.optString("id") == m.optString("id")
                val overrides = m.optJSONArray("capOverrides")?.length() ?: 0
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { picked = if (selected) null else m; patch = null }
                        .background(
                            if (selected) C.accent.copy(alpha = 0.10f) else C.cardBg,
                            RoundedCornerShape(14.dp),
                        )
                        .border(
                            1.dp,
                            if (selected) C.accent.copy(alpha = 0.5f) else C.border,
                            RoundedCornerShape(14.dp),
                        )
                        .padding(horizontal = 12.dp, vertical = 9.dp),
                ) {
                    Column(Modifier.weight(1f)) {
                        Text(
                            m.optString("id"), style = AgateType.BodyS, color = C.textMain,
                            fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
                        )
                        Text(m.optString("providerLabel"), style = AgateType.Caption, color = C.textDim)
                    }
                    if (overrides > 0) {
                        AgateBadge(text = "已覆盖 $overrides", tone = dev.agate.android.ui.components.BadgeTone.BLUE)
                    }
                }
                Spacer(Modifier.height(6.dp))
            }

            // ── 编辑卡 ──
            picked?.let { m ->
                Spacer(Modifier.height(10.dp))
                CapsEditCard(
                    model = m,
                    patch = patch,
                    busy = loading,
                    onPatch = ::putPatch,
                    onSave = {
                        val p = patch
                        if (p == null || p.length() == 0) {
                            Toast.makeText(ctx, "没有要保存的改动", Toast.LENGTH_SHORT).show()
                        } else {
                            scope.launch {
                                val r = ModelOps.setCapabilities(
                                    m.optString("provider"), m.optString("id"), p,
                                )
                                if (r.ok) {
                                    patch = null
                                    Toast.makeText(ctx, "能力位已保存", Toast.LENGTH_SHORT).show()
                                    // 就地更新选中行
                                    val id = m.optString("id"); val pv = m.optString("provider")
                                    r.data?.optJSONArray("models")?.let { arr ->
                                        (0 until arr.length()).firstOrNull { i ->
                                            val mm = arr.optJSONObject(i)
                                            mm != null && mm.optString("id") == id && mm.optString("provider") == pv
                                        }?.let { picked = arr.optJSONObject(it) }
                                    }
                                } else {
                                    Toast.makeText(ctx, "保存失败：${r.error ?: "未知"}", Toast.LENGTH_SHORT).show()
                                }
                            }
                        }
                    },
                )
            }
            Spacer(Modifier.height(28.dp))
        }
    }
}

/* ── 六键编辑卡 ── */
@Composable
private fun CapsEditCard(
    model: JSONObject,
    patch: JSONObject?,
    busy: Boolean,
    onPatch: (String, Any?) -> Unit,
    onSave: () -> Unit,
) {
    val C = agateColors()
    val caps = model.optJSONObject("capabilities") ?: JSONObject()
    val sources = model.optJSONObject("capSources") ?: JSONObject()
    val overrides = model.optJSONArray("capOverrides")?.let { arr ->
        (0 until arr.length()).map { arr.optString(it) }.toSet()
    } ?: emptySet()

    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(14.dp),
    ) {
        Text(
            "${model.optString("id")} · ${model.optString("providerLabel")}",
            style = AgateType.SectionTitle, color = C.textMain,
        )
        Spacer(Modifier.height(4.dp))
        Text(
            "来源：上游=各家实测 · 社区=models.dev 库 · 用户=已覆盖；未声明=该项无任何来源",
            style = AgateType.Caption, color = C.textDim,
        )
        Spacer(Modifier.height(10.dp))

        CAP_KEYS.forEach { key ->
            val source = sources.optString(key).ifBlank { null }
            val effective = caps.opt(key)
            val covered = key in overrides
            val pending = patch?.has(key) == true

            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(key, style = AgateType.BodyS, color = C.textMain, fontWeight = FontWeight.SemiBold)
                        if (pending) {
                            Spacer(Modifier.width(6.dp))
                            AgateBadge(text = "待保存", tone = dev.agate.android.ui.components.BadgeTone.AMBER)
                        } else if (covered) {
                            Spacer(Modifier.width(6.dp))
                            AgateBadge(text = "已覆盖", tone = dev.agate.android.ui.components.BadgeTone.BLUE)
                        }
                    }
                    Spacer(Modifier.height(2.dp))
                    Text(
                        sourceLabel(source, effective),
                        style = AgateType.Caption, color = C.textDim,
                    )
                }
                if (key in BOOL_KEYS) {
                    BoolTriplet(
                        effective = effective as? Boolean,
                        covered = covered,
                        onSelect = { mode ->
                            when (mode) {
                                0 -> if (covered) onPatch(key, null) // 恢复上游
                                1 -> onPatch(key, true)
                                else -> onPatch(key, false)
                            }
                        },
                        selected = when {
                            pending && patch?.opt(key) == JSONObject.NULL -> 0
                            pending && patch?.optBoolean(key) == true -> 1
                            pending -> 2
                            covered -> when (effective as? Boolean) { true -> 1; false -> 2; else -> 0 }
                            else -> 0
                        },
                    )
                }
            }
            if (key !in BOOL_KEYS) {
                Spacer(Modifier.height(6.dp))
                TokenInput(
                    key = key, effective = effective, covered = covered,
                    onPatch = onPatch,
                    onClear = { val p2 = patch; p2?.remove(key); if (p2 != null) patch = p2 },
                )
            }
            Spacer(Modifier.height(10.dp))
        }

        Row {
            Spacer(Modifier.weight(1f))
            val count = patch?.length() ?: 0
            Text(
                if (busy) "保存中…" else if (count > 0) "保存（$count 项）" else "保存",
                style = AgateType.BodyS,
                color = if (busy || count == 0) C.textDim else C.accent,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clickable(enabled = !busy && count > 0, onClick = onSave)
                    .background(C.accent.copy(alpha = 0.12f), AgateShape.small)
                    .padding(horizontal = 16.dp, vertical = 7.dp),
            )
        }
    }
}

private fun sourceLabel(source: String?, effective: Any?): String {
    val from = when (source) {
        "upstream" -> "上游实测"
        "modelsdev" -> "社区库"
        "user" -> "用户改"
        else -> "未声明"
    }
    val v = when (effective) {
        null, JSONObject.NULL -> "—"
        is Boolean -> if (effective) "开" else "关"
        else -> effective.toString()
    }
    return "当前 $v · 来源：$from"
}

/* ── 布尔键三档：跟随上游 / 开 / 关 ── */
@Composable
private fun BoolTriplet(
    effective: Boolean?,
    covered: Boolean,
    selected: Int,
    onSelect: (Int) -> Unit,
) {
    val C = agateColors()
    Row(
        Modifier
            .background(C.segBg, AgateShape.small)
            .padding(3.dp),
        horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(2.dp),
    ) {
        listOf(
            if (covered) "恢复" else "跟随",
            "开", "关",
        ).forEachIndexed { i, label ->
            val active = i == selected
            Text(
                label,
                fontSize = 11.sp,
                color = if (active) C.accent else C.textSub,
                fontWeight = if (active) FontWeight.Bold else FontWeight.Normal,
                modifier = Modifier
                    .clickable { onSelect(i) }
                    .background(
                        if (active) C.accent.copy(alpha = 0.14f) else C.bg.copy(alpha = 0f),
                        AgateShape.small,
                    )
                    .padding(horizontal = 9.dp, vertical = 5.dp),
            )
        }
    }
}

/* ── token 键输入（空 = 不动；覆盖中提供恢复） ── */
@Composable
private fun TokenInput(
    key: String,
    effective: Any?,
    covered: Boolean,
    onPatch: (String, Any?) -> Unit,
    onClear: () -> Unit,
) {
    val C = agateColors()
    var text by remember(key) { mutableStateOf("") }
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(
            if (effective is Number) "当前 ${effective.toLong()}" else "当前 —",
            style = AgateType.Caption, color = C.textDim,
            modifier = Modifier.width(110.dp),
        )
        AgateFormInput(
            value = text,
            onValueChange = { s ->
                text = s.filter { it.isDigit() }
                val v = text.toLongOrNull()
                if (v != null && v in 1..100_000_000L) onPatch(key, v) else if (text.isEmpty()) onClear()
            },
            hint = "新值 1 ~ 1 亿",
            modifier = Modifier.weight(1f),
        )
        if (covered) {
            Spacer(Modifier.width(8.dp))
            Text(
                "恢复",
                style = AgateType.Caption, color = C.accent, fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clickable { onPatch(key, null); text = "" }
                    .background(C.accent.copy(alpha = 0.10f), AgateShape.small)
                    .padding(horizontal = 10.dp, vertical = 6.dp),
            )
        }
    }
}
