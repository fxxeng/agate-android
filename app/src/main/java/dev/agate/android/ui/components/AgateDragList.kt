package dev.agate.android.ui.components

import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.PushPin
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalViewConfiguration
import androidx.compose.ui.unit.ViewConfiguration
import androidx.compose.ui.unit.dp
import androidx.compose.ui.zIndex
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors

/** v44 规范：350ms 长按抓起（系统默认 long-press 通常 400ms）。 */
private class AgateDragConfig(
    base: ViewConfiguration,
    override val longPressTimeoutMillis: Long = 350L,
) : ViewConfiguration by base

/**
 * v44 拖拽排序列表：
 * - 350ms 长按抓起（触觉反馈）→ 拖拽行浮起（阴影 + 1.02 微放大）
 * - 指针越过相邻行中点即换位（onMove），换位瞬间再次触觉反馈
 * - 拖拽期间顶部显示 amber 重排横幅
 * - 行内容完全开放；dragging 参数供调用方渲染排序角标（prov-rank）
 * - onPin：置顶按钮（可选）
 */
@Composable
fun <T> AgateDragList(
    items: List<T>,
    onMove: (Int, Int) -> Unit,
    modifier: Modifier = Modifier,
    onPin: ((Int) -> Unit)? = null,
    reorderHint: String = "拖拽排序中 · 松手保存顺序",
    itemContent: @Composable (item: T, index: Int, dragging: Boolean) -> Unit,
) {
    val C = agateColors()
    val haptic = LocalHapticFeedback.current

    var draggingIndex by remember { mutableStateOf<Int?>(null) }
    var dragOffset by remember { mutableFloatStateOf(0f) }
    val heights = remember { mutableStateMapOf<Int, Float>() }

    // 350ms 长按抓起：覆盖本子树的 longPress 超时
    val baseConfig = LocalViewConfiguration.current
    val dragConfig = remember(baseConfig) { AgateDragConfig(baseConfig) }

    CompositionLocalProvider(LocalViewConfiguration provides dragConfig) {
        Column(modifier = modifier.fillMaxWidth()) {
            if (draggingIndex != null) {
                Row(
                    Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(10.dp))
                        .padding(bottom = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(reorderHint, style = AgateType.BodyS, color = C.amber)
                }
            }
            items.forEachIndexed { index, item ->
                val dragging = draggingIndex == index
                // pointerInput(Unit) 闭包不随 items 重组重启；rememberUpdatedState 保证拿到最新 index
                val currentIdx by rememberUpdatedState(index)

                Box(
                    Modifier
                        .fillMaxWidth()
                        .zIndex(if (dragging) 1f else 0f)
                        .graphicsLayer {
                            translationY = if (dragging) dragOffset else 0f
                            val s = if (dragging) 1.02f else 1f
                            scaleX = s
                            scaleY = s
                            shadowElevation = if (dragging) 12.dp.toPx() else 0f
                        }
                        .onGloballyPositioned {
                            heights[currentIdx] = it.size.height.toFloat()
                        }
                        .pointerInput(Unit) {
                            detectDragGesturesAfterLongPress(
                                onDragStart = {
                                    draggingIndex = currentIdx
                                    dragOffset = 0f
                                    haptic.performHapticFeedback(HapticFeedbackType.LongPress)
                                },
                                onDrag = { change, amount ->
                                    change.consume()
                                    dragOffset += amount.y
                                    var i = currentIdx
                                    val myH = heights[i] ?: return@detectDragGesturesAfterLongPress
                                    // 向下越过下一行中点 → 与下一行换位
                                    while (dragOffset > (myH + (heights[i + 1] ?: 0f)) / 2f &&
                                        i + 1 < items.size
                                    ) {
                                        dragOffset -= (heights[i + 1] ?: 0f)
                                        onMove(i, i + 1)
                                        haptic.performHapticFeedback(HapticFeedbackType.TextHandleMove)
                                        i += 1
                                    }
                                    // 向上越过上一行中点 → 与上一行换位
                                    while (dragOffset < -(myH + (heights[i - 1] ?: 0f)) / 2f && i > 0) {
                                        dragOffset += (heights[i - 1] ?: 0f)
                                        onMove(i, i - 1)
                                        haptic.performHapticFeedback(HapticFeedbackType.TextHandleMove)
                                        i -= 1
                                    }
                                    draggingIndex = i
                                },
                                onDragEnd = {
                                    draggingIndex = null
                                    dragOffset = 0f
                                },
                                onDragCancel = {
                                    draggingIndex = null
                                    dragOffset = 0f
                                },
                            )
                        },
                ) {
                    itemContent(item, index, dragging)
                    if (onPin != null && !dragging) {
                        IconButton(
                            onClick = { onPin(index) },
                            modifier = Modifier
                                .align(Alignment.TopEnd)
                                .padding(top = 2.dp, end = 2.dp),
                        ) {
                            Icon(
                                imageVector = Icons.Filled.PushPin,
                                contentDescription = "置顶",
                                tint = C.textDim,
                            )
                        }
                    }
                }
                if (index != items.lastIndex) {
                    Spacer(Modifier.height(8.dp))
                }
            }
        }
    }
}
