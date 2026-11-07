package dev.agate.android.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors

enum class BadgeTone { GREEN, BLUE, RED, AMBER, GRAY }

/**
 * v44 徽标：12px 半粗、圆角 12、tone 色 15% 透明底 + 亮色文字 + 30% 描边。
 * 可选前置小圆点（在线状态语义）。
 */
@Composable
fun AgateBadge(
    text: String,
    tone: BadgeTone = BadgeTone.GRAY,
    withDot: Boolean = false,
    dotColor: Color? = null,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    // 描边一律从主题语义色派生：旧值写死了 slate 基色，换成纯黑档后
    // 徽标会带着一圈与文字不同色的旧蓝/旧灰边。
    val (fg, bg, border) = when (tone) {
        BadgeTone.GREEN -> Triple(C.badgeGreenText, C.greenBg, C.green.copy(alpha = 0.30f))
        BadgeTone.BLUE -> Triple(C.badgeBlueText, C.blueBg, C.blue.copy(alpha = 0.30f))
        BadgeTone.RED -> Triple(C.badgeRedText, C.redBg, C.red.copy(alpha = 0.30f))
        BadgeTone.AMBER -> Triple(C.badgeAmberText, C.amberBg, C.amber.copy(alpha = 0.30f))
        BadgeTone.GRAY -> Triple(
            C.badgeGrayText,
            C.textDim.copy(alpha = 0.12f),
            C.textDim.copy(alpha = 0.25f),
        )
    }
    Row(
        modifier = modifier
            .background(bg, RoundedCornerShape(12.dp))
            .border(1.dp, border, RoundedCornerShape(12.dp))
            .padding(horizontal = 8.dp, vertical = 3.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (withDot) {
            Box(
                Modifier
                    .padding(end = 4.dp)
                    .size(5.dp)
                    .background(dotColor ?: fg, CircleShape)
            )
        }
        Text(
            text = text,
            style = AgateType.BodyS.copy(fontWeight = FontWeight.SemiBold),
            color = fg,
        )
    }
}
