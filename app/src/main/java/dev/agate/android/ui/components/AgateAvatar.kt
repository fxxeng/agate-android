package dev.agate.android.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlin.math.abs

/** name → 0..360 稳定色相（同家账号/提供商恒定配色）。 */
fun hueFor(name: String): Float = (abs(name.hashCode()) % 360).toFloat()

/**
 * v44 头像：40dp 圆角方块（13dp 圆角）、同色相双色渐变、首字母大写白字。
 * Logo 槽位：传 icon 时显示线性图标替代字母（提供商官方图标位，M3 接入）。
 */
@Composable
fun AgateAvatar(
    name: String,
    modifier: Modifier = Modifier,
    size: Dp = 40.dp,
    corner: Dp = 13.dp,
    icon: ImageVector? = null,
    iconTint: Color? = null,
    letters: String? = null,
) {
    val h = hueFor(name)
    val c1 = Color.hsv(h, 0.45f, 0.38f)
    val c2 = Color.hsv((h + 24f) % 360f, 0.50f, 0.55f)
    Box(
        modifier = modifier
            .size(size)
            .background(Brush.linearGradient(listOf(c1, c2)), RoundedCornerShape(corner))
            .border(1.dp, Color.White.copy(alpha = 0.08f), RoundedCornerShape(corner)),
        contentAlignment = Alignment.Center,
    ) {
        if (icon != null) {
            Icon(
                imageVector = icon,
                contentDescription = name,
                tint = iconTint ?: Color.White,
                modifier = Modifier.size(size * 0.5f),
            )
        } else {
            Text(
                text = (letters ?: name.trim().take(1)).uppercase(),
                color = Color.White,
                fontSize = (size.value * 0.5f).sp,
                fontWeight = FontWeight.Bold,
            )
        }
    }
}
