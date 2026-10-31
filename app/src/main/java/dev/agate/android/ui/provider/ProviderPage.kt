package dev.agate.android.ui.provider

import androidx.compose.animation.animateContentSize
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
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.DisposableEffect
import android.widget.Toast
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.AccountOps
import dev.agate.android.data.CustomProviderOps
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.ProviderPreset
import dev.agate.android.data.PresetRegistry
import dev.agate.android.data.CheckinCenter
import dev.agate.android.data.dto.AccountDto
import dev.agate.android.ui.models.parseMultiplier
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateProviderAvatar
import dev.agate.android.ui.components.PinToTopButton
import dev.agate.android.ui.components.ReorderState
import dev.agate.android.ui.components.ReorderableColumn
import dev.agate.android.ui.components.ReorderableLazyColumn
import dev.agate.android.ui.components.dragHandle
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.components.AgateAvatar
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateHeaderAction
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.AgatePrimaryAction
import dev.agate.android.ui.components.AgateProgress
import dev.agate.android.ui.components.AgateSearchBar
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject

/** 折叠组视图：家 preset（可能为 null=自定义）+ 账号列表。 */
internal data class ProviderGroupView(
    val providerId: String,
    val label: String,
    val preset: ProviderPreset?,
    val accounts: List<AccountDto>,
    /**
     * 自定义家用户起的名字（内置家为空串）。
     *
     * 与 [label] 不是一回事：[label] 是**展示串**（自定义家填的是同一个名字，
     * 但查不到时会回落成 id），而图标反查需要的是「用户当初填的原值」——
     * 它与预置模板的名称做等值比较（`ProviderIcons.resOfCustomName`）。
     * 分开两个字段是为了不让「展示兜底」污染「身份判定」。
     */
    val customName: String = "",
) {
    val hasWarn: Boolean get() = accounts.any { !it.enabled || !it.hasCredentials || it.lowBalance }
}

/** 单账号的余额/在途读数（来自 /api/accounts/usage 与 /connections）。 */
internal data class AcctMetric(
    val text: String? = null,
    val pct: Int? = null,
    val failed: Boolean = false,
    /**
     * 可累加的数值读数（无则 null）。
     *
     * 卡片折叠态要显示**这家全部账号的累加余额** —— 展示串（[text]）是拼好的
     * （「共8,200 · 套餐0 · 奖励8,200」），拼不回去，所以数值要单独带出来。
     * 单位只在同一家内一致（WorkBuddy 是积分、ZCode 是 token、Cline 是 credit），
     * 因此按家累加安全，跨家不可比。
     */
    val amount: Double? = null,
    /** 读数单位（积分 / token / credits…），上游没给则 null。 */
    val unit: String? = null,
    /** 最近一份会作废额度的到期时刻（毫秒）与那一份的剩余量。 */
    val expiryAt: Long? = null,
    val expiryAmount: Double? = null,
)

/**
 * 提供商页（M3-3R，对齐 v44 Tab 2）：
 * - 页头三动作：⇅ 排序 / + 添加；搜索框 + 三档筛选（全部/有异常/正常）
 * - 折叠卡：官方 logo + 右下状态点 / 名称 + N 个账号 · 登录方式 / 「管理」→ 详情子页
 * - collapsed 显示摘要行（N 就绪 / N 停用徽标 + 启用占比细条）；展开显示 acct 块
 * - 排序：**长按卡片直接拖**（不需先进排序模式），松手单次提交；排序编辑态下
 *   非首项右侧出现「置顶」圆按钮，点卡片任意处即退出。两层各自独立：
 *   · 提供商层写 `GatewayPrefs.providerOrder`（**只影响显示顺序**）
 *   · 账号层写内核 priority（`/api/accounts/reorder`，**这才是出战顺序**）
 *   状态机与动画规格移植自参考实现 `Android-AI-gateway` 的 `Reorderable.kt`。
 */
@Composable
fun ProviderPage(
    modifier: Modifier = Modifier,
    onOpenDrawer: () -> Unit = {},
    onOpenAdd: () -> Unit = {},
    onOpenDetail: (String) -> Unit = {},
    onOpenQuotaCenter: () -> Unit = {},
    onOpenLimiter: (AccountDto) -> Unit = {},
) {
    val C = agateColors()
    val ctx = LocalContext.current

    val snap by GatewayRepository.snapshot.collectAsState()
    // 签到能力标读聚合快照里的 providerOptions（= CHECKIN_PROVIDERS ∩ supports_checkin）
    // —— 那是「哪家能签到」的唯一权威答案，本页不另维护一份清单
    val checkinSnap by CheckinCenter.snapshot.collectAsState()
    val scope = rememberCoroutineScope()

    var metrics by remember { mutableStateOf<Map<String, AcctMetric>>(emptyMap()) }
    // 「额度设限」的编辑目标（null = 弹窗关闭）
    var limiterTarget by remember { mutableStateOf<AccountDto?>(null) }
    var inflight by remember { mutableStateOf<Map<String, Int>>(emptyMap()) }
    var querying by remember { mutableStateOf(false) }

    DisposableEffect(Unit) {
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx))
        AccountOps.configure(GatewayPrefs.apiKey(ctx))
        CheckinCenter.configure(GatewayPrefs.apiKey(ctx))
        CustomProviderOps.configure(GatewayPrefs.apiKey(ctx))
        // 进本页立刻补拉一次：轮询已提到导航宿主，但它的第一个 5 秒刻度可能
        // 还没到；本页的主内容就是账号清单，等那 5 秒会是空白屏。refreshOnce
        // 自带在途去重，与轮询撞车也不会打两次。
        GatewayRepository.refreshOnce()
        CheckinCenter.refreshOnce()
        onDispose { }
    }

    /** 并发查询全部账号余额 + 在途连接数。silent=首屏自动拉取不弹提示。 */
    fun loadBalances(silent: Boolean = false) {
        if (querying) return
        querying = true
        scope.launch {
            val usage = AccountOps.usage()
            val conn = AccountOps.connections()
            querying = false
            if (usage != null) {
                val out = HashMap<String, AcctMetric>()
                val arr = usage.optJSONArray("results")
                if (arr != null) for (i in 0 until arr.length()) {
                    val row = arr.optJSONObject(i) ?: continue
                    val id = row.optString("id")
                    if (id.isBlank()) continue
                    // ⚠️ error 是 JSON null 时 `optString` 会返回字符串 "null"
                    //（org.json 的 objectToString），旧实现把它当成错误文案，
                    // 真机上显示「查询失败：null」。这里只认真正的字符串。
                    val errRaw = row.opt("error")
                    val err = (errRaw as? String)?.trim().orEmpty()
                    out[id] = when {
                        err.isNotEmpty() -> AcctMetric(text = "查询失败：$err", failed = true)
                        row.optJSONObject("usage") == null -> AcctMetric(text = "未返回余额数据")
                        else -> parseUsageMetric(row.optJSONObject("usage"))
                    }
                }
                metrics = out
            }
            conn?.optJSONObject("counts")?.let { c ->
                val map = HashMap<String, Int>()
                c.keys().forEach { k -> map[k] = c.optInt(k) }
                inflight = map
            }
            if (!silent) {
                Toast.makeText(
                    ctx,
                    if (usage == null) "余额查询失败" else "已刷新 ${metrics.size} 个账号余额",
                    Toast.LENGTH_SHORT,
                ).show()
            }
        }
    }

    // 首屏自动拉一次余额/在途（有账号时，静默）
    LaunchedEffect(snap.accountList.size) {
        if (snap.accountList.isNotEmpty() && metrics.isEmpty()) loadBalances(silent = true)
    }

    // 倍率区间：从客户端清单（/v1/models 同源）按家聚合 credits
    val multByProvider = remember(snap.models) {
        snap.models.groupBy { it.provider }.mapValues { (_, rows) ->
            val nums = rows.mapNotNull { parseMultiplier(it.credits) }
            if (nums.isEmpty()) null else nums.min() to nums.max()
        }
    }
    val checkinProviders = remember(checkinSnap.providerOptions) {
        checkinSnap.providerOptions.map { it.first }.toSet()
    }
    // ── 自定义提供商的展示名 ──────────────────────────────────
    // `custom-xxxx` 这种 id 在内置注册表里查不到，`labelOf` 会**原样回显 id** ——
    // 于是列表里出现「custom-7a3f9c21」这种名字（用户建的「NewAPI 中转」根本
    // 看不见）。上游 3.0.8 的做法是把 id→name 并进 `wbProviders.labelOf` 的
    // 查找链，一处合并、账号表/筛选器/日志处处生效；这里照同一口径。
    //
    // ── 为什么名字表要带「取到了没有」这个状态 ───────────────────
    // 旧写法：拉不到时得到**空表**，而空表与「确实没有自定义家」在界面上
    // 一模一样 —— 表现是**每个自定义家都显示成 `custom-xxxx`**，图标也因为
    // 按名反查不到而变成两字母色块（截图里的现象）。用户看到的是「我建的家
    // 全变成乱码了」，而界面上**没有任何一处**说明发生过什么 —— 一次临时的
    // 请求失败被伪装成了「数据就是这样」。
    //
    // 现在把失败如实带出来（`customNamesError`），界面上给一句可读的话 +
    // 一个重试入口；名字另有兜底来源（见 `customNames` 的组装）。
    var customNames by remember { mutableStateOf<Map<String, String>>(emptyMap()) }
    /** 名字表这次取到了没有（null = 取到了；非 null = 失败原因）。 */
    var customNamesError by remember { mutableStateOf<String?>(null) }

    /**
     * 组装 id → 展示名。
     *
     * 两个来源，按可靠度排：
     *  1. 提供商记录自己的 `name`（权威）；
     *  2. **账号的展示名**（兜底）—— 创建自定义家时，provider 的名字同时
     *     作为首个账号的备注名落进账号库（见内核 `create_custom_provider`），
     *     所以账号名能救回「provider 记录的 name 丢了」的情况。
     *
     * 两者都没有 → 留空（**不拿 id 冒充名字**，见 `labelOf`）。
     */
    fun buildCustomNames(
        records: Map<String, String>,
        accounts: List<AccountDto>,
    ): Map<String, String> {
        val out = LinkedHashMap<String, String>(records)
        // 账号名兜底：取该家**第一个**账号的展示名。账号名在用户改过备注名
        // 之后可能与 provider 名不同 —— 那时第 ① 项已经有真名字了，不会走到
        // 这条兜底，所以「第一个即可」不引入误导。
        for (account in accounts) {
            val pid = account.provider
            if (pid.isBlank() || out.containsKey(pid)) continue
            if (!CustomProviderOps.isCustomProviderId(pid)) continue
            val shown = account.displayName.trim()
            if (shown.isNotEmpty()) out[pid] = shown
        }
        return out
    }

    suspend fun loadCustomNames() {
        val result = CustomProviderOps.listResult()
        if (result.ok) {
            customNames = result.records
                .filter { it.name.isNotBlank() }
                .associate { it.id to it.name }
            customNamesError = null
        } else {
            // 取不到就**保留上一份好数据**（别把已经显示对的名字弄丢），
            // 同时把原因带出去 —— 界面据此提示并给重试入口。
            customNamesError = result.error
        }
    }
    LaunchedEffect(Unit) { loadCustomNames() }
    // 事件驱动刷新：内核在 core 的变更点广播（账号改动 / 自定义家改动），
    // 这里只重拉**本页要的两份**（快照 + 自定义家名字表），不做全局刷新。
    // 与 5s 兜底轮询并存：事件快、轮询保证「事件丢了也不会一直陈旧」。
    LaunchedEffect(Unit) {
        GatewayRepository.bus.collect { kind ->
            when (kind) {
                "accounts_changed" -> GatewayRepository.refreshOnce()
                "providers_changed" -> {
                    loadCustomNames()
                    GatewayRepository.refreshOnce()
                }
            }
        }
    }
    // 最终用的名字表：提供商记录里的名字 + 账号名兜底（**派生**，不额外请求）。
    //
    // 兜底之所以做成派生而不是塞进 state：账号快照每 5 秒来一次，塞进 state
    // 就得跟着重拉一次名字表（白白的往返）。派生则只在真正需要时重算。
    val resolvedNames = remember(customNames, snap.accountList) {
        buildCustomNames(customNames, snap.accountList.orEmpty())
    }
    val labelOf = remember(resolvedNames) {
        { pid: String ->
            resolvedNames[pid]
                // 拿不到名字时**不拿 id 冒充** —— 界面上给一个明确的占位
                // （并有提示说明「名字取不到」），而不是让用户以为自家的家
                // 就叫 `custom-7a3f9c21`。
                ?: if (CustomProviderOps.isCustomProviderId(pid)) "（名称未取到）" else PresetRegistry.labelOf(pid)
        }
    }
    // ── 分组：有账号的家按「偏好顺序 → PresetRegistry 顺序」；无账号家不出现 ──
    // ⚠️ 必须是 state 而不是 remember 只读一次：拖拽提交后要立刻让 `groups`
    // 按新顺序重算。只读一次的写法在「提交 → 快照刷新 → groups 重建」时会
    // 用旧顺序覆盖 orderState，表现为「拖完过一会儿自己弹回去」。
    var savedOrder by remember { mutableStateOf(GatewayPrefs.providerOrder(ctx)) }
    // ⚠️ key 里要用 `resolvedNames`（含账号名兜底的那个派生值），不是
    // `customNames` —— 否则兜底生效时 `groups` 不会重算，名字还是旧的。
    val groups = remember(snap.accountList, resolvedNames, savedOrder) {
        val byProvider = snap.accountList?.groupBy { it.provider } ?: emptyMap()
        val seq = ArrayList<ProviderGroupView>(byProvider.size)
        val seen = HashSet<String>()
        savedOrder.forEach { pid ->
            byProvider[pid]?.let { list ->
                val preset = PresetRegistry.ALL.find { it.id == pid }
                seq += ProviderGroupView(
                    pid, preset?.label ?: labelOf(pid), preset, list,
                    customName = resolvedNames[pid].orEmpty(),
                )
                seen += pid
            }
        }
        PresetRegistry.ALL.forEach { preset ->
            if (preset.id !in seen) {
                byProvider[preset.id]?.let { list ->
                    seq += ProviderGroupView(preset.id, preset.label, preset, list)
                    seen += preset.id
                }
            }
        }
        byProvider.forEach { (pid, list) ->
            if (pid !in seen) {
                seq += ProviderGroupView(
                    pid, labelOf(pid), null, list,
                    customName = resolvedNames[pid].orEmpty(),
                )
            }
        }
        seq
    }

    var query by remember { mutableStateOf("") }
    var filter by remember { mutableStateOf(0) }
    // 顺序的唯一事实源（列表渲染、筛选、落盘都读它）
    val orderState = remember { mutableStateListOf<ProviderGroupView>().also { l -> l.addAll(groups) } }
    // 拖拽状态机（移植自参考实现 Android-AI-gateway 的 Reorderable.kt）：
    // 长按起拖 → 排序编辑态（5s 无操作自动退出）→ 松手单次提交。
    val reorderState = remember { ReorderState() }
    LaunchedEffect(groups) {
        if (reorderState.draggingKey == null && !reorderState.isSortingMode) {
            orderState.clear()
            orderState.addAll(groups)
        }
    }

    // ── 列表派生（必须在 [commitProviderOrder] **之前**声明）──────────────
    // 提交入口要读 [listItems] 把「列表下标」换算成「全局下标」（筛选态下
    // 两者不同，见那边的说明）。Kotlin 的局部函数不能前向引用后声明的局部
    // 变量，所以这两段派生挪到提交入口上面 —— 顺序是语义的一部分，不是随意摆。
    val visible = orderState.filter { g ->
        (query.isBlank() || g.label.contains(query, ignoreCase = true) || g.providerId.contains(query, ignoreCase = true)) &&
            when (filter) {
                1 -> g.hasWarn
                2 -> !g.hasWarn
                else -> true
            }
    }
    // 排序中必须显示**全部**家：拖拽的落点下标是相对真实顺序算的，
    // 拿筛选后的子集去拖，松手会落到一个「看起来对、实际错」的位置。
    // （注意：这个切换发生在**长按起拖之后**，起拖那一刻列表内容会从子集
    //  扩成全量。跟手位移有屏幕坐标补偿兜底，落点换算则交给提交入口。）
    val listItems = if (reorderState.isSortingMode) orderState else visible
    val warnCount = orderState.count { it.hasWarn }
    val okCount = orderState.size - warnCount

    /**
     * 拖拽/置顶共用的**唯一提交入口**：把 [key] 挪到 [to] 并落盘。
     *
     * 只写「显示顺序」（`GatewayPrefs.providerOrder`）—— 它不影响转发选路，
     * 选路按各账号的 priority（见账号卡的置顶，那条才是真的出战顺序）。
     * 这一点必须说清：两个层级的「置顶」语义不同，混起来会让人以为
     * 「把某家拖到第一个就会优先走它」。
     *
     * ── [to] 是**列表下标**，必须先换成全局下标（筛选态下两者不同）────────
     * 排序态总会渲染全量 `orderState`，所以列表下标 = 全局下标，上面那句
     * 看起来不成立。但**筛选态下拖拽的落点是按「筛选后子集」算出来的**
     * （`listItems` 那时还是子集），直接拿去 `orderState` 里挪就落错了家：
     * 例如筛出「有异常」3 家、把其中第 1 拖到第 3，全局里那个下标对应的
     * 可能是完全不相干的一家。所以这里按 key 把「落在谁前面」换算成全局位置。
     */
    fun commitProviderOrder(key: String, to: Int) {
        val from = orderState.indexOfFirst { it.providerId == key }
        if (from < 0) return
        val items = listItems
        if (to !in items.indices) return
        // ⚠ 方向必须用**同一坐标系**判断。`to` 是列表下标、`from` 是全局下标，
        // 筛选态下两者不能直接比大小 —— 原来写 `to > from` 是把两个坐标混用，
        // 在筛选子集里会判反方向，落点跟着错一格。
        // 先取落点 key，再在**全局**里比一次：它比被拖项靠后就是向后拖。
        // 落点含义：拖到 items[to] 这个位置，即**插到「落点项」原本占据的那一格**。
        // 向后拖（落点在全局里更靠后）：落点项及其前面的项要往前补一格，
        // 所以插入下标直接取它的下标；实际效果是「越过它」。
        // 向前拖（落点在全局里更靠前）：插到它前面，下标也取它自己的下标。
        // 两种情形同为 `anchor`，差异由 removeAt(from) 的先后次序自然吃掉
        // （remove 在 add 之前执行，from < anchor 时无需减一）。
        val anchorKey = items[to].providerId
        if (anchorKey == key) return
        val anchor = orderState.indexOfFirst { it.providerId == anchorKey }
        if (anchor < 0) return
        val realTo = anchor.coerceIn(0, orderState.lastIndex)
        if (realTo == from) return
        orderState.add(realTo, orderState.removeAt(from))
        val ids = orderState.map { it.providerId }
        savedOrder = ids            // 让 groups 按新顺序重建（见 savedOrder 的说明）
        GatewayPrefs.setProviderOrder(ctx, ids)
    }

    /**
     * 一键置顶（提供商）：挪到显示顺序第一位。只影响展示，不影响选路。
     *
     * ⚠ 提交完**必须**再让列表滚到该项。只改顺序的话 `LazyListState` 会继续
     * 钉住「原来那个第一个可见项」，被置顶的卡落到视口**上方**——用户看到的
     * 第一张还是排序前那张，非得手动滑一下才看得到刚置顶的卡（用户报的第 3 条）。
     */
    fun pinProvider(providerId: String) {
        commitProviderOrder(providerId, 0)
        reorderState.requestScrollToTop(providerId)
    }

    /**
     * 账号拖拽/置顶的提交入口。
     *
     * ── 为什么要把「本家新序」摊回**全局队列** ──────────────────
     * 内核 `/api/accounts/reorder` 要的是**全部账号的一个排列**（队列是全局一条
     * 按 priority 升序的链，分组只是展示口径）。所以只交本家那几个 id 会被
     * 400 拒（「队列不完整」）。做法是：保持别家账号在队列里的**位置不动**，
     * 把本家账号按新顺序填回它原本占的那些槽位 —— 跨家交错关系因此不被打乱。
     */
    fun commitAccountOrder(key: String, to: Int, current: List<AccountDto>) {
        val local = current.map { it.id }.toMutableList()
        val from = local.indexOf(key)
        if (from < 0 || to !in local.indices || from == to) return
        local.add(to, local.removeAt(from))
        val all = snap.accountList?.map { it.id } ?: return
        val localSet = local.toSet()
        if (all.count { it in localSet } != local.size) return
        val iter = local.iterator()
        val queue = all.map { if (it in localSet) iter.next() else it }
        scope.launch {
            val r = AccountOps.reorder(queue)
            if (r.ok) {
                GatewayRepository.refreshOnce()
            } else {
                Toast.makeText(ctx, r.error ?: "排序保存失败", Toast.LENGTH_SHORT).show()
            }
        }
    }


    Column(Modifier.fillMaxSize().background(C.bg)) {
        // ── 页头（v44 .header-bar：菜单 + 标题 + 排序/添加）──
        AgatePageHeader(title = "提供商", onOpenDrawer = onOpenDrawer) {
            AgateHeaderAction(if (reorderState.isSortingMode) "完成" else "⇅ 排序") {
                // 进入 / 退出排序编辑态。顺序本来就在每次提交时落盘了，这里
                // 不需要「保存」这一步 —— 按钮只是显式的开关（长按卡片同样能进入）。
                if (reorderState.isSortingMode) reorderState.exitSortingMode()
                else reorderState.enterSortingMode()
            }
            AgatePrimaryAction("+ 添加") { onOpenAdd() }
        }

        // ── 头部区块：**高度恒定**，排序态只替换最后一行的文案 ──────────────
        // ⚠ 这里原本是 `if (排序态) 横幅 else 搜索栏+筛选栏`，两个分支高度差 80dp
        // 左右。排序态一开（长按起拖那一刻就开），列表整体被顶上去，而被拖卡片的
        // 跟手位移是按「列表内容坐标」算的 —— 列表自己在屏幕上动了，内容坐标里
        // 槽位却没变，补偿量算成 0，卡片就留在原地、内容上移，观感是
        // 「按住的卡片往上一跳、不跟手」（用户报的第 2 条）。
        // 现在搜索栏与筛选栏**常驻**，只把底部那行提示换成排序态文案，
        // 行高一致 → 列表不重排 → 卡片天然跟手；跟手位移另有一层屏幕坐标补偿
        // （ReorderState.lazyTranslationY 的 listTopDelta），双保险。
        Column(Modifier.padding(horizontal = 16.dp)) {
            Spacer(Modifier.height(6.dp))
            // ⚠️ 名字表取不到时的提示条（2026-10-30 加）
            //
            // 为什么必须有：名字表空着的时候，每个自定义家都会显示成
            // `custom-xxxx`（id）—— 用户看到的是「我建的家全变成乱码了」，
            // 而界面**没有任何一处**说明发生过什么。一次临时的请求失败被
            // 伪装成了「数据就是这样」，用户既没法判断是哪家坏了，也没法
            // 知道重试一下就好了。
            //
            // 所以：取不到就把原因说出来，并给一个重试入口。
            val namesError = customNamesError
            if (namesError != null) {
                Row(
                    Modifier
                        .fillMaxWidth()
                        .background(C.amber.copy(alpha = 0.10f), RoundedCornerShape(10.dp))
                        .padding(horizontal = 10.dp, vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        "部分自定义提供商的名称暂时取不到（$namesError），列表里显示的是占位名",
                        color = C.amber,
                        fontSize = 11.sp,
                        modifier = Modifier.weight(1f),
                    )
                    Text(
                        "重试",
                        color = C.accent,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier
                            .clickable {
                                scope.launch { loadCustomNames() }
                            }
                            .padding(horizontal = 8.dp, vertical = 3.dp),
                    )
                }
                Spacer(Modifier.height(8.dp))
            }
            AgateSearchBar(value = query, onValueChange = { query = it }, hint = "搜索提供商")
            Spacer(Modifier.height(8.dp))
            AgateSegmented(
                options = listOf("全部 · ${orderState.size}", "有异常 · $warnCount", "正常 · $okCount"),
                selected = filter,
                onSelect = { filter = it },
            )
            Spacer(Modifier.height(6.dp))
            // ★ 两种状态**共用同一套布局骨架**，只换文字、颜色与「底/边」的绘制。
            //   关键：`Modifier.background` 与 `Modifier.border` **不参与测量**，
            //   所以排序态的琥珀底 + 描边不会改变这一行的高度；padding、圆点、
            //   间距两种状态完全一致 → 这一行高度恒定 → 列表不会重排。
            //   （此前是 if/else 两个结构不同的分支，光靠「字号调小」凑等高是
            //   靠不住的：字体缩放、文案换行任何一处变化都会重新引入跳动。）
            Row(
                Modifier
                    .fillMaxWidth()
                    .then(
                        if (reorderState.isSortingMode) {
                            // v44 .reorder-banner：amber 虚线横幅（纯绘制，零布局影响）
                            Modifier
                                .background(C.amber.copy(alpha = 0.08f), RoundedCornerShape(10.dp))
                                .border(1.dp, C.amber.copy(alpha = 0.4f), RoundedCornerShape(10.dp))
                        } else {
                            Modifier
                        },
                    )
                    .padding(horizontal = 8.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                // 圆点在两种状态都占位（4dp < 行高，不影响行高），普通态只是暗色小点
                Box(
                    Modifier
                        .size(4.dp)
                        .background(if (reorderState.isSortingMode) C.amber else C.textSub.copy(alpha = 0.45f), CircleShape),
                )
                Text(
                    text = if (reorderState.isSortingMode) {
                        "排序中 · 拖动卡片 · 松手即保存 · 5 秒无操作自动退出"
                    } else {
                        "点头部展开账号 · 长按卡片或点「⇅ 排序」调整顺序 · 「管理」进详情"
                    },
                    color = if (reorderState.isSortingMode) C.amber else C.textSub,
                    fontSize = 11.sp,
                    maxLines = 1,
                    overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                )
            }
        }

        // ── 卡片列表 ──
        if (orderState.isEmpty()) {
            Column(
                Modifier.fillMaxSize().padding(32.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Center,
            ) {
                Text("还没有任何账号", color = C.textSub, fontSize = 14.sp)
                Spacer(Modifier.height(8.dp))
                Text(
                    "+ 添加首个提供商",
                    color = C.onAccent,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .background(C.accent, RoundedCornerShape(10.dp))
                        .clickable { onOpenAdd() }
                        .padding(horizontal = 16.dp, vertical = 10.dp),
                )
            }
        } else {
            // 拖拽排序（移植自参考实现）：长按卡片起拖、松手单次提交，
            // 排序编辑态下非首项右侧出现「置顶」圆按钮。
            // 与旧实现（页头进入排序模式 + 压成单行的 AgateDragList）的差别：
            // 拖的是**卡片本身**，不用先切形态，视觉上下文也不会跳。
            ReorderableLazyColumn(
                items = listItems,
                keyOf = { it.providerId },
                state = reorderState,
                onCommit = { key, toIndex -> commitProviderOrder(key, toIndex) },
                modifier = Modifier.fillMaxSize().padding(horizontal = 16.dp),
                contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = 10.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) { g ->
                val idx = orderState.indexOfFirst { it.providerId == g.providerId }
                ProviderCard(
                    g = g,
                    heads = snap.currentAccountIds,
                    metrics = metrics,
                    inflight = inflight,
                    mult = multByProvider[g.providerId],
                    checkinSupported = g.providerId in checkinProviders,
                    usageCapableProviders = snap.providerUsage,
                    dragState = reorderState,
                    isFirst = idx <= 0,
                    onPinToTop = {
                        pinProvider(g.providerId)
                        // 置顶完成 = 一次排序操作结束 → 自动退出编辑态
                        reorderState.exitSortingMode()
                    },
                    onCommitAccountOrder = { key, to ->
                        commitAccountOrder(key, to, g.accounts)
                    },
                    onOpenDetail = { onOpenDetail(g.providerId) },
                    onOpenQuotaCenter = onOpenQuotaCenter,
                    onOpenLimiter = onOpenLimiter,
                )
            }
        }
    }
    // ── 额度设限弹窗 ──
    // 只在用户点了某账号的入口后出现；保存走 PATCH /api/accounts 的 `limiters`
    // （整块替换），内核归一化后同步旧 `lowBalance` 供降级兼容。
    limiterTarget?.let { acc ->
        AccountLimiterDialog(
            accountName = acc.displayName,
            current = limiterRulesOf(acc.raw),
            onDismiss = { limiterTarget = null },
            onSave = { result ->
                limiterTarget = null
                scope.launch {
                    // 整块替换：内核 `apply_patch` 归一化校验后同步旧 lowBalance；
                    // 空数组 = 不限制（与 3.0.8「没有生效的规则就是不限制」一致）
                    val body = JSONObject()
                    body.put("limiters", JSONArray().apply { result.forEach { put(it.toJson()) } })
                    val r = AccountOps.update(acc.id, body)
                    Toast.makeText(
                        ctx,
                        if (r.ok) (if (result.isEmpty()) "已取消额度限制" else "已设置额度限制")
                        else (r.error ?: "保存失败"),
                        Toast.LENGTH_SHORT,
                    ).show()
                    if (r.ok) GatewayRepository.refreshOnce()
                }
            },
        )
    }
}

/* ── 折叠卡（v44 .prov-sort）：头部常显；collapsed→摘要行；展开→acct 块 ── */
@Composable
private fun ProviderCard(
    g: ProviderGroupView,
    heads: Map<String, String>,
    metrics: Map<String, AcctMetric>,
    inflight: Map<String, Int>,
    mult: Pair<Double, Double>?,
    checkinSupported: Boolean,
    /** provider id → 该家是否支持余额查询（内核能力投影）。 */
    usageCapableProviders: Map<String, Boolean>,
    /** 拖拽状态机（长按卡片起拖；排序编辑态下显示置顶按钮）。 */
    dragState: ReorderState? = null,
    /** 是否已经是显示顺序的第一个（是则不显示置顶按钮）。 */
    isFirst: Boolean = true,
    onPinToTop: () -> Unit = {},
    /** 账号拖拽落点提交（providerId 由调用方闭包带）。 */
    onCommitAccountOrder: (key: String, toIndex: Int) -> Unit = { _, _ -> },
    onOpenDetail: () -> Unit,
    onOpenQuotaCenter: () -> Unit = {},
    onOpenLimiter: (AccountDto) -> Unit = {},
) {
    val C = agateColors()
    var expanded by remember(g.providerId) { mutableStateOf(false) }
    // 账号层的拖拽状态：一家一个（各家顺序互不影响，共用一份会让两家的 key 串台）
    val accountState = remember(g.providerId) { ReorderState() }
    // 排序编辑态的位次（读拖拽状态机的本地乐观序，换位时实时变化）
    val orderRank = dragState?.keys?.indexOf(g.providerId)?.takeIf { it >= 0 } ?: 0
    // 这一家有没有余额能力（决定要不要显示「额度限设」入口）
    val usageCapable = usageCapableProviders[g.providerId] ?: false
    // ── 就绪数 vs 账号总数 ────────────────────────────────────
    // 两者不是一回事，这是「N 就绪」徽章存在的理由：
    //   账号总数 = 这家名下所有账号（含停用、缺凭证的）；
    //   就绪    = 队列里**真正能用**的：启用 + 有凭证 + 未被低余额规则跳过 + 无查询失败。
    // 「余额低」只有配了跳过规则时才算不可用（只是徽章提示的不影响选号）。
    // 该家不支持余额查询时，就绪 = 启用 + 有凭证（不看余额）。
    val readyCount = g.accounts.count { acc ->
        if (!acc.enabled || !acc.hasCredentials) return@count false
        if (!usageCapable) return@count true
        val m = metrics[acc.id] ?: return@count true   // 还没查过：不因此判为不可用
        if (m.failed) return@count false
        val skippedByBalance = limiterRulesOf(acc.raw).any { rule ->
            rule.enabled && rule.type == "balance" && rule.action == "skip" &&
                m.amount != null && m.amount < rule.threshold
        }
        !skippedByBalance
    }

    // ── 这家整体的读数（折叠态那一行）─────────────────────────
    // 1) 累加余额：只累加**读到了数值**的账号（同家单位一致，可相加）；
    // 2) 最近到期：全部账号里最早到期的那一份，带上它自己的剩余量；
    //    日包也在内 —— 它的额度是真实可用的，到期即重置（文案落到「今日到期」）。
    val mine = g.accounts.mapNotNull { metrics[it.id] }
    val amounts = mine.mapNotNull { it.amount }
    val totalAmount = if (amounts.isEmpty()) null else amounts.sum()
    val balanceUnit = mine.firstNotNullOfOrNull { it.unit }
    val failedCount = mine.count { it.failed }
    val headExpiry = mine.filter { it.expiryAt != null }.minByOrNull { it.expiryAt!! }

    // 2.5) 水位条只在有真比例时画 —— 无数据画满格会被读成「额度充足」
    val avgPct = mine.mapNotNull { it.pct }.takeIf { it.isNotEmpty() }?.average()?.toInt()

    // 3) 副行：常态「N 个账号 · 最低 ×倍率」；有问题时整行换成待办
    val minMult = mult?.first
    val pending = buildList {
        if (failedCount > 0) add("$failedCount 个查询失败")
        val lowCount = g.accounts.count { it.lowBalance }
        if (lowCount > 0) add("$lowCount 个余额低")
        val noCred = g.accounts.count { !it.hasCredentials }
        if (noCred > 0) add("$noCred 个缺凭证")
        val soonest = g.accounts.mapNotNull { credentialExpiryMs(it) }.minOrNull()
        val leftMs = soonest?.minus(System.currentTimeMillis())
        if (leftMs != null && leftMs <= 3L * 86_400_000L) {
            add("凭证${expiryPhrase(leftMs).replace("到期", "")}到期")
        }
    }
    val subLine = if (pending.isNotEmpty()) pending.joinToString(" · ")
    else buildString {
        append("${g.accounts.size} 个账号")
        minMult?.let { append(" · 最低 ×${fmtMult(it)}") }
    }

    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
            .border(
                1.dp,
                if (g.hasWarn) C.amber.copy(alpha = 0.45f) else C.border,
                RoundedCornerShape(20.dp),
            )
            // 长按起拖（与参考实现同款：挂在卡片根上，长按成立前不消费 down，
            // 因此卡内的「管理」「明细」等 clickable 照常可用）。
            // 展开态不挂 —— 展开区里有账号卡，长按该由账号卡自己接管。
            .then(
                if (dragState != null && !expanded) {
                    Modifier.dragHandle(
                        key = g.providerId,
                        state = dragState,
                        onStart = { dragState.startLazy(g.providerId) },
                    )
                } else {
                    Modifier
                },
            )
            .clickable {
                // 拖拽抬手后 350ms 内的点击是手势残留，必须吞掉，
                // 否则「长按排序」会顺手把卡片展开/收起
                if (dragState?.shouldIgnoreClick() == true) return@clickable
                // 排序编辑态下点卡片 = 退出排序（此时不该展开，
                // 也不能什么都不做 —— 那会让人不知道怎么退出去）
                if (dragState?.isSortingMode == true) dragState.exitSortingMode()
                else expanded = !expanded
            }
            .padding(14.dp)
            .animateContentSize(),
    ) {
        // ── 第 1 行：头像 + 名称 + 就绪徽章 + 管理 + 折叠箭头 ──
        // 名称独占剩余宽度并单行省略：数值挪到第 3 行，不再和名称抢位置
        //（旧版挤在一行，窄屏上名称会被截成「WorkBuddy...」）。
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box {
                // `customName`：自定义家（`custom-xxxx`）按名字反查预置模板的 logo，
                // 否则商汤日日新这类从模板建出来的家只剩两字母色块（见 ProviderIcons）
                AgateProviderAvatar(
                    providerId = g.providerId,
                    label = g.label,
                    size = 38.dp,
                    customName = g.customName,
                )
                Box(
                    Modifier
                        .align(Alignment.BottomEnd)
                        .size(11.dp)
                        .background(if (g.hasWarn) C.amber else C.green, CircleShape)
                        .border(2.dp, C.cardBg, CircleShape),
                )
            }
            Spacer(Modifier.width(9.dp))
            // 排序编辑态的位次角标：这个数字就是**拖动后的最终顺序**，
            // 让用户看得见「拖完会变成第几」。
            if (dragState?.isSortingMode == true) {
                Box(
                    Modifier
                        .size(22.dp)
                        .background(C.accent, CircleShape),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        "${orderRank + 1}",
                        color = C.onAccent,
                        fontSize = 11.sp,
                        fontWeight = FontWeight.Bold,
                    )
                }
                Spacer(Modifier.width(8.dp))
            }
            Text(
                g.label,
                color = C.textMain,
                fontSize = 16.sp,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
            // 自定义家（含预置 API 建出来的）挂一枚角标：它们在注册表里没有位置，
            // 不标的话「NewAPI 中转」这种名字与内置的 24 家混在一起分不出来。
            if (CustomProviderOps.isCustomProviderId(g.providerId)) {
                Spacer(Modifier.width(6.dp))
                AgateBadge("自定义", BadgeTone.BLUE)
            }
            Spacer(Modifier.width(8.dp))
            // 就绪徽章：有账号掉队时写 n/N（分子可用 / 分母总数）
            if (g.accounts.isNotEmpty()) {
                if (readyCount == g.accounts.size) {
                    AgateBadge("$readyCount 就绪", BadgeTone.GREEN)
                } else {
                    AgateBadge(
                        "$readyCount/${g.accounts.size} 就绪",
                        if (readyCount == 0) BadgeTone.RED else BadgeTone.AMBER,
                    )
                }
            }
            Spacer(Modifier.weight(1f))
            // 一键置顶：只在排序编辑态、且不是第一个时出现（第一个再「置顶」没有意义）。
            // 语义说明：这一层改的是**显示顺序**（不影响转发选路）；真正的出战顺序
            // 在账号卡上（那里改的是 priority）。
            if (dragState?.isSortingMode == true && !isFirst) {
                PinToTopButton(onClick = onPinToTop, dragState = dragState)
                Spacer(Modifier.width(6.dp))
            }
            Text(
                "管理",
                color = C.accent,
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .background(C.accent.copy(alpha = 0.10f), RoundedCornerShape(9.dp))
                    .clickable(onClick = onOpenDetail)
                    .padding(horizontal = 10.dp, vertical = 5.dp),
            )
            Spacer(Modifier.width(2.dp))
            Icon(
                Icons.Filled.KeyboardArrowDown,
                contentDescription = if (expanded) "收起" else "展开",
                tint = C.textDim,
                modifier = Modifier.size(18.dp).rotate(if (expanded) 180f else 0f),
            )
        }

        // ── 第 2、3 行：缩进到与名称左对齐，各自独占整行 ──
        // 缩进 38+9=47dp，与头像右侧的名称起点对齐
        Spacer(Modifier.height(3.dp))
        Column(Modifier.padding(start = 47.dp)) {
            // 第 2 行：状态副行（常态是账号数 + 最低倍率；有问题时换成待办）
            Text(
                subLine,
                color = if (pending.isNotEmpty()) C.amber else C.textSub,
                fontSize = 12.sp,
                maxLines = 1,
                overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
            )
            // 第 3 行：额度与最近到期**同一行**（不再换行）
            val amountText = totalAmount?.let { fmtNum(it) + (balanceUnit?.let { u -> " $u" } ?: "") }
            val expText = headExpiry?.let { exp ->
                val remain = exp.expiryAmount?.let { "${fmtNum(it)} · " } ?: ""
                remain + expiryPhrase(exp.expiryAt!! - System.currentTimeMillis())
            }
            if (amountText != null || expText != null) {
                Spacer(Modifier.height(2.dp))
                Row(verticalAlignment = Alignment.Bottom) {
                    if (amountText != null) {
                        Text(
                            amountText,
                            color = C.textMain,
                            fontSize = 15.sp,
                            fontWeight = FontWeight.SemiBold,
                            maxLines = 1,
                            overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                        )
                    }
                    if (amountText != null && expText != null) Spacer(Modifier.width(9.dp))
                    if (expText != null) {
                        val leftMs = headExpiry!!.expiryAt!! - System.currentTimeMillis()
                        Text(
                            expText,
                            color = when {
                                leftMs <= 0 -> C.red
                                leftMs <= 3L * 86_400_000L -> C.red
                                leftMs <= 7L * 86_400_000L -> C.amber
                                else -> C.textDim
                            },
                            fontSize = 12.sp,
                            maxLines = 1,
                            overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                        )
                    }
                }
            }
        }

        if (!expanded) {
            // 水位条：**只在有真比例时画**。旧实现在无数据时按「启用账号占比」画
            // 一条满格绿条 —— 那会被读成「额度充足」，是最误导的一处。
            if (avgPct != null) {
                Spacer(Modifier.height(9.dp))
                AgateProgress(
                    fraction = avgPct / 100f,
                    thick = false,
                    fillColor = when {
                        avgPct >= 50 -> C.green
                        avgPct >= 20 -> C.accent
                        else -> C.amber
                    },
                )
            }
        } else {
            // v44 .acct 块 —— 账号层同样可拖拽排序。
            // 顺序 = 内核 priority 升序 = **选号队列位次**，所以这里的「置顶」
            // 是真的「优先出战」（与提供商层那个只管显示顺序的置顶不同）。
            Spacer(Modifier.height(10.dp))
            ReorderableColumn(
                items = g.accounts,
                keyOf = { it.id },
                state = accountState,
                onCommit = { key, to -> onCommitAccountOrder(key, to) },
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) { acc, index, isTarget ->
                AcctBlock(
                    acc = acc,
                    rank = index + 1,
                    isHead = heads[g.providerId] == acc.id,
                    metric = metrics[acc.id],
                    inflight = inflight[acc.id] ?: 0,
                    usageCapable = usageCapable,
                    dragState = accountState,
                    isFirst = index == 0,
                    isTarget = isTarget,
                    onPinToTop = {
                        onCommitAccountOrder(acc.id, 0)
                        accountState.exitSortingMode()
                    },
                    onOpenQuotaCenter = onOpenQuotaCenter,
                    onOpenLimiter = onOpenLimiter,
                )
            }
        }
    }
}

/* ── v44 .acct：序号 + 名称 + 状态；余额行 + 细条 + 在途并发点 ── */
@Composable
private fun AcctBlock(
    acc: AccountDto,
    rank: Int,
    isHead: Boolean,
    metric: AcctMetric?,
    inflight: Int,
    usageCapable: Boolean,
    /** 拖拽状态机（账号层）。null = 不可拖（本页所有账号卡都传，留默认值便于预览）。 */
    dragState: ReorderState? = null,
    isFirst: Boolean = true,
    /** 是否被拖拽项选为落点（高亮描边）。 */
    isTarget: Boolean = false,
    onPinToTop: () -> Unit = {},
    onOpenQuotaCenter: () -> Unit = {},
    onOpenLimiter: (AccountDto) -> Unit = {},
) {
    val C = agateColors()
    val cap = acc.raw.optInt("maxConcurrent", 0)
    Column(
        Modifier
            .fillMaxWidth()
            .background(C.insetBg, RoundedCornerShape(12.dp))
            // 落点高亮：拖拽中目标卡描边，让「会落到哪」一眼可见
            .then(
                if (isTarget) {
                    Modifier.border(1.5.dp, C.accent, RoundedCornerShape(12.dp))
                } else {
                    Modifier
                },
            )
            // 长按整张账号卡起拖。账号卡没有自己的点击动作，所以不必像
            // 提供商卡那样区分「点击 vs 长按」；页面是 LazyColumn，
            // 这里用 Column 变体（startColumn + 无页面级自动滚动）。
            .then(
                if (dragState != null) {
                    Modifier.dragHandle(
                        key = acc.id,
                        state = dragState,
                        onStart = { dragState.startColumn(acc.id, 0f) },
                    )
                } else {
                    Modifier
                },
            )
            .padding(horizontal = 12.dp, vertical = 10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            // 排序编辑态下位次反白显示：这个数字就是拖动后的最终队列位次
            if (dragState?.isSortingMode == true) {
                Box(
                    Modifier.size(20.dp).background(C.accent, CircleShape),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        "$rank",
                        color = C.onAccent,
                        fontSize = 10.5.sp,
                        fontWeight = FontWeight.Bold,
                    )
                }
            } else {
                Text("#$rank", color = C.textDim, fontSize = 12.sp)
            }
            Spacer(Modifier.width(8.dp))
            Text(
                (if (isHead) "★ " else "") + acc.displayName,
                color = C.textMain,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f),
            )
            // 一键置顶（账号层）= 优先出战：改的是内核 priority，不只是显示顺序
            if (dragState?.isSortingMode == true && !isFirst) {
                PinToTopButton(onClick = onPinToTop, dragState = dragState)
                Spacer(Modifier.width(6.dp))
            }
            when {
                !acc.enabled -> AgateBadge("停用", BadgeTone.GRAY)
                !acc.hasCredentials -> AgateBadge("缺凭证", BadgeTone.RED)
                acc.lowBalance -> AgateBadge("余额低", BadgeTone.AMBER)
                else -> AgateBadge("就绪", BadgeTone.GREEN)
            }
        }

        Spacer(Modifier.height(6.dp))
        // ── 余额 ──
        // 读数行只放「还剩多少 + 单位」，不再重复拼「套餐 0 · 奖励 0」（那截长文字
        // 会挤压读数）；额度包明细去点「明细 ›」。
        val balanceText = metric?.amount?.let { fmtNum(it) + (metric.unit?.let { u -> " $u" } ?: "") }
            ?: metric?.text
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text("余额", color = C.textSub, fontSize = 12.sp, modifier = Modifier.weight(1f))
            Text(
                balanceText ?: "未查询",
                color = when {
                    metric?.failed == true -> C.red
                    balanceText == null -> C.textDim
                    else -> C.textMain
                },
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(2f, fill = false),
                textAlign = androidx.compose.ui.text.style.TextAlign.End,
                maxLines = 1,
                overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
            )
            Spacer(Modifier.size(6.dp))
            Text(
                "明细 ›",
                color = C.accent,
                fontSize = 11.sp,
                modifier = Modifier
                    .clickable { onOpenQuotaCenter() }
                    .padding(horizontal = 2.dp, vertical = 2.dp),
            )
        }

        // ── 凭证：有效期与自动刷新合并一行 ──
        // 字段名按家不同（`expiresAt` / `tokenExpiresAt`），两个都认；
        // 没有该字段的家（kuku / antigravity / chatglm）整行不渲染。
        credentialExpiryMs(acc)?.let { at ->
            val leftMs = at - System.currentTimeMillis()
            Row(Modifier.fillMaxWidth().padding(top = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                Text("凭证", color = C.textSub, fontSize = 12.sp, modifier = Modifier.weight(1f))
                Text(
                    expiryPhrase(leftMs) + " · 自动刷新",
                    color = when {
                        leftMs <= 0 -> C.red
                        leftMs <= 24L * 3_600_000L -> C.red
                        leftMs <= 3L * 86_400_000L -> C.amber
                        else -> C.textSub
                    },
                    fontSize = 12.sp,
                    maxLines = 1,
                    overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                )
            }
        }

        // ── 额度设限：**配置了才显示** ──
        // 两个条件都满足才渲染：该家支持余额查询 + 该账号确实配了规则。
        // 没配就不占一行（常态界面更干净）；想设限的入口在详情页「设置」。
        if (usageCapable) {
            limiterRulesOf(acc.raw).firstOrNull()?.let { rule ->
                LimiterEntryRow(
                    summary = rule.summary(),
                    onOpen = { onOpenLimiter(acc) },
                )
            }
        }
        Spacer(Modifier.height(4.dp))
        AgateProgress(
            fraction = (metric?.pct ?: 0) / 100f,
            thick = false,
            fillColor = when {
                metric?.pct == null -> C.border
                metric.pct >= 50 -> C.green
                metric.pct >= 20 -> C.accent
                else -> C.amber
            },
        )
        if (metric?.failed == true && cap >= 0) {
            // 查询失败时给一行说明，不画在途点（信息已在上方）
            Spacer(Modifier.height(6.dp))
            Text(metric.text ?: "", color = C.red, fontSize = 11.sp, maxLines = 1)
        }
        Spacer(Modifier.height(6.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                "在途 $inflight" + if (cap > 0) "/$cap" else "",
                color = C.textDim,
                fontSize = 11.sp,
            )
            if (cap in 1..6) {
                Spacer(Modifier.width(8.dp))
                repeat(cap) { i ->
                    Box(
                        Modifier
                            .padding(end = 3.dp)
                            .size(6.dp)
                            .background(if (i < inflight) C.green else C.border, CircleShape),
                    )
                }
            }
        }
    }
}

/** 余额百分比/文案：usage 各家字段不同，按候选键取值。 */
internal fun parseUsageMetric(o: JSONObject?): AcctMetric {
    if (o == null) return AcctMetric()

    fun d(vararg ks: String): Double? = ks.firstNotNullOfOrNull { k ->
        // 布尔不能当数字（JSONObject.getBoolean 是真假两值；这里只认数字与数字串）
        val raw = o.opt(k) ?: return@firstNotNullOfOrNull null
        when (raw) {
            is Number -> raw.toDouble().takeIf { it.isFinite() }
            is String -> raw.trim().toDoubleOrNull()?.takeIf { it.isFinite() }
            else -> null
        }
    }

    // ── 形状 1：WorkBuddy 既有契约 `{kind, unlimited, totalLeft, planLeft, bonusLeft}` ──
    // 这个形状**没有** total/used 一类字段，当年必须在这里单开一支；否则
    // `totalLeft` 认不出来，界面就恒显示「--」（真机踩到过）。
    // 判据是字段存在性，与内核 `usage_records::extract_remaining` 同源。
    if (o.has("totalLeft")) {
        val unlimited = o.optBoolean("unlimited", false) ||
            (o.opt("unlimited")?.let { it !is Boolean && it.toString().isNotEmpty() } == true)
        val total = d("totalLeft")
        if (unlimited) return AcctMetric(text = "∞ 不限量")

        // 三份明细（总 / 套餐 / 奖励）都是上游给的数，只做展示不做二次计算
        val parts = ArrayList<String>(3)
        total?.let { parts += "共${fmtNum(it)}" }
        d("planLeft")?.let { parts += "套餐${fmtNum(it)}" }
        d("bonusLeft")?.let { parts += "奖励${fmtNum(it)}" }
        val exp1 = extractExpiry(o)
        return when {
            total != null && parts.isNotEmpty() -> AcctMetric(
                text = parts.joinToString(" · "),
                pct = null,
                amount = total,
                unit = o.optString("unit").ifBlank { null },
                expiryAt = exp1?.atMs,
                expiryAmount = exp1?.amount,
            )
            else -> AcctMetric(text = null)
        }
    }

    // ── 形状 2：归一化形状 `{available, availableView, unit, wallets, total, used}` ──
    // `availableView` 是后端给的**紧凑展示串**（如「1亿 token」）；余额列只有
    // 几十像素宽，数字太长会挤爆，有展示串就用它，没有才按数值拼。
    val left = d("available", "left", "remain", "remaining", "balance", "total_granted")
    val total = d("total", "quota", "granted")
    val used = d("used", "usage", "consumed")
    val pct = when {
        total != null && total > 0 && left != null -> ((left / total) * 100).toInt().coerceIn(0, 100)
        total != null && total > 0 && used != null -> (((total - used) / total) * 100).toInt().coerceIn(0, 100)
        else -> null
    }

    // 钱包/额度桶明细：主读数缺失（上游只给比例，如 Accio 的 usagePercent）时，
    // 从有实际读数的钱包里兜一个出来 —— 有可读的就不再显示「可用 —」。
    val wallets = o.optJSONArray("wallets")
    val fallbackWallet = if (left == null && wallets != null) {
        var picked: Pair<Double, String>? = null
        for (i in 0 until wallets.length()) {
            val w = wallets.optJSONObject(i) ?: continue
            val value = w.opt("balance")?.let { raw ->
                when (raw) {
                    is Number -> raw.toDouble()
                    is String -> raw.trim().toDoubleOrNull()
                    else -> null
                }
            } ?: continue
            val unit = w.optString("unit").ifBlank { "" }
            if (picked == null) picked = value to unit
        }
        picked
    } else {
        null
    }

    val unit = o.optString("unit").ifBlank { "" }
    val view = o.optString("availableView").ifBlank { null }
    val text = when {
        view != null -> view
        left != null && pct != null -> "${fmtNum(left)} · $pct%"
        left != null -> "${fmtNum(left)}${if (unit.isNotEmpty()) " $unit" else ""}"
        fallbackWallet != null ->
            "${fmtNum(fallbackWallet.first)}" + if (fallbackWallet.second.isNotEmpty()) " ${fallbackWallet.second}" else ""
        used != null -> "已用 ${fmtNum(used)}"
        else -> null
    }

    // 半次失败：一台网关读到、另一台没读到（CodeArts 的订阅统计 / 福利网关）。
    // 读数是真的但不完整 —— 不能显示成一片绿，否则会被读成「额度用完了」。
    val partial = listOf(
        o.optString("statisticsError"),
        o.optString("benefitError"),
    ).count { it.isNotBlank() }

    // 累加口径：优先 `available`，退回钱包里的实际读数
    val exp2 = extractExpiry(o)
    return AcctMetric(
        text = when {
            text == null -> null
            partial > 0 -> "$text · ⚠ 部分未读到"
            else -> text
        },
        pct = pct,
        amount = left ?: fallbackWallet?.first,
        unit = unit.ifBlank { fallbackWallet?.second ?: "" }.ifBlank { null },
        expiryAt = exp2?.atMs,
        expiryAmount = exp2?.amount,
    )
}

/**
 * 到期文案（全局唯一口径）：`今日到期` / `N 天后到期` / `已过期`。
 *
 * 「0 天后到期」这种说法是机器话 —— 到期当天就该写「今日到期」。
 * 卡片读数行、账号卡的凭证行、额度包明细共用这一个函数，避免几处各写一套。
 */
internal fun expiryPhrase(leftMs: Long): String {
    if (leftMs <= 0) return "已过期"
    val days = leftMs / 86_400_000L
    return if (days <= 0L) "今日到期" else "${days} 天后到期"
}

/**
 * 从 usage 里挑出**最近一份到期**的额度：到期时刻 + 那一份的剩余量。
 *
 * ── 日包按普通包处理（这是刻意的）──────────────────────────
 * 上游确实有「每日赠送 / 日额度」这类包（loomy 的每日赠送积分、workbuddy 的日额度
 * 包、zcode 的 `period == "daily"` 权益、autoclaw 的赠送积分），它们的 `expireAt`
 * 是「今天 24 点」。**但那个包里的额度是真实可用的**（比如「每日刷新给 100 积分」
 * 就是 100），所以它跟普通额度包一样参与「最近到期」的比较与展示，只是到期文案
 * 会落到「今日到期」—— 语义正确：今天用完就重置。
 * 把它排除会让这类账号永远没有到期信息，反而更误导。
 *
 * ── 选取规则 ──────────────────────────────────────────────
 * 优先在「剩余 > 0」的包里挑最早到期的（有余额的才是用户关心的）；
 * 全都没剩余时退回全部里最早到期的那个；再没有未来时刻就取最晚的一个。
 * 递归扫描的理由与额度中心一致：各家 usage 形状不同，但「剩余键」与「到期键」
 * 就那么几个名字在轮转，按字段存在性探测比按 25 家写死映射表稳。
 */
internal data class ExpiryInfo(
    val atMs: Long,
    val amount: Double?,
)

internal fun extractExpiry(usage: JSONObject): ExpiryInfo? {
    val candidates = ArrayList<Pair<Double, Long>>() // (剩余, 到期毫秒)
    scanQuota(usage, candidates, 0)
    if (candidates.isEmpty()) return null
    val now = System.currentTimeMillis()

    val withLeft = candidates.filter { it.first > 0.0 }
    val pool = if (withLeft.isNotEmpty()) withLeft else candidates
    val picked = pool.filter { it.second > now }.minByOrNull { it.second }
        ?: pool.maxByOrNull { it.second }
    return picked?.let { ExpiryInfo(it.second, it.first) }
}

private val EXPIRY_KEYS = listOf(
    "expireAt", "expiresAt", "expiredAt", "endAt", "endTime", "validEndTime",
    "periodEnd", "period_end", "endsAt", "expireTime",
)
private val LEFT_KEYS = listOf(
    "left", "remain", "remaining", "balance", "available", "credits", "points",
    "totalPoint", "totalLeft",
)

private fun scanQuota(
    node: Any?,
    out: MutableList<Pair<Double, Long>>,
    depth: Int,
) {
    if (depth > 6 || node == null) return
    when (node) {
        is JSONObject -> {
            val left = LEFT_KEYS.firstNotNullOfOrNull { k ->
                when (val raw = node.opt(k)) {
                    is Number -> raw.toDouble().takeIf { it.isFinite() }
                    is String -> raw.trim().toDoubleOrNull()?.takeIf { it.isFinite() }
                    else -> null
                }
            }
            val at = EXPIRY_KEYS.firstNotNullOfOrNull { k -> parseEpochMs(node.opt(k)) }
            if (left != null && at != null) out += left to at
            for (key in node.keys()) scanQuota(node.opt(key), out, depth + 1)
        }
        is org.json.JSONArray -> for (i in 0 until node.length()) {
            scanQuota(node.opt(i), out, depth + 1)
        }
    }
}

/**
 * 到期时间三种形态：毫秒数 / 秒数 / 可解析的时间串。
 * `< 1e11` 视为秒（Trae 的凭据从 auth 文件进来是 10 位秒，不归一会得到 1970）。
 */
internal fun parseEpochMs(value: Any?): Long? {
    val raw = when (value) {
        null, JSONObject.NULL -> return null
        is Number -> value.toLong()
        is String -> {
            val text = value.trim()
            if (text.isEmpty()) return null
            text.toLongOrNull() ?: runCatching {
                java.time.OffsetDateTime.parse(text).toInstant().toEpochMilli()
            }.getOrNull() ?: return null
        }
        else -> return null
    }
    if (raw <= 0) return null
    return if (raw < 100_000_000_000L) raw * 1000 else raw
}

/** 倍率展示：整数不带小数，其余去掉多余的 0。 */
internal fun fmtMult(v: Double): String =
    if (v % 1.0 == 0.0) String.format("%.0f", v)
    else String.format("%.2f", v).trimEnd('0').trimEnd('.')

/**
 * 凭证到期时刻（毫秒）。两个字段名都认：内置家里 `expiresAt`（workbuddy /
 * officeace / trae / zcode / qoder / accio / cline …）与 `tokenExpiresAt`
 * （raccoon / autoclaw / catpaw …）各占一半，取值口径一致。
 *
 * **秒与毫秒要归一**（照 3.0.8 的 `expiryMillis`）：落盘的到期值在两者之间漂过
 * —— Trae 的凭据从 CPA auth 文件或手工粘贴进来的是 10 位秒，不归一时
 * `1791009732` 当毫秒读就是 1970-01-21，账号一进面板就红着报「已过期」。
 *
 * 没有该字段（或值不可解析）→ null，界面不渲染这一行。
 */
internal fun credentialExpiryMs(acc: AccountDto): Long? {
    val raw = acc.raw.opt("expiresAt") ?: acc.raw.opt("tokenExpiresAt") ?: return null
    val value = when (raw) {
        is Number -> raw.toLong()
        is String -> raw.trim().toLongOrNull()
        else -> null
    } ?: return null
    if (value <= 0) return null
    return if (value < 100_000_000_000L) value * 1000 else value
}

/**
 * 凭证有效期一行：文案收短成「30 天后」，已过期标红。
 *
 * `internal` 而非 private：列表页与**详情页**两处账号卡都要用它 —— 同一个事实
 * 在两处必须长得一样，复制一份迟早会漂。
 */
@Composable
internal fun ExpiryLine(expiresAtMs: Long) {
    val C = agateColors()
    val left = expiresAtMs - System.currentTimeMillis()
    // 不足一天时给更细的刻度（小时 / 分钟），否则统一走 expiryPhrase
    val text = when {
        left <= 0 -> "已过期"
        left < 3_600_000L -> "${(left / 60_000L).coerceAtLeast(1)} 分钟后到期"
        left < 86_400_000L -> "${(left / 3_600_000L).coerceAtLeast(1)} 小时后到期"
        else -> expiryPhrase(left)
    }
    Row(
        Modifier.fillMaxWidth().padding(vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text("凭证有效期", color = C.textSub, fontSize = 12.sp, modifier = Modifier.weight(1f))
        Text(
            text,
            color = if (left <= 0) C.red else if (left < 3_600_000L) C.amber else C.textSub,
            fontSize = 12.sp,
        )
    }
    Box(Modifier.fillMaxWidth().height(1.dp).background(C.border))
}

private fun fmtNum(v: Double): String =
    if (v >= 1000) String.format("%,.0f", v)
    else String.format("%.2f", v).trimEnd('0').trimEnd('.')

