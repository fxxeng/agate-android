package dev.agate.android.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Menu
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.scale
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.ui.theme.AgateShape
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors

/**
 * v44 `.header-bar`：汉堡菜单 + 20px 粗体标题 + 右侧动作槽。
 * 所有页面（含二级页）统一使用，替代 Material3 TopAppBar，
 * 消除「Scaffold 顶栏 + 页面自绘顶栏」双层标题的重复问题。
 */
@Composable
fun AgatePageHeader(
    title: String,
    onOpenDrawer: () -> Unit,
    modifier: Modifier = Modifier,
    onBack: (() -> Unit)? = null,
    actions: @Composable RowScope.() -> Unit = {},
) {
    val C = agateColors()
    Row(
        modifier = modifier
            .fillMaxWidth()
            // v18 吸顶：外壳实色底（与系统状态栏同色，无缝）+ 底部分隔线。
            // statusBarsPadding 放在 background 之后：背景延伸到状态栏下方，
            // 消除状态栏与页头的分界缝（Android 15 edge-to-edge）。
            .background(C.chromeBg)
            .statusBarsPadding()
            .padding(start = 16.dp, end = 12.dp, top = 8.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        // v44 .menu-icon（v18 增量）：36dp 圆角底 + 22dp 图标。
        // 二级页（onBack != null）显示返回箭头，一级页显示汉堡菜单。
        Box(
            modifier = Modifier
                .size(36.dp)
                .background(C.textSub.copy(alpha = 0.10f), RoundedCornerShape(10.dp))
                .clickable { if (onBack != null) onBack() else onOpenDrawer() },
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                imageVector = if (onBack != null) Icons.AutoMirrored.Filled.ArrowBack else Icons.Filled.Menu,
                contentDescription = if (onBack != null) "返回" else "打开侧栏",
                tint = C.textMain,
                modifier = Modifier.size(22.dp),
            )
        }
        Spacer(Modifier.width(12.dp))
        Text(
            text = title,
            color = C.textMain,
            fontSize = 22.sp,
            fontWeight = FontWeight.Bold,
            modifier = Modifier.weight(1f),
        )
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            content = actions,
        )
    }
    // 底部分隔线（吸顶栏与内容的分界）
    Box(Modifier.fillMaxWidth().height(1.dp).background(C.border))
}

/**
 * 二级页专用页头：只有返回箭头，没有抽屉入口。
 *
 * 为什么单独一个函数而不是让调用方写 `onOpenDrawer = {}`：
 * 二级页本来就没有侧栏（返回箭头已经占掉了左侧那个 36dp 的槽位），
 * 让每个二级页都传一个永远不调用的 `{}` 是纯粹的噪声 —— 而且一旦有人
 * 忘了传，尾随 lambda 会被编译器绑给 `onOpenDrawer`（`() -> Unit`），
 * 于是页面内容被塞进一个非 Composable 的槽位，报出的却是
 * 「@Composable invocations can only happen from the context of a @Composable function」，
 * 与真正的原因（少传参数）完全对不上。这个重载把那个坑填掉。
 */
@Composable
fun AgateSubPageHeader(
    title: String,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
    actions: @Composable RowScope.() -> Unit = {},
) = AgatePageHeader(
    title = title,
    onOpenDrawer = {},
    modifier = modifier,
    onBack = onBack,
    actions = actions,
)

/** 底栏一项（路由 + 文案 + 图标）。 */
data class AgateTabItem(val route: String, val label: String, val icon: ImageVector)

/**
 * v44 `.nav-bar`：70dp 高、nav-bg 半透明底、顶部 1px 分隔线、图标 22dp、
 * 选中项 accent 半粗。不采用 Material3 NavigationBar 的胶囊指示器。
 */
@Composable
fun AgateBottomBar(
    items: List<AgateTabItem>,
    currentRoute: String,
    onSelect: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    Column(
        modifier = modifier
            .fillMaxWidth()
            // 与系统导航栏同色（chromeBg），消除底部分界缝
            .background(C.chromeBg),
    ) {
        Box(Modifier.fillMaxWidth().height(1.dp).background(C.border))
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .navigationBarsPadding()
                .padding(bottom = 8.dp, top = 6.dp),
            horizontalArrangement = Arrangement.SpaceEvenly,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            items.forEach { item ->
                val active = currentRoute == item.route
                AgateBottomBarItem(
                    item = item,
                    active = active,
                    onClick = { if (!active) onSelect(item.route) },
                    modifier = Modifier.weight(1f),
                )
            }
        }
    }
}

@Composable
private fun AgateBottomBarItem(
    item: AgateTabItem,
    active: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    Column(
        modifier = modifier
            .scale(if (pressed) 0.9f else 1f)
            .clickable(interactionSource = interaction, indication = null, onClick = onClick)
            .padding(vertical = 2.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        // v44 .nav-item.active svg：图标 22dp + 水平 16dp 内边距 → 16dp 圆角胶囊底（accent 16%）
        Box(
            Modifier
                .background(
                    if (active) C.accent.copy(alpha = 0.16f) else Color.Transparent,
                    RoundedCornerShape(16.dp),
                )
                .padding(horizontal = 16.dp, vertical = 4.dp),
        ) {
            Icon(
                imageVector = item.icon,
                contentDescription = item.label,
                tint = if (active) C.accent else C.textDim,
                modifier = Modifier.size(22.dp),
            )
        }
        Spacer(Modifier.height(2.dp))
        Text(
            text = item.label,
            style = AgateType.Caption,
            fontWeight = if (active) FontWeight.SemiBold else FontWeight.Normal,
            color = if (active) C.accent else C.textDim,
        )
    }
}

/** v44 `.header-action`：accent 10% 底 + 9dp 圆角小胶囊按钮（页头动作用）。 */
@Composable
fun AgateHeaderAction(text: String, onClick: () -> Unit) {
    val C = agateColors()
    Text(
        text = text,
        color = C.accent,
        fontSize = 13.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier
            .background(C.accent.copy(alpha = 0.10f), AgateShape.small)
            .clickable(onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 9.dp),
    )
}

/** v44 `.btn-primary`：accent 实底小按钮（页头主动作用）。 */
@Composable
fun AgatePrimaryAction(text: String, onClick: () -> Unit) {
    val C = agateColors()
    Text(
        text = text,
        color = C.onAccent,
        fontSize = 13.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier
            .background(C.accent, AgateShape.small)
            .clickable(onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 9.dp),
    )
}
