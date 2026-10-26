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
import dev.agate.android.ui.components.AgateBottomBar
import dev.agate.android.ui.components.AgateTabItem
import dev.agate.android.ui.home.ConsolePage
import dev.agate.android.ui.models.ModelCapsPage
import dev.agate.android.ui.models.ModelMappingPage
import dev.agate.android.ui.models.ModelsPage
import dev.agate.android.ui.pages.GatewayControlPage
import dev.agate.android.ui.pages.ShowcasePage
import dev.agate.android.ui.provider.AddProviderPage
import dev.agate.android.ui.provider.ProviderDetailPage
import dev.agate.android.ui.provider.ProviderPage
import dev.agate.android.ui.tasks.TasksPage
import dev.agate.android.ui.pages.SkeletonPage
import dev.agate.android.ui.theme.AgateThemeMode
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
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
    const val TASK_WELFARE = "task_welfare"
    const val TASK_CAPTCHA = "task_captcha"
    const val TASK_CHECKIN = "task_checkin"
    const val TASK_KEEPALIVE = "task_keepalive"
    const val TASK_SCHEDULES = "task_schedules"
    const val TASK_HISTORY = "task_history"
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
                        onOpenCaps = { navController.navigate(Routes.MODEL_CAPS) },
                        onOpenMapping = { navController.navigate(Routes.MODEL_MAPPING) },
                    )
                }
                composable(Routes.TASKS) {
                    TasksPage(
                        onOpenDrawer = { openDrawer() },
                        onOpenWelfare = { navController.navigate(Routes.TASK_WELFARE) },
                        onOpenCaptcha = { navController.navigate(Routes.TASK_CAPTCHA) },
                        onOpenCheckin = { navController.navigate(Routes.TASK_CHECKIN) },
                        onOpenKeepalive = { navController.navigate(Routes.TASK_KEEPALIVE) },
                        onOpenSchedules = { navController.navigate(Routes.TASK_SCHEDULES) },
                        onOpenHistory = { navController.navigate(Routes.TASK_HISTORY) },
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
                composable(Routes.MODEL_SPEEDTEST) { skeleton(it, navController, "模型测速", "M3", "并发测速（SSE 流式）、思考等级、多账号同测。", ::openDrawer) }
                composable(Routes.MODEL_MAPPING) { ModelMappingPage(onBack = { navController.popBackStack() }) }
                composable(Routes.MODEL_CAPS) { ModelCapsPage(onBack = { navController.popBackStack() }) }
                composable(Routes.TASK_WELFARE) { skeleton(it, navController, "福利中心", "M4", "一次性项：新手任务 + 平台福利。", ::openDrawer) }
                composable(Routes.TASK_CAPTCHA) { skeleton(it, navController, "验证码池", "M4", "ZCode 活动套餐票据池、水位补货、阿里 V3 滑块铸造。", ::openDrawer) }
                composable(Routes.TASK_CHECKIN) { skeleton(it, navController, "定时签到", "M4", "北京时间签到计划（08:00/20:00）、Qoder 国际版 10:00 窗口、分组视图。", ::openDrawer) }
                composable(Routes.TASK_KEEPALIVE) { skeleton(it, navController, "保活链", "M4", "日活任务链编辑：WorkBuddy intl 活跃任务等。", ::openDrawer) }
                composable(Routes.TASK_SCHEDULES) { skeleton(it, navController, "周期任务", "M4", "后台周期任务总览与下次执行时间。", ::openDrawer) }
                composable(Routes.TASK_HISTORY) { skeleton(it, navController, "签到台账", "M4", "批量轮次记录、失败明细前 5 条、checkinAt 落账。", ::openDrawer) }
                composable(Routes.QUOTA) {
                    dev.agate.android.ui.quota.QuotaCenterPage(
                        onOpenDrawer = { openDrawer() },
                        onOpenProvider = { pid -> navController.navigate("provider_detail/$pid") },
                    )
                }
                composable(Routes.REPORTS) { skeleton(it, navController, "数据报表", "M4", "6 档时间窗、365 天热力图、24h 双轴、三维构成占比。", ::openDrawer) }
                composable(Routes.LOGS) { skeleton(it, navController, "调用记录", "M3", "转发请求流水、调用报文、出网取证 Diff、筛选 Sheet。", ::openDrawer) }
                composable(Routes.APPLOGS) { skeleton(it, navController, "应用日志", "M3", "内核 console_line 转发的应用侧日志流。", ::openDrawer) }
                composable(Routes.PROXY) { skeleton(it, navController, "出网代理池", "M4", "代理分组、健康检查、绑定提供商。", ::openDrawer) }
                composable(Routes.ROUTING) { skeleton(it, navController, "路由", "M3", "三策略选号预演、候选队列跨家交错拖拽、自愈演练与倒计时回队、限制器。", ::openDrawer) }
                composable(Routes.ASSISTANT) { skeleton(it, navController, "AI 运维助手", "M4", "4 大快捷诊断 Chips、工具调用气泡、流式对话。", ::openDrawer) }
                composable(Routes.SETTINGS) { skeleton(it, navController, "设置", "M4", "手风琴分组：主题三档 / 传输 / 脱敏 / 备份与恢复中心。", ::openDrawer) }
                composable(Routes.KEYS) { skeleton(it, navController, "访问密钥", "M3", "网关 API Key 管理：创建/吊销，/v1 闸门依据。", ::openDrawer) }
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
