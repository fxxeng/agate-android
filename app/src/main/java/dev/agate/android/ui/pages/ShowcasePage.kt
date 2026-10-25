package dev.agate.android.ui.pages

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Bolt
import androidx.compose.material.icons.filled.Menu
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import dev.agate.android.ui.components.AgateAccordion
import dev.agate.android.ui.components.AgateAvatar
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateChip
import dev.agate.android.ui.components.AgateDragList
import dev.agate.android.ui.components.AgateDualAxisChart
import dev.agate.android.ui.components.AgateHeatmap
import dev.agate.android.ui.components.AgateHeatmapLegend
import dev.agate.android.ui.components.AgateKpiStrip
import dev.agate.android.ui.components.AgateProgress
import dev.agate.android.ui.components.AgateSearchBar
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.AgateStatusDots
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.KpiCell
import dev.agate.android.ui.theme.AgateThemeMode
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlin.math.abs
import kotlin.math.sin

/**
 * M2 组件走查页：12 个通用组件全展示 + 三档主题切换。
 * 验收口径：对照 123_v44.html 逐项视觉走查 + 深浅色跟随系统切换丝滑。
 */
@Composable
fun ShowcasePage(
    themeMode: AgateThemeMode,
    onThemeModeChange: (AgateThemeMode) -> Unit,
    onOpenDrawer: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    var search by remember { mutableStateOf("") }
    var segIdx by remember { mutableIntStateOf(0) }
    val dragItems = remember { mutableStateListOf("CatPaw", "WorkBuddy", "Qoder", "KukuAI", "Trae") }

    Column(
        modifier = modifier
            .fillMaxSize()
            .background(C.bg)
            .statusBarsPadding()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 16.dp),
    ) {
        Spacer(Modifier.height(6.dp))
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Icon(
                Icons.Filled.Menu,
                contentDescription = "打开侧栏",
                tint = C.textMain,
                modifier = Modifier.size(24.dp).clickable(onClick = onOpenDrawer),
            )
            Spacer(Modifier.width(10.dp))
            Text("组件走查", style = AgateType.PageTitle, color = C.textMain)
        }
        Spacer(Modifier.height(10.dp))

        // 主题三档（走查用；正式切换入口在设置页，仅三档不增）
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            AgateChip("跟随系统", themeMode == AgateThemeMode.FOLLOW_SYSTEM, onClick = { onThemeModeChange(AgateThemeMode.FOLLOW_SYSTEM) })
            AgateChip("浅色", themeMode == AgateThemeMode.LIGHT, onClick = { onThemeModeChange(AgateThemeMode.LIGHT) })
            AgateChip("深色", themeMode == AgateThemeMode.DARK, onClick = { onThemeModeChange(AgateThemeMode.DARK) })
        }
        Spacer(Modifier.height(18.dp))

        // 1 徽标
        Section("1 · 徽标 AgateBadge")
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            AgateBadge("运行中", BadgeTone.GREEN, withDot = true)
            AgateBadge("受限", BadgeTone.AMBER)
            AgateBadge("离线", BadgeTone.RED)
            AgateBadge("中转", BadgeTone.BLUE)
            AgateBadge("草稿", BadgeTone.GRAY)
        }

        // 2 并发点
        Section("2 · 并发点 AgateStatusDots")
        Column {
            LabeledDots("8 槽 3 活跃", 8, 3)
            LabeledDots("6 槽 6 满载", 6, 6, C.red)
            LabeledDots("4 槽 0 空闲", 4, 0)
        }

        // 3 胶囊
        Section("3 · 胶囊 AgateChip")
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            AgateChip("深度思考")
            AgateChip("已选模式", selected = true)
            AgateChip("限时", onClick = {})
        }

        // 4 分段控件
        Section("4 · 分段控件 AgateSegmented")
        AgateSegmented(listOf("按模型", "按提供商", "混合"), segIdx, onSelect = { segIdx = it })

        // 5 KPI 四联（tnum）
        Section("5 · KPI 四联 AgateKpiStrip（数字 tnum 等宽）")
        AgateKpiStrip(
            listOf(
                KpiCell("今日请求", "1,284"),
                KpiCell("活跃账号", "7/8", C.green),
                KpiCell("成功率", "99.2%", C.accent),
                KpiCell("P50 延迟", "842ms"),
            ),
        )

        // 6 进度条
        Section("6 · 进度条 AgateProgress（粗 8dp / 细 5dp）")
        Column {
            AgateProgress(0.72f, thick = true, label = "额度水位 72%（粗）")
            Spacer(Modifier.height(6.dp))
            AgateProgress(0.35f, thick = false, fillColor = C.amber, label = "账号余额 35%（细）")
        }

        // 7 热力图
        Section("7 · 热力图 AgateHeatmap（26 周）")
        val heat: List<Float> = remember {
            List(26 * 7) { i ->
                val v = sin(i * 0.37) * 0.5 + 0.5
                ((v * 100).toInt() % 100) / 100f
            }
        }
        AgateHeatmap(values = heat, weeks = 26)
        Spacer(Modifier.height(6.dp))
        AgateHeatmapLegend()

        // 8 双轴图
        Section("8 · 双轴图 AgateDualAxisChart（24h）")
        AgateDualAxisChart(
            volumes = remember {
                List(24) { h -> (abs(sin(h * 1.3)) * 220).toInt() + (if (h in 9..22) 60 else 0) }
            },
            successRates = remember {
                List(24) { h -> 0.86f + (abs(sin(h * 0.71)) * 0.13f).toFloat() }
            },
        )

        // 9 拖拽列表
        Section("9 · 拖拽列表 AgateDragList（350ms 长按抓起 · 触觉反馈 · 置顶）")
        AgateDragList(
            items = dragItems,
            onMove = { from, to ->
                val item = dragItems.removeAt(from)
                dragItems.add(to, item)
            },
            onPin = { index ->
                val item = dragItems.removeAt(index)
                dragItems.add(0, item)
            },
            itemContent = { name, index, dragging ->
                Row(
                    Modifier
                        .fillMaxWidth()
                        .background(if (dragging) C.cardBg else C.insetBg, RoundedCornerShape(12.dp))
                        .border(
                            1.dp,
                            if (dragging) C.accent else C.border,
                            RoundedCornerShape(12.dp),
                        )
                        .padding(horizontal = 12.dp, vertical = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    if (dragging) {
                        androidx.compose.foundation.layout.Box(
                            Modifier
                                .size(22.dp)
                                .background(C.accent, CircleShape),
                            contentAlignment = Alignment.Center,
                        ) {
                            Text("${index + 1}", style = AgateType.Caption, color = C.onAccent)
                        }
                        Spacer(Modifier.width(10.dp))
                    }
                    AgateAvatar(name = name, size = 34.dp, corner = 10.dp)
                    Spacer(Modifier.width(10.dp))
                    Text(name, style = AgateType.Body, color = C.textMain)
                    Spacer(Modifier.weight(1f))
                    AgateStatusDots(total = 3, active = (index + 1) % 4)
                }
            },
        )

        // 10 手风琴
        Section("10 · 手风琴 AgateAccordion")
        AgateAccordion(title = "深链路与令牌刷新", initiallyExpanded = true) {
            Text(
                "展开态内容：这里放真实页的业务块。animateContentSize 平滑开合，" +
                    "标题行右侧可挂 Badge。",
                style = AgateType.BodyS,
                color = C.textSub,
            )
        }
        Spacer(Modifier.height(8.dp))
        AgateAccordion(title = "折叠示例（点标题开合）") {
            Text("收起态只占标题行高度。", style = AgateType.BodyS, color = C.textSub)
        }

        // 11 头像
        Section("11 · 头像 AgateAvatar（色相 Hash · Logo 槽位）")
        Row(
            horizontalArrangement = Arrangement.spacedBy(10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            AgateAvatar("CatPaw")
            AgateAvatar("WorkBuddy")
            AgateAvatar("Qoder")
            AgateAvatar("KukuAI")
            AgateAvatar("Trae")
            AgateAvatar("Agent", icon = Icons.Filled.Bolt)
        }

        // 12 搜索框
        Section("12 · 搜索框 AgateSearchBar")
        AgateSearchBar(value = search, onValueChange = { search = it }, hint = "搜索提供商 / 模型 / 账号")

        Spacer(Modifier.height(8.dp))
        Text(
            "走查口径：对照 123_v44.html 逐项比对形态与色值；深浅三档切换回弹无跳变。",
            style = AgateType.Caption,
            color = C.textDim,
        )
        Spacer(Modifier.height(28.dp))
    }
}

@Composable
private fun Section(title: String) {
    val C = agateColors()
    Column {
        Spacer(Modifier.height(10.dp))
        Text(
            title,
            style = AgateType.SectionTitle,
            fontWeight = FontWeight.SemiBold,
            color = C.textSub,
        )
        Spacer(Modifier.height(8.dp))
    }
}

@Composable
private fun LabeledDots(label: String, total: Int, active: Int, activeColor: androidx.compose.ui.graphics.Color? = null) {
    val C = agateColors()
    Row(
        Modifier
            .fillMaxWidth()
            .padding(vertical = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, style = AgateType.BodyS, color = C.textSub, modifier = Modifier.weight(1f))
        AgateStatusDots(total = total, active = active, activeColor = activeColor)
    }
}
