package dev.agate.android.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.unit.dp
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors

/**
 * v44 热力图（数据报表 365 天 / 周视图通用）：
 * 周为列（默认 26 列 × 7 行，可传 53 列覆盖全年），
 * cell = 圆角 1.5dp 小方块，四级强度 0 空 → 3 满（heatLevels）。
 */
@Composable
fun AgateHeatmap(
    values: List<Float>,
    modifier: Modifier = Modifier,
    weeks: Int = 26,
) {
    val C = agateColors()
    val shape = RoundedCornerShape(1.5.dp)
    Row(modifier = modifier) {
        repeat(weeks) { w ->
            Column(modifier = Modifier.padding(horizontal = 1.dp)) {
                repeat(7) { d ->
                    val idx = w * 7 + d
                    val v = values.getOrNull(idx) ?: 0f
                    val level = when {
                        v <= 0f -> 0
                        v < 0.34f -> 1
                        v < 0.67f -> 2
                        else -> 3
                    }
                    Box(
                        Modifier
                            .padding(1.dp)
                            .size(5.dp)
                            .clip(shape)
                            .background(C.heatLevels[level])
                    )
                }
            }
        }
    }
}

/** 热力图图例：少 → 多 四级 + 标签。 */
@Composable
fun AgateHeatmapLegend(modifier: Modifier = Modifier) {
    val C = agateColors()
    Row(modifier = modifier, verticalAlignment = Alignment.CenterVertically) {
        Text("少", style = AgateType.Caption, color = C.textDim)
        C.heatLevels.forEach { c ->
            Box(
                Modifier
                    .padding(horizontal = 1.5.dp)
                    .size(5.dp)
                    .background(c, RoundedCornerShape(1.5.dp))
            )
        }
        Text("多", style = AgateType.Caption, color = C.textDim)
    }
}
