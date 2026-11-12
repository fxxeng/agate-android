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
import androidx.compose.ui.platform.LocalContext
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
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject

/**
 * 模型映射（M3 · MODEL_MAPPING 子页）—— **只做「改名」这一件事**。
 *
 * 一条映射 = 「对外名 → 上游模型（家 × 真名）」，同名可多家主备。数据源
 * GET /api/models/manage 的 `mappings[]`；新增 / 改思考等级 / 切开关都走
 * POST /api/models/mappings（同三元组 = 命中已有条目），写接口返回最新 manage
 * 视图就地替换。reasoning 候选表用响应里随发的 reasoningLevels
 * （内核 model_rules::REASONING_LEVELS，前端不自己抄一份避免漂移）。
 *
 * ── 这里刻意**不再**出现「默认绑定」（2026-11-12）────────────────
 * `alias == target` 的条目不是改名，它是「这个模型自己的状态」（启停 + 思考
 * 等级）的存储形态 —— 内核把模型状态也存在 `mappings` 里，用同名表示「没改名」。
 * 把它摆在映射页，用户会以为那是一条改名规则（真机反馈的原话）。现在：
 *   · 列表只认 `alias != target`（连点开关后落盘的自映射也一起挡掉）；
 *   · 模型的启停与思考等级搬到「模型」页每一行（`ModelLine` 的「思考」入口），
 *     那里才是「模型属性」该在的地方。
 * 内核侧存储与判据不变（`default_enabled` 继续读那条自映射），只是不再把它
 * 当成映射展示。
 */
@Composable
fun ModelMappingPage(
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    var view by remember { mutableStateOf<JSONObject?>(null) }
    var loading by remember { mutableStateOf(false) }
    var showAdd by remember { mutableStateOf(false) }
    // 行内等级选择展开态：alias|target|provider 标识哪一行
    var expandedRow by remember { mutableStateOf<String?>(null) }
    // 删除映射的待确认项（误删一条映射要重新配一遍，值得一次确认）
    var pendingDelete by remember { mutableStateOf<Triple<String, String, String?>?>(null) }

    fun toast(ok: Boolean, what: String, error: String?) {
        Toast.makeText(ctx, if (ok) "${what}成功" else "${what}失败：${error ?: "未知"}", Toast.LENGTH_SHORT).show()
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

    // ── 用户映射：默认隐藏「失效」的（目标模型已不存在）──────────
    // `dangling` 由内核按「该家当前清单里有没有这个 target」算出。目标都没了，
    // 这条映射转发时必然 404 —— 留在首屏除了制造噪音没有别的用。
    // 但仍要能看见与清理：底部给一行「另有 N 条失效」，点开才铺开。
    var showDangling by remember { mutableStateOf(false) }
    // ── 只认「真的改了名」的映射（alias ≠ target）────────────────
    // `alias == target` 是「模型默认值」（启停 + 思考等级）的存储形态：内核把
    // 模型状态也存在 `mappings` 里、用同名表示「没改名」。这种条目不会改任何
    // 名字，列在这里只会让人误以为映射页里混着改名规则 —— 而模型的启停与思考
    // 等级已经搬去「模型」页每一行上设置（2026-11-12，用户反馈「默认绑定被
    // 误以为映射改名」）。这里一并挡住它两种来源：`isDefault` 的视图合成行，
    // 以及点过开关后落盘的 `isDefault=false` 自映射。
    val allUserRows = mappings.filter { !it.isDefault && it.alias != it.target }
    val danglingRows = allUserRows.filter { it.dangling }
    val userRows = if (showDangling) allUserRows else allUserRows.filterNot { it.dangling }
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

            // ── 统计行 ──
            Row(verticalAlignment = Alignment.CenterVertically) {
                AgateBadge(text = "映射 ${userRows.size}", tone = dev.agate.android.ui.components.BadgeTone.BLUE)
                if (danglingCount > 0) {
                    Spacer(Modifier.width(8.dp))
                    AgateBadge(text = "悬空 $danglingCount", tone = dev.agate.android.ui.components.BadgeTone.AMBER)
                }
            }
            Spacer(Modifier.height(8.dp))
            // 这一页只做一件事：改名。模型的启停与思考等级已经搬去「模型」页
            // 每一行（2026-11-12）—— 那本来就是模型自己的属性，不是映射关系。
            Text(
                "改名：对外名 → 上游模型。同名可多家主备，路由时原生模型优先",
                style = AgateType.Caption, color = C.textDim,
            )
            Spacer(Modifier.height(4.dp))
            Text(
                "模型自身的启停与思考等级在「模型」页每行设置，不在这页",
                style = AgateType.Caption, color = C.textDim,
            )
            Spacer(Modifier.height(10.dp))

            // 有失效条目时给一个开关（默认隐藏，见上面 userRows 的说明）
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

            // ── 映射行列表 ──
            if (userRows.isEmpty()) {
                Spacer(Modifier.height(18.dp))
                Text(
                    "暂无映射 —— 建一条把多家同名模型串成主备",
                    style = AgateType.Caption, color = C.textDim,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
            userRows.forEach { row ->
                MappingRowCard(
                    row = row,
                    levels = levels,
                    expanded = expandedRow == row.key(),
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
                    onExpandLevel = {
                        expandedRow = if (expandedRow == row.key()) null else row.key()
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
            )
            // 失效条数说明白（它们被隐藏了，不是被删了）
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

    pendingDelete?.let { (alias, target, provider) ->
        AlertDialog(
            onDismissRequest = { pendingDelete = null },
            title = { Text("删除映射") },
            text = { Text("删除「$alias → $target」这条映射？删除后这个名字不再对外提供（上游模型的原名照旧可用）。", fontSize = 13.sp) },
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

/* ── 一条映射行 ── */
@Composable
private fun MappingRowCard(
    row: ModelOps.MappingRow,
    levels: List<String>,
    expanded: Boolean,
    onToggle: (Boolean) -> Unit,
    onPickLevel: (String) -> Unit,
    onExpandLevel: () -> Unit,
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
                Text(
                    "${row.alias}  →  ${row.target}",
                    style = AgateType.Body, color = C.textMain, fontWeight = FontWeight.SemiBold,
                )
                Spacer(Modifier.height(3.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    AgateBadge(text = row.provider ?: "全部承载家", tone = dev.agate.android.ui.components.BadgeTone.GRAY)
                    if (row.dangling) {
                        Spacer(Modifier.width(6.dp))
                        AgateBadge(text = "目标已不存在", tone = dev.agate.android.ui.components.BadgeTone.AMBER)
                    }
                }
            }
            Switch(checked = row.enabled, onCheckedChange = onToggle)
        }

        Spacer(Modifier.height(8.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                "思考等级", style = AgateType.Caption, color = C.textDim,
                modifier = Modifier.width(58.dp),
            )
            // 等级 chip：点击展开候选行；空 = 未绑定（跟随对话请求）
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
            // 8 档 + 清除；等级表随 manage 响应下发（内核事实来源）
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(
                    "清除",
                    style = AgateType.Caption, color = C.textSub,
                    modifier = Modifier
                        .clickable { onPickLevel("") }
                        .background(C.segBg, AgateShape.small)
                        .padding(horizontal = 8.dp, vertical = 5.dp),
                )
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

/**
 * 空查询时每家最多列几条候选模型。
 *
 * 为什么是「每家」而不是「总共」：见 [AddMappingCard] 里 `filtered` 的说明 ——
 * 总量上限会让第一家吃掉整个清单。这个数只要够看清一家有哪些模型即可，
 * 用户真在找的时候就打字搜（那时上限是 80）。
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

    // ── 候选清单：按「家」分配名额（2026-11-01 修「只看得到一家」）──────
    // 旧写法是 `hit.take(40)`：`models` 由 manage 按家顺序给出，**第一家
    // （WorkBuddy）的模型就能占满 40 条**，后面所有家被整段截掉 ——
    // 不打字搜索时永远看不到别家。这不是「太多了截一下」，是**清单被一家吃掉**。
    //
    // 现在：空查询时每家各出至多 8 条（家在 manage 里是连续段，按家轮转取样），
    // 保证任何一家都至少露头；有查询时那是用户在找具体模型，按匹配取 80 条。
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

        // 目标模型选择：点开搜索列表（收起时显示已选）
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

            // 登记表单：家选择 + 模型 id
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

            // 已登记的自定义模型
            if (manual.isNotEmpty()) {
                Spacer(Modifier.height(10.dp))
                manual.forEach { m ->
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
                    ) {
                        Text(
                            m.optString("id"), style = AgateType.BodyS, color = C.textMain,
                            modifier = Modifier.weight(1f),
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
