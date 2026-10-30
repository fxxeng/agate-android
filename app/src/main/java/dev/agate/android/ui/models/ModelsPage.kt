package dev.agate.android.ui.models

import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
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
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.draw.scale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import android.widget.Toast
import kotlinx.coroutines.launch
import dev.agate.android.data.CustomProviderOps
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
    /** 跳测速页。[model] 非空时那一页只显示这一条（胶囊点进来的路径）。 */
    onOpenSpeedtest: (provider: String?, model: String?) -> Unit = { _, _ -> },
    onOpenCaps: (String, String) -> Unit = { _, _ -> },
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

    /**
     * 列表「范围」—— 替代原来的「两种视图」。
     *
     * `null` = 全部：跨家合并，回答「这个模型哪家最优、谁在出战」。
     * 非空 = 某一家：这家的清单，回答「有哪些、开了哪些、哪条挂了」。
     *
     * 为什么不再用两个并列视图：同一个模型换个视图就少一半操作（能力位/测速
     * 各只有一边有），用户要记两套。上游 3.0.8 也是「一个表格 + 左栏选范围」，
     * 「全部」只是范围的一种，不是另一种渲染器。
     */
    var listScope by remember { mutableStateOf<String?>(null) }
    var query by remember { mutableStateOf("") }
    var refreshing by remember { mutableStateOf(false) }
    // 最近一次刷新的逐家结果（v44：卡片要能直接看到「哪家失败」，不必进子页）
    var refreshRows by remember { mutableStateOf<List<RefreshRow>>(emptyList()) }
    var refreshAt by remember { mutableStateOf(0L) }
    var showPull by remember { mutableStateOf(false) }
    // 客户端所见清单的状态 + 推演计数（来自 /api/models/list-view）
    var listView by remember { mutableStateOf(ListView()) }
    var configuring by remember { mutableStateOf(false) }
    // 管理视图（含已禁用条目 + 官方可见性）。`snap.models` 只有「已启用」，
    // 光靠它画不出开关，也画不出被关掉的那些（它们已从清单里消失）。
    var manage by remember { mutableStateOf<List<ModelOps.ManageModelRow>>(emptyList()) }
    /**
     * 自定义 / 预置 API 建出来的家。
     *
     * 「拉取模型」必须覆盖它们：内核的 `/api/models/refresh` 只遍历**内置**适配器
     * （`implemented_kinds()`），`custom-xxxx` 那条路走的是另一个端点
     * （`/api/custom-providers/fetch-models`）。不并进这份列表的话，从「预置 API」
     * 卡片建出来的家（商汤日日新、OpenAI…）在弹层里根本不出现 —— 用户看到的就是
     * 「预置 API 不能拉取模型」。
     */
    var customProviders by remember { mutableStateOf<List<CustomProviderOps.Record>>(emptyList()) }
    /** 正在切换的条目键（provider|id），用来禁掉重复点击。 */
    var toggling by remember { mutableStateOf<String?>(null) }
    /**
     * 「登记模型」弹层的目标家（null = 不显示）。
     *
     * 只对自定义家开放：内置家的清单来自适配器，用户登记进去也不会生效
     * （`/api/models/manage` 的启停才是它们该用的开关）。
     */
    var registryTarget by remember { mutableStateOf<String?>(null) }
    /** 登记弹层的基线（打开时读一次，避免边改边被重算覆盖）。models 与 mappings 各一份。 */
    var registryBaseline by remember { mutableStateOf(org.json.JSONArray()) }
    var registryMappings by remember { mutableStateOf(org.json.JSONArray()) }
    var registrySaving by remember { mutableStateOf(false) }
    /**
     * 逐模型的**可用账号数**（内核选路预演的结果）。
     *
     * 只装「真问过内核」的条目：缺失 = 还没算/算失败，界面显示「—」而不是拿
     * 近似值顶上去。两种口径混在同一颗圆点里，比没有数字更糟。
     */
    var availability by remember { mutableStateOf<Map<String, Int>>(emptyMap()) }
    /** 让「展开的胶囊」重新问一次内核（账号/模型变更后旧数字就过期了）。 */
    var availToken by remember { mutableStateOf(0) }

    /**
     * 提供商 (id, label) 列表：混合模式的 pin 选择要用。
     *
     * ⚠️ 必须 **manage ∪ session** 两处取并集。只从 `models`（`/api/session`）
     * 取的话，那份暂时为空时（内核刚起、会话还没拉到、或本次请求失败）pin 的
     * 芯片会**一个都不显示**，而页面其余部分照常（它们读的是 manage）—— 用户
     * 看到的就是「混合模式没有可勾的提供商」，而且没有任何报错。
     */
    val providers = remember(models, manage) {
        (manage.map { it.provider to it.providerLabel } + models.map { it.provider to it.providerLabel })
            .filter { it.first.isNotBlank() }
            .distinctBy { it.first }
            .sortedBy { it.second }
    }

    fun loadListView() {
        scope.launch { listView = ListView.parse(ModelOps.fetchListView().data) }
    }

    fun loadCustom() {
        scope.launch { customProviders = CustomProviderOps.Record.listOf(CustomProviderOps.list()) }
    }

    /**
     * 「拉取模型」弹层的候选家：内置 ∪ 自定义。
     *
     * 与 [providers]（混合模式的 pin 芯片）**刻意不同**：pin 的是「客户端能见到的
     * 模型属于谁」，一家还没有任何模型时 pin 它毫无意义；而拉取恰恰是**为了让
     * 它有模型**，所以这里必须包含「零模型」的自定义家 —— 否则就变成
     * 「要有模型才能拉模型」的死循环（用户反馈的形态）。
     */
    val pullProviders = remember(providers, customProviders) {
        val customIds = customProviders.map { it.id }.toSet()
        val built = providers.filter { it.first !in customIds }
        (built + customProviders.map { it.id to it.displayName })
            .distinctBy { it.first }
            .sortedBy { it.second }
    }

    /**
     * 自定义家「已登记几个模型」—— 弹层里给尚未拉取的行一个真实读数。
     *
     * 没有它，尚未拉取的自定义家只会显示「尚未拉取」，用户无从判断
     * 「这家到底登记过没有」（预置 API 建出来的家常常已经有一条）。内置家不在此表，
     * 它们的条数由内核 `results.count` 给。
     */
    val customModelCounts = remember(customProviders) {
        customProviders.associate { it.id to it.modelCount }
    }

    fun loadManage() {
        scope.launch { manage = ModelOps.ManageModelRow.parse(ModelOps.fetchManage().data) }
    }

    // 管理视图进页面拉一次：启停状态与「桌面端是否列出」只有它给得出。
    // 放在 `loadManage` 之后 —— 局部函数必须先声明后使用。
    LaunchedEffect(Unit) { loadManage() }
    LaunchedEffect(Unit) { loadCustom() }
    // 事件驱动刷新：模型开关或自定义家清单变了，本页要重拉的是**三份派生数据**
    // （管理视图的启停、客户端所见清单的推演计数、自定义家名字）。会话清单本身
    // 由快照轮询带回，这里不重复拉。
    LaunchedEffect(Unit) {
        GatewayRepository.bus.collect { kind ->
            when (kind) {
                "models_changed" -> {
                    loadManage()
                    loadListView()
                    availToken++      // 开关变了 → 可用账号数变了
                }
                "providers_changed" -> {
                    loadCustom()
                    loadManage()
                    loadListView()
                    availToken++
                }
                "accounts_changed" -> availToken++
            }
        }
    }

    /** 批量启停的忙态（与单条 `toggling` 分开：批量期间不希望每行都变成「…」）。 */
    var batchBusy by remember { mutableStateOf(false) }

    /**
     * 整家批量启停：**一次请求、一次落盘**。
     *
     * 内核新增的 `POST /api/models/state/batch` 在同一把锁内改完再写一次盘。
     * 逐条打单条接口的话，一家 48 个模型 = 48 次序列化+写盘，中途失败还会留下
     * 改了一半的状态。
     */
    fun toggleAll(providerId: String, ids: List<String>, enabled: Boolean) {
        if (ids.isEmpty() || batchBusy) return
        batchBusy = true
        scope.launch {
            try {
                val r = ModelOps.setStates(providerId, ids, enabled)
                if (r.ok) {
                    loadManage()
                    loadListView()
                    GatewayRepository.refreshOnce()
                } else {
                    Toast.makeText(ctx, r.error ?: "批量保存失败", Toast.LENGTH_SHORT).show()
                }
            } finally {
                batchBusy = false
            }
        }
    }

    /**
     * 启停一条（提供商 × 模型 id）。
     *
     * ── 为什么改完要拉两次 ───────────────────────────────────
     * `setState` 的结果立刻反映在 manage 视图（开关状态），但**客户端所见清单**
     * （`/api/session` 的 models、以及 `list-view` 的推演计数）是另一个出口 ——
     * 关掉一个模型后「客户端会看到几个」要跟着变，所以两边都得刷。
     *
     * 不做乐观更新：开关回弹比慢半秒更让人困惑（而且内核可能拒绝，比如 tid
     * 不属于任何已知提供商）。`toggling` 期间禁用本行，避免连点打架。
     */
    fun toggle(row: GatewayRepository.ModelRow) {
        // 会话清单里的 id 在 provider 形态下带 `provider/` 前缀，
        // 而启停接口要的是**上游裸 id**。
        val bare = if ('/' in row.id) row.id.substringAfter('/') else row.id
        val key = "${row.provider}|$bare"
        if (toggling != null) return
        toggling = key
        scope.launch {
            val r = ModelOps.setState(bare, !row.enabled, row.provider.ifBlank { null })
            toggling = null
            if (r.ok) {
                loadManage()
                loadListView()
                GatewayRepository.refreshOnce()
            } else {
                Toast.makeText(ctx, r.error ?: "切换失败", Toast.LENGTH_SHORT).show()
            }
        }
    }

    /** 切模式 / 改 pin：一起提交（两者表达同一件事，见内核 set_models_list 的说明）。 */
    // ── 清单形态的写入：串行化，**不丢最后一次意图** ─────────────
    //
    // 旧写法是 `if (configuring) return` —— 前一次保存还没回来时的点击会被
    // **静默丢掉**（连 toast 都没有）。而用户的操作恰恰是「先点模式、紧接着点
    // 提供商芯片」：第一次请求在飞，第二次被吞 → 芯片不亮、计数不变，看上去
    // 就是「混合模式没生效」。这条 bug 之所以难查，是因为它不留任何痕迹。
    //
    // 现在用一条容量 1 的管道（CONFLATED）：期间连点只保留最后一次意图，
    // 前一次完成立即执行最新那次 —— 既不并发写配置，也不丢动作。
    val listViewSaves = remember {
        kotlinx.coroutines.channels.Channel<Pair<String, List<String>>>(
            kotlinx.coroutines.channels.Channel.CONFLATED
        )
    }
    androidx.compose.runtime.LaunchedEffect(Unit) {
        for ((mode, pins) in listViewSaves) {
            configuring = true
            try {
                val r = ModelOps.setListView(mode, pins)
                val parsed = ListView.parse(r.data)
                if (r.ok) {
                    listView = parsed.copy(providers = providers)
                    // 回包里没有刚提交的 pin = 服务端没接受。静默弹回去最难查，
                    // 所以这里明说。
                    val lost = pins.filterNot { want -> parsed.pins.any { it.equals(want, true) } }
                    if (lost.isNotEmpty()) {
                        Toast.makeText(
                            ctx,
                            "已保存模式，但提供商前缀未生效：${lost.joinToString("、")}",
                            Toast.LENGTH_LONG,
                        ).show()
                    }
                } else {
                    Toast.makeText(ctx, r.error ?: "保存失败", Toast.LENGTH_SHORT).show()
                }
            } finally {
                configuring = false
            }
        }
    }

    fun saveListView(mode: String, pins: List<String>) {
        listViewSaves.trySend(mode to pins)
    }

    // 首次进入与模型清单变化时各拉一次。
    //
    // ⚠️ 不能再加 `if (models.isNotEmpty())`：清单形态（三种模式 + 计数）**不依赖**
    // 模型清单，模型那份还没到时也该把「按模型 / 3 项」这类读数显示出来。
    // 覆盖安装后第一次进页面显示「0 项会下发」、切一下模式才正常，就是这个条件
    // 把首拉吞掉了 —— 切模式会走 `saveListView`，它自己会带回一份最新形态。
    androidx.compose.runtime.LaunchedEffect(models.size) {
        loadListView()
    }

    /**
     * 拉取一家**自定义 / 预置 API** 的模型清单。
     *
     * ── 与内置家的差别（为什么不复用 `/api/models/refresh`）──────────
     * 内置家的清单由适配器给（远程目录 + 静态表），刷新就是重拉目录；自定义家的
     * 清单是**用户登记的那份运行期数据**，内核有专门的端点：
     *   `POST /api/custom-providers/fetch-models` 拉上游（**不落盘**）
     *   `POST /api/custom-providers/models`       整表保存
     * 两步式是刻意的（见内核模块头）：拉回来的清单要让用户确认后再存。但这里是
     * 「拉取模型」按钮，用户的预期就是「拉进来」—— 所以做**合并保存**：
     * 上游有、本地没有的追加；本地已登记的原样保留（启停状态 / 手填能力位不动）。
     * 不做「上游没有就删」：那会把用户手工登记的模型和它们的启停状态一起抹掉，
     * 而上游清单里某个模型暂时消失（改期、灰度）并不等于用户不想再要它。
     */
    suspend fun pullCustom(pid: String): RefreshRow {
        val label = customProviders.firstOrNull { it.id == pid }?.displayName ?: pid
        val fetched = CustomProviderOps.fetchModels(pid)
        if (!fetched.ok) {
            return RefreshRow(
                provider = pid, label = label, status = "failed", count = 0,
                message = fetched.error ?: "拉取失败",
            )
        }
        val upstream = fetched.data?.optJSONArray("models") ?: org.json.JSONArray()
        // 基线：已登记的条目 + 映射。拉不到（家被删了）就当空的 —— 此时覆盖也不会
        // 丢用户数据，因为本来就没有。
        val baseline = CustomProviderOps.registeredModels(pid)
        val existing = baseline?.first ?: org.json.JSONArray()
        val mappings = baseline?.second ?: org.json.JSONArray()
        val known = LinkedHashSet<String>()
        for (i in 0 until existing.length()) {
            existing.optJSONObject(i)?.optString("id")?.takeIf { it.isNotBlank() }?.let { known += it }
        }
        val merged = org.json.JSONArray()
        // 既有条目原样搬过来（保持用户的 enabled / reasoning / capabilities）
        for (i in 0 until existing.length()) {
            existing.optJSONObject(i)?.let { merged.put(it) }
        }
        var added = 0
        for (i in 0 until upstream.length()) {
            // fetch-models 回的是字符串数组；容错一下对象形态（未来可能带元数据）
            val id = upstream.optJSONObject(i)?.optString("id")?.takeIf { it.isNotBlank() }
                ?: upstream.optString(i).takeIf { it.isNotBlank() }
                ?: continue
            if (id in known) continue
            known += id
            merged.put(org.json.JSONObject().put("id", id).put("enabled", true))
            added++
        }
        val saved = CustomProviderOps.saveModels(pid, merged, mappings)
        if (!saved.ok) {
            return RefreshRow(
                provider = pid, label = label, status = "failed", count = 0,
                message = saved.error ?: "保存失败",
            )
        }
        return RefreshRow(
            provider = pid, label = label, status = "refreshed", count = merged.length(),
            message = if (added > 0) "新增 $added 个 · 共 ${merged.length()} 个" else "已是最新（${merged.length()} 个）",
            refreshedAt = System.currentTimeMillis(),
        )
    }

    /**
     * 「全部拉取」：内置家一次批量 + 自定义家逐个 fetch→合并保存。
     *
     * ── 为什么两段串在**同一个忙态**里 ──────────────────────────
     * 两条链路是不同端点（内置 `/api/models/refresh` 一次遍历所有适配器；
     * 自定义 `/api/custom-providers/fetch-models` 一家一请求）。若各自管
     * 自己的 `refreshing`，内置那段一结束按钮就恢复可点，用户能对着仍在
     * 跑的自定义家再点一次 —— 同一家并发拉两遍，结论互相覆盖。
     */
    fun refreshAll() {
        if (refreshing) return
        refreshing = true
        scope.launch {
            try {
                // 1) 内置家：一次批量，逐家结论落到状态（卡上直接显示失败的家）
                val r = ModelOps.refresh()
                RefreshRow.parseAll(r.data).let { rows ->
                    if (rows.isNotEmpty()) {
                        refreshRows = rows
                        refreshAt = System.currentTimeMillis()
                    }
                }
                // 2) 自定义家：逐个走「拉上游 + 合并保存」
                val customs = customProviders.map { it.id }
                if (customs.isNotEmpty()) {
                    val rows = customs.map { pullCustom(it) }
                    refreshRows = refreshRows.filterNot { it.provider in customs } + rows
                }
                Toast.makeText(
                    ctx,
                    if (r.ok) "已刷新模型清单" else "刷新失败：${r.error ?: "未知"}",
                    Toast.LENGTH_SHORT,
                ).show()
                if (r.ok) GatewayRepository.refreshOnce()
                // 新拉到（或下架）的模型要立刻反映在开关列表里：manage 是全量的那份，
                // 不重拉的话「刚拉的模型」不在页面上，用户会以为没拉成功。
                if (r.ok) loadManage()
                loadCustom()
            } finally {
                refreshing = false
            }
        }
    }

    // ── 合并「已启用（会话清单）」与「管理视图（含已禁用 + 可见性）」──
    // 会话清单（`/api/session` 的 models）给的是富数据（上下文 / 能力位），但
    // **只含已启用**的、且没有 `enabled` 字段；管理视图给全量 + 启停状态，但
    // 能力位在 `capabilities` 子对象里。两者按 (provider, 裸 id) 对齐 ——
    // 会话清单在 provider 形态下 id 带 `provider/` 前缀，管理视图永远给裸 id。
    //
    // 结果 = 会话清单那些（补上 official）+ 只存在于管理视图的那些（已禁用）。
    // 关掉的模型因此仍留在页面上（置灰），这正是「全部列出、关掉的置灰」。
    val allRows: List<GatewayRepository.ModelRow> = remember(models, manage) {
        fun bare(id: String) = if ('/' in id) id.substringAfter('/') else id
        val byKey = manage.associateBy { "${it.provider.lowercase()}|${it.id.lowercase()}" }
        val fromSession = models.map { m ->
            val mg = byKey["${m.provider.lowercase()}|${bare(m.id).lowercase()}"]
            m.copy(enabled = true, official = mg?.official)
        }
        val present = fromSession
            .map { "${it.provider.lowercase()}|${bare(it.id).lowercase()}" }
            .toHashSet()
        val extra = manage
            .filter { "${it.provider.lowercase()}|${it.id.lowercase()}" !in present }
            .map { it.toModelRow() }
        fromSession + extra
    }
    // 范围内的行（计数与列表都以此为准，避免「全部 10/48」出现在单家页上）
    val scopeRows = remember(allRows, listScope) {
        val s0 = listScope
        if (s0 == null) allRows else allRows.filter { it.provider == s0 }
    }
    val enabledCount = scopeRows.count { it.enabled }

    val filtered = allRows.filter {
        query.isBlank() ||
            it.id.contains(query, ignoreCase = true) ||
            it.providerLabel.contains(query, ignoreCase = true)
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "模型", onOpenDrawer = onOpenDrawer) {
            // 页头这颗是「不指定模型，进测速页看全量」；指定模型从下面胶囊点进去
            AgateHeaderAction("测速") { onOpenSpeedtest(null, null) }
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
                // 胶囊右侧的「N」要两份数据：裸模型名 → **全部**承载它的家，
                // 以及每家的就绪账号数。取「全部」而不是「第一个」：
                // 同名模型常常被好几家提供（正是「按模型」合并要解决的情形），
                // 只取第一家会把可用账号数算少 —— 一个模型在 A 家有 3 个号、
                // B 家有 2 个，胶囊上写 3 就是错的。
                modelProviders = remember(models) {
                    models.groupBy({ it.id.substringAfter('/') }) { it.provider }
                        .mapValues { (_, list) -> list.distinct() }
                },
                availability = availability,
                availToken = availToken,
                onRequestAvailability = { ids ->
                    scope.launch { availability = ModelOps.availability(ids) }
                },
                onOpenSpeedtest = onOpenSpeedtest,
            )

            Spacer(Modifier.height(12.dp))
            AgateSearchBar(value = query, onValueChange = { query = it }, hint = "搜索模型 / 提供商…")
            Spacer(Modifier.height(10.dp))
            // ── 范围 chips：全部 + 每家 ─────────────────────────────
            //
            // ⚠️ 家列表**不能只从模型行派生**（2026-10-30 修）
            //
            // 旧写法是 `allRows.groupBy { it.provider }` —— 只列「真有模型」的
            // 家。于是「刚建好、还没登记模型」的自定义家（预置 API 建出来的那批
            // 正是如此）在模型页**连一颗芯片都没有**：用户建完家去模型页找它，
            // 什么也没有，而界面上没有任何报错能解释它去哪儿了。
            //
            // 它与「拉取模型」构成死循环：拉取弹层的候选要「零模型也列出」才
            // 能拉第一家，而第一家拉不到模型就永远进不了这个筛选行。
            //
            // 现在芯片表 = 「有模型的家」∪「**已登记存在的自定义家**」——
            // 后者来自 `customProviders`（内核的 `/api/custom-providers`，
            // 列出全部自定义家，与有没有模型无关）。点进去看到「这家没有匹配
            // 的模型」是**诚实**的读数：这家确实还没登记模型，而「登记模型」
            // 的入口就在那个视图里（见下面的自定义家段落）。
            val scopes = remember(allRows, customProviders) {
                val order = PresetRegistry.ALL.map { it.id }
                val fromRows = allRows.groupBy { it.provider }
                    .map { (pid, list) ->
                        pid to (list.firstOrNull()?.providerLabel ?: PresetRegistry.labelOf(pid))
                    }
                // 零模型的自定义家也要有芯片（见上）。名字取自定义家自己的
                // `name`（`providerLabel` 那份来自模型行，零模型时压根没有行）。
                val fromCustoms = customProviders.map { it.id to it.displayName }
                (fromRows + fromCustoms)
                    .distinctBy { it.first }
                    .sortedBy { pair ->
                        // 注册表内的家按注册表顺序排，自定义家排在最后
                        val idx = order.indexOf(pair.first)
                        if (idx < 0) Int.MAX_VALUE else idx
                    }
            }
            Row(
                modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                RangeChip("全部", selected = listScope == null) { listScope = null }
                scopes.forEach { (pid, label) ->
                    RangeChip(label, selected = listScope == pid) {
                        listScope = if (listScope == pid) null else pid
                    }
                }
            }
            Spacer(Modifier.height(10.dp))
            // 「已启用 / 全部」放在分段控件下面一行：它是**读数**（客户端真会
            // 收到几个），不是筛选器 —— 关掉的模型仍然列在下面（置灰），
            // 所以这里不给第二个分段，避免用户以为是一种新的视图形态。
            if (allRows.isNotEmpty()) {
                Text(
                    "已启用 $enabledCount / ${scopeRows.size}" +
                        if (enabledCount < scopeRows.size) " · 未启用/未显示的在卡片下方折叠" else "",
                    style = AgateType.Caption,
                    color = C.textDim,
                )
                Spacer(Modifier.height(8.dp))
            }

            // ⚠️ 判断顺序：**先看有没有选中某家**，再看「全局是不是空的」
            // （2026-10-30 修）。旧写法是 `allRows.isEmpty()` 在前，于是
            // 「只有零模型的自定义家」时 —— 用户明明点了那颗芯片，页面却给
            // 一句「还没有模型」的全局空态，那家的视图根本出不来，「登记模型」
            // 的入口也就够不着了。
            if (listScope != null) {
                // ── 单家范围：这家的完整清单（不折叠，一进来就是展开的）──
                val pid = listScope!!
                // 自定义家：清单是用户登记的，给一个直接编辑的入口 ——
                // 上游不提供 /models 时这是唯一能把模型加进来的路（见弹层说明）。
                if (CustomProviderOps.isCustomProviderId(pid)) {
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .background(C.insetBg, RoundedCornerShape(10.dp))
                            .padding(horizontal = 10.dp, vertical = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            "这家是自定义提供商 · 模型清单由你登记",
                            color = C.textSub,
                            fontSize = 11.5.sp,
                            modifier = Modifier.weight(1f),
                        )
                        Text(
                            "登记模型",
                            color = C.accent,
                            fontSize = 12.sp,
                            fontWeight = FontWeight.SemiBold,
                            modifier = Modifier
                                .clickable {
                                    scope.launch {
                                        // 两份基线一起取：保存是整表替换，漏一份就会把
                                        // 用户已有的别名映射抹掉
                                        CustomProviderOps.registeredModels(pid)?.let { (models, maps) ->
                                            registryBaseline = models
                                            registryMappings = maps
                                        } ?: run {
                                            registryBaseline = org.json.JSONArray()
                                            registryMappings = org.json.JSONArray()
                                        }
                                        registryTarget = pid
                                    }
                                }
                                .padding(horizontal = 6.dp, vertical = 4.dp),
                        )
                    }
                    Spacer(Modifier.height(8.dp))
                }
                val list = filtered.filter { it.provider == pid }
                if (list.isEmpty()) {
                    Text("这家没有匹配的模型", color = C.textDim, fontSize = 12.sp)
                } else {
                    ProviderModelGroup(
                        providerId = pid,
                        label = list.firstOrNull()?.providerLabel ?: PresetRegistry.labelOf(pid),
                        models = list,
                        toggling = toggling,
                        onToggle = { toggle(it) },
                        onToggleAll = { ids, enabled -> toggleAll(pid, ids, enabled) },
                        batchBusy = batchBusy,
                        onOpenCaps = { modelId -> onOpenCaps(pid, modelId) },
                        // 单家范围就是「看这家」，没有折叠的必要
                        expandByDefault = true,
                        hybrid = listView.mode == "hybrid",
                        pins = listView.pins,
                        onToggleEntry = { provider, model ->
                            // 单条 pin 与整家 pin 共用一个数组：`provider` = 整家，
                            // `provider/model` = 单条（内核按同一个字符串判）。
                            val key = "$provider/$model"
                            val next =
                                if (key in listView.pins) listView.pins - key
                                else listView.pins + key
                            saveListView("hybrid", next)
                        },
                    )
                }
            } else if (allRows.isEmpty()) {
                // 全局没有任何模型行时的空态（选中了某家时走上面那条分支，
                // 哪怕这一家是零模型 —— 那家的视图里有「登记模型」的入口，
                // 空态提示反而会把它盖住）。
                EmptyHint(fetched = snap.fetchedAtMs > 0)
            } else {
                // ── 按模型聚合（v44 #view-model）──
                val byId = filtered.groupBy { it.id }
                byId.forEach { (id, list) ->
                    ModelAggregateCard(
                        id = id,
                        providers = list,
                        toggling = toggling,
                        onToggle = { toggle(it) },
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

    // ── 登记模型弹层（自定义家专用）──────────────────────────
    registryTarget?.let { pid ->
        ModelRegistryDialog(
            providerLabel = customProviders.firstOrNull { it.id == pid }?.displayName ?: pid,
            baseline = registryBaseline,
            mappingsBaseline = registryMappings,
            saving = registrySaving,
            onDismiss = { if (!registrySaving) registryTarget = null },
            onSave = { models, mappings ->
                registrySaving = true
                scope.launch {
                    val r = CustomProviderOps.saveModels(pid, models, mappings)
                    registrySaving = false
                    if (r.ok) {
                        registryTarget = null
                        Toast.makeText(
                            ctx,
                            "已保存 ${models.length()} 个模型 · ${mappings.length()} 条映射",
                            Toast.LENGTH_SHORT,
                        ).show()
                        // 清单变了：会话清单 / 管理视图 / 计数全都得重拉
                        GatewayRepository.refreshOnce()
                        loadManage()
                        loadListView()
                        loadCustom()
                    } else {
                        Toast.makeText(ctx, r.error ?: "保存失败", Toast.LENGTH_SHORT).show()
                    }
                }
            },
        )
    }

    // ── 拉取模型弹层（v44/3.0.8：全量 or 按家）──────────────────
    if (showPull) {
        PullModelsDialog(
            providers = pullProviders,
            rows = refreshRows,
            customModelCounts = customModelCounts,
            customPresetKeys = customProviders
                .filter { it.presetKey.isNotBlank() }
                .associate { it.id to it.presetKey },
            refreshing = refreshing,
            onDismiss = { if (!refreshing) showPull = false },
            onPullAll = { refreshAll() },
            onPullOne = { pid ->
                if (!refreshing) {
                    refreshing = true
                    scope.launch {
                        // 自定义家：另一个端点（拉上游 + 合并保存），见 pullCustom
                        if (CustomProviderOps.isCustomProviderId(pid)) {
                            val row = pullCustom(pid)
                            refreshing = false
                            refreshRows = refreshRows.filterNot { it.provider == pid } + row
                            refreshAt = System.currentTimeMillis()
                            if (!row.failed) {
                                GatewayRepository.refreshOnce()
                                loadManage()
                                loadCustom()
                            }
                            if (row.failed) {
                                Toast.makeText(ctx, row.message ?: "拉取失败", Toast.LENGTH_SHORT).show()
                            }
                            return@launch
                        }
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
                        if (r.ok) loadManage()
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
    /** 裸模型名 → 承载它的**全部**家（测速定位取第一个，账号数按全部求和）。 */
    modelProviders: Map<String, List<String>> = emptyMap(),
    /** 模型 → 此刻能接它的账号数（内核预演；缺失 = 还没算）。 */
    availability: Map<String, Int> = emptyMap(),
    /** 变化即重新问一次（账号/模型变更、手动刷新）。 */
    availToken: Int = 0,
    /** 展开时按需问内核要这批模型的可用数。 */
    onRequestAvailability: (List<String>) -> Unit = {},
    onOpenSpeedtest: (provider: String?, model: String?) -> Unit = { _, _ -> },
) {
    val C = agateColors()
    var expanded by remember { mutableStateOf(false) }
    val count = view.countFor(view.mode)
    val tooMany = count > 80
    // 胶囊上的数字是「此刻」的读数，所以只在**真的展开**时才去问内核 ——
    // 折叠态根本看不到胶囊，问了就是白算（每个模型一次选路预演）。
    // 依赖 `availToken`：账号/模型一变，旧数字立刻过期。
    LaunchedEffect(expanded, view.ids, availToken) {
        if (expanded && view.ids.isNotEmpty()) {
            onRequestAvailability(view.ids.take(EXPOSED_PILL_LIMIT))
        }
    }

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
            // 常驻读数：这个数字跟着模式切换与启停实时变（它就是客户端的答案）
            AgateBadge(
                "客户端可见 $count",
                if (tooMany) BadgeTone.AMBER else BadgeTone.BLUE,
            )
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

        // ── 模式切换：常驻 ──────────────────────────────────────
        // 这段原来只在展开后可点。之所以提上来：它是这一页最常用的开关
        // （「客户端到底看到什么」），藏在一层折叠后面等于每次都要多点一次。
        // 它不在标题行的 clickable 里，所以点滑块不会误触折叠。
        Spacer(Modifier.height(10.dp))
        AgateSegmented(
            options = listOf("按模型", "按提供商", "混合"),
            selected = when (view.mode) {
                "provider" -> 1
                "hybrid" -> 2
                else -> 0
            },
            onSelect = { onSetMode(listOf("model", "provider", "hybrid")[it]) },
        )

        if (!expanded) return@Column

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
        // ── 三种模式与下方「范围」的对应关系 ──────────────────────
        // 用户的原话：三个模式应该对应下面的模型管理。确实该对应 —— 只是两边
        // 的口径不同（这里按「客户端收到几个」去重，下面按「本机开了几条」数），
        // 所以不写清就会觉得数字对不上。
        Spacer(Modifier.height(4.dp))
        Text(
            when (view.mode) {
                "provider" -> "= 下方各家里启用的模型，每家各列一条并带「提供商/」前缀"
                "hybrid" -> "= 下方的「全部」（同名合并）+ 你勾的那几家再各加一份带前缀条目"
                else -> "= 下方「全部」里启用的模型（同名跨家合并成一条，多家可用时按路由队列选）"
            },
            color = C.textDim,
            fontSize = 10.sp,
        )

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
            if (view.providers.isEmpty()) {
                // 取不到提供商时**说出来**。以前这里渲染一个空列表，界面上一片
                // 空白、没有任何解释，看起来就像「混合模式没有可勾的东西」。
                Text(
                    "还没取到提供商列表 · 点右上「拉取模型」后重试",
                    color = C.amber,
                    fontSize = 11.sp,
                )
            } else {
                FlowChips(
                    labels = view.providers.map { it.second },
                    isOn = { index -> view.providers[index].first in view.pins },
                    onClick = { index -> onTogglePin(view.providers[index].first) },
                    enabled = !configuring,
                )
            }
            // 单条暴露（`provider/model`）：在家级芯片下面列出来，否则用户勾完
            // 只看到「带前缀 N 项」，不知道是哪几条、也无从取消。
            val entries = view.pins.filter { '/' in it }
            if (entries.isNotEmpty()) {
                Spacer(Modifier.height(6.dp))
                Text(
                    "已单独暴露 ${entries.size} 条：" +
                        entries.take(4).joinToString("、") +
                        if (entries.size > 4) " 等" else "",
                    color = C.accent,
                    fontSize = 10.sp,
                )
            }
        }

        // ── 清单预览 ──
        if (view.ids.isNotEmpty()) {
            Spacer(Modifier.height(10.dp))
            Text("预览客户端看到的清单", color = C.textSub, fontSize = 12.sp)
            Spacer(Modifier.height(6.dp))
            // 胶囊阵列：比一排只读的 id 徽章多两件事 —— 每个胶囊右侧的小圆点是
            // 承载它的那家的**就绪账号数**（0 = 灰、1~2 = 琥珀、≥3 = 绿），
            // 点一下直接带着这个模型去测速页。
            ExposedPills(
                ids = view.ids,
                modelProviders = modelProviders,
                availability = availability,
                onOpen = onOpenSpeedtest,
            )
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

/**
 * 暴露清单的胶囊阵列：每个胶囊 = 一项客户端会看到的 id。
 *
 * ── 比原来那排只读 id 徽章多什么 ────────────────────────────
 *   · 右侧小圆点 = **承载它的那家的就绪账号数**（0 灰 / 1~2 琥珀 / ≥3 绿）。
 *     这是「这个模型现在真能用吗」最快的读数 —— 一个模型挂在只有 0 个就绪账号
 *     的家里，客户端点了必然失败，而原来的界面完全看不出来。
 *   · 点一下直接带着它去测速页（少一次「先去测速页再找这个模型」）。
 *
 * ⚠️ 圆点读的是**按家**的账号数，不是按模型的：内核没有「某个模型有哪几个账号
 * 可用」这个维度（模型清单与账号清单在两个不相交的端点）。所以胶囊上只写数字，
 * 图例里明说它是「该家的就绪账号数」——不写成「3 号在线」那种会被理解成
 * 「这个模型有 3 个账号」的说法。
 *
 * 只铺前 [EXPOSED_PILL_LIMIT] 项：80+ 项时全部铺开会把卡片撑得比内容还长，
 * 而多出来的那些在下面「按范围」里本来就能逐家看。
 */
@OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)
@Composable
private fun ExposedPills(
    ids: List<String>,
    modelProviders: Map<String, List<String>>,
    availability: Map<String, Int>,
    onOpen: (provider: String?, model: String?) -> Unit,
) {
    val C = agateColors()
    val shown = ids.take(EXPOSED_PILL_LIMIT)
    Column {
        androidx.compose.foundation.layout.FlowRow(
            Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(5.dp),
            verticalArrangement = Arrangement.spacedBy(5.dp),
        ) {
            shown.forEach { id ->
                // `provider/model` 形态自带家；裸名要反查一次
                // `provider/model` 形态自带家（就是那一家）；裸名要反查承载家
                val carriers = if ('/' in id) listOf(id.substringBefore('/'))
                else modelProviders[id.substringAfter('/')].orEmpty()
                val model = id.substringAfter('/')
                // 测速定位只需要一家（跳过去那一页会列出该模型的全部可用账号）
                val provider = carriers.firstOrNull()
                // 数字来自内核预演（`plan_route`）：null = 还没算到，显示「—」。
                // **不用近似值顶替** —— 混口径的数字比没有数字更容易误导。
                val usable = availability[id]
                val dot = when {
                    usable == null -> C.textDim
                    usable >= 3 -> C.green
                    usable >= 1 -> C.amber
                    else -> C.red
                }
                Row(
                    Modifier
                        .background(C.insetBg, RoundedCornerShape(8.dp))
                        .border(1.dp, C.border, RoundedCornerShape(8.dp))
                        .clickable { onOpen(provider, model) }
                        .padding(horizontal = 8.dp, vertical = 3.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        id,
                        color = if ('/' in id) C.accent else C.textSub,
                        fontSize = 10.sp,
                        maxLines = 1,
                        overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                    )
                    Spacer(Modifier.width(5.dp))
                    Box(Modifier.size(5.dp).background(dot, androidx.compose.foundation.shape.CircleShape))
                    Spacer(Modifier.width(3.dp))
                    Text(
                        usable?.toString() ?: "—",
                        color = C.textDim,
                        fontSize = 9.sp,
                    )
                }
            }
        }
        if (ids.size > shown.size) {
            Spacer(Modifier.height(5.dp))
            Text(
                "还有 ${ids.size - shown.size} 项未铺开（共 ${ids.size} 项）· 用上方范围逐家看",
                color = C.textDim,
                fontSize = 10.sp,
            )
        }
        Spacer(Modifier.height(4.dp))
        Text(
            "小圆点 = 此刻能接该模型的账号数（内核选路预演：已排除冷却/限制器/并发满/无凭证）· 点胶囊去测速",
            color = C.textDim,
            fontSize = 10.sp,
        )
    }
}

/** 胶囊最多铺几项。 */
private const val EXPOSED_PILL_LIMIT = 40

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
    /** 自定义家「已登记几个模型」；内置家不在表里（条数由 [rows] 给）。 */
    customModelCounts: Map<String, Int> = emptyMap(),
    /** id → 预置模板 slug（图标身份，随创建固化；空 = 无身份记录） */
    customPresetKeys: Map<String, String> = emptyMap(),
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
                        AgateProviderAvatar(
                            providerId = pid,
                            label = label,
                            size = 28.dp,
                            corner = 9.dp,
                            customPresetKey = customPresetKeys[pid].orEmpty(),
                        )
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
                                    row == null -> {
                                        // 自定义家：已登记过就给真实条数（预置 API 建出来的
                                        // 常常已经有一条），没有记录才是「尚未拉取」。
                                        // 内置家没有这张表，保持原文案。
                                        val known = customModelCounts[pid]
                                        if (known != null) "已登记 $known 个 · 尚未拉取"
                                        else "尚未拉取"
                                    }
                                    row.skipped -> row.message ?: "该家没有远程目录"
                                    row.failed -> row.message?.take(30) ?: "拉取失败"
                                    // 自定义家的 message 是「新增 N 个 · 共 M 个」，比条数更有信息
                                    row.message != null && row.provider in customModelCounts ->
                                        "${row.count} 个模型 · ${row.message}"
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
    toggling: String?,
    onToggle: (GatewayRepository.ModelRow) -> Unit,
    /** 整家批量启停（作用于**当前可见**的模型，见下面批量行的说明）。 */
    onToggleAll: (List<String>, Boolean) -> Unit,
    batchBusy: Boolean,
    onOpenCaps: (String) -> Unit,
    /** 单家范围用：进来就展开、且不可折叠（这时卡片只是容器，不是手风琴）。 */
    expandByDefault: Boolean = false,
    /** 当前清单形态是不是混合（只有混合下「前缀」才有意义）。 */
    hybrid: Boolean = false,
    /** 混合模式当前的 pin 列表（元素是家 id 或 `provider/model`）。 */
    pins: List<String> = emptyList(),
    /** 切换某一条的前缀暴露。 */
    onToggleEntry: (String, String) -> Unit = { _, _ -> },
) {
    val C = agateColors()
    var expanded by remember(providerId) { mutableStateOf(false) }
    val open = expandByDefault || expanded
    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .clickable(enabled = !expandByDefault) { expanded = !expanded }
            .padding(14.dp)
            .animateContentSize(),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            AgateProviderAvatar(providerId = providerId, label = label, size = 32.dp, corner = 10.dp)
            Spacer(Modifier.width(8.dp))
            Text(label, color = C.textMain, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.width(8.dp))
            Text(
                "${models.count { it.enabled }} / ${models.size} 启用",
                color = C.textSub,
                fontSize = 12.sp,
                modifier = Modifier.weight(1f),
            )
            if (!expandByDefault) {
                Text("⌄", color = C.textDim, fontSize = 14.sp, modifier = Modifier.rotate(if (open) 180f else 0f))
            }
        }
        if (open) {
            Spacer(Modifier.height(10.dp))
            // ── 批量：作用于**当前可见**的模型（已受搜索影响）──────────
            // 为什么不是「这家全部 48 个」：搜索后点「全部启用」却把看不见的也开
            // 了，属于看不见的误伤。要「一个不留全关」的话，清掉搜索框再点。
            Row(
                Modifier.fillMaxWidth().padding(bottom = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    if (batchBusy) "批量保存中…" else "批量",
                    color = C.textDim,
                    fontSize = 11.sp,
                    modifier = Modifier.weight(1f),
                )
                val visibleIds = models.map { m -> if ('/' in m.id) m.id.substringAfter('/') else m.id }
                SmallPill("全部启用", enabled = !batchBusy) { onToggleAll(visibleIds, true) }
                SmallPill("全部关闭", enabled = !batchBusy) { onToggleAll(visibleIds, false) }
            }
            if (hybrid && providerId !in pins) {
                Text(
                    "混合模式 · 点每行的「前缀」可只把这一条加进客户端清单（整家暴露在卡片「额外展开」里勾）",
                    color = C.textDim,
                    fontSize = 10.sp,
                    modifier = Modifier.padding(bottom = 6.dp),
                )
            }

            // ── 正列：已启用且桌面端也列出的 ──────────────────────────
            // 「桌面端未列」（official=false）的模型即使开着也收进折叠区：这一页
            // 的读数是「和桌面端一致」，把官方清单之外的混在正列里会让对齐失真。
            val onRows = models.filter { it.enabled && it.official != false }
            val offRows = models.filterNot { it.enabled && it.official != false }
            var showOff by remember(providerId) { mutableStateOf(false) }

            // 整家已勾选时，行内不再给「前缀」开关（每一条本来就带前缀，见 ModelLine 的说明）
            val homePinned = providerId in pins
            onRows.forEach { m ->
                val bare = if ('/' in m.id) m.id.substringAfterLast('/') else m.id
                ModelLine(
                    m = m,
                    toggling = toggling,
                    onToggle = onToggle,
                    onOpenCaps = onOpenCaps,
                    entryOn = if (!hybrid || homePinned) null else ("$providerId/$bare" in pins),
                    onToggleEntry = { onToggleEntry(providerId, bare) },
                )
                Box(Modifier.fillMaxWidth().height(1.dp).background(C.border.copy(alpha = 0.4f)))
            }
            if (offRows.isNotEmpty()) {
                val offEnabled = offRows.count { it.enabled }
                Row(
                    Modifier
                        .fillMaxWidth()
                        .clickable { showOff = !showOff }
                        .padding(vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        (if (showOff) "⌃ " else "⌄ ") + "未显示 ${offRows.size} 个" +
                            if (offEnabled > 0) "（其中 $offEnabled 个开着）" else "",
                        color = C.textDim,
                        fontSize = 11.sp,
                        modifier = Modifier.weight(1f),
                    )
                    Text(
                        if (showOff) "收起" else "点开查看",
                        color = C.accent,
                        fontSize = 11.sp,
                    )
                }
                if (showOff) {
                    offRows.forEach { m ->
                        Box(Modifier.fillMaxWidth().height(1.dp).background(C.border.copy(alpha = 0.4f)))
                        val bare = if ('/' in m.id) m.id.substringAfterLast('/') else m.id
                        ModelLine(
                            m = m,
                            toggling = toggling,
                            onToggle = onToggle,
                            onOpenCaps = onOpenCaps,
                            entryOn = if (!hybrid || homePinned) null else ("$providerId/$bare" in pins),
                            onToggleEntry = { onToggleEntry(providerId, bare) },
                        )
                    }
                }
            }
        }
    }
}

/**
 * 一行模型：名称 + 可选标记 + 倍率 + 启停开关。
 *
 * 抽出来是因为「已启用」与「已关闭」两段要画同样的行（见上面折叠区的说明），
 * 复制一份会让两处样式慢慢走偏。
 */
@Composable
private fun ModelLine(
    m: GatewayRepository.ModelRow,
    toggling: String?,
    onToggle: (GatewayRepository.ModelRow) -> Unit,
    onOpenCaps: (String) -> Unit,
    /**
     * 「这一条也带前缀暴露」的状态：
     * `null` = 不显示（非混合模式，或这家已整家勾选 —— 那时每一条本来就带前缀）；
     * `false/true` = 未暴露 / 已暴露。
     */
    entryOn: Boolean? = null,
    onToggleEntry: () -> Unit = {},
) {
    val C = agateColors()
    val scope = rememberCoroutineScope()
    val bareId = if ('/' in m.id) m.id.substringAfter('/') else m.id
    val busy = toggling == "${m.provider}|$bareId"
    // 行内测速：结果与「模型测速」页共用一份（见 ModelTestStore 的说明）
    val testResults by ModelTestStore.results.collectAsState()
    val testing by ModelTestStore.running.collectAsState()
    val testKey = ModelTestStore.key(m.provider, bareId)
    val testBusy = testKey in testing
    val testResult = testResults[testKey]
    Row(
        Modifier.fillMaxWidth().padding(vertical = 3.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        // 关掉的用 textDim：一眼能分出「在列但没开」
        Column(Modifier.weight(1f)) {
            Text(
                "↳ ${m.id}",
                color = if (m.enabled) C.textSub else C.textDim,
                fontSize = 12.sp,
            )
            if (testResult != null) {
                Text(
                    testResult.summary(),
                    color = if (testResult.ok) C.green else C.red,
                    fontSize = 9.sp,
                    maxLines = 1,
                )
            }
        }
        // 「前缀」：把这一条单独加进混合模式的带前缀清单（写 pins 的 `provider/model`）。
        // 整家已勾选时传 null 不显示 —— 那时每一条都带前缀，再给开关会让人以为可以单独关掉。
        entryOn?.let { on ->
            Text(
                if (on) "前缀 ✓" else "前缀",
                color = if (on) C.onAccent else C.accent,
                fontSize = 9.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .padding(end = 6.dp)
                    .background(
                        if (on) C.accent else C.accent.copy(alpha = 0.10f),
                        RoundedCornerShape(7.dp),
                    )
                    .clickable { onToggleEntry() }
                    .padding(horizontal = 7.dp, vertical = 3.dp),
            )
        }
        // 「能力位」：带 (provider, model) 上下文跳到编辑器并直接落到这一条。
        // 之前只能在聚合卡里跳一次「全量列表」，找不到刚点的那条。
        Text(
            "能力位",
            color = C.accent,
            fontSize = 9.sp,
            fontWeight = FontWeight.SemiBold,
            modifier = Modifier
                .padding(end = 6.dp)
                .background(C.accent.copy(alpha = 0.10f), RoundedCornerShape(7.dp))
                .clickable { onOpenCaps(bareId) }
                .padding(horizontal = 7.dp, vertical = 3.dp),
        )
        // 「测速」：单条真实请求（会花额度），所以只在行上给一颗小按钮，
        // 不自动跑 —— 用户点哪条测哪条。
        Text(
            if (testBusy) "…" else "测速",
            color = if (testBusy) C.textDim else C.accent,
            fontSize = 9.sp,
            fontWeight = FontWeight.SemiBold,
            modifier = Modifier
                .padding(end = 6.dp)
                .background(C.accent.copy(alpha = 0.10f), RoundedCornerShape(7.dp))
                .clickable(enabled = !testBusy) {
                    scope.launch { ModelTestStore.run(m.provider, bareId) }
                }
                .padding(horizontal = 7.dp, vertical = 3.dp),
        )
        if (m.official == false) {
            Text(
                "桌面端未列",
                color = C.textDim,
                fontSize = 9.sp,
                modifier = Modifier
                    .padding(end = 6.dp)
                    .border(1.dp, C.border, RoundedCornerShape(6.dp))
                    .padding(horizontal = 4.dp, vertical = 1.dp),
            )
        }
        m.credits?.let { CreditsTag(it) }
        Spacer(Modifier.width(6.dp))
        ModelToggle(
            enabled = m.enabled,
            busy = busy,
            onToggle = { onToggle(m) },
        )
    }
}

/** 范围选择用的胶囊（全部 / 某一家）。 */
@Composable
private fun RangeChip(text: String, selected: Boolean, onClick: () -> Unit) {
    val C = agateColors()
    Text(
        text,
        maxLines = 1,
        color = if (selected) C.onAccent else C.textSub,
        fontSize = 11.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier
            .background(
                if (selected) C.accent else C.cardBg,
                RoundedCornerShape(999.dp),
            )
            .border(
                1.dp,
                if (selected) Color.Transparent else C.border,
                RoundedCornerShape(999.dp),
            )
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 6.dp),
    )
}

/** 卡片里的胶囊小按钮（批量启用/关闭用；本文件内自用）。 */
@Composable
private fun SmallPill(text: String, enabled: Boolean, onClick: () -> Unit) {
    val C = agateColors()
    Text(
        text,
        color = if (enabled) C.accent else C.textDim,
        fontSize = 10.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier
            .background(C.accent.copy(alpha = if (enabled) 0.10f else 0.04f), RoundedCornerShape(7.dp))
            .clickable(enabled = enabled, onClick = onClick)
            .padding(horizontal = 8.dp, vertical = 4.dp),
    )
}

/**
 * 单条模型的启停开关。
 *
 * 用 Material3 的 `Switch` 缩放 0.7 —— 列表一行里有 id、可能还有「桌面端未列」
 * 标记和倍率标签，原尺寸的 Switch（52×32）会把它们挤到换行。缩放在布局里占的
 * 仍然是原尺寸的测量框，所以外面套一层固定宽的 Box 剪掉多余留白。
 *
 * `busy` 期间禁用：请求还没回来就允许再点，会产生「开关已经翻了、状态又被打回」
 * 的观感（内核可能拒绝）。
 */
@Composable
private fun ModelToggle(enabled: Boolean, busy: Boolean, onToggle: () -> Unit) {
    val C = agateColors()
    Box(Modifier.width(34.dp), contentAlignment = Alignment.Center) {
        Switch(
            checked = enabled,
            onCheckedChange = { if (!busy) onToggle() },
            enabled = !busy,
            modifier = Modifier.scale(0.68f),
            colors = SwitchDefaults.colors(
                checkedThumbColor = C.onAccent,
                checkedTrackColor = C.accent,
                uncheckedThumbColor = C.textDim,
                uncheckedTrackColor = C.btnBg,
                uncheckedBorderColor = C.border,
            ),
        )
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
    toggling: String?,
    onToggle: (GatewayRepository.ModelRow) -> Unit,
    onOpenCaps: (String, String) -> Unit,
) {
    val C = agateColors()
    val scope = rememberCoroutineScope()
    // 行内测速：与分组卡、测速页共用同一份（ModelTestStore）
    val testResults by ModelTestStore.results.collectAsState()
    val testing by ModelTestStore.running.collectAsState()
    // 倍率升序：最便宜的排第一（`null` 视为最差，排最后）。
    // **已启用的排在前面**：关掉的家仍然是候选（可以随时开回来），但不该
    // 占着「最优」的位置 —— 一个关着的家再便宜也用不上。
    val ranked = providers.sortedWith(
        compareByDescending<GatewayRepository.ModelRow> { it.enabled }
            .thenBy { parseMultiplier(it.credits ?: "") ?: Double.MAX_VALUE }
    )
    val usable = ranked.filter { it.enabled }
    val best = usable.firstOrNull() ?: ranked.first()
    val bestMult = best.takeIf { it.enabled }?.let { parseMultiplier(it.credits ?: "") }
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
                        append("${usable.size}/${ranked.size} 家启用")
                        best.takeIf { it.enabled }?.let { append(" · 最优 ${it.providerLabel}") }
                    },
                    color = C.textSub,
                    fontSize = 12.sp,
                )
            }
            Column(horizontalAlignment = Alignment.End) {
                best.takeIf { it.enabled }?.credits?.let {
                    Text(it, color = C.textMain, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                }
                if (bestMult != null && usable.size > 1) {
                    Text("最低", color = C.green, fontSize = 10.sp)
                }
            }
        }

        // ── 各家候选（已启用在前，同组内倍率升序）──
        Spacer(Modifier.height(8.dp))
        ranked.forEachIndexed { i, m ->
            val bareId = if ('/' in m.id) m.id.substringAfter('/') else m.id
            val busy = toggling == "${m.provider}|$bareId"
            Row(
                Modifier.fillMaxWidth().padding(vertical = 2.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    m.providerLabel,
                    color = when {
                        !m.enabled -> C.textDim
                        i == 0 && usable.size > 1 -> C.green
                        else -> C.textSub
                    },
                    fontSize = 12.sp,
                    modifier = Modifier.weight(1f),
                )
                if (m.official == false) {
                    Text(
                        "桌面端未列",
                        color = C.textDim,
                        fontSize = 9.sp,
                        modifier = Modifier
                            .padding(end = 6.dp)
                            .border(1.dp, C.border, RoundedCornerShape(6.dp))
                            .padding(horizontal = 4.dp, vertical = 1.dp),
                    )
                }
                // 测速：聚合卡的价值就是「对比各家」，能在这一屏直接测才有意义
                val testKey = ModelTestStore.key(m.provider, bareId)
                Text(
                    if (testKey in testing) "…" else "测速",
                    color = if (testKey in testing) C.textDim else C.accent,
                    fontSize = 9.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .padding(end = 6.dp)
                        .background(C.accent.copy(alpha = 0.10f), RoundedCornerShape(7.dp))
                        .clickable(enabled = testKey !in testing) {
                            scope.launch { ModelTestStore.run(m.provider, bareId) }
                        }
                        .padding(horizontal = 7.dp, vertical = 3.dp),
                )
                m.credits?.let { CreditsTag(it) }
                Spacer(Modifier.width(6.dp))
                ModelToggle(enabled = m.enabled, busy = busy, onToggle = { onToggle(m) })
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
        // 能力位按 **(provider, model)** 存：聚合卡里一行可能对应多家，所以这里
        // 必须逐家给入口 —— 一颗「编辑」跳过去，编辑器根本不知道该改谁。
        Row(
            Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("能力位", color = C.textDim, fontSize = 12.sp, modifier = Modifier.padding(end = 8.dp))
            ranked.forEach { m ->
                val bare = if ('/' in m.id) m.id.substringAfter('/') else m.id
                SmallPill(m.providerLabel, true) { onOpenCaps(m.provider, bare) }
                Spacer(Modifier.width(6.dp))
            }
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