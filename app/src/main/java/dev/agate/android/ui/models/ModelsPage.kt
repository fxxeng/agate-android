package dev.agate.android.ui.models

import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import android.widget.Toast
import kotlinx.coroutines.launch
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.ModelOps
import dev.agate.android.data.PresetRegistry
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateProviderAvatar
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.components.AgateAvatar
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateHeaderAction
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.AgateSearchBar
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors

/**
 * 一次目录刷新里**某一家的结论**（`/api/models/refresh` 的 `results[]`）。
 *
 * 状态取值是前后端契约（见内核 `catalog_refresh`）：`refreshed` / `skipped` /
 * `failed`。卡片据此分三档显示 —— 失败的要能一眼看到并重试，跳过的要说明
 * 「这家用固定清单（重试也没用）」，成功的只是在汇总里计数。
 */
internal data class RefreshRow(
    val provider: String,
    val label: String,
    val status: String,
    val count: Int,
    val message: String?,
    /** 这一家上次拉取成功的时刻（毫秒；0 = 未知）。 */
    val refreshedAt: Long = 0L,
) {
    val failed: Boolean get() = status == "failed"
    val skipped: Boolean get() = status == "skipped"

    companion object {
        fun parseAll(data: org.json.JSONObject?): List<RefreshRow> {
            val arr = data?.optJSONArray("results") ?: return emptyList()
            val out = ArrayList<RefreshRow>(arr.length())
            for (i in 0 until arr.length()) {
                val o = arr.optJSONObject(i) ?: continue
                val pid = o.optString("provider")
                if (pid.isBlank()) continue
                out += RefreshRow(
                    provider = pid,
                    label = o.optString("providerLabel").ifBlank { pid },
                    status = o.optString("status").ifBlank { "refreshed" },
                    count = o.optInt("count", 0),
                    message = o.optString("message").ifBlank { null },
                    refreshedAt = o.optLong("refreshedAt", 0L),
                )
            }
            return out
        }
    }
}

/**
 * 客户端所见清单的**状态 + 推演计数**（来自 `/api/models/list-view`）。
 *
 * ── 为什么三个计数一次全要 ──────────────────────────────────
 * 界面要显示「切到别的模式会变成多少」。三种形态的项数由内核一次算全，
 * 切换时数字**即时**变化 —— 不必再往返一次（也不会出现「切过去先显示旧
 * 数字、等下才刷新」的闪动）。
 *
 * ── 计数与 `/v1/models` 同源 ────────────────────────────────
 * 内核两条路走**同一个函数**（`models_response_with_mode`），所以「预览说 42」
 * 就等于「客户端真会收到 42」。这是这张卡存在的全部意义 —— 数字对不上会比
 * 不预览更糟。
 */
internal data class ListView(
    val mode: String = "model",
    val pins: List<String> = emptyList(),
    val counts: Map<String, Int> = emptyMap(),
    /** 当前形态下的**完整**清单（界面自己截断前 N 项，见 XpCard）。 */
    val ids: List<String> = emptyList(),
    /** 提供商 `(id, label)`，混合模式的 pin 选择用。 */
    val providers: List<Pair<String, String>> = emptyList(),
) {
    fun countFor(m: String): Int = counts[m] ?: if (m == mode) ids.size else 0

    /** 副行拆解（v44 的「各家合计 N 条，同名合并后 M 个」那一行）。 */
    fun breakdown(): String? {
        val all = counts["model"] ?: return null
        val provider = counts["provider"] ?: return null
        val hybrid = counts["hybrid"] ?: return null
        return when (mode) {
            "provider" -> {
                val dup = (provider - all).coerceAtLeast(0)
                "各家各列一遍，共 $provider 项（其中同名模型重复 $dup 条）"
            }
            "hybrid" -> {
                val prefixed = (hybrid - all).coerceAtLeast(0)
                "合并后 $all 项 + 带前缀 $prefixed 项"
            }
            else -> {
                // 「各家合计」只有 provider 模式的全量能近似表达（含重复的条数）
                "同名合并后 $all 项（各家明细合计 $provider 条）"
            }
        }
    }

    companion object {
        fun parse(o: org.json.JSONObject?): ListView {
            if (o == null) return ListView()
            val countsObj = o.optJSONObject("counts")
            val counts = HashMap<String, Int>()
            countsObj?.keys()?.forEach { k -> counts[k] = countsObj.optInt(k, 0) }
            val pins = ArrayList<String>()
            o.optJSONArray("pins")?.let { arr ->
                for (i in 0 until arr.length()) arr.optString(i).takeIf { it.isNotBlank() }?.let { pins += it }
            }
            val ids = ArrayList<String>()
            o.optJSONArray("ids")?.let { arr ->
                for (i in 0 until arr.length()) arr.optString(i).takeIf { it.isNotBlank() }?.let { ids += it }
            }
            return ListView(
                mode = o.optString("mode").ifBlank { "model" },
                pins = pins,
                counts = counts,
                ids = ids,
            )
        }
    }
}

/**
 * 模型页（M3-5，对齐 v44 Tab 3）：
 * - 客户端所见清单卡（GET /v1/models 口径）：总数 + 两档视图（按模型聚合 / 按提供商分组）
 * - 搜索 + 提供商分组折叠（mgroups）或按模型聚合（提供商候选列表 + 倍率）
 * - 自定义模型登记 / 模型目录刷新入口
 *
 * 数据源：/api/session.models（与 /v1/models 同源）；倍率由 credits 串解析。
 * v44 的「暴露模式三档（按模型/按提供商/混合）+ pinned 展开」内核暂无对应端点，
 * 界面按两档视图呈现（诚实标注，不做假开关）。
 */
@Composable
fun ModelsPage(
    onOpenDrawer: () -> Unit = {},
    onOpenSpeedtest: () -> Unit = {},
    onOpenCaps: () -> Unit = {},
    onOpenMapping: () -> Unit = {},
    onOpenCatalog: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val snap by GatewayRepository.snapshot.collectAsState()
    val models = snap.models

    DisposableEffect(Unit) {
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
        ModelOps.configure(GatewayPrefs.apiKey(ctx))
        onDispose { }
    }

    var view by remember { mutableIntStateOf(1) }   // 0 按模型聚合 / 1 按提供商分组（默认分组）
    var query by remember { mutableStateOf("") }
    var refreshing by remember { mutableStateOf(false) }
    // 最近一次刷新的逐家结果（v44：卡片要能直接看到「哪家失败」，不必进子页）
    var refreshRows by remember { mutableStateOf<List<RefreshRow>>(emptyList()) }
    var refreshAt by remember { mutableStateOf(0L) }
    var showPull by remember { mutableStateOf(false) }
    // 客户端所见清单的状态 + 推演计数（来自 /api/models/list-view）
    var listView by remember { mutableStateOf(ListView()) }
    var configuring by remember { mutableStateOf(false) }

    // 提供商 (id, label) 列表：混合模式的 pin 选择要用（模型行里带 providerLabel）
    val providers = remember(models) {
        models.map { it.provider to it.providerLabel }
            .filter { it.first.isNotBlank() }
            .distinctBy { it.first }
            .sortedBy { it.second }
    }

    fun loadListView() {
        scope.launch { listView = ListView.parse(ModelOps.fetchListView().data) }
    }

    /** 切模式 / 改 pin：一起提交（两者表达同一件事，见内核 set_models_list 的说明）。 */
    fun saveListView(mode: String, pins: List<String>) {
        if (configuring) return
        configuring = true
        scope.launch {
            val r = ModelOps.setListView(mode, pins)
            configuring = false
            val parsed = ListView.parse(r.data)
            if (r.ok) {
                listView = parsed.copy(providers = providers)
            } else {
                Toast.makeText(ctx, r.error ?: "保存失败", Toast.LENGTH_SHORT).show()
            }
        }
    }

    // 首次进入与模型清单变化时拉一次（清单变了计数就该重算）
    androidx.compose.runtime.LaunchedEffect(models.size) {
        if (models.isNotEmpty()) loadListView()
    }

    fun doRefresh() {
        if (refreshing) return
        refreshing = true
        scope.launch {
            val r = ModelOps.refresh()
            refreshing = false
            // 逐家结论落到状态（卡上直接显示失败的家，见 RefreshCard）——
            // toast 只给一句话，细节必须留在界面上，否则用户错过就没了
            RefreshRow.parseAll(r.data).let { rows ->
                if (rows.isNotEmpty()) {
                    refreshRows = rows
                    refreshAt = System.currentTimeMillis()
                }
            }
            Toast.makeText(
                ctx,
                if (r.ok) "已刷新模型清单" else "刷新失败：${r.error ?: "未知"}",
                Toast.LENGTH_SHORT,
            ).show()
            if (r.ok) GatewayRepository.refreshOnce()
        }
    }

    val filtered = models.filter {
        query.isBlank() || it.id.contains(query, ignoreCase = true) || it.providerLabel.contains(query, ignoreCase = true)
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "模型", onOpenDrawer = onOpenDrawer) {
            AgateHeaderAction("测速") { onOpenSpeedtest() }
            // 「映射」按 v44 放在**页头**而不是页面底部 —— 它是配置入口，
            // 与「测速」同级；放底部会与本页的「清单 + 模型列表」混在一起，
            // 且要滚到最下方才够得着。
            AgateHeaderAction("映射") { onOpenMapping() }
            // 「拉取模型」按 v44/3.0.8 放在页头（与测速、映射同级）：
            // 它是对本页数据的动作，不该在列表最底部等用户滚到底
            AgateHeaderAction(if (refreshing) "拉取中…" else "拉取模型") {
                if (!refreshing) showPull = true
            }
        }

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))

            // ── 客户端所见清单（v44 #xp-card，两层：折叠摘要 + 展开推演）──
            XpCard(
                view = listView.copy(providers = providers),
                configuring = configuring,
                onSetMode = { mode -> saveListView(mode, listView.pins) },
                onTogglePin = { pid ->
                    val next = if (pid in listView.pins) listView.pins - pid else listView.pins + pid
                    saveListView(listView.mode, next)
                },
            )

            Spacer(Modifier.height(12.dp))
            AgateSearchBar(value = query, onValueChange = { query = it }, hint = "搜索模型 / 提供商…")
            Spacer(Modifier.height(10.dp))
            AgateSegmented(
                options = listOf("按模型聚合", "按提供商分组"),
                selected = view,
                onSelect = { view = it },
            )
            Spacer(Modifier.height(10.dp))

            if (models.isEmpty()) {
                EmptyHint(fetched = snap.fetchedAtMs > 0)
            } else if (view == 1) {
                // ── 按提供商分组（v44 #view-provider）──
                val byProvider = filtered.groupBy { it.provider }
                val order = PresetRegistry.ALL.map { it.id }
                byProvider.entries
                    .sortedBy { e -> order.indexOf(e.key).let { if (it < 0) Int.MAX_VALUE else it } }
                    .forEach { (pid, list) ->
                        ProviderModelGroup(
                            providerId = pid,
                            label = list.firstOrNull()?.providerLabel ?: PresetRegistry.labelOf(pid),
                            models = list,
                            onOpenCaps = onOpenCaps,
                        )
                        Spacer(Modifier.height(10.dp))
                    }
            } else {
                // ── 按模型聚合（v44 #view-model）──
                val byId = filtered.groupBy { it.id }
                byId.forEach { (id, list) ->
                    ModelAggregateCard(
                        id = id,
                        providers = list,
                        onOpenCaps = onOpenCaps,
                    )
                    Spacer(Modifier.height(10.dp))
                }
            }

            Spacer(Modifier.height(4.dp))
            Text(
                "本页聚焦「谁出战」· 账号资产与额度见「提供商」",
                style = AgateType.Caption,
                color = C.textDim,
                modifier = Modifier.fillMaxWidth(),
                textAlign = androidx.compose.ui.text.style.TextAlign.Center,
            )
            Spacer(Modifier.height(28.dp))
        }
    }

    // ── 拉取模型弹层（v44/3.0.8：全量 or 按家）──────────────────
    if (showPull) {
        PullModelsDialog(
            providers = providers,
            rows = refreshRows,
            refreshing = refreshing,
            onDismiss = { if (!refreshing) showPull = false },
            onPullAll = { doRefresh() },
            onPullOne = { pid ->
                if (!refreshing) {
                    refreshing = true
                    scope.launch {
                        val r = ModelOps.refresh(providers = listOf(pid))
                        refreshing = false
                        RefreshRow.parseAll(r.data).takeIf { it.isNotEmpty() }?.let { fresh ->
                            // 单家结果并回既有列表：只替换这一家的行，
                            // 其余家的结论与时间保持不变（否则一拉一家就把别家的清空）
                            val merged = refreshRows.filterNot { it.provider == pid } + fresh
                            refreshRows = merged
                        }
                        refreshAt = System.currentTimeMillis()
                        if (r.ok) GatewayRepository.refreshOnce()
                    }
                }
            },
        )
    }
}

/**
 * 客户端所见清单卡（对齐 v44 #xp-card）。
 *
 * ── 为什么是两层而不是把开关全铺开 ──────────────────────────
 * 这是一个**配置推演器**（回答「客户端会看到什么」），不是读数 —— 首屏铺开
 * 三模式切换 + 预览清单 + 开关会占掉大半屏，而用户大多数时候只想看到那个数字。
 * 所以折叠态只留一行「模式 + 项数」，需要时再展开。
 *
 * ── 两处刻意的省略（与 v44 原稿的差异）──────────────────────
 *   · **没有「只列有可用账号的提供商」开关**：关掉它就是把「没登录 / 全部停用」
 *     的家列给客户端，而客户端点了必然报错 —— 列出来毫无意义。内核的
 *     `active_manifests` 已经是硬过滤，这里不给用户关掉它的机会。
 *   · **没有「只列已上架模型」开关**：下架就是下架。下架的开关在「按提供商分组」
 *     视图里已经有（那是它的自然位置），不需要第二个开关表达同一件事。
 *
 * 因此本卡只保留**真正改变输出形态**的三个模式 + 混合模式的 pin 选择。
 */
@Composable
private fun XpCard(
    view: ListView,
    configuring: Boolean,
    onSetMode: (String) -> Unit,
    onTogglePin: (String) -> Unit,
) {
    val C = agateColors()
    var expanded by remember { mutableStateOf(false) }
    val count = view.countFor(view.mode)
    val tooMany = count > 80

    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(14.dp)
            .animateContentSize(),
    ) {
        // ── 第 1 行：标题 + API 徽章 + 折叠箭头 ──
        Row(
            Modifier.fillMaxWidth().clickable { expanded = !expanded },
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                "客户端所见清单",
                style = AgateType.SectionTitle,
                fontWeight = FontWeight.SemiBold,
                color = C.textMain,
                modifier = Modifier.weight(1f),
            )
            AgateBadge("GET /v1/models", BadgeTone.GRAY)
            Spacer(Modifier.width(6.dp))
            Text("⌄", color = C.textDim, fontSize = 14.sp, modifier = Modifier.rotate(if (expanded) 180f else 0f))
        }

        // ── 第 2 行（摘要）：模式 + 项数 + 必要时「偏多」──
        Spacer(Modifier.height(7.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                modeLabel(view.mode),
                color = C.accent,
                fontSize = 11.sp,
                modifier = Modifier
                    .background(C.accent.copy(alpha = 0.12f), RoundedCornerShape(7.dp))
                    .padding(horizontal = 7.dp, vertical = 2.dp),
            )
            Spacer(Modifier.width(8.dp))
            Text(
                buildString {
                    append(count.toString())
                    append(" 项会下发")
                },
                color = C.textSub,
                fontSize = 12.sp,
                modifier = Modifier.weight(1f),
            )
            if (tooMany) AgateBadge("偏多", BadgeTone.AMBER)
        }

        if (!expanded) return@Column

        // ── 展开：模式切换 ──
        Spacer(Modifier.height(12.dp))
        AgateSegmented(
            options = listOf("按模型", "按提供商", "混合"),
            selected = when (view.mode) {
                "provider" -> 1
                "hybrid" -> 2
                else -> 0
            },
            onSelect = { onSetMode(listOf("model", "provider", "hybrid")[it]) },
        )

        // ── 主读数 + 副行拆解 ──
        Spacer(Modifier.height(10.dp))
        Row(verticalAlignment = Alignment.Bottom) {
            Text(
                count.toString(),
                color = if (tooMany) C.amber else C.textMain,
                fontSize = 24.sp,
                fontWeight = FontWeight.Bold,
            )
            Spacer(Modifier.width(8.dp))
            Text("个模型会出现在客户端的列表里", color = C.textSub, fontSize = 12.sp)
        }
        view.breakdown()?.let {
            Spacer(Modifier.height(3.dp))
            Text(it, color = C.textSub, fontSize = 11.sp)
        }

        // ── 模式说明 ──
        Spacer(Modifier.height(5.dp))
        Text(modeDesc(view.mode), color = C.textDim, fontSize = 11.sp)

        // ── 偏多提醒（>80 项）──
        if (tooMany) {
            Spacer(Modifier.height(8.dp))
            Text(
                "超过 80 项：Cherry Studio、Cline 等客户端的下拉列表会变得很难选，建议改用「按模型」。",
                color = C.amber,
                fontSize = 11.sp,
            )
        }

        // ── 混合模式：pin 提供商 ──
        if (view.mode == "hybrid") {
            Spacer(Modifier.height(10.dp))
            Text("额外展开带前缀条目的提供商（可多选）", color = C.textSub, fontSize = 12.sp)
            Spacer(Modifier.height(6.dp))
            FlowChips(
                labels = view.providers.map { it.second },
                isOn = { index -> view.providers[index].first in view.pins },
                onClick = { index -> onTogglePin(view.providers[index].first) },
                enabled = !configuring,
            )
        }

        // ── 清单预览 ──
        if (view.ids.isNotEmpty()) {
            Spacer(Modifier.height(10.dp))
            Text("预览客户端看到的清单", color = C.textSub, fontSize = 12.sp)
            Spacer(Modifier.height(6.dp))
            FlowIds(view.ids.take(12))
            if (view.ids.size > 12) {
                Spacer(Modifier.height(4.dp))
                Text("… 共 ${view.ids.size} 项", color = C.textDim, fontSize = 11.sp)
            }
        }

        Spacer(Modifier.height(9.dp))
        Text(
            "清单只决定「客户端看见什么」；两种写法在任何模式下都能调用：" +
                "裸名由「路由」队列选账号（可跨家转移），provider/id 锁定这一家。",
            style = AgateType.Caption,
            color = C.textDim,
        )
    }
}

/** 模式短标签。 */
private fun modeLabel(mode: String): String = when (mode) {
    "provider" -> "按提供商"
    "hybrid" -> "混合"
    else -> "按模型"
}

/** 模式说明（解释该形态下客户端的行为差异）。 */
private fun modeDesc(mode: String): String = when (mode) {
    "provider" ->
        "每家的每个模型都列一遍，写成「提供商/模型」。客户端可以精确指定某一家，" +
            "但同一个模型会重复出现很多次。"
    "hybrid" ->
        "平时按模型合并；只为上面勾选的提供商额外列出带前缀的条目 —— " +
            "用于必须指定某家的场景（如自建中转、要单独核算的账号）。"
    else ->
        "同名模型只出现一次，不带前缀；用哪一家由「路由」队列决定，并可在同款模型之间" +
            "故障转移。客户端列表最短、最好选，推荐默认。"
}

/* ── 小件：chip 流式布局与 id 徽章流 ── */

@Composable
private fun FlowChips(
    labels: List<String>,
    isOn: (Int) -> Boolean,
    onClick: (Int) -> Unit,
    enabled: Boolean,
) {
    val C = agateColors()
    Column {
        labels.chunked(3).forEach { row ->
            Row(Modifier.fillMaxWidth().padding(vertical = 2.dp), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                row.forEach { label ->
                    val index = labels.indexOf(label)
                    val on = isOn(index)
                    Text(
                        label,
                        color = if (on) C.accent else C.textSub,
                        fontSize = 11.sp,
                        modifier = Modifier
                            .background(
                                if (on) C.accent.copy(alpha = 0.15f) else C.insetBg,
                                RoundedCornerShape(8.dp),
                            )
                            .border(
                                1.dp,
                                if (on) C.accent.copy(alpha = 0.45f) else C.border,
                                RoundedCornerShape(8.dp),
                            )
                            .clickable(enabled = enabled) { onClick(index) }
                            .padding(horizontal = 9.dp, vertical = 3.dp),
                    )
                }
            }
        }
    }
}

@Composable
private fun FlowIds(ids: List<String>) {
    val C = agateColors()
    Column {
        ids.chunked(2).forEach { row ->
            Row(Modifier.fillMaxWidth().padding(vertical = 2.dp), horizontalArrangement = Arrangement.spacedBy(5.dp)) {
                row.forEach { id ->
                    Text(
                        id,
                        color = if (id.contains('/')) C.accent else C.textSub,
                        fontSize = 10.sp,
                        maxLines = 1,
                        overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                        modifier = Modifier
                            .weight(1f, fill = false)
                            .background(C.insetBg, RoundedCornerShape(6.dp))
                            .border(1.dp, C.border, RoundedCornerShape(6.dp))
                            .padding(horizontal = 7.dp, vertical = 2.dp),
                    )
                }
            }
        }
    }
}

/**
 * 「拉取模型」弹层（v44 / 3.0.8 的目录刷新交互）。
 *
 * ── 为什么先给选择、不直接全量 ────────────────────────────
 * 各家目录有 15 分钟缓存，全量拉一遍会逐家打上游。用户常常只想补某一家
 * （比如刚加了账号、或某家刚失败过），所以主路径给「按家」，全量作为快捷项。
 *
 * ── 三种行状态各自要有出路 ───────────────────────────────
 *   · 失败 → 排最上、标红、按钮变「重试」；
 *   · 固定清单（该家没有远程目录）→ 标「固定清单」、按钮置灰并说明原因，
 *     而不是点了没反应；
 *   · 正常 → 给「N 个模型 · 多久之前」，按钮「拉取」。
 */
@Composable
private fun PullModelsDialog(
    providers: List<Pair<String, String>>,
    rows: List<RefreshRow>,
    refreshing: Boolean,
    onDismiss: () -> Unit,
    onPullAll: () -> Unit,
    onPullOne: (String) -> Unit,
) {
    val C = agateColors()
    val byProvider = rows.associateBy { it.provider }
    val failed = rows.filter { it.failed }
    // 失败家排最上：它们才是用户打开这个弹层的原因
    val ordered = providers.sortedBy { (pid, _) ->
        when {
            byProvider[pid]?.failed == true -> 0
            else -> 1
        }
    }

    androidx.compose.ui.window.Dialog(onDismissRequest = onDismiss) {
        Column(
            Modifier
                .fillMaxWidth()
                .background(C.cardBg, RoundedCornerShape(18.dp))
                .border(1.dp, C.border, RoundedCornerShape(18.dp))
                .padding(16.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "拉取模型",
                    color = C.textMain,
                    fontSize = 15.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    "✕",
                    color = C.textDim,
                    fontSize = 15.sp,
                    modifier = Modifier
                        .clickable(enabled = !refreshing) { onDismiss() }
                        .padding(4.dp),
                )
            }
            Spacer(Modifier.height(3.dp))
            Text(
                "各家目录有 15 分钟缓存，短时间内重复拉取会返回缓存",
                color = C.textDim,
                fontSize = 11.sp,
            )
            Spacer(Modifier.height(11.dp))

            Row(horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                RefreshButton(
                    if (refreshing) "拉取中…" else "↻ 全部拉取",
                    C.accent,
                    Modifier.weight(1f),
                ) { if (!refreshing) onPullAll() }
                if (failed.isNotEmpty()) {
                    RefreshButton("仅重试失败 · ${failed.size}", C.amber, Modifier.weight(1f)) {
                        if (!refreshing) failed.take(1).forEach { onPullOne(it.provider) }
                    }
                }
            }

            if (rows.isNotEmpty()) {
                Spacer(Modifier.height(9.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    val ok = rows.count { it.status == "refreshed" }
                    val skipped = rows.count { it.skipped }
                    if (ok > 0) AgateBadge("$ok 成功", BadgeTone.GREEN)
                    if (failed.isNotEmpty()) AgateBadge("${failed.size} 失败", BadgeTone.AMBER)
                    if (skipped > 0) AgateBadge("$skipped 固定清单", BadgeTone.GRAY)
                }
            }

            Spacer(Modifier.height(10.dp))
            Column(Modifier.heightIn(max = 340.dp).verticalScroll(rememberScrollState())) {
                ordered.forEach { (pid, label) ->
                    val row = byProvider[pid]
                    Row(
                        Modifier.fillMaxWidth().padding(vertical = 7.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        AgateProviderAvatar(providerId = pid, label = label, size = 28.dp, corner = 9.dp)
                        Spacer(Modifier.width(9.dp))
                        Column(Modifier.weight(1f)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(
                                    label,
                                    color = C.textMain,
                                    fontSize = 13.sp,
                                    maxLines = 1,
                                    overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                                )
                                if (row?.failed == true) {
                                    Spacer(Modifier.width(5.dp))
                                    AgateBadge("失败", BadgeTone.RED)
                                } else if (row?.skipped == true) {
                                    Spacer(Modifier.width(5.dp))
                                    AgateBadge("固定清单", BadgeTone.GRAY)
                                }
                            }
                            Text(
                                when {
                                    row == null -> "尚未拉取"
                                    row.skipped -> row.message ?: "该家没有远程目录"
                                    row.failed -> row.message?.take(30) ?: "拉取失败"
                                    else -> "${row.count} 个模型 · ${agoLabel(row.refreshedAt.takeIf { it > 0 } ?: refreshAtFallback())}"
                                },
                                color = when {
                                    row?.failed == true -> C.red
                                    else -> C.textDim
                                },
                                fontSize = 10.sp,
                                maxLines = 1,
                                overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                            )
                        }
                        Spacer(Modifier.width(8.dp))
                        val canPull = !refreshing && row?.skipped != true
                        Text(
                            when {
                                row?.failed == true -> "重试"
                                else -> "拉取"
                            },
                            color = if (canPull) C.accent else C.textDim,
                            fontSize = 11.sp,
                            modifier = Modifier
                                .border(
                                    1.dp,
                                    if (canPull) C.accent.copy(alpha = 0.4f) else C.border,
                                    RoundedCornerShape(7.dp),
                                )
                                .clickable(enabled = canPull) { onPullOne(pid) }
                                .padding(horizontal = 9.dp, vertical = 4.dp),
                        )
                    }
                }
            }
        }
    }
}

/** 行上没有 refreshedAt 时的兜底（取「现在」会让文案恒为「刚刚」，故返回 0 → 显示「—」）。 */
private fun refreshAtFallback(): Long = 0L

/**
 * 模型目录刷新卡（v44 quota-entry 改状态卡）。
 *
 * ── 为什么不再是一张「入口卡」──────────────────────────────
 * 旧版只有「标题 + 一句话 + ❯」，用户点完只能靠一处几秒即逝的 toast 知道结果，
 * 错过就得进子页或翻日志。而「哪家拉失败」恰恰是必须看见的信息 —— 它直接
 * 影响这台网关能用哪些模型。
 *
 * ── 常态与异常的区别只体现在「要不要多几行」──────────────────
 *   · 全部成功：两行（模型数 · 时间 · 汇总徽章），点整卡即刷新；
 *   · 有失败：边框转琥珀 + 逐条列出失败/有变更的家 + 卡上给重试按钮。
 * 失败家最多列 3 条，其余聚合成「其余 N 家」—— 卡片不因家多而变长。
 */
@Composable
private fun RefreshCard(
    total: Int,
    refreshing: Boolean,
    rows: List<RefreshRow>,
    refreshAt: Long,
    onRefreshAll: () -> Unit,
    onRetryFailed: (List<String>) -> Unit,
) {
    val C = agateColors()
    val failed = rows.filter { it.failed }
    val hasTrouble = failed.isNotEmpty()
    val refreshed = rows.count { it.status == "refreshed" }
    val skipped = rows.count { it.skipped }

    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(
                1.dp,
                if (hasTrouble) C.amber.copy(alpha = 0.45f) else C.border,
                RoundedCornerShape(20.dp),
            )
            .clickable(enabled = !refreshing, onClick = onRefreshAll)
            .padding(14.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            // 图标块：颜色本身就是状态（正常绿 / 有失败琥珀）
            Box(
                Modifier
                    .size(34.dp)
                    .background(
                        if (hasTrouble) C.amber.copy(alpha = 0.16f) else C.green.copy(alpha = 0.15f),
                        RoundedCornerShape(11.dp),
                    ),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    if (refreshing) "…" else "↻",
                    color = if (hasTrouble) C.amber else C.green,
                    fontSize = 17.sp,
                )
            }
            Spacer(Modifier.width(11.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    "模型目录刷新",
                    color = C.textMain,
                    fontSize = 14.sp,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    buildString {
                        if (refreshing) {
                            append("正在刷新…")
                        } else {
                            append("$total 个模型")
                            if (refreshAt > 0) {
                                append(" · ")
                                append(agoLabel(refreshAt))
                            }
                            when {
                                rows.isEmpty() -> append(" · 每 6 小时自动")
                                hasTrouble -> {}
                                else -> append(" · $refreshed 家正常")
                            }
                        }
                    },
                    color = C.textSub,
                    fontSize = 12.sp,
                    maxLines = 1,
                    overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                )
            }
            when {
                refreshing -> {}
                hasTrouble -> AgateBadge("${failed.size} 家失败", BadgeTone.AMBER)
                else -> Text("❯", color = C.textDim, fontSize = 13.sp)
            }
        }

        // ── 异常时才铺开：只列失败的家（+ 汇总其余）──
        if (!refreshing && hasTrouble) {
            Spacer(Modifier.height(9.dp))
            Box(Modifier.fillMaxWidth().height(1.dp).background(C.border.copy(alpha = 0.5f)))
            Spacer(Modifier.height(8.dp))
            failed.take(3).forEach { row ->
                Row(Modifier.fillMaxWidth().padding(vertical = 2.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        row.label,
                        color = C.textSub,
                        fontSize = 12.sp,
                        maxLines = 1,
                        overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                    Spacer(Modifier.width(8.dp))
                    Text(
                        row.message?.take(28) ?: "失败",
                        color = C.red,
                        fontSize = 11.sp,
                        maxLines = 1,
                        overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                    )
                }
            }
            if (failed.size > 3) {
                Text("… 还有 ${failed.size - 3} 家失败", color = C.textDim, fontSize = 11.sp)
            }
            // 其余家聚合成一句，避免卡片被家数拉长
            val rest = rows.size - failed.take(3).size
            if (rest > 0) {
                Text(
                    buildString {
                        append("其余 $rest 家")
                        if (refreshed > 0) append(" · $refreshed 成功")
                        if (skipped > 0) append(" · $skipped 停用")
                    },
                    color = C.textDim,
                    fontSize = 11.sp,
                    modifier = Modifier.padding(top = 2.dp),
                )
            }
            Spacer(Modifier.height(9.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                RefreshButton("↻ 全量刷新", C.accent, Modifier.weight(1f), onRefreshAll)
                RefreshButton("仅重试失败 · ${failed.size}", C.amber, Modifier.weight(1f)) {
                    onRetryFailed(failed.map { it.provider })
                }
            }
        }
    }
}

@Composable
private fun RefreshButton(
    text: String,
    color: androidx.compose.ui.graphics.Color,
    modifier: Modifier = Modifier,
    onClick: () -> Unit,
) {
    Text(
        text,
        color = color,
        fontSize = 12.sp,
        fontWeight = FontWeight.SemiBold,
        textAlign = androidx.compose.ui.text.style.TextAlign.Center,
        maxLines = 1,
        modifier = modifier
            .background(color.copy(alpha = 0.10f), RoundedCornerShape(9.dp))
            .border(1.dp, color.copy(alpha = 0.38f), RoundedCornerShape(9.dp))
            .clickable(onClick = onClick)
            .padding(vertical = 7.dp),
    )
}

/** 「N 分钟前 / N 小时前 / N 天前」——刷新距今多久。 */
private fun agoLabel(atMs: Long): String {
    // 0 = 没有时间戳（不是「1970 年至今」）。别把缺数据渲染成「2 万天前」
    if (atMs <= 0L) return "—"
    val delta = (System.currentTimeMillis() - atMs).coerceAtLeast(0)
    return when {
        delta < 60_000L -> "刚刚"
        delta < 3_600_000L -> "${delta / 60_000L} 分钟前"
        delta < 86_400_000L -> "${delta / 3_600_000L} 小时前"
        else -> "${delta / 86_400_000L} 天前"
    }
}

/* ── v44 .mgroup：按提供商分组折叠卡 ── *//* ── v44 .mgroup：按提供商分组折叠卡 ── */
@Composable
private fun ProviderModelGroup(
    providerId: String,
    label: String,
    models: List<GatewayRepository.ModelRow>,
    onOpenCaps: () -> Unit,
) {
    val C = agateColors()
    var expanded by remember(providerId) { mutableStateOf(false) }
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .clickable { expanded = !expanded }
            .padding(14.dp)
            .animateContentSize(),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            AgateProviderAvatar(providerId = providerId, label = label, size = 32.dp, corner = 10.dp)
            Spacer(Modifier.width(8.dp))
            Text(label, color = C.textMain, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.width(8.dp))
            Text("${models.size} 个模型", color = C.textSub, fontSize = 12.sp, modifier = Modifier.weight(1f))
            Text("⌄", color = C.textDim, fontSize = 14.sp, modifier = Modifier.rotate(if (expanded) 180f else 0f))
        }
        if (expanded) {
            Spacer(Modifier.height(10.dp))
            models.forEachIndexed { i, m ->
                Row(
                    Modifier.fillMaxWidth().padding(vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text("↳ ${m.id}", color = C.textSub, fontSize = 12.sp, modifier = Modifier.weight(1f))
                    m.credits?.let { CreditsTag(it) }
                }
                if (i < models.size - 1) Box(Modifier.fillMaxWidth().height(1.dp).background(C.border.copy(alpha = 0.4f)))
            }
        }
    }
}

/* ── v44 #view-model：按模型聚合卡（候选提供商 + 倍率 + 能力位）── */
/**
 * 按模型聚合卡（v44 #view-model）。
 *
 * ── 聚合的意义：一张卡回答「这个模型哪家最便宜」────────────────
 * 同一个模型 id 在多家都有时合成一张卡，各家按**倍率升序**排（最优那家绿色
 * 高亮）。这比 3.0.8 的表格更进一步 —— 那边要一行行对比倍率，这里一眼是答案。
 *
 * ── 能力徽章与上下文取「各家生效值里最全的那条」──────────────
 * 同一模型在不同家的声明可能不同（上游目录质量不一）。展示时取第一条**声明了
 * 该项**的记录，而不是简单地取第一家 —— 否则第一家没声明就会让整卡看起来
 * 「什么能力都没有」。来源标记（虚线底 / 小点）留给能力弹窗，卡上不重复表达。
 */
@Composable
private fun ModelAggregateCard(
    id: String,
    providers: List<GatewayRepository.ModelRow>,
    onOpenCaps: () -> Unit,
) {
    val C = agateColors()
    // 倍率升序：最便宜的排第一（`null` 视为最差，排最后）
    val ranked = providers.sortedBy { parseMultiplier(it.credits ?: "") ?: Double.MAX_VALUE }
    val best = ranked.firstOrNull()
    val bestMult = best?.let { parseMultiplier(it.credits ?: "") }
    // 能力与上下文：取「第一条真的声明了这一项」的记录
    val capsSource = ranked.firstOrNull { it.maxInput != null }
        ?: ranked.firstOrNull { it.toolCall || it.images || it.reasoning }
        ?: ranked.first()

    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(14.dp),
    ) {
        // ── 标题行：模型 id + 倍率（最优）──
        Row(verticalAlignment = Alignment.Top) {
            Column(Modifier.weight(1f)) {
                Text(id, color = C.textMain, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
                Text(
                    buildString {
                        append("${ranked.size} 家可用")
                        best?.let { append(" · 最优 ${it.providerLabel}") }
                    },
                    color = C.textSub,
                    fontSize = 12.sp,
                )
            }
            Column(horizontalAlignment = Alignment.End) {
                best?.credits?.let {
                    Text(it, color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                }
                if (bestMult != null && ranked.size > 1) {
                    Text("最低", color = C.green, fontSize = 10.sp)
                }
            }
        }

        // ── 各家候选（倍率升序）──
        Spacer(Modifier.height(8.dp))
        ranked.forEachIndexed { i, m ->
            Row(
                Modifier.fillMaxWidth().padding(vertical = 3.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    m.providerLabel,
                    color = if (i == 0 && ranked.size > 1) C.green else C.textSub,
                    fontSize = 12.sp,
                    modifier = Modifier.weight(1f),
                )
                m.credits?.let { CreditsTag(it) }
            }
        }

        // ── 能力徽章（三态可分辨）──
        Spacer(Modifier.height(8.dp))
        CapBadgeRow(capsSource)

        // ── 上下文 / 输出 ──
        val ctxLine = buildString {
            capsSource.maxInput?.let { append("上下文 ${fmtTokens(it)}") }
            capsSource.maxOutput?.let {
                if (isNotEmpty()) append("   ")
                append("输出 ${fmtTokens(it)}")
            }
        }
        if (ctxLine.isNotEmpty()) {
            Spacer(Modifier.height(6.dp))
            Text(ctxLine, color = C.textSub, fontSize = 11.sp)
        }

        Spacer(Modifier.height(9.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("能力位", color = C.textDim, fontSize = 12.sp, modifier = Modifier.weight(1f))
            Text(
                "编辑 ❯",
                color = C.accent,
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(C.accent.copy(alpha = 0.10f), RoundedCornerShape(9.dp))
                    .clickable(onClick = onOpenCaps)
                    .padding(horizontal = 10.dp, vertical = 5.dp),
            )
        }
    }
}

/**
 * 四个能力徽章，**三态视觉可分辨**。
 *
 * 三态必须分开显示是 3.0.8 反复强调的纪律：把「未声明」画成「不支持」，用户会
 * 照着错值配置下游。这里的判据是「有没有声明」而不是「真假」——
 *   · 支持   → 绿色实线
 *   · 不支持 → 灰色实线
 *   · 未声明 → 灰色**虚线**
 */
@Composable
private fun CapBadgeRow(m: GatewayRepository.ModelRow) {
    val C = agateColors()
    Row(horizontalArrangement = Arrangement.spacedBy(5.dp)) {
        CapChip("工具", m.toolCall)
        CapChip("图片", m.images)
        CapChip("思考", m.reasoning)
    }
}

@Composable
private fun CapChip(label: String, on: Boolean) {
    val C = agateColors()
    Text(
        label,
        color = if (on) C.green else C.textDim,
        fontSize = 10.sp,
        modifier = Modifier
            .background(
                if (on) C.green.copy(alpha = 0.12f) else androidx.compose.ui.graphics.Color.Transparent,
                RoundedCornerShape(6.dp),
            )
            .border(
                1.dp,
                if (on) C.green.copy(alpha = 0.35f) else C.border,
                RoundedCornerShape(6.dp),
            )
            .padding(horizontal = 7.dp, vertical = 2.dp),
    )
}

/** token 数紧凑显示：`200000` → `200K`、`1000000` → `1M`。 */
internal fun fmtTokens(v: Int): String = when {
    v >= 1_000_000 -> "${v / 1_000_000}M"
    v >= 1_000 -> "${v / 1_000}K"
    else -> v.toString()
}

@Composable
private fun CreditsTag(credits: String) {
    val C = agateColors()
    val mult = parseMultiplier(credits)
    val tone = when {
        mult == null -> BadgeTone.GRAY
        mult <= 0.0 -> BadgeTone.GREEN
        mult <= 0.2 -> BadgeTone.GREEN
        mult <= 0.5 -> BadgeTone.BLUE
        else -> BadgeTone.AMBER
    }
    AgateBadge(credits, tone)
}

@Composable
private fun EmptyHint(fetched: Boolean) {
    val C = agateColors()
    Column(
        Modifier.fillMaxWidth().padding(vertical = 28.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            if (fetched) "还没有可暴露的模型" else "尚未加载模型清单",
            color = C.textSub,
            fontSize = 14.sp,
        )
        Spacer(Modifier.height(6.dp))
        Text(
            "添加提供商并登录后，模型会出现在这里；也可点下方「模型目录刷新」强制拉取",
            color = C.textDim,
            fontSize = 12.sp,
            textAlign = androidx.compose.ui.text.style.TextAlign.Center,
        )
    }
}

@Composable
internal fun EntryCard(icon: String, title: String, sub: String, onClick: () -> Unit) {
    val C = agateColors()
    Row(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .clickable(onClick = onClick)
            .padding(16.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(icon, fontSize = 22.sp, color = C.accent)
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(title, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            Text(sub, color = C.textSub, fontSize = 12.sp)
        }
        Text("❯", color = C.textDim, fontSize = 13.sp)
    }
}

internal fun initialsOf(label: String): String =
    label.split(" ", "-", "_").filter { it.isNotBlank() }.take(2)
        .joinToString("") { it.first().uppercase() }.ifBlank { label.take(2).uppercase() }

/** 解析上游 credits 串（如 "x0.16 credits"）为倍率数值；无法解析返回 null。 */
internal fun parseMultiplier(credits: String?): Double? {
    if (credits.isNullOrBlank()) return null
    return Regex("([0-9]+(?:\\.[0-9]+)?)").find(credits)?.groupValues?.get(1)?.toDoubleOrNull()
}

private fun capsOf(m: GatewayRepository.ModelRow): String {
    val parts = ArrayList<String>()
    m.maxInput?.let { parts += "${fmtK(it)} 上下文" }
    m.maxOutput?.let { parts += "${fmtK(it)} 输出" }
    if (m.toolCall) parts += "工具 ✓"
    if (m.images) parts += "图像 ✓"
    if (m.reasoning) parts += "思考 ✓"
    return if (parts.isEmpty()) "未声明" else parts.joinToString(" · ")
}

private fun fmtK(n: Int): String = when {
    n >= 1_000_000 -> "${n / 1_000_000}M"
    n >= 1000 -> "${n / 1000}K"
    else -> n.toString()
}