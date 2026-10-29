/**
 * 拖拽排序 / 一键置顶的状态机与容器（提供商卡片、账号卡片共用）。
 *
 * 移植来源：次要参考项目 `Android-AI-gateway` 的 `ui/Reorderable.kt`。
 * **状态机、动画规格、让位算法逐字保留** —— 那些数字（长按 250ms、让位时间常数
 * 0.055s、抬升 1.035 倍、空闲 5s 退出…）是那边在真机上反复调出来的，不要按
 * 「看起来更合理」重算。适配只做了三件事：包名、主题取色（MaterialTheme →
 * `agateColors()`）、卡片圆角（→ 20dp，与本仓卡片外壳一致）。
 *
 * ⚠️ 注释里出现的 `v2.21.xx` 一类版本号是**参考项目的历史**，不是本仓版本。
 * 保留它们是因为那些注释记的正是「为什么不能这样写」的踩坑现场（尤其是
 * `detectTapGestures` 会 consume down、把卡内 clickable 全饿死那一条），
 * 去掉版本号就等于把结论与来由拆开。
 *
 * ── 两个变体，不要混用 ──────────────────────────────────────
 * - [ReorderableLazyColumn]：列表页（提供商卡片列表）。live 重排 + `animateItem`
 *   让位，命中判定走 `layoutInfo`；
 * - [ReorderableColumn]：嵌在别的滚动容器里的那一组（账号卡片）。**不** live 重排
 *   （变高卡无法稳定让位），拖拽中把中间项整体平移一格做空位。
 */
package dev.agate.android.ui.components

import android.os.SystemClock
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.SpringSpec
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.clickable
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListItemInfo
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.VerticalAlignTop
import androidx.compose.material3.Icon
import dev.agate.android.ui.theme.agateColors
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.layout.onPlaced
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.platform.LocalViewConfiguration
import androidx.compose.ui.platform.ViewConfiguration
import androidx.compose.ui.unit.dp
import androidx.compose.ui.zIndex
import kotlin.math.abs
import kotlin.math.exp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/** 页面级滚动状态（PageScaffold 提供）：账号列表拖拽时做页面级边缘自动滚动。 */
val LocalPageScroll = staticCompositionLocalOf<ScrollState?> { null }

/**
 * 自动滚动速度（dp/秒，v2.16.2）。
 *
 * 原先是「每帧固定 30px」，与刷新率绑定：60Hz 约 1800px/s，120Hz 直接翻倍到
 * 3600px/s —— 同一台设备换个刷新率设置手感就变了。改成按帧间隔换算。
 */
private const val SCROLL_SPEED_DP_PER_SEC = 900f

/** 单帧最大计入时长（秒）：切后台再回来时帧间隔会是个巨大值，不能让它一次滚飞。 */
private const val MAX_FRAME_DT_SEC = 0.05f

/**
 * 拖拽抬升缩放。
 *
 * 1.02 在 3x 屏上只有不到 1px 的边，几乎看不出来 —— 抬升感主要靠阴影，
 * 但缩放太小会让「卡片离开桌面」这件事缺少体积感。1.035 是「明显但不夸张」
 * 的甜点：再大会让相邻卡片的圆角互相压住。
 */
private const val LIFT_SCALE = 1.035f

/**
 * 抬起后的阴影高度（dp，v2.21.86）。
 *
 * 原来固定 12dp 且**瞬间**出现/消失：长按那一刻卡片「啪」一下长出阴影，
 * 松手又「啪」一下没了 —— 这是拖拽手感里最廉价的一环。现在由
 * [LIFT_UP_SPEC] / [LIFT_DOWN_SPEC] 驱动 `lift` 0→1 渐变，阴影随之生长。
 * 16dp 比 12dp 更能把卡片从背景里「托」起来。
 */
private const val LIFT_ELEVATION_DP = 16f

/**
 * 抬起规格：临界阻尼、刚度中等 → 约 130ms 到位。
 *
 * 抬起必须**快**：长按 250ms 后手指已经在等反馈了，再慢吞吞放大就显得黏。
 * 阻尼取 0.85（轻微过冲）让「拿起来」有一点弹性，不是机械的线性放大。
 */
private val LIFT_UP_SPEC: SpringSpec<Float> = spring(
    dampingRatio = 0.85f,
    stiffness = Spring.StiffnessMedium,
)

/**
 * 落下规格：无过冲、刚度低两档 → 约 220ms 柔落。
 *
 * 松手是「把卡片放回桌面」，必须比抬起慢、且绝不能回弹过头（那会和落位位移
 * 的 spring 叠在一起，看着像卡片在抖）。刚度取 [Spring.StiffnessLow]：
 * 抬起的 StiffnessMedium 约 90ms 到位，这里约 220ms，一快一慢的对比正是
 * 「拿起来 / 放下去」的重量感来源。
 */
private val LIFT_DOWN_SPEC: SpringSpec<Float> = spring(
    dampingRatio = 1f,
    stiffness = Spring.StiffnessLow,
)

/** 拖拽中其余卡片的退让透明度：把注意力收到被拖项上，但不至于发灰。 */
private const val DIM_ALPHA = 0.82f

/** 退让透明度过渡时长（毫秒）：比抬起慢一点，避免「整屏闪一下」。 */
private const val DIM_ANIM_MS = 180

/** 相邻卡片让位动画时长（毫秒，v2.21.86）：原先用默认 spring 偏黏，改成定长 tween 更贴手。 */
private const val ITEM_MOVE_MS = 170

/**
 * 排序编辑态的空闲超时（毫秒，v2.21.88）。
 *
 * 用户要求「长按激活排序后，一段时间没有屏幕操作就回到激活前状态」——
 * 否则置顶按钮会一直挂在卡片右侧，看着像页面进入了一个奇怪的模式。
 * 5s：够用户看清箭头在哪、也够点一下；比 3s 宽容，比 10s 干脆。
 */
private const val SORTING_IDLE_TIMEOUT_MS = 5000L

/**
 * 指针事件节流窗口（毫秒，v2.21.88）。
 *
 * 手势移动事件每帧都来（120Hz 下每秒 120 个），每个都重启一次计时协程太浪费。
 * 150ms 的窗口对「人停了多久」这件事完全够用。
 */
private const val INTERACTION_THROTTLE_MS = 150L

/**
 * 拖拽手柄起拖所需的长按时长（毫秒）。
 *
 * 系统默认 400ms（`ViewConfiguration.longPressTimeoutMillis`），对拖拽手柄偏迟钝
 * —— 早期注释曾长期误写为「~150ms」，实际从未生效过。这里取 250ms：**不取 150ms**
 * 是因为「按住即滑 = 页面/列表滚动」这一分流需要留出判定窗口，太短会让想滚动页面
 * 的人频繁误触发拖拽。
 */
private const val DRAG_LONG_PRESS_TIMEOUT_MS = 250L

/**
 * 视口边缘的自动滚动感应带（dp，v2.16.2）。
 *
 * 原先硬编码 96**px**，在 3x 屏上只有 32dp，比常见手指还窄，等于没有感应带。
 */
private const val AUTO_SCROLL_MARGIN_DP = 56f

/**
 * 让位偏移趋近目标的时间常数（秒，v2.17.0；v2.21.86 0.07 → 0.055）。
 *
 * 指数趋近 `1 - e^(-dt/τ)`：τ 越小越贴手。0.07s 时整段让位约 300ms，
 * 快速拖动时会明显感到「空位追不上手指」；0.055s 收到约 230ms，仍不生硬。
 * **不能做成线性插值**——拖拽中目标会反复变（换一次位目标就翻向），
 * 线性插值在目标翻转时会突然反向冲一段。
 */
private const val GAP_TAU_SEC = 0.055f

/** 小于该值的偏移直接归零，避免指数趋近永远差最后一像素。 */
private const val GAP_EPSILON = 0.5f

/**
 * 松手后等待新序布局落地的超时（毫秒，v2.17.0）。
 *
 * Column 变体不 live 重排，落位动画要等提交后的新布局（被拖项 `onPlaced`）才能算出
 * 续接位移。提交被拒/布局没变时 `onPlaced` 不会来，靠这个兜底收尾，否则卡片会一直
 * 停在松手位置。
 */
private const val SETTLE_FALLBACK_MS = 600L

/**
 * 松手落位动画规格。
 *
 * 轻微回弹：被拖项从手指下滑进槽位，末端有一点「咔」进去的手感。不用
 * `DampingRatioNoBouncy` 是因为列表项位移通常跨好几格，纯减速显得拖沓。
 */
private val SETTLE_SPEC: SpringSpec<Float> = spring(
    dampingRatio = Spring.DampingRatioLowBouncy,
    stiffness = Spring.StiffnessMedium,
)

/**
 * 拖拽排序共享状态。
 *
 * 设计要点：
 * - 拖拽全程**只动本地乐观序**（[keys]），松口才经 [onCommit] 提交一次；
 * - LazyColumn 变体走 live 重排（keys 实时 move，`animateItem` 让位），命中判定用
 *   `layoutInfo.visibleItemsInfo`（P3）；被拖项 translationY =
 *   dragDelta + (起拖槽位 offset − 当前槽位 offset)，视觉钉在手指下（P2）；
 * - Column 变体**不** live 重排（变高卡无法稳定让位）：改为把 from..to 之间的项整体
 *   平移一格做出空位（[gapOffset]），被拖项跟手 + 目标卡描边高亮（P5），
 *   translationY = dragDelta + (当前页面滚动 − 起拖页面滚动)（页面级自动滚动补偿，P6）；
 * - 松口落位：Lazy 变体槽位即最终槽位，直接把跟手位移动画归零；
 *   Column 变体要等新序布局落地后用「松手视觉中心 − 新布局中心」续接再归零；
 * - 被拖项 zIndex(1f) 防穿模（P1）；自动滚动只在被拖项越出视口时发生，
 *   被拖项天然留在视口内（P8）。
 */
class ReorderState {
    /** 正在拖拽的 key；null = 空闲。 */
    var draggingKey: String? by mutableStateOf(null)
        private set

    /** 拖拽中的落点目标（Column 变体高亮用；Lazy 变体不用于高亮）。 */
    var targetKey: String? by mutableStateOf(null)
        private set

    /** 本地乐观序（Lazy 变体随拖拽实时 move；Column 变体始终等于真实序）。 */
    val keys = mutableStateListOf<String>()

    /** 松口提交：(key, 目标下标)。由容器 Composable 注入。 */
    var onCommit: ((key: String, toIndex: Int) -> Unit)? = null

    // ------------------------------------------------------------ 内部拖拽态

    private var dragDelta by mutableFloatStateOf(0f)

    /** 起拖锚点：Lazy = 被拖项起拖槽位 offset；Column = 起拖时的页面滚动值。 */
    private var dragStartAnchor by mutableFloatStateOf(0f)

    /** 起拖时被拖项在 [keys] 里的下标（提交时判断「是否真的动了」）。 */
    private var dragStartIndex = -1

    /** 被拖项最近一次可见时的高度（px）：滚出 visibleItemsInfo 后仍要能算边缘（见 [lazyTick]）。 */
    private var lastDraggedSize by mutableFloatStateOf(0f)

    /**
     * 被拖项最近一次可见时的槽位 offset（px，v2.21.86）。
     *
     * [lazyTranslationY] 要靠「当前槽位 offset」算补偿量，但快速甩动 + 边缘自动滚动时
     * 被拖项可能短暂滚出 `visibleItemsInfo` —— 原来那里 `?: return 0f`，补偿量瞬间变成
     * 0，卡片会**瞬移回槽位**再跳回来，手感上是「拖到边缘卡片闪一下」。
     * 用最近一次的值兜底即可，滚出去的这一两帧偏差肉眼看不出来。
     */
    private var lastVisibleOffset by mutableFloatStateOf(0f)

    /** Lazy 变体 true（keys 实时重排）/ Column 变体 false。由容器注入。 */
    internal var liveReorder = false

    /** 触觉反馈（抓起重震动 + 跨越轻震动）；由 dragHandle 在 onDragStart 时注入。 */
    internal var haptics: androidx.compose.ui.hapticfeedback.HapticFeedback? = null

    /**
     * 长按进入排序编辑态。
     *
     * true 时显示排序控件（右侧置顶箭头 + 左侧点状拖拽手柄），平时隐藏让卡片干净。
     * **两个方向都收口在 [enterSortingMode] / [exitSortingMode]**：
     * - 进入：**起拖**（[startCommon]，v2.21.85）—— 「长按卡片激活排序」在
     *   状态机这一层兑现，UI 侧不必再挂长按检测器；
     * - 退出：拖拽**换了位**（[end] 内 `if (changed)`）或一键置顶完成
     *   （UI 回调显式调）。
     *
     * ⚠ v2.21.85 的进入点为什么从 UI 侧下沉到 [startCommon]：v2.21.83 用的
     * `sortingModeHandle`（detectTapGestures 的 onLongPress）**会 consume
     * down**，挂在卡片根上时作为卡内全部子节点的祖先，把子 clickable 全饿死
     * —— 置顶箭头点不动（用户报「激活排序后，一键置顶没有反应」）。
     * 拖拽用的是 detectDragGesturesAfterLongPress，长按成立前不消费 down，
     * 与子 clickable 天然共存；把激活挂在它的起拖时刻就同时拿到了「长按」
     * 语义与「不抢手势」两件事。
     *
     * ⚠ v2.21.82 及之前**只有进入没有退出**：全仓唯一写入点是 `= true`，
     * 长按过一次之后排序控件常驻、用户没有任何退出手段 —— 用户报的
     * 「排序后自动退出保存」正是缺的这一环。写入已收紧为 private set，
     * 外部只能走这两个方法，防再出现单方向状态。
     *
     * ⚠ v2.21.83 一度矫枉过正：在 end() 里**无条件**退出，而「长按卡片激活」
     * 这个手势同时起拖 + 置编辑态，用户长按激活后松手即走 end() → 排序态
     * 一闪而没，置顶箭头根本点不到（「点击出现的箭头没反应」）。
     * v2.21.84 收正为「换了位才退」。
     */
    var isSortingMode by mutableStateOf(false)
        private set

    /**
     * 空闲计时器的重启次数：纯给测试看，不参与渲染。
     *
     * 「排序态下多久没操作就自动退出」这条逻辑全靠 `delay`，不能真等 5 秒来测；
     * 于是把「计时器被重启了一次」这件事记成一个计数，测试只需断言计数变化，
     * 不必碰时钟。
     */
    internal var idleTimerRestarts = 0
        private set

    /** 上一个「屏幕操作」的时间戳（uptimeMillis）；用来给手势事件流做节流。 */
    private var lastInteractionUptime = 0L

    /**
     * 进入排序编辑态（幂等），并启动空闲计时：**一段时间没有屏幕操作就自动退出**
     * 。
     */
    fun enterSortingMode() {
        isSortingMode = true
        // 清掉节流基准：不清的话进入排序态后的第一个手势事件（uptime 是个很大的值）
        // 也会被 [noteInteraction] 的节流窗口挡掉。
        lastInteractionUptime = 0L
        restartIdleTimer()
    }

    /**
     * 退出排序编辑态（幂等）：拖拽结束 / 一键置顶完成 / 空闲超时 之后调。
     *
     * 注意 [cancel] **不**调它 —— 拖拽被打断（拖出列表 / 被拖项消失）时
     * 保持编辑态，用户还能接着拖，不该因为一次误触就丢掉刚激活的模式。
     */
    fun exitSortingMode() {
        isSortingMode = false
        idleJob?.cancel()
        idleJob = null
    }

    /**
     * 最近一次拖拽手势结束的时刻（uptimeMillis；0 = 还没拖过）。
     *
     * 唯一消费者是 [shouldIgnoreClick]。
     */
    private var lastDragEndUptime = 0L

    /**
     * 这次点击是不是**拖拽手势抬手**的副产物 —— 是就该忽略。
     *
     * ── 为什么必须有这道守卫（用户报的两个现象同一个根因）──────────
     * `dragHandle` 用的是 `detectDragGesturesAfterLongPress`，它与卡内子节点的
     * `clickable` **共存**。但共存的代价是：
     * 长按成立 → 起拖 → 抬手时，**子 clickable 的 press→release 仍会走完**，
     * 于是
     * · 卡头触发「展开」→ 用户报「长按卡片中部还是会展开卡片」；
     * · 名称铅笔触发「进入改名」→ 用户报「长按卡头同时激活了名称编辑」。
     *
     * ── 为什么不能靠 isSortingMode 判断（上一版就是这么错的）──────────
     * `end()` 在**换了位**时会 `exitSortingMode()`，而抬手那次 click 紧接其后 ——
     * 那时 `isSortingMode` 已是 false，「拖完松手」照样展开。所以必须看**时间**。
     *
     * ⚠ 窗口取 350ms：短于一次有心点击之间的间隔（否则用户拖完想展开要连点两次
     * 才算数——那种「第一次没反应」的体验更糟），长于手势收尾 + onClick 派发的耗时。
     */
    fun shouldIgnoreClick(now: Long = nowUptime()): Boolean {
        // ★ 0 = **从未拖过**，必须短路。否则区间算出来是 `now - 0`：设备开机不足
        //   350ms 时它就是「窗口内」，进页面第一下点击会被吞掉（单测抓到的边界）。
        if (lastDragEndUptime == 0L) return false
        return now - lastDragEndUptime in 0..DRAG_CLICK_GUARD_MS
    }

    /**
     * 单调时钟（毫秒）。
     *
     * ⚠ 必须能在 JVM 单测里跑：`android.os.SystemClock` 在单元测试里是 **stub**，
     * 直接调用会抛 `Method uptimeMillis not mocked` —— 而 [cancel] 会被大量
     * 排序状态机测试调用，一处直连就能把整批测试打挂。退回 `nanoTime` 的不影响语义：只要求**单调**，
     * 且写入端（[cancel]）与读取端（本函数）同源即可。
     */
    private fun nowUptime(): Long =
        runCatching { SystemClock.uptimeMillis() }
            .getOrElse { System.nanoTime() / 1_000_000 }

    /**
     * 测试入口：直接设定「上次拖拽结束时刻」。
     *
     * 生产代码走 [cancel] 里的 `SystemClock.uptimeMillis()`；单测不能碰真时钟，
     * 所以留这个口子（与 [shouldIgnoreClick] 的 `now` 参数配对使用）。
     */
    internal fun markDragEndAt(uptime: Long) {
        lastDragEndUptime = uptime
    }

    /**
     * 记录一次屏幕操作：排序态下重置空闲计时。
     *
     * 由容器在**根节点**上以 `PointerEventPass.Initial` 旁听指针事件后调用 ——
     * 只观察、不消费，所以不会像 v2.21.85 那个 `sortingModeHandle` 一样
     * 把子节点的 clickable 饿死。
     *
     * [uptimeMillis] 用指针事件自带的时间戳，不用墙钟：① 单调，② 测试能直接喂值。
     * 同一个手势的移动事件每帧都来，用 [INTERACTION_THROTTLE_MS] 节流，
     * 免得 120Hz 下每秒重启 120 次计时器。
     */
    fun noteInteraction(uptimeMillis: Long) {
        if (!isSortingMode) return
        if (uptimeMillis - lastInteractionUptime < INTERACTION_THROTTLE_MS) return
        lastInteractionUptime = uptimeMillis
        restartIdleTimer()
    }

    /**
     * 重启空闲计时：[SORTING_IDLE_TIMEOUT_MS] 内没有新操作就退出排序态。
     *
     * 拖拽中不算空闲 —— 手指按着卡片不动，用户并没有走开。这种情况下超时后
     * **不退出**，等 [end] 里再重启一次计时（松手后才重新开始倒数）。
     */
    private fun restartIdleTimer() {
        idleTimerRestarts++
        // 注意：**不**在这里清 lastInteractionUptime。清了的话
        // noteInteraction 刚记下的时间戳立刻被抹掉，节流窗口等于不存在。
        // 需要重置基准的只有 enterSortingMode（它自己清）。
        val s = scope ?: return
        idleJob?.cancel()
        idleJob = s.launch {
            delay(SORTING_IDLE_TIMEOUT_MS)
            // 拖拽中（手指按着）不算「用户走开」：不退出，等 end() 重新计时
            if (draggingKey == null) exitSortingMode()
        }
    }

    internal var listState: LazyListState? = null

    /** Column 变体的每项 window 坐标（onPlaced 收集；页面滚动会自动重放刷新）。 */
    internal val bounds = mutableStateMapOf<String, Rect>()

    internal var pageScroll: ScrollState? = null

    /** 边缘自动滚动感应带（px）；由容器按屏幕密度注入（见 [AUTO_SCROLL_MARGIN_DP]）。 */
    internal var autoScrollMarginPx by mutableFloatStateOf(0f)

    /** Column 变体的 item 间距（px）：让位偏移要含一格间距，否则空位比被拖项矮一截。 */
    internal var columnSpacingPx by mutableFloatStateOf(0f)

    /** 落位动画协程作用域；由容器经 `rememberCoroutineScope()` 注入。 */
    internal var scope: CoroutineScope? = null

    /** 排序态空闲计时协程；每次有操作就取消重建。 */
    private var idleJob: Job? = null

    internal companion object {
        /** 拖拽抬手后多久内的点击算手势残留（见 [shouldIgnoreClick] 的取时理由）。 */
        const val DRAG_CLICK_GUARD_MS = 350L
    }

    // ------------------------------------------------------------ 让位与落位

    /**
     * 各 key 的让位目标偏移（px）：被拖项离开槽位后，from..to 之间的项整体平移
     * 「被拖项高度 + 间距」一格。
     *
     * 只在拖拽中、且 [targetKey] 变化时重算；松手后自然冻结（不再有人改它），
     * 等新序布局落地时连当前偏移一起清零。
     */
    private val gapTargets = mutableStateMapOf<String, Float>()

    /** 各 key 的让位当前偏移（px）：指数逼近 [gapTargets]，松手后平滑归零。 */
    internal val gapOffset = mutableStateMapOf<String, Float>()

    /**
     * 松手落位：等新序布局落地后要续接位移的 key（Column 变体，由 onPlaced 置空）。
     *
     * 非空表示「提交已发出、新布局还没到」；此时让位偏移冻结在目标值。
     */
    internal var pendingSettleKey by mutableStateOf<String?>(null)
        private set

    /** 松手时被拖项的视觉中心（window 坐标），算续接位移用。 */
    private var pendingSettleCenterY by mutableFloatStateOf(0f)

    /**
     * 新布局落地后算出的续接位移。
     *
     * 由 onPlaced 写、帧循环消费 —— **不在 onPlaced 里直接起动画**：`settleKey` 是组合里
     * 要读的状态，布局期写它会逼出一次额外重组；而这里要写的东西（gapOffset 等）只在
     * 图层块里读，布局期写是安全的。分开之后 onPlaced 只做「算出来记下」。
     */
    private var pendingSettleItemKey: String? = null
    private var pendingSettleFromY = 0f
    private var pendingSettleReady = false

    /**
     * 新序布局落地时要一次性补回让位偏移的 key 集合。
     *
     * 让位偏移是挂在**旧**布局基准上的；新序一落地这些项各自平移了一格，偏移不跟着补，
     * 视觉位置就会整体多跳一格。所以提交落地那一刻要按同一方向补回去再归零。
     */
    private val gapCommitKeys = mutableSetOf<String>()

    /** 上一条的补偿量（px，符号跟随拖动方向）。 */
    private var gapCommitDelta = 0f

    /** 松手落位动画进行中的 key；非空时该项从当前视觉位置滑回槽位。 */
    internal var settleKey by mutableStateOf<String?>(null)
        private set

    /** 落位动画当前偏移（px）。 */
    internal var settleOffsetY by mutableFloatStateOf(0f)

    // v2.21.86 删除了 settleScale：缩放不再由状态机逐帧写，改由渲染侧
    // `animateFloatAsState` 驱动的 `lift`（0→1）统一控制 —— 起拖/落位共用
    // 一条曲线，松手瞬间不会因为「settleScale 已归 1、lift 还没落」而跳一下。

    /**
     * 真实序同步：空闲时对齐；拖拽中跳过（保留本地序，提交后 refresh 自然对齐）；
     * 被拖项消失（账号被删/数据刷新）→ 取消拖拽（边界 E2）。
     */
    fun syncKeys(real: List<String>) {
        val dragging = draggingKey
        if (dragging != null) {
            if (dragging !in real) cancel()
            return
        }
        if (pendingSettleKey != null && pendingSettleKey !in real) {
            pendingSettleKey = null
            pendingSettleReady = false
            pendingSettleItemKey = null
            pendingSettleFromY = 0f
            gapTargets.clear()
        }
        if (settleKey != null && settleKey !in real) {
            settleKey = null
            settleOffsetY = 0f
        }
        if (keys != real) {
            keys.clear()
            keys.addAll(real)
        }
    }

    // ------------------------------------------------------------ 起拖（两种容器）

    /** LazyColumn 变体起拖：锚点 = 被拖项当前槽位 offset（内容坐标）。 */
    fun startLazy(key: String) {
        startCommon(key) {
            listState?.layoutInfo?.visibleItemsInfo
                ?.firstOrNull { it.key == key }?.offset?.toFloat() ?: 0f
        }
    }

    /** Column 变体起拖：锚点 = 起拖时的页面滚动值（页面自动滚动补偿用，P6）。 */
    fun startColumn(key: String, scrollValue: Float) {
        startCommon(key) { scrollValue }
    }

    private inline fun startCommon(key: String, anchor: () -> Float) {
        if (draggingKey != null) return
        // 上一次落位动画还没完就起新拖：先把旧动画的状态清掉，否则两个位移会叠在一起
        settleKey = null
        settleOffsetY = 0f
        pendingSettleKey = null
        pendingSettleReady = false
        pendingSettleItemKey = null
        pendingSettleFromY = 0f
        gapTargets.clear()
        draggingKey = key
        targetKey = key
        dragDelta = 0f
        dragStartIndex = keys.indexOf(key)
        dragStartAnchor = anchor()
        // 起拖这一刻被拖项必然可见，锚点就是它当下的槽位 offset —— 直接当兜底值
        lastVisibleOffset = dragStartAnchor
        lastDraggedSize = 0f
        // ★ 起拖即进入排序编辑态：「长按卡片激活排序」在
        // 状态机这一层兑现，UI 侧不必再挂任何长按检测器。
        //
        // 为什么必须下沉到这里：此前 UI 侧用 `SortingModeHandle`
        // （= detectTapGestures 的 onLongPress）置编辑态 —— 那个检测器
        // **会 consume down 事件**，而它挂在卡片根上时是卡内所有子节点的
        // 祖先，于是**饿死了卡内全部 clickable**：置顶箭头点不动、卡片
        // 点不开（用户报「激活排序后，一键置顶没有反应」）。
        // dragHandle 用的是 detectDragGesturesAfterLongPress，长按成立前
        // 不消费 down，与子 clickable 天然共存 —— 把激活挂在它的起拖时刻
        // 就同时拿到了「长按」语义和「不抢手势」两件事。
        enterSortingMode()
    }

    /** 累计拖拽位移（px）；由 [dragHandle] 的 onDrag 调用。 */
    fun drag(deltaY: Float) {
        if (draggingKey != null) dragDelta += deltaY
    }

    /**
     * 松手：算出目标下标并提交一次；原地松手不提交（避免无意义 notice），
     * 但被拖项仍会滑回自己的槽位。
     *
     * 排序编辑态的退出见方法体内 `if (changed)` 处的说明：
     * **换了位才退**，纯长按激活松手不退。
     */
    fun end() {
        val key = draggingKey ?: return
        val from = keys.indexOf(key)
        val to = if (liveReorder) {
            from
        } else {
            keys.indexOf(targetKey ?: key).takeIf { it >= 0 } ?: from
        }
        val changed = when {
            from < 0 -> false
            liveReorder -> from != dragStartIndex
            else -> to != from
        }
        // cancel 会清掉拖拽态，落位要用的量必须先取好
        val lazyFrom = if (liveReorder) lazyTranslationY() else 0f
        val columnFrom = if (liveReorder) 0f else columnTranslationY()
        // null = 被拖项已滚出可见区、取不到视觉中心（极少见）：走不到「等新布局」那条路
        val columnCenter = if (liveReorder) null else columnDragCenterY(key)
        val draggedHeight = bounds[key]?.height ?: 0f
        cancel()
        // ★ 只有**真的换了位**才退出排序编辑态。
        // v2.21.83 曾在这里无条件退出，那是错的：「长按卡片激活排序」这个手势
        // 会同时起拖 + 置编辑态，用户长按激活后**松手**就会走 end() —— 无条件退出让
        // 排序态一闪而没，用户根本来不及点置顶箭头（用户报「点击出现的箭头
        // 没反应」）。纯激活（没拖/抖回原地）松手必须保持编辑态；退出只留给
        // 「排序已完成」：换了位（本处）或一键置顶（UI 回调里显式 exit）。
        if (changed) {
            exitSortingMode()
        } else {
            // 没换位 = 只是「长按激活了一下」：退出留给空闲超时。
            // 这里必须重启一次计时 —— 拖拽中不算空闲，那次超时是空转的
            // （见 [restartIdleTimer]），不重启的话松手后计时器就再也不响了。
            restartIdleTimer()
        }
        if (liveReorder) {
            if (changed) onCommit?.invoke(key, from)
            // 槽位即最终槽位：把跟手位移动画归零就是滑进新槽位
            beginSettle(key, lazyFrom)
            return
        }
        if (!changed || columnCenter == null) {
            // 原地松手 / 取不到视觉中心：让位平滑收起，被拖项按当前跟手位移滑回槽位
            gapTargets.clear()
            if (changed) onCommit?.invoke(key, to)
            beginSettle(key, columnFrom)
            return
        }
        // 冻结让位目标：偏移是挂在旧布局基准上的，等新序落地时按同一方向补回一格再归零，
        // 视觉位置才连续（见 [consumePendingSettle]）。
        updateGapTargets(from, to, draggedHeight)
        // 先写视觉中心再置 pendingSettleKey：反过来的话，读 pendingSettleKey
        // 的重组可能先跑、拿到的是上一轮的旧中心，卡片会闪一下错位置。
        pendingSettleCenterY = columnCenter
        pendingSettleKey = key
        onCommit?.invoke(key, to)
        // 兜底：提交被拒/布局没变（onPlaced 不触发）时强制收尾
        scope?.launch {
            delay(SETTLE_FALLBACK_MS)
            if (pendingSettleKey != key && !(pendingSettleReady && pendingSettleItemKey == key)) return@launch
            val fromY = if (pendingSettleReady && pendingSettleItemKey == key) pendingSettleFromY else 0f
            pendingSettleKey = null
            pendingSettleReady = false
            pendingSettleItemKey = null
            pendingSettleFromY = 0f
            gapTargets.clear()
            beginSettle(key, fromY)
        }
    }

    /** 取消（拖出列表 / 被拖项消失 / 手势打断）：丢弃本地序，不提交。 */
    fun cancel() {
        // 记下「拖拽手势结束」的时刻：子节点的 clickable 会在抬手时照常触发
        // onClick（长按起拖与子 click **共存**，见 [shouldIgnoreClick]），
        // 这里的时间戳是让它能认出「这次点击是手势残留」的唯一依据。
        // 只在真的有拖拽时记 —— 页面销毁时的被动 cancel 不该吞掉后续点击。
        if (draggingKey != null) lastDragEndUptime = nowUptime()
        draggingKey = null
        targetKey = null
        dragDelta = 0f
        dragStartIndex = -1
        lastDraggedSize = 0f
        lastVisibleOffset = 0f
        gapTargets.clear()
        pendingSettleKey = null
        pendingSettleReady = false
        pendingSettleItemKey = null
        pendingSettleFromY = 0f
        settleKey = null
        settleOffsetY = 0f
    }

    /**
     * 松手落位动画：被拖项从当前视觉位置滑回槽位（缩放/阴影的回落由渲染侧的
     * `lift` 动画独立承担，见 [LIFT_DOWN_SPEC]）。
     *
     * Lazy 变体拖拽中已 live 重排，松手时槽位即最终槽位，[fromY] 直接取松手前的跟手位移；
     * Column 变体不 live 重排，[fromY] 要等提交后的新布局落地后由 onPlaced 用
     * 「松手视觉中心 − 新布局中心」算出——否则新序一下排好，位移基准就换了，只能瞬移。
     *
     * [fromY] 为 0 时什么都不做（本来就在槽位上）——此时也不设 [settleKey]，
     * 被拖项立刻退出「抬升」分支，`lift` 自然从 1 柔落到 0，观感上就是卡片原地放下。
     */
    internal fun beginSettle(key: String, fromY: Float) {
        if (fromY == 0f) return
        val scope = scope ?: return
        settleKey = key
        settleOffsetY = fromY
        scope.launch {
            try {
                val anim = Animatable(fromY)
                anim.animateTo(0f, SETTLE_SPEC) { settleOffsetY = value }
            } finally {
                if (settleKey == key) {
                    settleKey = null
                    settleOffsetY = 0f
                }
            }
        }
    }

    /**
     * 新序布局落地（Column 变体的 onPlaced 调用，v2.17.0）。
     *
     * 只负责「算出来记下」，不起动画 —— 动画交给帧循环的 [launchPendingSettle]，
     * 免得在布局期写组合里要读的状态（见 [pendingSettleItemKey] 的注释）。做两件事：
     * 1. 让位偏移按同一方向补回一格再归零 —— 偏移原本挂在旧布局基准上，新序一落地
     *    这些项各自平移了一格，不补就会整体多跳一格；
     * 2. 记下「松手时的视觉中心 − 新布局中心」作为落位动画起点。
     */
    internal fun onSettleLayoutLanded(key: String, newCenterY: Float) {
        if (pendingSettleKey != key) return
        pendingSettleItemKey = key
        pendingSettleFromY = pendingSettleCenterY - newCenterY
        pendingSettleReady = true
        // ⚠ 这里**不**清 pendingSettleKey。清了之后到 [launchPendingSettle]
        // 真正起动画之间会隔至少一帧，那一帧既不是 pending 也不是 settling，
        // translationY 掉回槽位 —— 松手落位会闪一下。改由 launchPendingSettle 在
        // 起动画的同一帧清（见那边的注释）。
        if (gapCommitDelta != 0f) {
            for (k in gapCommitKeys) {
                val cur = gapOffset[k] ?: continue
                gapOffset[k] = cur + gapCommitDelta
            }
        }
        gapTargets.clear()
    }

    /**
     * 帧循环调用：把「等新布局落地」的落位动画起起来。
     *
     * 顺序要紧：先 [beginSettle]（它同步置 `settleKey`），**再**撤
     * `pendingSettleKey`。反过来的话两帧之间会出现「既非 pending 也非 settling」
     * 的空档，被拖项闪回槽位。
     */
    internal fun launchPendingSettle() {
        if (!pendingSettleReady) return
        val key = pendingSettleItemKey ?: return
        val fromY = pendingSettleFromY
        pendingSettleReady = false
        pendingSettleItemKey = null
        pendingSettleFromY = 0f
        beginSettle(key, fromY)
        if (pendingSettleKey == key) pendingSettleKey = null
    }

    /**
     * 重算让位目标：被拖项从 [from] 挪到 [to] 时，中间那些项整体平移一格。
     *
     * 平移量 = 被拖项高度 + 一格间距 —— 这正是新序里这些项的位移量，所以松手提交落地后
     * 把偏移按同一方向补回去再归零，视觉位置恰好连续，不会二次跳动。
     */
    private fun updateGapTargets(from: Int, to: Int, draggedHeight: Float) {
        gapTargets.clear()
        gapCommitKeys.clear()
        gapCommitDelta = 0f
        if (from < 0 || to < 0 || from == to || draggedHeight <= 0f) return
        val shift = draggedHeight + columnSpacingPx
        val down = from < to
        for (i in keys.indices) {
            if (i == from) continue
            val inGap = if (down) i in (from + 1)..to else i in to until from
            if (!inGap) continue
            gapTargets[keys[i]] = if (down) -shift else shift
            gapCommitKeys.add(keys[i])
        }
        gapCommitDelta = if (down) shift else -shift
    }

    /**
     * 让位偏移指数逼近目标（每帧调用，v2.17.0）；返回是否还在动。
     *
     * 拖拽中逼近 [gapTargets]，松手后目标被清空 → 平滑归零。目标翻向时（换一次位）
     * 旧项从 -shift 归零、新项从 0 到 -shift，观感是空位整体滑过去而不是闪现。
     */
    fun advanceGap(dtSec: Float): Boolean {
        val k = if (dtSec <= 0f) 0f else 1f - exp(-dtSec / GAP_TAU_SEC)
        if (k <= 0f) return gapOffset.isNotEmpty() || gapTargets.isNotEmpty()
        var moving = false
        for ((key, target) in gapTargets) {
            val cur = gapOffset[key] ?: 0f
            if (abs(target - cur) >= GAP_EPSILON) {
                gapOffset[key] = cur + (target - cur) * k
                moving = true
            } else if (gapOffset[key] != target) {
                gapOffset[key] = target
                moving = true
            }
        }
        // 目标里没有的（松手收起 / 被拖项自己）→ 归零
        for (key in gapOffset.keys.toList()) {
            if (key in gapTargets) continue
            val cur = gapOffset[key] ?: continue
            if (abs(cur) >= GAP_EPSILON) {
                gapOffset[key] = cur * (1f - k)
                moving = true
            } else {
                gapOffset.remove(key)
                moving = true
            }
        }
        return moving
    }

    // ------------------------------------------------------------ LazyColumn 变体（每帧）

    /**
     * Lazy 变体每帧：命中判定 → keys 实时 move（官方中线守卫防抖），
     * 返回边缘自动滚动量 —— 只在被拖项越出视口时非零，天然钳制（P8）。
     */
    fun lazyTick(): Float {
        val key = draggingKey ?: return 0f
        val st = listState ?: return 0f
        val dragged = st.layoutInfo.visibleItemsInfo.firstOrNull { it.key == key }
        if (dragged != null) {
            maybeReorderLazy(key, dragged)
            lastDraggedSize = dragged.size.toFloat()
            lastVisibleOffset = dragged.offset.toFloat()
        }
        // 判定与边缘检测统一用**视觉位置** = 起拖锚点 + 跟手位移。
        // 不能写 dragged.offset + dragDelta：被拖项的槽位 offset 每次换位后就变了，
        // 那样算出的坐标会比手指多走一格，越过第二项时换位条件自动重新满足
        // → 一路连跳到列表底（原注释声称的「中线守卫防抖」在坐标漂移下失效）。
        //
        // 顺带修掉「滚出可见区就停」：dragged 为 null 时沿用 [lastDraggedSize] 继续算
        // 边缘，否则手指一拖到屏幕外自动滚动就断，被拖项再也滚不回来。
        val start = dragStartAnchor + dragDelta
        val size = lastDraggedSize
        val li = st.layoutInfo
        // 边缘感应带：原先要等被拖项**整个**越出视口才滚，那时手指基本
        // 已经拖到屏幕外了。改成与 Column 变体一致的感应带，进入带内就开始滚。
        // 返回的是**越出感应带的深度**（px，带符号），调用方按
        // `(深度 / 感应带宽)² × 速度上限` 做 ease-in—— 刚进带时慢，
        // 深入后到全速，不会一碰边缘就猛冲。
        val margin = autoScrollMarginPx
        return when {
            start < li.viewportStartOffset + margin ->
                (start - (li.viewportStartOffset + margin)).coerceAtMost(0f)
            start + size > li.viewportEndOffset - margin ->
                (start + size - (li.viewportEndOffset - margin)).coerceAtLeast(0f)
            else -> 0f
        }
    }

    private fun maybeReorderLazy(key: String, dragged: LazyListItemInfo) {
        // 同 [lazyTick]：命中判定用视觉位置，不是 dragged.offset + dragDelta。
        val middle = dragStartAnchor + dragDelta + dragged.size / 2f
        val target = listState?.layoutInfo?.visibleItemsInfo?.firstOrNull { item ->
            item.key != key && middle.toInt() in item.offset..(item.offset + item.size)
        } ?: return
        val from = keys.indexOf(key)
        val to = keys.indexOf(target.key.toString())
        if (from < 0 || to < 0 || from == to) return
        // 中线守卫：拖拽项中线越过目标项中线才换位，防止来回抖动
        val targetMiddle = target.offset + target.size / 2f
        val passed = if (from < to) targetMiddle < middle else targetMiddle > middle
        if (!passed) return
        keys.add(to, keys.removeAt(from))
        targetKey = target.key.toString()
        // 跨越轻震动（拖拽 UX 补强）：两张卡物理换位时的「齿轮拨动」手感
        haptics?.performHapticFeedback(HapticFeedbackType.TextHandleMove)
    }

    /**
     * 被拖项的跟手位移（P2 补偿式）：视觉钉在手指下，槽位变化由 animateItem 平滑补位。
     *
     * 取不到当前槽位（被拖项这一帧滚出了 `visibleItemsInfo`）时用 [lastVisibleOffset]
     * 兜底 —— 原来这里 `return 0f`，会让卡片瞬移回槽位再跳回来（拖到边缘闪一下）。
     */
    fun lazyTranslationY(): Float {
        val key = draggingKey ?: return 0f
        val st = listState ?: return 0f
        val current = st.layoutInfo.visibleItemsInfo
            .firstOrNull { it.key == key }?.offset?.toFloat() ?: lastVisibleOffset
        return dragDelta + dragStartAnchor - current
    }

    // ------------------------------------------------------------ Column 变体（每帧）

    /**
     * Column 变体每帧：命中判定（最近项规则）→ 更新 [targetKey]（目标卡描边高亮，P5）
     * 并重算让位目标，返回页面级自动滚动量（被拖项中心近视口上/下边缘时，P6）。
     */
    fun columnTick(viewportHeightPx: Float): Float {
        val key = draggingKey ?: return 0f
        val center = columnDragCenterY(key) ?: return 0f
        maybeTargetColumn(key, center)
        if (pageScroll == null) return 0f
        val margin = autoScrollMarginPx
        return when {
            center < margin -> center - margin
            center > viewportHeightPx - margin -> center - (viewportHeightPx - margin)
            else -> 0f
        }
    }

    /** 被拖项视觉中心 Y（window 坐标）= 布局中心 + 跟手位移。 */
    private fun columnDragCenterY(key: String): Float? {
        val rect = bounds[key] ?: return null
        return rect.center.y + columnTranslationY()
    }

    private fun maybeTargetColumn(key: String, center: Float) {
        val own = bounds[key] ?: return
        val from = keys.indexOf(key)
        if (from < 0) return
        val draggedHeight = own.height
        // 先判被拖项有没有真的离开自己的槽位，以及离开的方向。**方向必须判**：
        // 最近项规则本身不认方向，抓着第一项往上轻轻一抖（或抓着最后一项往下抖）时，
        // 旁边那一项恰好就是「最近项」，targetKey 被指到它身上，松手直接换位
        // —— 纯粹的手势误判，v2.16.2 及之前一直存在。
        val direction = when {
            center < own.top -> -1
            center > own.bottom -> 1
            else -> 0
        }
        if (direction == 0) {
            // 回到自己槽位里：目标复位，让位收起
            if (targetKey != key) {
                targetKey = key
                updateGapTargets(from, from, draggedHeight)
            }
            return
        }
        var best: String? = null
        var bestDist = Float.MAX_VALUE
        for ((k, rect) in bounds) {
            if (k == key) continue
            val idx = keys.indexOf(k)
            if (idx < 0) continue
            // 只认拖动方向上的候选：往下拖不会盯上上面的项，往上拖不会盯上下面的项
            if (direction > 0 && idx <= from) continue
            if (direction < 0 && idx >= from) continue
            val dist = when {
                center < rect.top -> rect.top - center
                center > rect.bottom -> center - rect.bottom
                else -> 0f
            }
            if (dist < bestDist) {
                bestDist = dist
                best = k
            }
        }
        // 只有真的换了目标才重算让位：updateGapTargets 会清空再重填 gapTargets，
        // 每帧无条件重填等于每帧写一遍状态图，白白让所有卡片的重组/层块失效。
        if (best != null && best != targetKey) {
            targetKey = best
            updateGapTargets(from, keys.indexOf(best), draggedHeight)
            // 跨越轻震动（拖拽 UX 补强）
            haptics?.performHapticFeedback(HapticFeedbackType.TextHandleMove)
        }
    }

    /** 被拖项的跟手位移（Column 变体）：无 live 重排，仅补偿页面滚动（P6）。 */
    fun columnTranslationY(): Float {
        // ScrollState.value 是 Int，转 Float 参与 px 运算
        val now = (pageScroll?.value ?: 0).toFloat()
        return dragDelta + now - dragStartAnchor
    }

    /**
     * 松手后、新序布局落地前，被拖项相对**当前槽位**的位移。
     *
     * 为什么要这一支：`cancel()` 一清拖拽态，`translationY` 就归 0，卡片会先
     * **弹回原槽位**；而 `onCommit` 走的是异步 action（写盘 + refresh），新序
     * 布局往往要过一两帧甚至更久才到 —— 这段时间里卡片停在原位、邻居却已经让开，
     * 观感是「松手后卡片先缩回去、再飞过去」。用「松手视觉中心 − 当前布局中心」
     * 把它钉在松手位置，等 [onSettleLayoutLanded] 交接给落位动画，全程连续。
     */
    internal fun pendingSettleTranslationY(key: String): Float {
        val rect = bounds[key] ?: return 0f
        return pendingSettleCenterY - rect.center.y
    }
}

/**
 * 边缘自动滚动的每帧位移（px，v2.21.86）。
 *
 * [over] 是「被拖项越出感应带的深度」（带符号，px），[marginPx] 是感应带宽，
 * [limitPx] 是本帧的速度上限。深度归一化后取**平方**：刚进带时几乎不动，
 * 深入后线性升到全速。
 *
 * 原先直接把深度 `coerceIn(-limitPx, limitPx)` —— 深度只要超过一帧的位移量
 * （约 15dp）就顶到上限，等于**一碰边缘就全速冲**，卡片会从手指下「甩出去」。
 * 平方曲线让进入过程有 100ms 左右的加速段，观感是「推着列表滚」而不是「列表自己跑」。
 */
private fun autoScrollDelta(over: Float, marginPx: Float, limitPx: Float): Float {
    if (over == 0f || marginPx <= 0f) return 0f
    val depth = (abs(over) / marginPx).coerceIn(0f, 1f)
    val speed = limitPx * depth * depth
    return if (over < 0f) -speed else speed
}

/**
 * 拖拽手柄：长按 [DRAG_LONG_PRESS_TIMEOUT_MS] 起拖（touchSlop 死区由检测器内置）+ 起拖 haptics。
 *
 * 手柄内的 ↑↓ 图标点击不受影响 —— 手势三分流：
 * 快速点按 = 箭头点击；按住不动到阈值 = 起拖；按住即滑（未满阈值）= 页面/列表正常滚动。
 * [key] 必须是稳定的 item key（uid/providerId），**不能用 index**（重排会重启手势检测器）。
 *
 * 阈值由外层的 [ProvideDragHandleViewConfig] 经 [LocalViewConfiguration] 覆写生效
 * —— Modifier 工厂自身无法 emit CompositionLocalProvider，所以不能直接写在这里。
 */
@Composable
fun Modifier.dragHandle(key: String, state: ReorderState, onStart: () -> Unit): Modifier {
    val haptics = LocalHapticFeedback.current
    return pointerInput(key, state) {
        detectDragGesturesAfterLongPress(
            onDragStart = {
                haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                state.haptics = haptics // 跨越轻震动用（拖拽期间复用）
                onStart()
            },
            onDrag = { change, amount ->
                change.consume()
                state.drag(amount.y)
            },
            onDragEnd = { state.end() },
            onDragCancel = { state.cancel() },
        )
    }
}

// ⚠ v2.21.85 已从本文件删除 `Modifier.sortingModeHandle`。它用 `detectTapGestures` 的 onLongPress，
// 而 **detectTapGestures 会 consume down 事件**；当它被挂在卡片根上时，
// 是卡内所有子节点的**祖先**，于是把卡内全部 clickable 饿死 —— 置顶箭头
// 点不动、卡片点不开（用户报「激活排序后，一键置顶没有反应」）。
// 激活已下沉到 [ReorderState.startCommon]（起拖即置态），UI 侧不再需要它。
// 若将来又要「长按某小块只置态不起拖」，请改用 detectDragGesturesAfterLongPress
// 或把检测挂到**叶子**节点，切勿再挂到容器/卡根。

// ⚠ v2.21.88 已从本文件删除 `DragDotsHandle`。用户要求去掉：它只是在卡片左边多占一列、
// 和 logo 挤在一起，而拖拽入口本来就有 —— 列表页长按 logo、详情页/自定义页
// 长按整卡，都能直接起拖，这个手柄并没有新增任何能力。
// 如需恢复：Column + 3 个 3.5dp 圆点，外面套 [Modifier.dragHandle] 即可。

/**
 * 一键置顶按钮：排序态下出现在卡片右侧的圆底小按钮。
 *
 * 为什么换掉原来的 `KeyboardArrowUp`：
 * - **形状撞车**：那颗是 chevron（`^`），和卡片最右侧「展开/收起」的
 *   `ExpandMore` / `ExpandLess` 完全同形，只靠颜色区分 —— 用户直接读成
 *   「大于号小于号」，一眼看不出这是「置顶」；
 * - **语义不直**：chevron 表达的是「方向」，不是「移到最上面」。
 *
 * `VerticalAlignTop`（横线 + 上箭头，⤒）语义就是「顶到最上」，和 chevron
 * 形状也彻底分开；再垫一层 12% primary 的圆底，让它读起来是个**按钮**
 * 而不是一颗裸图标，同时把点击热区做到 32dp。
 */
@Composable
fun PinToTopButton(
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    /**
     * 拖拽状态；给了就在「手势残留窗口」内吞掉点击。
     *
     * 必需：这颗按钮就在卡头上，长按起拖后抬手若落在这里，会顺手把账号置顶 ——
     * 而用户只是想排序。传 null 表示调用方自己管守卫。
     */
    dragState: ReorderState? = null,
) {
    val C = agateColors()
    Box(
        modifier = modifier
            .size(32.dp)
            .clip(CircleShape)
            .background(C.accent.copy(alpha = 0.12f))
            .clickable {
                if (dragState?.shouldIgnoreClick() == true) return@clickable
                onClick()
            },
        contentAlignment = Alignment.Center,
    ) {
        Icon(
            Icons.Filled.VerticalAlignTop,
            contentDescription = "置顶为优先出战",
            tint = C.accent,
            modifier = Modifier.size(19.dp),
        )
    }
}

/**
 * 为拖拽手柄缩短长按阈值：`detectDragGesturesAfterLongPress` 的阈值取自
 * [LocalViewConfiguration.longPressTimeoutMillis]（系统默认 400ms）。
 *
 * 必须是**组合**而非 Modifier 扩展——[Modifier.dragHandle] 无法自己 emit
 * CompositionLocalProvider，只能由调用方的组合层级提供。
 */
@Composable
private fun ProvideDragHandleViewConfig(content: @Composable () -> Unit) {
    val base = LocalViewConfiguration.current
    val overridden = remember(base) {
        object : ViewConfiguration by base {
            override val longPressTimeoutMillis: Long get() = DRAG_LONG_PRESS_TIMEOUT_MS
        }
    }
    CompositionLocalProvider(LocalViewConfiguration provides overridden, content = content)
}

/**
 * 可拖拽排序的 LazyColumn（供应商列表，方案 v1 §4.2 / v2 P1–P3）。
 *
 * - 渲染顺序由 [ReorderState.keys] 驱动，拖拽中实时重排，`animateItem` 自动让位；
 * - 被拖项 zIndex(1f) + 抬升缩放（P1）；命中判定走 layoutInfo（P3）；
 * - 松手落位动画：拖拽中已 live 重排，松手时槽位即最终槽位，
 *   把松手前的跟手位移 spring 归零即滑进新槽位，抬升缩放同步淡出；
 * - 首帧在 keys 同步前按真实序渲染（避免空态闪烁）。
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun <T> ReorderableLazyColumn(
    items: List<T>,
    keyOf: (T) -> String,
    state: ReorderState,
    onCommit: (key: String, toIndex: Int) -> Unit,
    modifier: Modifier = Modifier,
    contentPadding: PaddingValues = PaddingValues(0.dp),
    verticalArrangement: Arrangement.Vertical = Arrangement.spacedBy(14.dp),
    emptyContent: (@Composable () -> Unit)? = null,
    itemContent: @Composable (T) -> Unit,
) {
    val listState = rememberLazyListState()
    val density = LocalDensity.current
    val scope = rememberCoroutineScope()
    SideEffect {
        state.liveReorder = true
        state.listState = listState
        state.onCommit = onCommit
        state.scope = scope
        state.autoScrollMarginPx = with(density) { AUTO_SCROLL_MARGIN_DP.dp.toPx() }
    }
    LaunchedEffect(items) { state.syncKeys(items.map(keyOf)) }

    val realKeys = items.map(keyOf)
    val renderKeys = state.keys.ifEmpty { realKeys }
    // 按 key 建索引：原来在 items{} 里逐项 firstOrNull 查找，
    // 每项每次重组 O(n) → 整个列表 O(n²)，列表一长就明显掉帧。
    val byKey = remember(items) { items.associateBy(keyOf) }

    LazyColumn(
        state = listState,
        // 排序态空闲自动退出：在根节点**旁听**指针事件。
        // 关键：走 `PointerEventPass.Initial` 且**从不 consume** —— 只观察，
        // 所以不会重蹈 v2.21.85 那个「祖先吃掉 down、卡内 clickable 全饿死」的覆辙。
        modifier = modifier.pointerInput(state) {
            awaitPointerEventScope {
                while (true) {
                    val event = awaitPointerEvent(PointerEventPass.Initial)
                    val uptime = event.changes.firstOrNull()?.uptimeMillis
                    if (uptime != null) state.noteInteraction(uptime)
                }
            }
        },
        contentPadding = contentPadding,
        verticalArrangement = verticalArrangement,
    ) {
        if (renderKeys.isEmpty() && emptyContent != null) {
            item(key = "empty") { emptyContent() }
        } else {
            items(renderKeys.size, key = { renderKeys[it] }) { index ->
                val key = renderKeys[index]
                val item = byKey[key] ?: return@items
                val dragged = state.draggingKey == key
                val settling = !dragged && key == state.settleKey
                // pendingSettle 也要算「抬着」：Column 变体松手后要等新序
                // 布局落地才 beginSettle，中间至少隔一帧。不把它算进来的话，lift 会先
                // 从 1 落到一半、等 settleKey 设上再升回去 —— 松手瞬间闪一下。
                val lifted = dragged || settling || state.pendingSettleKey == key
                // 抬升/落下统一由 lift 驱动：起拖 0→1 快（~130ms），
                // 松手 1→0 柔（~260ms）。此前 scale/阴影是硬切的 —— 长按那一下
                // 「啪」地长出阴影、松手又「啪」地消失，是拖拽手感里最廉价的一环。
                // `animateFloatAsState` 首次组合直接取目标值，所以进页面不会闪。
                val lift by animateFloatAsState(
                    targetValue = if (lifted) 1f else 0f,
                    animationSpec = if (lifted) LIFT_UP_SPEC else LIFT_DOWN_SPEC,
                    label = "reorderLift",
                )
                // 其余卡片轻微退让：把注意力收到被拖项上。
                // 被拖项自己 alpha 保持 1（它是被「拿起来」的实体，不是变淡的幽灵）。
                val dim by animateFloatAsState(
                    targetValue = if (state.draggingKey != null && !lifted) DIM_ALPHA else 1f,
                    animationSpec = tween(DIM_ANIM_MS),
                    label = "reorderDim",
                )
                val cardShape = RoundedCornerShape(20.dp)
                Box(
                    Modifier
                        // 被拖项**不挂**位置动画：它的位置由 translationY
                        // 全程补偿跟手，再叠一层「从旧槽位平移到新槽位」的动画，
                        // 两套位移会在每次换位时打架，表现为卡片跳动。
                        // 让位时长改为定长 tween：默认 spring 在连续换位时
                        // 会拖着尾巴追，快速拖动能明显感到空位跟不上手指。
                        .then(
                            if (lifted) {
                                Modifier
                            } else {
                                Modifier.animateItem(
                                    fadeInSpec = null,
                                    placementSpec = tween(ITEM_MOVE_MS, easing = FastOutSlowInEasing),
                                    fadeOutSpec = null,
                                )
                            },
                        )
                        .zIndex(if (lifted) 1f else 0f)
                        .graphicsLayer {
                            // ★ 阴影必须画在**这个**图层里。
                            // 原来用 `Modifier.shadow(...)`：它是**外层**图层，而
                            // translationY / scale 在**内层** graphicsLayer 上 ——
                            // 结果卡片被拖走了、阴影还留在原槽位，视觉上就是
                            // 「被拖卡片下方多出一张带阴影的白卡片」（用户截图）。
                            // 写进同一个 graphicsLayer 后阴影跟着位移和缩放一起走。
                            shape = cardShape
                            clip = false
                            shadowElevation = LIFT_ELEVATION_DP.dp.toPx() * lift
                            if (dragged) translationY = state.lazyTranslationY()
                            else if (settling) translationY = state.settleOffsetY
                            if (lift > 0f) {
                                scaleX = 1f + (LIFT_SCALE - 1f) * lift
                                scaleY = scaleX
                            }
                            alpha = dim
                        },
                ) {
                    itemContent(item)
                }
            }
        }
    }

    LaunchedEffect(state.draggingKey) {
        if (state.draggingKey == null) return@LaunchedEffect
        var lastNanos = 0L
        while (isActive) {
            val nanos = withFrameNanos { it }
            // 按「dp/秒 × 帧间隔」换算：原来是每帧固定 px，与刷新率绑定，
            // 120Hz 屏上正好快一倍。首帧没有上一帧时间戳，dt 记 0（本帧不滚）。
            val dtSec = if (lastNanos == 0L) {
                0f
            } else {
                ((nanos - lastNanos) / 1_000_000_000f).coerceAtMost(MAX_FRAME_DT_SEC)
            }
            lastNanos = nanos
            val limitPx = SCROLL_SPEED_DP_PER_SEC * density.density * dtSec
            if (limitPx > 0f) {
                val over = state.lazyTick()
                val delta = autoScrollDelta(over, state.autoScrollMarginPx, limitPx)
                if (delta != 0f) listState.dispatchRawDelta(delta)
            }
        }
    }

    // 拖拽中页面销毁（返回 / 切 Tab / 旋转）时手势回调走不完，拖拽态会残留：
    // keys 停在乱序态、被拖项还挂着抬升缩放，且下次进页面看到的就是错的。
    // 退出组合时强制清理。
    DisposableEffect(Unit) { onDispose { state.cancel() } }
}

/**
 * 可拖拽排序的普通 Column（账号列表，嵌在 PageScaffold 的 verticalScroll 里，方案 v1 §4.3）。
 *
 * - **不 live 重排**（变高卡无法稳定让位）：拖拽中把 from..to 之间的项整体平移一格
 *   （被拖项高度 + 间距）做出空位，被拖项跟手 + 目标卡描边高亮（P5）；
 * - 让位量恰好等于「新序里这些项的位移」，所以松口提交落地后把偏移按同一方向补回一格
 *   再归零，视觉位置连续，不会有二次跳动；
 * - 松口落位动画：等新序布局落地（被拖项 onPlaced）后用「松手时的视觉中心
 *   − 新布局中心」续上位移再 spring 归零 —— 否则新序一下排好，位移基准就换了，只能瞬移；
 * - bounds 用 window 坐标（onPlaced 收集），嵌套不影响命中判定；
 * - 经 [LocalPageScroll] 拿到页面滚动：拖拽中页面级边缘自动滚动（P6）。
 */
@Composable
fun <T> ReorderableColumn(
    items: List<T>,
    keyOf: (T) -> String,
    state: ReorderState,
    onCommit: (key: String, toIndex: Int) -> Unit,
    modifier: Modifier = Modifier,
    verticalArrangement: Arrangement.Vertical = Arrangement.spacedBy(14.dp),
    itemContent: @Composable (item: T, index: Int, isTarget: Boolean) -> Unit,
) {
    val pageScroll = LocalPageScroll.current
    val view = LocalView.current
    val density = LocalDensity.current
    val scope = rememberCoroutineScope()
    SideEffect {
        state.liveReorder = false
        state.onCommit = onCommit
        state.pageScroll = pageScroll
        state.scope = scope
        state.autoScrollMarginPx = with(density) { AUTO_SCROLL_MARGIN_DP.dp.toPx() }
        state.columnSpacingPx = with(density) { verticalArrangement.spacing.toPx() }
    }
    LaunchedEffect(items) { state.syncKeys(items.map(keyOf)) }

    Column(
        // 排序态空闲自动退出：与 Lazy 变体同款，只旁听不消费。
        modifier = modifier.pointerInput(state) {
            awaitPointerEventScope {
                while (true) {
                    val event = awaitPointerEvent(PointerEventPass.Initial)
                    val uptime = event.changes.firstOrNull()?.uptimeMillis
                    if (uptime != null) state.noteInteraction(uptime)
                }
            }
        },
        verticalArrangement = verticalArrangement,
    ) {
        items.forEachIndexed { index, item ->
            val key = keyOf(item)
            val dragged = state.draggingKey == key
            val settling = !dragged && key == state.settleKey
            // pendingSettle 也算「抬着」：见 Lazy 变体同处的说明
            val lifted = dragged || settling || state.pendingSettleKey == key
            val isTarget = state.draggingKey != null && !dragged && state.targetKey == key
            // 抬升/落下与退让：与 Lazy 变体同款
            val lift by animateFloatAsState(
                targetValue = if (lifted) 1f else 0f,
                animationSpec = if (lifted) LIFT_UP_SPEC else LIFT_DOWN_SPEC,
                label = "reorderLift",
            )
            val dim by animateFloatAsState(
                targetValue = if (state.draggingKey != null && !lifted) DIM_ALPHA else 1f,
                animationSpec = tween(DIM_ANIM_MS),
                label = "reorderDim",
            )
            val cardShape = RoundedCornerShape(20.dp)
            Box(
                Modifier
                    .onPlaced { coords ->
                        val rect = coords.boundsInWindow()
                        state.bounds[key] = rect
                        // 松手落位：提交刚落地、被拖项换了槽位 → 算出续接位移交给帧循环起动画。
                        // onPlaced 只在布局真的变了时才触发，所以这里恰好就是「新序排好」那一刻。
                        state.onSettleLayoutLanded(key, rect.center.y)
                    }
                    .zIndex(if (lifted) 1f else 0f)
                    .graphicsLayer {
                        // 阴影写进同一个图层：原来 Modifier.shadow 在外层、
                        // translationY 在内层，卡片飘走了阴影却留在原槽位。详见 Lazy 变体。
                        shape = cardShape
                        clip = false
                        shadowElevation = LIFT_ELEVATION_DP.dp.toPx() * lift
                        when {
                            dragged -> translationY = state.columnTranslationY()
                            settling -> translationY = state.settleOffsetY
                            // 松手后、新序落地前：钉在松手位置，别弹回原槽位
                            state.pendingSettleKey == key ->
                                translationY = state.pendingSettleTranslationY(key)
                            else -> {
                                val gap = state.gapOffset[key]
                                if (gap != null) translationY = gap
                            }
                        }
                        if (lift > 0f) {
                            scaleX = 1f + (LIFT_SCALE - 1f) * lift
                            scaleY = scaleX
                        }
                        alpha = dim
                    },
            ) {
                itemContent(item, index, isTarget)
            }
        }
    }

    // 拖拽中 + 落位中都要跑帧循环：前者驱动边缘自动滚动与让位趋近，后者把「等新布局
    // 落地」的落位动画起起来、并让让位偏移在松手后继续收敛。三者都安静就退出，
    // 不白占帧回调。
    LaunchedEffect(state.draggingKey, state.settleKey, state.pendingSettleKey) {
        if (state.draggingKey == null && state.settleKey == null &&
            state.pendingSettleKey == null && state.gapOffset.isEmpty()
        ) {
            return@LaunchedEffect
        }
        var lastNanos = 0L
        while (isActive) {
            val nanos = withFrameNanos { it }
            // 同 Lazy 变体：按帧间隔换算速度，不再每帧固定 px。
            val dtSec = if (lastNanos == 0L) {
                0f
            } else {
                ((nanos - lastNanos) / 1_000_000_000f).coerceAtMost(MAX_FRAME_DT_SEC)
            }
            lastNanos = nanos
            val dragging = state.draggingKey != null
            val limitPx = SCROLL_SPEED_DP_PER_SEC * density.density * dtSec
            if (dragging && limitPx > 0f) {
                val over = state.columnTick(view.height.toFloat())
                val delta = autoScrollDelta(over, state.autoScrollMarginPx, limitPx)
                if (delta != 0f) pageScroll?.dispatchRawDelta(delta)
            }
            state.launchPendingSettle()
            val gapMoving = state.advanceGap(dtSec)
            if (!dragging && state.settleKey == null && !gapMoving && state.gapOffset.isEmpty()) break
        }
    }

    // 同 Lazy 变体：页面销毁时清掉残留拖拽态。
    DisposableEffect(Unit) { onDispose { state.cancel() } }
}
