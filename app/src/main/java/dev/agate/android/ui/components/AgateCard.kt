package dev.agate.android.ui.components

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.unit.dp
import dev.agate.android.ui.theme.agateColors

/**
 * 卡片层次：浅色档用柔和投影（v44 v18 --shadow-card），**深色档不投影**。
 *
 * 纯黑底上黑色阴影等于没画，靠阴影分层会变成一片糊；深色档改由
 * `C.cardBg`（#101014）与调用方的 1dp `C.border` 描边承担层次。
 * 用法：`Modifier.agateCard().background(C.cardBg, shape).border(1.dp, C.border, shape)`。
 */
@Composable
fun Modifier.agateCard(shape: Shape = RoundedCornerShape(20.dp)): Modifier {
    val C = agateColors()
    val light = C.bg.luminance() > 0.5f
    if (!light) return this
    return this.shadow(
        elevation = 3.dp,
        shape = shape,
        clip = false,
        ambientColor = Color(0x140F172A),
        spotColor = Color(0x1F0F172A),
    )
}