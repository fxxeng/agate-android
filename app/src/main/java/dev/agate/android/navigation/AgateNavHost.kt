package dev.agate.android.navigation

import android.app.Activity
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Assignment
import androidx.compose.material.icons.filled.Dns
import androidx.compose.material.icons.filled.Key
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Dashboard
import androidx.compose.material.icons.filled.Dataset
import androidx.compose.material.icons.filled.ReceiptLong
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material.icons.filled.ViewQuilt
import androidx.compose.material.icons.filled.WorkspacePremium
import androidx.compose.material.icons.filled.Route
import androidx.compose.material.icons.filled.Bolt
import androidx.compose.material.icons.filled.BugReport
import androidx.compose.material.icons.filled.Lan
import androidx.compose.material3.DrawerValue
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ModalNavigationDrawer
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.rememberDrawerState
import androidx.compose.material.icons.filled.Verified
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.SideEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import androidx.navigation.NavType
import androidx.navigation.navArgument
import dev.agate.android.data.GatewayRepository
import dev.agate.android.ui.components.AgateBottomBar
import dev.agate.android.ui.components.AgateTabItem
import dev.agate.android.ui.home.ConsolePage
import dev.agate.android.ui.logs.AppLogPage
import dev.agate.android.ui.logs.CallLogPage
import dev.agate.android.ui.logs.RequestDetailPage
import dev.agate.android.ui.models.ModelCapsFocus
import dev.agate.android.ui.models.ModelCapsPage
import dev.agate.android.ui.models.ModelMappingPage
import dev.agate.android.ui.models.ModelSpeedtestPage
import dev.agate.android.ui.models.ModelsPage
import dev.agate.android.ui.routing.RoutingPage
import dev.agate.android.ui.pages.GatewayControlPage
import dev.agate.android.ui.pages.ShowcasePage
import dev.agate.android.ui.provider.AddProviderPage
import dev.agate.android.ui.provider.ProviderDetailPage
import dev.agate.android.ui.provider.ProviderPage
import dev.agate.android.ui.tasks.TasksPage
import dev.agate.android.ui.tasks.CheckinCenterPage
import dev.agate.android.ui.tasks.CheckinSchedulePage
import dev.agate.android.ui.tasks.CheckinRecordsPage
import dev.agate.android.ui.tasks.PeriodicTasksPage
import dev.agate.android.ui.pages.SkeletonPage
import dev.agate.android.ui.theme.AgateThemeMode
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import dev.agate.core.GatewayPrefs
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/** 路由常量（4 主 Tab + 22 二级页）。 */
object Routes {
    const val HOME = "home"
    const val PROVIDERS = "providers"
    const val MODELS = "models"
    const val TASKS = "tasks"

    const val GATEWAY = "gateway"
    const val PROVIDER_DETAIL = "provider_detail"
    const val PROVIDER_ADD = "provider_add"
    const val MODEL_SPEEDTEST = "model_speedtest"
    const val MODEL_MAPPING = "model_mapping"
    const val MODEL_CAPS = "model_caps"
    const val TASK_CHECKIN = "task_checkin"
    const val TASK_SCHEDULE = "task_schedule"
    const val TASK_RECORDS = "task_records"
    const val TASK_PERIODIC = "task_periodic"
    const val QUOTA = "quota"
    const val REPORTS = "reports"
    const val LOGS = "logs"
    const val APPLOGS = "applogs"
    const val PROXY = "proxy"
    const val ROUTING = "routing"
    const val ASSISTANT = "assistant"
    const val SETTINGS = "settings"
    const val KEYS = "keys"
    const val SHOWCASE = "showcase"
}

private data class DrawerEntry(val route: String, val label: String, val icon: ImageVector)
private data class DrawerSection(val title: String?, val items: List<DrawerEntry>)

/**
 * 侧抽屉分组（对齐 v44 `.drawer`：宽度 78%、页头 + 版本/状态、分组标题、
 * 选中项 accent 12% 底 + accent 文字、组间虚线分隔）。主入口置顶，
 * 其余按「数据与日志 / 运维 / 其他」归类。
 */
private val DrawerSections: List<DrawerSection> = listOf(
    DrawerSection(null, listOf(
        DrawerEntry(Routes.HOME, "控制台", Icons.Filled.Dashboard),
        DrawerEntry(Routes.PROVIDERS, "提供商", Icons.Filled.Dns),
        DrawerEntry(Routes.MODELS, "模型", Icons.Filled.Dataset),
        DrawerEntry(Routes.TASKS, "任务中心", Icons.Filled.Assignment),
    )),
    DrawerSection("数据与报表", listOf(
        DrawerEntry(Routes.QUOTA, "额度中心", Icons.Filled.WorkspacePremium),
        DrawerEntry(Routes.REPORTS, "数据报表", Icons.Filled.Dashboard),
        DrawerEntry(Routes.LOGS, "调用记录", Icons.Filled.ReceiptLong),
        DrawerEntry(Routes.APPLOGS, "应用日志", Icons.Filled.Terminal),
    )),
    DrawerSection("运维与路由", listOf(
        DrawerEntry(Routes.GATEWAY, "网关控制", Icons.Filled.Bolt),
        DrawerEntry(Routes.ROUTING, "路由预演", Icons.Filled.Route),
        DrawerEntry(Routes.PROXY, "出网代理池", Icons.Filled.Lan),
        DrawerEntry(Routes.ASSISTANT, "AI 运维助手", Icons.Filled.Verified),
    )),
    DrawerSection("设置", listOf(
        DrawerEntry(Routes.KEYS, "访问密钥", Icons.Filled.Key),
        DrawerEntry(Routes.SETTINGS, "设置", Icons.Filled.Settings),
    )),
    DrawerSection("开发调试", listOf(
        DrawerEntry(Routes.SHOWCASE, "组件走查", Icons.Filled.ViewQuilt),
    )),
)

@Composable
private fun AgateDrawer(
    currentRoute: String,
    onSelect: (String) -> Unit,
) {
    val C = agateColors()
    val drawerWidth = (LocalConfiguration.current.screenWidthDp * 0.78f).dp
    Column(
        Modifier
            .width(drawerWidth)
            .fillMaxHeight()
            .background(C.cardBg)
            .statusBarsPadding()
            .navigationBarsPadding(),
    ) {
        // 页头（固定，v44 .drawer-header）
        Column(
            Modifier
                .fillMaxWidth()
                .padding(start = 24.dp, end = 24.dp, top = 16.dp, bottom = 14.dp),
        ) {
            Text("Agate", fontSize = 20.sp, fontWeight = FontWeight.Bold, color = C.textMain)
            Spacer(Modifier.height(4.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("AI Gateway", fontSize = 12.sp, color = C.textSub)
                Spacer(Modifier.width(8.dp))
                Box(Modifier.size(6.dp).background(C.green, CircleShape))
                Spacer(Modifier.width(4.dp))
                Text("网关", fontSize = 12.sp, color = C.green)
            }
        }
        DashedDivider()

        // 分组列表（独立滚动）
        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 12.dp, vertical = 8.dp),
        ) {
            DrawerSections.forEachIndexed { idx, section ->
                section.title?.let {
                    Text(
                        it,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.SemiBold,
                        color = C.textDim,
                        modifier = Modifier.padding(start = 12.dp, top = 12.dp, bottom = 4.dp),
                    )
                }
                section.items.forEach { entry ->
                    DrawerItemRow(entry, currentRoute == entry.route) { onSelect(entry.route) }
                }
                if (idx < DrawerSections.size - 1) {
                    Spacer(Modifier.height(8.dp))
                    DashedDivider()
                }
            }
            Spacer(Modifier.height(8.dp))
        }

        // 页脚（固定）
        DashedDivider()
        Text(
            "Agate · v0.4.0-m3",
            fontSize = 11.sp,
            color = C.textDim,
            modifier = Modifier.padding(start = 24.dp, top = 10.dp, bottom = 12.dp),
        )
    }
}

@Composable
private fun DrawerItemRow(entry: DrawerEntry, active: Boolean, onClick: () -> Unit) {
    val C = agateColors()
    Row(
        Modifier
            .fillMaxWidth()
            .background(
                if (active) C.accent.copy(alpha = 0.12f) else Color.Transparent,
                RoundedCornerShape(12.dp),
            )
            .clickable(onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(
            imageVector = entry.icon,
            contentDescription = null,
            tint = if (active) C.accent else C.textSub,
            modifier = Modifier.size(20.dp),
        )
        Spacer(Modifier.width(12.dp))
        Text(
            entry.label,
            fontSize = 14.sp,
            fontWeight = if (active) FontWeight.SemiBold else FontWeight.Normal,
            color = if (active) C.accent else C.textSub,
        )
    }
}

/** v44 .drawer-divider：虚线分隔（1px dashed card-border）。 */
@Composable
private fun DashedDivider() {
    val C = agateColors()
    Canvas(
        Modifier
            .fillMaxWidth()
            .height(1.dp)
            .padding(horizontal = 12.dp),
    ) {
        drawLine(
            color = C.border,
            start = Offset(0f, size.height / 2),
            end = Offset(size.width, size.height / 2),
            strokeWidth = 1.dp.toPx(),
            pathEffect = PathEffect.dashPathEffect(floatArrayOf(8f, 8f), 0f),
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AgateNavHost(
    themeMode: AgateThemeMode,
    onThemeModeChange: (AgateThemeMode) -> Unit,
    modifier: Modifier = Modifier,
    navController: NavHostController = rememberNavController(),
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val drawerState = rememberDrawerState(DrawerValue.Closed)
    val scope = rememberCoroutineScope()
    val backStack by navController.currentBackStackEntryAsState()
    val currentRoute = backStack?.destination?.route ?: Routes.HOME

    // 导航栏配色随层级切换：一级页有底栏(chromeBg)，二级页无底栏 → 与页面底色一致，
    // 否则手势条区域会出现一条明显色带（尤其 API<35 未强制 edge-to-edge 时）。
    val isTopLevel = currentRoute in TopLevelRoutes
    val view = LocalView.current
    SideEffect {
        val window = (view.context as? Activity)?.window ?: return@SideEffect
        @Suppress("DEPRECATION")
        window.navigationBarColor = (if (isTopLevel) C.chromeBg else C.bg).toArgb()
    }

    fun openDrawer() = scope.launch { drawerState.open() }

    // ── 全应用一份快照 ───────────────────────────────────────────
    // `GatewayRepository.snapshot` 是进程级单例，提供商 / 模型 / 额度中心 /
    // 调用日志 / 控制台五个页面都从它读账号与模型。但轮询此前只由控制台页启动，
    // 于是**没先进过控制台**就拿不到数据：覆盖安装后直接打开提供商页会看到
    // 「全部 · 0 / 还没有任何账号」，而同一时刻签到中心（自己的端点）明明有账号。
    //
    // 生命周期提到导航宿主：进应用就探活 + 轮询，退到后台（组合销毁）再停。
    // 这里同时也接管了原先控制台页那份 `Gateway.status()` 循环 —— 两处都探活
    // 会让 `started` 被反复置位、SSE 连两次。
    LaunchedEffect(Unit) {
        // 端口必须一起灌进去：SSE 与探活都按它拼地址（见 configure 的说明）
        GatewayRepository.configure(GatewayPrefs.apiKey(ctx), GatewayPrefs.port(ctx))
        GatewayRepository.startPolling()
        while (true) {
            GatewayRepository.syncGatewayRunning()
            delay(5_000L)
        }
    }
    DisposableEffect(Unit) {
        onDispose { GatewayRepository.stopPolling() }
    }

    ModalNavigationDrawer(
        drawerState = drawerState,
        modifier = modifier,
        drawerContent = {
            AgateDrawer(
                currentRoute = currentRoute,
                onSelect = { route ->
                    scope.launch { drawerState.close() }
                    if (currentRoute != route) {
                        navController.navigate(route) {
                            popUpTo(Routes.HOME) { saveState = true }
                            launchSingleTop = true
                            restoreState = true
                        }
                    }
                },
            )
        },
    ) {
        // v44 外壳：页头由各页面自绘（AgatePageHeader），此处只留底栏；
        // 移除 Material3 TopAppBar 以消除「顶栏 + 页面标题」双层重复。
        // contentWindowInsets = 0：系统栏内边距交给页头(statusBarsPadding)与底栏
        // (navigationBarsPadding) 各自处理，页头/底栏背景才能延伸到系统栏下方，
        // 消除「状态栏 / 导航栏」与外壳之间的分界缝（Android 15 强制 edge-to-edge）。
        Scaffold(
            containerColor = C.bg,
            contentWindowInsets = WindowInsets(0, 0, 0, 0),
            bottomBar = {
                // v44：二级页为全屏覆盖（无底栏），仅一级 Tab 显示底栏
                if (currentRoute in TopLevelRoutes) {
                    AgateBottomBar(
                        items = TabItems,
                        currentRoute = currentRoute,
                        onSelect = { route ->
                            navController.navigate(route) {
                                popUpTo(Routes.HOME) { saveState = true }
                                launchSingleTop = true
                                restoreState = true
                            }
                        },
                    )
                }
            },
        ) { padding ->
            NavHost(
                navController = navController,
                startDestination = Routes.HOME,
                modifier = Modifier.fillMaxSize().padding(padding),
            ) {
                composable(Routes.HOME) {
                    ConsolePage(
                        onOpenDrawer = { openDrawer() },
                        onNavigate = { route ->
                            navController.navigate(route) {
                                popUpTo(Routes.HOME) { saveState = true }
                                launchSingleTop = true
                                restoreState = true
                            }
                        },
                    )
                }
                composable(Routes.PROVIDERS) {
                    ProviderPage(
                        onOpenDrawer = { openDrawer() },
                        onOpenAdd = { navController.navigate(Routes.PROVIDER_ADD) },
                        onOpenDetail = { id -> navController.navigate("provider_detail/" + id) },
                        // 「明细 ›」跳到额度中心：这一行只够一个读数，逐份额度包
                        // （workbuddy 的 credits[]、trae 的权益包等）在那边展开看
                        onOpenQuotaCenter = { navController.navigate(Routes.QUOTA) },
                    )
                }
                composable(Routes.MODELS) {
                    ModelsPage(
                        onOpenDrawer = { openDrawer() },
                        onOpenSpeedtest = { navController.navigate(Routes.MODEL_SPEEDTEST) },
                        onOpenCaps = { providerId, modelId ->
                            // 带上上下文：能力位页打开就落在这一条上（见 ModelCapsFocus）
                            ModelCapsFocus.set(providerId, modelId)
                            navController.navigate(Routes.MODEL_CAPS)
                        },
                        onOpenMapping = { navController.navigate(Routes.MODEL_MAPPING) },
                    )
                }
                composable(Routes.TASKS) {
                    TasksPage(
                        onOpenDrawer = { openDrawer() },
                        onOpenCheckin = { navController.navigate(Routes.TASK_CHECKIN) },
                        onOpenSchedule = { navController.navigate(Routes.TASK_SCHEDULE) },
                        onOpenRecords = { navController.navigate(Routes.TASK_RECORDS) },
                        onOpenPeriodic = { navController.navigate(Routes.TASK_PERIODIC) },
                    )
                }

                composable(Routes.GATEWAY) { GatewayControlPage(onOpenDrawer = { openDrawer() }) }
                composable(Routes.SHOWCASE) { ShowcasePage(themeMode, onThemeModeChange, onOpenDrawer = { openDrawer() }) }

                composable(
                    "provider_detail/{id}",
                    arguments = listOf(navArgument("id") { type = NavType.StringType }),
                ) { entry ->
                    ProviderDetailPage(
                        providerId = entry.arguments?.getString("id") ?: "",
                        onBack = { navController.popBackStack() },
                        onAddAccount = {
                            val pid = entry.arguments?.getString("id").orEmpty()
                            navController.navigate("provider_add?providerId=$pid")
                        },
                        // 账号 tab 底部的「队列位置在路由页调整」指路行
                        onOpenRouting = { navController.navigate(Routes.ROUTING) },
                    )
                }
                composable(
                    "provider_add?providerId={providerId}",
                    arguments = listOf(
                        navArgument("providerId") {
                            type = NavType.StringType
                            nullable = true
                            defaultValue = null
                        },
                    ),
                ) { entry ->
                    AddProviderPage(
                        initialProviderId = entry.arguments?.getString("providerId"),
                        onBack = { navController.popBackStack() },
                    )
                }
                // 模型测速：原先指向骨架页（「占位区块」），现在接真实页面。
                // 内核 `POST /api/models/test` 早已就绪（走生产转发链路、钉账号），
                // 缺的一直只是这块界面。
                composable(Routes.MODEL_SPEEDTEST) {
                    ModelSpeedtestPage(onBack = { navController.popBackStack() })
                }
                composable(Routes.MODEL_MAPPING) { ModelMappingPage(onBack = { navController.popBackStack() }) }
                composable(Routes.MODEL_CAPS) { ModelCapsPage(onBack = { navController.popBackStack() }) }
                composable(Routes.TASK_CHECKIN) {
                    // 签到中心：总览四格 + 每日签到（分组/单账号/国际版三档）
                    // + 新手福利 + 活动福利 + 成长任务 —— 进页面零上游请求
                    CheckinCenterPage(
                        onBack = { navController.popBackStack() },
                        onOpenSchedule = { navController.navigate(Routes.TASK_SCHEDULE) },
                        onOpenRecords = { navController.navigate(Routes.TASK_RECORDS) },
                    )
                }
                composable(Routes.TASK_SCHEDULE) {
                    // 定时签到：总开关 + 条目 CRUD（行内表单）+ 保活链拖动排序
                    CheckinSchedulePage(onBack = { navController.popBackStack() })
                }
                composable(Routes.TASK_RECORDS) {
                    // 签到记录：每账号一行的明细（服务端分页 + 四筛选 + all=1 护栏）
                    CheckinRecordsPage(onBack = { navController.popBackStack() })
                }
                composable(Routes.TASK_PERIODIC) {
                    // 周期任务：backend 三条（可立即执行）+ frontend 三条（不给按钮）
                    PeriodicTasksPage(onBack = { navController.popBackStack() })
                }
                composable(Routes.QUOTA) {
                    dev.agate.android.ui.quota.QuotaCenterPage(
                        onOpenDrawer = { openDrawer() },
                        onOpenProvider = { pid -> navController.navigate("provider_detail/$pid") },
                    )
                }
                composable(Routes.REPORTS) {
                    // 数据报表：概览 / 趋势 / 24h 双轴 / 365 天热力图 / 用量占比 / 三张排行
                    dev.agate.android.ui.reports.ReportsPage(onOpenDrawer = ::openDrawer)
                }
                composable(Routes.LOGS) {
                    // 调用记录：模型调用明细（列表 + 筛选 + 终止 + 清理）
                    CallLogPage(
                        onOpenDrawer = ::openDrawer,
                        onOpenDetail = { id -> navController.navigate("request_detail/$id") },
                    )
                }
                composable(
                    "request_detail/{id}",
                    arguments = listOf(navArgument("id") { type = NavType.StringType }),
                ) { entry ->
                    RequestDetailPage(
                        requestId = entry.arguments?.getString("id").orEmpty(),
                        onBack = { navController.popBackStack() },
                    )
                }
                composable(Routes.APPLOGS) {
                    // 应用日志：App 自身运行事件流（分类 / 级别 / 关键词 + 导出）
                    AppLogPage(onOpenDrawer = ::openDrawer)
                }
                composable(Routes.PROXY) {
                    // 出网代理池：条目列表 / 三种来源 / 测试 / 同步 Clash / 端口范围设置
                    dev.agate.android.ui.proxy.ProxyPoolPage(onOpenDrawer = ::openDrawer)
                }
                composable(Routes.ROUTING) {
                    // 路由预演：三策略 + 候选队列（含跳过原因）+ 自愈规则
                    RoutingPage(onOpenDrawer = ::openDrawer)
                }
                composable(Routes.ASSISTANT) {
                    // AI 运维助手：端口 SSE 流式对话 + 8 只工具（改动类先弹确认卡）
                    dev.agate.android.ui.assistant.AssistantPage(onOpenDrawer = ::openDrawer)
                }
                composable(Routes.SETTINGS) {
                    // 设置：十组手风琴（服务接入 / 外观 / 流式 / 超时 / 重试 /
                    // 脱敏与安全 / 提示词 / 数据 / 备份 / 关于），见该页模块头
                    dev.agate.android.ui.settings.SettingsPage(
                        themeMode = themeMode,
                        onThemeModeChange = onThemeModeChange,
                        onOpenDrawer = ::openDrawer,
                        onBack = { navController.popBackStack() },
                    )
                }
                composable(Routes.KEYS) {
                    dev.agate.android.ui.keys.KeysPage(
                        onOpenDrawer = ::openDrawer,
                        onBack = { navController.popBackStack() },
                    )
                }
            }
        }
    }
}

@Composable
private fun skeleton(
    @Suppress("UNUSED_PARAMETER") entry: androidx.navigation.NavBackStackEntry,
    navController: NavHostController,
    title: String,
    milestone: String,
    hint: String,
    onOpenDrawer: () -> Unit,
) {
    SkeletonPage(
        title = title,
        milestone = milestone,
        hint = hint,
        onOpenDrawer = onOpenDrawer,
        onBack = { navController.popBackStack() },
    )
}

private val TabItems = listOf(
    AgateTabItem(Routes.HOME, "控制台", Icons.Filled.Dashboard),
    AgateTabItem(Routes.PROVIDERS, "提供商", Icons.Filled.Dns),
    AgateTabItem(Routes.MODELS, "模型", Icons.Filled.Dataset),
    AgateTabItem(Routes.TASKS, "任务中心", Icons.Filled.Assignment),
)

/** 一级 Tab（显示底栏）；其余路由均按二级页处理（全屏覆盖、无底栏）。 */
private val TopLevelRoutes = setOf(Routes.HOME, Routes.PROVIDERS, Routes.MODELS, Routes.TASKS)
