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
 * v44 v18 --shadow-card：卡片统一柔和投影（浅色 6% / 深色 25%）。
 * 用法：`Modifier.agateCard().background(C.cardBg, shape).border(...)`。
 * Compose 默认阴影偏重，这里显式给 ambient/spot 颜色贴合设计。
 */
@Composable
fun Modifier.agateCard(shape: Shape = RoundedCornerShape(20.dp)): Modifier {
    val C = agateColors()
    val light = C.bg.luminance() > 0.5f
    return this.shadow(
        elevation = 3.dp,
        shape = shape,
        clip = false,
        ambientColor = if (light) Color(0x140F172A) else Color(0x40000000),
        spotColor = if (light) Color(0x1F0F172A) else Color(0x66000000),
    )
}