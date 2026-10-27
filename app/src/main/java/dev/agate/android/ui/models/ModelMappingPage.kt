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
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
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
import dev.agate.android.data.GatewayRepository
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
 * 模型映射（M3 · MODEL_MAPPING 子页）。
 *
 * 数据源 GET /api/models/manage：mappings[] 拆「用户映射」（isDefault=false，
 * 可删可改）与「默认绑定」（isDefault=true，视图合成的 alias=target 行，只展示
 * 与改思考等级，不提供删除——内核里它不是一条独立实体）。新增 / 改等级 / 切
 * 开关都走 POST /api/models/mappings（同三元组 = 命中已有条目），写接口返回
 * 最新 manage 视图就地替换。reasoning 候选表用响应里随发的 reasoningLevels
 * （内核 model_rules::REASONING_LEVELS，前端不自己抄一份避免漂移）。
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
    var tab by remember { mutableIntStateOf(0) } // 0 用户映射 / 1 默认绑定
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
    val allUserRows = mappings.filter { !it.isDefault }
    val danglingRows = allUserRows.filter { it.dangling }
    val userRows = if (showDangling) allUserRows else allUserRows.filterNot { it.dangling }

    val allDefaultRows = mappings.filter { it.isDefault }

    // ── 内置映射默认只显示「有账号的家」──────────────────────────
    // 没登录的提供商的默认绑定点了也用不了（`/v1/models` 根本不会广告它，
    // 内核 `active_manifests` 也是同一口径）。把它们全列出来会让这一页被
    // 20 多家的空绑定淹没 —— 这正是「太混乱」的来源。
    // 仍保留「全部」切换：配映射之前想看看某家有哪些模型名是合理需求。
    val snap by GatewayRepository.snapshot.collectAsState()
    var showAllProviders by remember { mutableStateOf(false) }
    val activeProviders = remember(snap.providerCounts) {
        snap.providerCounts.filterValues { it > 0 }.keys
    }
    // ── 默认绑定：只显示「**真配了东西**」的那些 ─────────────────
    // 每条清单模型都天然带一条 `alias == target` 的默认绑定，而绝大多数是
    // 「启用 + 没设思考等级 + 没挂别名」—— 那是**没有配置**，不是映射。
    // 全列出来就是用户说的「全部显示内置默认映射」：40 个模型 × N 家 = 一屏
    // 全是同名的 alias→target，真正配过的几条反而找不到。
    //
    // 所以默认只留「有信息量」的三类：
    //   · 被关掉的（`enabled == false`）—— 用户显式做的决定；
    //   · 设了思考等级的（`reasoning != null`）；
    //   · 挂了用户别名的（同一 target 另有 alias≠target 的映射）。
    var showPlainDefaults by remember { mutableStateOf(false) }
    val aliasedTargets = remember(mappings) {
        mappings.filter { !it.isDefault && it.alias != it.target }
            .map { (it.provider ?: "") to it.target }
            .toSet()
    }
    val providerFiltered = if (showAllProviders || activeProviders.isEmpty()) {
        allDefaultRows
    } else {
        allDefaultRows.filter { it.provider.isNullOrBlank() || it.provider in activeProviders }
    }
    val defaultRows = if (showPlainDefaults) {
        providerFiltered
    } else {
        // 「有信息量」与「有账号的家」两个条件同时成立才算要显示
        providerFiltered.filter { row ->
            !row.enabled || row.reasoning != null ||
                aliasedTargets.contains((row.provider ?: "") to row.target)
        }
    }
    val hiddenCount = allDefaultRows.size - defaultRows.size
    val hiddenPlainCount = providerFiltered.size - defaultRows.size
    val rows = if (tab == 0) userRows else defaultRows
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
                AgateBadge(text = "用户映射 ${userRows.size}", tone = dev.agate.android.ui.components.BadgeTone.BLUE)
                Spacer(Modifier.width(8.dp))
                AgateBadge(text = "默认绑定 ${defaultRows.size}")
                if (danglingCount > 0) {
                    Spacer(Modifier.width(8.dp))
                    AgateBadge(text = "悬空 $danglingCount", tone = dev.agate.android.ui.components.BadgeTone.AMBER)
                }
            }
            Spacer(Modifier.height(8.dp))
            Text(
                "alias 是对外名，可自由命名；路由时同名模型的原生映射优先，用户映射是追加的主备路",
                style = AgateType.Caption, color = C.textDim,
            )
            Spacer(Modifier.height(10.dp))

            // ── 视图切换 + 新增入口 ──
            AgateSegmented(
                options = listOf("用户映射", "默认绑定"),
                selected = tab,
                onSelect = { tab = it },
            )
            // 用户映射页：有失效条目时给一个开关（默认隐藏，见上面 userRows 的说明）
            if (tab == 0 && danglingCount > 0) {
                Spacer(Modifier.height(8.dp))
                AgateSegmented(
                    options = listOf("有效映射", "含失效 · $danglingCount"),
                    selected = if (showDangling) 1 else 0,
                    onSelect = { showDangling = it == 1 },
                )
            }
            // 默认绑定页：只显示有账号的家 / 全部（默认前者，见上面 defaultRows 的说明）
            if (tab == 1 && activeProviders.isNotEmpty()) {
                Spacer(Modifier.height(8.dp))
                AgateSegmented(
                    options = listOf("仅有账号", "全部"),
                    selected = if (showAllProviders) 1 else 0,
                    onSelect = { showAllProviders = it == 1 },
                )
            }
            // 默认绑定页：默认只看「有配置」的（关掉 / 设了思考等级 / 挂了别名），
            // 否则一屏全是 alias==target 的同名行，真正配过的反而找不到
            if (tab == 1 && hiddenPlainCount > 0) {
                Spacer(Modifier.height(8.dp))
                AgateSegmented(
                    options = listOf("仅有配置", "含未配置 · $hiddenPlainCount"),
                    selected = if (showPlainDefaults) 1 else 0,
                    onSelect = { showPlainDefaults = it == 1 },
                )
            }
            Spacer(Modifier.height(10.dp))

            if (tab == 0) {
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
            }

            // ── 映射行列表 ──
            if (rows.isEmpty()) {
                Spacer(Modifier.height(18.dp))
                Text(
                    if (tab == 0) "暂无用户映射 —— 建一条把多家同名模型串成主备"
                    else "尚无模型清单（先到「模型」页刷新目录）",
                    style = AgateType.Caption, color = C.textDim,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
            rows.forEach { row ->
                MappingRowCard(
                    row = row,
                    levels = levels,
                    expanded = expandedRow == row.key(),
                    allowDelete = tab == 0,
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
            // 用户映射页：失效条数说明白（它们被隐藏了，不是被删了）
            if (tab == 0 && danglingCount > 0) {
                Spacer(Modifier.height(10.dp))
                Text(
                    "另有 $danglingCount 条失效映射（目标模型已不在该家清单里，转发会 404）；" +
                        "切到「含失效」可查看并删除",
                    style = AgateType.Caption,
                    color = C.textDim,
                )
            }
            // 被隐藏的条数要说明白：不能让用户以为「我的绑定丢了」
            if (tab == 1 && hiddenCount > 0) {
                Spacer(Modifier.height(10.dp))
                Text(
                    "另有 $hiddenCount 条默认绑定属于未登录的提供商（没账号，客户端也不会看到）；" +
                        "切到「全部」可查看",
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

/* ── 一条映射行 ── */
@Composable
private fun MappingRowCard(
    row: ModelOps.MappingRow,
    levels: List<String>,
    expanded: Boolean,
    allowDelete: Boolean,
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
            if (allowDelete) {
                Text(
                    "删除",
                    style = AgateType.BodyS, color = C.badgeRedText, fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .clickable(onClick = onDelete)
                        .background(C.redBg, AgateShape.small)
                        .padding(horizontal = 10.dp, vertical = 4.dp),
                )
            }
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

    val filtered = remember(query, models) {
        val q = query.trim()
        val hit = if (q.isEmpty()) models else models.filter {
            it.optString("id").contains(q, ignoreCase = true) ||
                it.optString("providerLabel").contains(q, ignoreCase = true)
        }
        hit.take(40)
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
