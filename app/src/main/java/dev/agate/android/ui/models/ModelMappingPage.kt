package dev.agate.android.ui.models

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
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
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
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
import android.widget.Toast
import dev.agate.core.GatewayPrefs
import dev.agate.android.data.ModelOps
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateFormInput
import dev.agate.android.ui.components.AgateSearchBar
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.theme.AgateShape
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.agateColors
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.BadgeTone
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject

/**
 * 模型映射（M3 · MODEL_MAPPING 子页）—— 这一页只做一件事：**改名**。
 *
 * 模型的启停与思考等级是模型自己的属性，已在「模型」页每行设置
 * （`models[].enabled` / `reasoningBinding`），不再在这里以「默认绑定」的形式
 * 冒充映射。映射页只维护 `alias → target` 的改名关系：
 *   · 三元组 (alias, target, provider) 是身份，建好后锁定（内核无原子 rename，
 *     改名要删旧建新，风险大）——编辑弹窗只让它改开关与那条别名自己的思考档位；
 *   · 内核 `mappings[].effective` / `issue` 是生效真值与诊断码，界面只读不算，
 *     据 issue 显示具体原因，而不是「未生效」三个字；
 *   · alias / target / model id 一键复制。
 *
 * 数据源 GET /api/models/manage；所有写走 POST /api/models/mappings（同三元组
 * = 命中已有条目），写接口返回最新 manage 视图就地替换。reasoning 候选表用
 * 响应里随发的 reasoningLevels（内核事实来源，前端不自己抄一份避免漂移）。
 */
@Composable
fun ModelMappingPage(
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val clipboard = LocalClipboardManager.current

    var view by remember { mutableStateOf<JSONObject?>(null) }
    var loading by remember { mutableStateOf(false) }
    var showAdd by remember { mutableStateOf(false) }
    var expandedRow by remember { mutableStateOf<String?>(null) }
    var editing by remember { mutableStateOf<ModelOps.MappingRow?>(null) }
    var pendingDelete by remember { mutableStateOf<Triple<String, String, String?>?>(null) }

    fun toast(ok: Boolean, what: String, error: String?) {
        Toast.makeText(ctx, if (ok) "${what}成功" else "${what}失败：${error ?: "未知"}", Toast.LENGTH_SHORT).show()
    }

    fun copy(text: String, label: String) {
        if (text.isBlank()) return
        clipboard.setText(AnnotatedString(text))
        Toast.makeText(ctx, "已复制$label", Toast.LENGTH_SHORT).show()
    }

    fun applyResult(r: ModelOps.OpResult, what: String) {
        if (r.ok) view = r.data
        toast(r.ok, what, r.error)
    }

    fun load() {
        if (loading) return
        loading = true
        scope.launch {
            val r = ModelOps.fetchManage()
            loading = false
            if (r.ok) view = r.data else toast(false, "加载", r.error)
        }
    }

    androidx.compose.runtime.DisposableEffect(Unit) {
        ModelOps.configure(GatewayPrefs.apiKey(ctx))
        load()
        onDispose { }
    }

    val models = remember(view) {
        val arr = view?.optJSONArray("models") ?: JSONArray()
        (0 until arr.length()).mapNotNull { i ->
            arr.optJSONObject(i)?.takeIf { it.optString("id").isNotBlank() }
        }
    }
    val mappings = remember(view) {
        val arr = view?.optJSONArray("mappings") ?: JSONArray()
        (0 until arr.length()).mapNotNull { i -> arr.optJSONObject(i)?.let { ModelOps.MappingRow.from(it) } }
    }
    val levels = remember(view) {
        val arr = view?.optJSONArray("reasoningLevels") ?: JSONArray()
        (0 until arr.length()).map { arr.optString(it) }.filter { it.isNotBlank() }
    }

    // 默认隐藏「失效」的（目标模型已不存在，转发必然 404），底部给开关铺开。
    var showDangling by remember { mutableStateOf(false) }
    // 内核已不再输出 isDefault=true 的自映射（那是模型自己的状态），但老配置/
    // 老内核可能还发；兜底过滤，不让「A → A」混进改名页。
    val renameRows = mappings.filter { !it.isDefault }
    val danglingRows = renameRows.filter { it.dangling }
    val userRows = if (showDangling) renameRows else renameRows.filterNot { it.dangling }
    val danglingCount = danglingRows.size

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "模型映射", onOpenDrawer = {}, onBack = onBack)

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))

            Row(verticalAlignment = Alignment.CenterVertically) {
                AgateBadge(text = "改名映射 ${userRows.size}", tone = BadgeTone.BLUE)
                if (danglingCount > 0) {
                    Spacer(Modifier.width(8.dp))
                    AgateBadge(text = "悬空 $danglingCount", tone = BadgeTone.AMBER)
                }
            }
            Spacer(Modifier.height(8.dp))
            Text(
                "alias 是对外名，可自由命名；模型名点一下即复制。开关与思考等级在改名行上就地可改；" +
                    "标了原因的行当前调不通，与开关未必相关。模型的启停与思考等级在「模型」页每行设置。",
                style = AgateType.Caption, color = C.textDim,
            )
            Spacer(Modifier.height(10.dp))

            if (danglingCount > 0) {
                AgateSegmented(
                    options = listOf("有效映射", "含失效 · $danglingCount"),
                    selected = if (showDangling) 1 else 0,
                    onSelect = { showDangling = it == 1 },
                )
                Spacer(Modifier.height(10.dp))
            }

            if (showAdd) {
                AddMappingCard(
                    models = models,
                    levels = levels,
                    busy = loading,
                    onCancel = { showAdd = false },
                    onSubmit = { alias, target, provider, reasoning ->
                        scope.launch {
                            val r = ModelOps.setMapping(
                                alias = alias, target = target, provider = provider,
                                reasoning = reasoning, reasoningExplicit = reasoning != null,
                            )
                            applyResult(r, "新增映射")
                            if (r.ok) showAdd = false
                        }
                    },
                )
                Spacer(Modifier.height(10.dp))
            } else {
                EntryCard(
                    icon = "＋", title = "新增映射",
                    sub = "alias → 上游模型 · 同名可多家主备",
                    onClick = { showAdd = true },
                )
                Spacer(Modifier.height(10.dp))
            }

            if (userRows.isEmpty()) {
                Spacer(Modifier.height(18.dp))
                Text(
                    "暂无改名映射 —— 建一条把多家同名模型串成主备",
                    style = AgateType.Caption, color = C.textDim,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
            userRows.forEach { row ->
                MappingRowCard(
                    row = row,
                    levels = levels,
                    onCopyAlias = { copy(row.alias, "对外名") },
                    onCopyTarget = { copy(row.target, "上游模型名") },
                    onToggle = { checked ->
                        scope.launch {
                            applyResult(
                                ModelOps.setMapping(
                                    alias = row.alias, target = row.target, provider = row.provider,
                                    enabled = checked,
                                ), "切换开关",
                            )
                        }
                    },
                    onEdit = { editing = row },
                    onExpandLevel = { expandedRow = if (expandedRow == row.key()) null else row.key() },
                    expanded = expandedRow == row.key(),
                    onPickLevel = { level ->
                        expandedRow = null
                        scope.launch {
                            applyResult(
                                ModelOps.setMapping(
                                    alias = row.alias, target = row.target, provider = row.provider,
                                    reasoning = level, reasoningExplicit = true,
                                ), "设置思考等级",
                            )
                        }
                    },
                    onDelete = { pendingDelete = Triple(row.alias, row.target, row.provider) },
                )
                Spacer(Modifier.height(8.dp))
            }

            Spacer(Modifier.height(12.dp))
            CustomModelCard(
                models = models,
                busy = loading,
                onAdd = { pv, mid ->
                    scope.launch { applyResult(ModelOps.addCustomModel(pv, mid), "登记自定义模型") }
                },
                onRemove = { pv, mid ->
                    scope.launch { applyResult(ModelOps.removeCustomModel(pv, mid), "移除自定义模型") }
                },
                onCopy = { copy(it, "模型 id") },
            )
            if (danglingCount > 0) {
                Spacer(Modifier.height(10.dp))
                Text(
                    "另有 $danglingCount 条失效映射（目标模型已不在该家清单里，转发会 404）；" +
                        "切到「含失效」可查看并删除",
                    style = AgateType.Caption,
                    color = C.textDim,
                )
            }
            Spacer(Modifier.height(28.dp))
        }
    }

    editing?.let { row ->
        EditMappingDialog(
            row = row,
            levels = levels,
            onDismiss = { editing = null },
            onCopyAlias = { copy(row.alias, "对外名") },
            onCopyTarget = { copy(row.target, "上游模型名") },
            onSave = { enabled, level ->
                scope.launch {
                    val r = ModelOps.setMapping(
                        alias = row.alias, target = row.target, provider = row.provider,
                        enabled = enabled, reasoning = level, reasoningExplicit = true,
                    )
                    if (r.ok) editing = null
                    applyResult(r, "保存映射")
                }
            },
        )
    }

    pendingDelete?.let { (alias, target, provider) ->
        AlertDialog(
            onDismissRequest = { pendingDelete = null },
            title = { Text("删除映射") },
            text = { Text("删除「$alias → $target」这条映射？删除后该别名会回落到默认绑定。", fontSize = 13.sp) },
            confirmButton = {
                TextButton(onClick = {
                    pendingDelete = null
                    scope.launch {
                        applyResult(ModelOps.removeMapping(alias, target, provider), "删除映射")
                    }
                }) { Text("删除") }
            },
            dismissButton = { TextButton(onClick = { pendingDelete = null }) { Text("取消") } },
        )
    }
}

private fun ModelOps.MappingRow.key(): String = "$alias|$target|${provider ?: ""}"

/** 诊断码 → 人话原因。未知码不显示具体原因（不猜）。 */
private fun issueText(issue: String?): String? = when (issue) {
    "mapping-off" -> "别名开关已关闭"
    "target-absent" -> "目标模型已不在清单"
    "target-off" -> "目标模型已被关闭"
    "shadowed" -> "别名被原生名抢占"
    "no-account" -> "该家暂无可用账号"
    else -> null
}

/* ── 一条改名映射行 ── */
@Composable
private fun MappingRowCard(
    row: ModelOps.MappingRow,
    levels: List<String>,
    expanded: Boolean,
    onCopyAlias: () -> Unit,
    onCopyTarget: () -> Unit,
    onToggle: (Boolean) -> Unit,
    onEdit: () -> Unit,
    onExpandLevel: () -> Unit,
    onPickLevel: (String) -> Unit,
    onDelete: () -> Unit,
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
            Column(Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        row.alias, style = AgateType.Body, color = C.textMain,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier.clickable(onClick = onCopyAlias),
                    )
                    Spacer(Modifier.width(4.dp))
                    Text("⧉", fontSize = 10.sp, color = C.textDim,
                        modifier = Modifier.clickable(onClick = onCopyAlias))
                }
                Spacer(Modifier.height(2.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("→ ", style = AgateType.Caption, color = C.textDim)
                    Text(
                        row.target, style = AgateType.BodyS, color = C.textSub,
                        modifier = Modifier.clickable(onClick = onCopyTarget),
                    )
                    Spacer(Modifier.width(4.dp))
                    Text("⧉", fontSize = 10.sp, color = C.textDim,
                        modifier = Modifier.clickable(onClick = onCopyTarget))
                }
                Spacer(Modifier.height(5.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    AgateBadge(text = row.provider ?: "全部承载家", tone = BadgeTone.GRAY)
                    val reason = issueText(row.issue)
                    if (!row.effective && reason != null) {
                        Spacer(Modifier.width(6.dp))
                        AgateBadge(text = reason, tone = BadgeTone.AMBER)
                    } else if (!row.effective) {
                        Spacer(Modifier.width(6.dp))
                        AgateBadge(text = "未生效", tone = BadgeTone.AMBER)
                    }
                }
            }
            Switch(checked = row.enabled, onCheckedChange = onToggle)
        }

        Spacer(Modifier.height(8.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("编辑", style = AgateType.BodyS, color = C.accent, fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clickable(onClick = onEdit)
                    .background(C.accent.copy(alpha = 0.10f), AgateShape.small)
                    .padding(horizontal = 10.dp, vertical = 4.dp))
            Spacer(Modifier.width(8.dp))
            Text("等级", style = AgateType.Caption, color = C.textDim)
            Spacer(Modifier.width(4.dp))
            Text(
                row.reasoning ?: "未绑定",
                style = AgateType.BodyS,
                color = if (row.reasoning != null) C.accent else C.textDim,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clickable(onClick = onExpandLevel)
                    .background(C.accent.copy(alpha = 0.10f), AgateShape.small)
                    .padding(horizontal = 10.dp, vertical = 4.dp),
            )
            Spacer(Modifier.weight(1f))
            Text(
                "删除",
                style = AgateType.BodyS, color = C.badgeRedText, fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clickable(onClick = onDelete)
                    .background(C.redBg, AgateShape.small)
                    .padding(horizontal = 10.dp, vertical = 4.dp),
            )
        }

        if (expanded) {
            Spacer(Modifier.height(8.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                Text("清除", style = AgateType.Caption, color = C.textSub,
                    modifier = Modifier
                        .clickable { onPickLevel("") }
                        .background(C.segBg, AgateShape.small)
                        .padding(horizontal = 8.dp, vertical = 5.dp))
                levels.forEach { level ->
                    Text(
                        level,
                        style = AgateType.Caption,
                        color = if (level == row.reasoning) C.accent else C.textSub,
                        fontWeight = if (level == row.reasoning) FontWeight.Bold else FontWeight.Normal,
                        modifier = Modifier
                            .clickable { onPickLevel(level) }
                            .background(
                                if (level == row.reasoning) C.accent.copy(alpha = 0.14f) else C.segBg,
                                AgateShape.small,
                            )
                            .padding(horizontal = 8.dp, vertical = 5.dp),
                    )
                }
            }
        }
    }
}

/* ── 编辑映射弹窗：身份只读 + 复制，开关/思考可改 ── */
@Composable
private fun EditMappingDialog(
    row: ModelOps.MappingRow,
    levels: List<String>,
    onDismiss: () -> Unit,
    onCopyAlias: () -> Unit,
    onCopyTarget: () -> Unit,
    onSave: (enabled: Boolean, level: String?) -> Unit,
) {
    val C = agateColors()
    var enabled by remember { mutableStateOf(row.enabled) }
    var level by remember { mutableStateOf(row.reasoning) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("编辑映射", fontSize = 15.sp) },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                Text("对外名（不可改，点可复制）", style = AgateType.Caption, color = C.textDim)
                Text(row.alias, style = AgateType.Body, color = C.textMain, fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.clickable(onClick = onCopyAlias).padding(vertical = 4.dp))
                Spacer(Modifier.height(6.dp))
                Text("上游模型（不可改，点可复制）", style = AgateType.Caption, color = C.textDim)
                Text(row.target, style = AgateType.Body, color = C.textMain,
                    modifier = Modifier.clickable(onClick = onCopyTarget).padding(vertical = 4.dp))
                Spacer(Modifier.height(10.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("启用", style = AgateType.Body, color = C.textMain, modifier = Modifier.weight(1f))
                    Switch(checked = enabled, onCheckedChange = { enabled = it })
                }
                Spacer(Modifier.height(10.dp))
                Text("思考等级（这条别名的档位）", style = AgateType.Caption, color = C.textDim)
                Spacer(Modifier.height(4.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text("清除", style = AgateType.Caption,
                        color = if (level == null) C.onAccent else C.textSub,
                        modifier = Modifier
                            .clickable { level = null }
                            .background(if (level == null) C.accent else C.segBg, AgateShape.small)
                            .padding(horizontal = 8.dp, vertical = 5.dp))
                    levels.forEach { lv ->
                        Text(lv, style = AgateType.Caption,
                            color = if (lv == level) C.onAccent else C.textSub,
                            modifier = Modifier
                                .clickable { level = lv }
                                .background(if (lv == level) C.accent else C.segBg, AgateShape.small)
                                .padding(horizontal = 8.dp, vertical = 5.dp))
                    }
                }
                if (levels.isEmpty()) {
                    Text("该环境未声明思考档位", style = AgateType.Caption, color = C.textDim)
                }
            }
        },
        confirmButton = {
            TextButton(onClick = { onSave(enabled, level) }) { Text("保存") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("取消") } },
    )
}

/**
 * 空查询时每家最多列几条候选模型。
 *
 * 为什么是「每家」而不是「总共」：总量上限会让第一家（manage 按家顺序给出）吃掉
 * 整个清单。这个数只要够看清一家有哪些模型即可，用户真在找的时候就打字搜
 * （那时上限是 80）。
 */
private const val PER_PROVIDER_IN_PICKER = 8

/* ── 新增映射卡 ── */
@Composable
private fun AddMappingCard(
    models: List<JSONObject>,
    levels: List<String>,
    busy: Boolean,
    onCancel: () -> Unit,
    onSubmit: (alias: String, target: String, provider: String?, reasoning: String?) -> Unit,
) {
    val C = agateColors()
    var alias by remember { mutableStateOf("") }
    var query by remember { mutableStateOf("") }
    var picked by remember { mutableStateOf<JSONObject?>(null) }
    var picking by remember { mutableStateOf(false) }
    var level by remember { mutableStateOf<String?>(null) }

    // ── 候选清单：按「家」分配名额（修「只看得到一家」）──────────────
    // 旧写法 `hit.take(40)`：models 由 manage 按家顺序给出，第一家就能占满 40 条，
    // 后面所有家被整段截掉 —— 不打字搜索时永远看不到别家。现在空查询时每家各出
    // 至多 8 条（家在 manage 里是连续段，按家分组取样），保证任何一家都露头；
    // 有查询时那是用户在找具体模型，按匹配取 80 条。
    val filtered = remember(query, models) {
        val q = query.trim()
        if (q.isNotEmpty()) {
            models.filter {
                it.optString("id").contains(q, ignoreCase = true) ||
                    it.optString("providerLabel").contains(q, ignoreCase = true)
            }.take(80)
        } else {
            // 按家分组（保持 manage 里各家的原始顺序），每家取前 PER_PROVIDER 条
            val perProvider = linkedMapOf<String, MutableList<JSONObject>>()
            models.forEach { m ->
                val owner = m.optString("provider")
                perProvider.getOrPut(owner) { ArrayList() }.add(m)
            }
            perProvider.values.flatMap { it.take(PER_PROVIDER_IN_PICKER) }
        }
    }

    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(14.dp),
    ) {
        Text("新增映射", style = AgateType.SectionTitle, color = C.textMain)
        Spacer(Modifier.height(10.dp))

        AgateFormInput(value = alias, onValueChange = { alias = it }, hint = "映射名（对外 alias，如 my-gpt）")
        Spacer(Modifier.height(8.dp))

        if (picked == null || picking) {
            AgateSearchBar(value = query, onValueChange = { query = it }, hint = "搜索目标模型 id / 家")
            Spacer(Modifier.height(6.dp))
            Column {
                filtered.forEach { m ->
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable {
                                picked = m; picking = false; query = ""
                            }
                            .background(if (picked === m) C.accent.copy(alpha = 0.08f) else C.bg.copy(alpha = 0f), AgateShape.small)
                            .padding(horizontal = 8.dp, vertical = 7.dp),
                    ) {
                        Text(
                            m.optString("id"),
                            style = AgateType.BodyS, color = C.textMain,
                            modifier = Modifier.weight(1f),
                        )
                        Text(m.optString("providerLabel"), style = AgateType.Caption, color = C.textDim)
                    }
                }
                if (filtered.isEmpty()) {
                    Text(
                        if (models.isEmpty()) "清单为空（先到「模型」页刷新目录）" else "无匹配模型",
                        style = AgateType.Caption, color = C.textDim,
                    )
                }
            }
        } else {
            val m = picked
            if (m != null) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { picking = true }
                        .background(C.segBg, AgateShape.small)
                        .padding(horizontal = 10.dp, vertical = 9.dp),
                ) {
                    Text(
                        "${m.optString("id")} · ${m.optString("providerLabel")}",
                        style = AgateType.BodyS, color = C.textMain,
                        modifier = Modifier.weight(1f),
                    )
                    Text("更换 ❯", style = AgateType.Caption, color = C.accent)
                }
            }
        }

        Spacer(Modifier.height(10.dp))
        Text("思考等级（可选，留空 = 不绑定）", style = AgateType.Caption, color = C.textDim)
        Spacer(Modifier.height(6.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            levels.forEach { lv ->
                Text(
                    lv,
                    style = AgateType.Caption,
                    color = if (lv == level) C.accent else C.textSub,
                    fontWeight = if (lv == level) FontWeight.Bold else FontWeight.Normal,
                    modifier = Modifier
                        .clickable { level = if (lv == level) null else lv }
                        .background(
                            if (lv == level) C.accent.copy(alpha = 0.14f) else C.segBg,
                            AgateShape.small,
                        )
                        .padding(horizontal = 8.dp, vertical = 5.dp),
                )
            }
        }

        Spacer(Modifier.height(12.dp))
        Row {
            Text(
                "取消", style = AgateType.BodyS, color = C.textSub,
                modifier = Modifier
                    .clickable(onClick = onCancel)
                    .background(C.segBg, AgateShape.small)
                    .padding(horizontal = 14.dp, vertical = 7.dp),
            )
            Spacer(Modifier.weight(1f))
            Text(
                if (busy) "保存中…" else "保存",
                style = AgateType.BodyS,
                color = if (busy || alias.isBlank() || picked == null) C.textDim else C.accent,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clickable(enabled = !busy && alias.isNotBlank() && picked != null) {
                        val m = picked ?: return@clickable
                        onSubmit(
                            alias.trim(),
                            m.optString("id"),
                            m.optString("provider").ifBlank { null },
                            level,
                        )
                    }
                    .background(C.accent.copy(alpha = 0.12f), AgateShape.small)
                    .padding(horizontal = 16.dp, vertical = 7.dp),
            )
        }
    }
}

/* ── 自定义模型登记（POST /api/models/custom · custom/remove）── */
@Composable
private fun CustomModelCard(
    models: List<JSONObject>,
    busy: Boolean,
    onAdd: (String, String) -> Unit,
    onRemove: (String, String) -> Unit,
    onCopy: (String) -> Unit,
) {
    val C = agateColors()
    var open by remember { mutableStateOf(false) }
    var query by remember { mutableStateOf("") }
    var pickedP by remember { mutableStateOf<JSONObject?>(null) }
    var picking by remember { mutableStateOf(false) }
    var modelId by remember { mutableStateOf("") }

    val manual = models.filter { it.optString("source") == "manual" }
    val providerRows = remember(models) {
        val seen = LinkedHashMap<String, JSONObject>()
        models.forEach { m ->
            val pv = m.optString("provider")
            if (pv.isNotBlank() && !seen.containsKey(pv)) seen[pv] = m
        }
        seen.values.toList()
    }
    val filteredP = remember(query, providerRows) {
        val q = query.trim()
        (if (q.isEmpty()) providerRows else providerRows.filter {
            it.optString("provider").contains(q, ignoreCase = true) ||
                it.optString("providerLabel").contains(q, ignoreCase = true)
        }).take(30)
    }

    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(14.dp),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth().clickable { open = !open },
        ) {
            Column(Modifier.weight(1f)) {
                Text("自定义模型", style = AgateType.SectionTitle, color = C.textMain)
                Text(
                    "登记目录里没有的模型 id（登记后即可建映射）· 已登记 ${manual.size}",
                    style = AgateType.Caption, color = C.textDim,
                )
            }
            Text(if (open) "▾" else "▸", color = C.textDim, fontSize = 13.sp)
        }

        if (open) {
            Spacer(Modifier.height(10.dp))

            if (pickedP == null || picking) {
                AgateSearchBar(value = query, onValueChange = { query = it }, hint = "搜索提供商")
                Spacer(Modifier.height(6.dp))
                filteredP.forEach { m ->
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable { pickedP = m; picking = false; query = "" }
                            .background(
                                if (pickedP === m) C.accent.copy(alpha = 0.08f) else C.bg.copy(alpha = 0f),
                                AgateShape.small,
                            )
                            .padding(horizontal = 8.dp, vertical = 7.dp),
                    ) {
                        Text(
                            m.optString("providerLabel"), style = AgateType.BodyS, color = C.textMain,
                            modifier = Modifier.weight(1f),
                        )
                        Text(m.optString("provider"), style = AgateType.Caption, color = C.textDim)
                    }
                }
            } else {
                val m = pickedP
                if (m != null) {
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable { picking = true }
                            .background(C.segBg, AgateShape.small)
                            .padding(horizontal = 10.dp, vertical = 9.dp),
                    ) {
                        Text(
                            "${m.optString("providerLabel")} · ${m.optString("provider")}",
                            style = AgateType.BodyS, color = C.textMain,
                            modifier = Modifier.weight(1f),
                        )
                        Text("更换 ❯", style = AgateType.Caption, color = C.accent)
                    }
                }
            }
            Spacer(Modifier.height(8.dp))
            AgateFormInput(value = modelId, onValueChange = { modelId = it }, hint = "模型 id（上游目录里没有的）")

            Spacer(Modifier.height(10.dp))
            Row {
                Spacer(Modifier.weight(1f))
                val canAdd = !busy && pickedP != null && modelId.isNotBlank()
                Text(
                    if (busy) "提交中…" else "登记",
                    style = AgateType.BodyS,
                    color = if (canAdd) C.accent else C.textDim,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .clickable(enabled = canAdd) {
                            val pv = pickedP?.optString("provider") ?: return@clickable
                            val mid = modelId.trim()
                            onAdd(pv, mid)
                            modelId = ""
                        }
                        .background(C.accent.copy(alpha = 0.12f), AgateShape.small)
                        .padding(horizontal = 16.dp, vertical = 7.dp),
                )
            }

            if (manual.isNotEmpty()) {
                Spacer(Modifier.height(10.dp))
                manual.forEach { m ->
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
                    ) {
                        Text(
                            m.optString("id"), style = AgateType.BodyS, color = C.textMain,
                            modifier = Modifier.weight(1f).clickable { onCopy(m.optString("id")) },
                        )
                        Text(m.optString("providerLabel"), style = AgateType.Caption, color = C.textDim)
                        Spacer(Modifier.width(8.dp))
                        Text(
                            "移除",
                            style = AgateType.Caption, color = C.badgeRedText, fontWeight = FontWeight.SemiBold,
                            modifier = Modifier
                                .clickable(enabled = !busy) {
                                    onRemove(m.optString("provider"), m.optString("id"))
                                }
                                .background(C.redBg, AgateShape.small)
                                .padding(horizontal = 8.dp, vertical = 4.dp),
                        )
                    }
                }
            }
        }
    }
}
