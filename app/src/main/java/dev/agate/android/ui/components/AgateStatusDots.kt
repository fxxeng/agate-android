package dev.agate.android.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.agate.android.ui.theme.agateColors

/**
 * v44 并发点：6px 圆点组，亮=语义色（默认 accent），灭=dotOff + dotBorder 描边。
 * 用于账号池并发窗口、任务执行槽位等「N 中 M 活跃」语义。
 */
@Composable
fun AgateStatusDots(
    total: Int,
    active: Int,
    modifier: Modifier = Modifier,
    activeColor: androidx.compose.ui.graphics.Color? = null,
) {
    val C = agateColors()
    Row(modifier = modifier, verticalAlignment = Alignment.CenterVertically) {
        repeat(total) { i ->
            val on = i < active
            Box(
                Modifier
                    .padding(horizontal = 1.5.dp)
                    .size(6.dp)
                    .then(
                        if (on) Modifier.background(activeColor ?: C.accent, CircleShape)
                        else Modifier
                            .border(1.dp, C.dotBorder, CircleShape)
                            .background(C.dotOff, CircleShape)
                    )
            )
        }
    }
}
