package dev.agate.android.navigation

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Assignment
import androidx.compose.material.icons.filled.Dns
import androidx.compose.material.icons.filled.Key
import androidx.compose.material.icons.filled.Menu
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Speed
import androidx.compose.material.icons.filled.Dashboard
import androidx.compose.material.icons.filled.Dataset
import androidx.compose.material.icons.filled.ReceiptLong
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material.icons.filled.ViewQuilt
import androidx.compose.material.icons.filled.WorkspacePremium
import androidx.compose.material.icons.filled.Route
import androidx.compose.material.icons.filled.Bolt
import androidx.compose.material.icons.filled.BugReport
import androidx.compose.material.icons.filled.Lan
import androidx.compose.material3.DrawerValue
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalDrawerSheet
import androidx.compose.material3.ModalNavigationDrawer
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.rememberDrawerState
import androidx.compose.material.icons.filled.Verified
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import androidx.navigation.NavType
import androidx.navigation.navArgument
import dev.agate.android.ui.home.ConsolePage
import dev.agate.android.ui.pages.GatewayControlPage
import dev.agate.android.ui.pages.ShowcasePage
import dev.agate.android.ui.provider.AddProviderPage
import dev.agate.android.ui.provider.ProviderDetailPage
import dev.agate.android.ui.provider.ProviderPage
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

/** 侧抽屉入口（11 项；其余二级页由各自 Tab 内进入）。 */
private val DrawerItems = listOf(
    DrawerEntry(Routes.GATEWAY, "网关控制", Icons.Filled.Bolt),
    DrawerEntry(Routes.REPORTS, "数据报表", Icons.Filled.Dashboard),
    DrawerEntry(Routes.QUOTA, "额度中心", Icons.Filled.WorkspacePremium),
    DrawerEntry(Routes.LOGS, "调用记录", Icons.Filled.ReceiptLong),
    DrawerEntry(Routes.APPLOGS, "应用日志", Icons.Filled.Terminal),
    DrawerEntry(Routes.PROXY, "出网代理池", Icons.Filled.Lan),
    DrawerEntry(Routes.ROUTING, "路由预演", Icons.Filled.Route),
    DrawerEntry(Routes.ASSISTANT, "AI 运维助手", Icons.Filled.Verified),
    DrawerEntry(Routes.SETTINGS, "设置", Icons.Filled.Settings),
    DrawerEntry(Routes.KEYS, "访问密钥", Icons.Filled.Key),
    DrawerEntry(Routes.SHOWCASE, "组件走查", Icons.Filled.ViewQuilt),
)

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

    ModalNavigationDrawer(
        drawerState = drawerState,
        modifier = modifier,
        drawerContent = {
            ModalDrawerSheet {
                Text(
                    "Agate",
                    style = AgateType.PageTitle,
                    color = C.textMain,
                    modifier = Modifier.padding(18.dp),
                )
                DrawerItems.forEach { entry ->
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .clickable {
                                scope.launch { drawerState.close() }
                                if (currentRoute != entry.route) {
                                    navController.navigate(entry.route) {
                                        popUpTo(Routes.HOME) { saveState = true }
                                        launchSingleTop = true
                                        restoreState = true
                                    }
                                }
                            }
                            .padding(horizontal = 16.dp, vertical = 12.dp),
                    ) {
                        Icon(
                            imageVector = entry.icon,
                            contentDescription = null,
                            tint = if (currentRoute == entry.route) C.accent else C.textSub,
                            modifier = Modifier.size(20.dp),
                        )
                        Spacer(Modifier.size(12.dp))
                        Text(
                            entry.label,
                            style = AgateType.Body,
                            color = if (currentRoute == entry.route) C.accent else C.textSub,
                        )
                    }
                }
            }
        },
    ) {
        Scaffold(
            topBar = {
                TopAppBar(
                    title = { Text(titleOf(currentRoute), style = AgateType.PageTitle) },
                    navigationIcon = {
                        IconButton(onClick = { scope.launch { drawerState.open() } }) {
                            Icon(Icons.Filled.Menu, contentDescription = "打开侧栏")
                        }
                    },
                )
            },
            bottomBar = {
                NavigationBar {
                    TabItems.forEach { tab ->
                        val selected = currentRoute == tab.route
                        NavigationBarItem(
                            selected = selected,
                            onClick = {
                                if (!selected) {
                                    navController.navigate(tab.route) {
                                        popUpTo(Routes.HOME) { saveState = true }
                                        launchSingleTop = true
                                        restoreState = true
                                    }
                                }
                            },
                            icon = { Icon(tab.icon, contentDescription = tab.label) },
                            label = { Text(tab.label, style = AgateType.Caption) },
                        )
                    }
                }
            },
        ) { padding ->
            NavHost(
                navController = navController,
                startDestination = Routes.HOME,
                modifier = Modifier.padding(padding),
            ) {
                composable(Routes.HOME) {
                    ConsolePage(
                        onOpenDrawer = { scope.launch { drawerState.open() } },
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
                        onOpenAdd = { navController.navigate(Routes.PROVIDER_ADD) },
                        onOpenDetail = { id -> navController.navigate("provider_detail/" + id) },
                    )
                }
                composable(Routes.MODELS) {
                    SkeletonPage(
                        "模型", "M3",
                        "客户端所见清单（xp-card 推演）、按模型/按提供商/混合三模式、测速、映射、能力位与目录刷新。",
                    )
                }
                composable(Routes.TASKS) {
                    SkeletonPage(
                        "任务中心", "M4",
                        "今日待办、福利中心、验证码池、定时签到、日活保活链、周期任务与 50 轮台账。",
                    )
                }

                composable(Routes.GATEWAY) { GatewayControlPage() }
                composable(Routes.SHOWCASE) { ShowcasePage(themeMode, onThemeModeChange) }

                composable(
                    "provider_detail/{id}",
                    arguments = listOf(navArgument("id") { type = NavType.StringType }),
                ) { entry ->
                    ProviderDetailPage(
                        providerId = entry.arguments?.getString("id") ?: "",
                        onBack = { navController.popBackStack() },
                    )
                }
                composable(Routes.PROVIDER_ADD) {
                    AddProviderPage(onBack = { navController.popBackStack() })
                }
                composable(Routes.MODEL_SPEEDTEST) { skeleton(it, "模型测速", "并发测速（SSE 流式）、思考等级、多账号同测。") }
                composable(Routes.MODEL_MAPPING) { skeleton(it, "模型映射", "alias → target 多家主备映射、enabled 开关、同三元组幂等。") }
                composable(Routes.MODEL_CAPS) { skeleton(it, "能力位", "六键：maxInput/maxOutput/toolCall/images/video/reasoning，capOverrides 标记。") }
                composable(Routes.TASK_WELFARE) { skeleton(it, "福利中心", "一次性项：新手任务 + 平台福利。") }
                composable(Routes.TASK_CAPTCHA) { skeleton(it, "验证码池", "ZCode 活动套餐票据池、水位补货、阿里 V3 滑块铸造。") }
                composable(Routes.TASK_CHECKIN) { skeleton(it, "定时签到", "北京时间签到计划（08:00/20:00）、Qoder 国际版 10:00 窗口、分组视图。") }
                composable(Routes.TASK_KEEPALIVE) { skeleton(it, "保活链", "日活任务链编辑：WorkBuddy intl 活跃任务等。") }
                composable(Routes.TASK_SCHEDULES) { skeleton(it, "周期任务", "后台周期任务总览与下次执行时间。") }
                composable(Routes.TASK_HISTORY) { skeleton(it, "签到台账", "批量轮次记录、失败明细前 5 条、checkinAt 落账。") }
                composable(Routes.QUOTA) { skeleton(it, "额度中心", "各账号余额与额度水位、刷新与下限限制器联动。") }
                composable(Routes.REPORTS) { skeleton(it, "数据报表", "6 档时间窗、365 天热力图、24h 双轴、三维构成占比。") }
                composable(Routes.LOGS) { skeleton(it, "调用记录", "转发请求流水、调用报文、出网取证 Diff、筛选 Sheet。") }
                composable(Routes.APPLOGS) { skeleton(it, "应用日志", "内核 console_line 转发的应用侧日志流。") }
                composable(Routes.PROXY) { skeleton(it, "出网代理池", "代理分组、健康检查、绑定提供商。") }
                composable(Routes.ROUTING) { skeleton(it, "路由", "三策略选号预演、候选队列跨家交错拖拽、自愈演练与倒计时回队、限制器。") }
                composable(Routes.ASSISTANT) { skeleton(it, "AI 运维助手", "4 大快捷诊断 Chips、工具调用气泡、流式对话。") }
                composable(Routes.SETTINGS) { skeleton(it, "设置", "手风琴分组：主题三档 / 传输 / 脱敏 / 备份与恢复中心。") }
                composable(Routes.KEYS) { skeleton(it, "访问密钥", "网关 API Key 管理：创建/吊销，/v1 闸门依据。") }
            }
        }
    }
}

@Composable
private fun skeleton(@Suppress("UNUSED_PARAMETER") entry: androidx.navigation.NavBackStackEntry, title: String, hint: String) {
    val ms = if (title in setOf("访问密钥")) "M3" else "M4"
    SkeletonPage(title, ms, hint)
}

private fun titleOf(route: String): String = when (route) {
    Routes.HOME -> "控制台"
    Routes.PROVIDERS -> "提供商"
    Routes.MODELS -> "模型"
    Routes.TASKS -> "任务中心"
    else -> DrawerItems.firstOrNull { it.route == route }?.label ?: "Agate"
}

private data class TabEntry(val route: String, val label: String, val icon: ImageVector)

private val TabItems = listOf(
    TabEntry(Routes.HOME, "控制台", Icons.Filled.Dashboard),
    TabEntry(Routes.PROVIDERS, "提供商", Icons.Filled.Dns),
    TabEntry(Routes.MODELS, "模型", Icons.Filled.Dataset),
    TabEntry(Routes.TASKS, "任务", Icons.Filled.Assignment),
)
