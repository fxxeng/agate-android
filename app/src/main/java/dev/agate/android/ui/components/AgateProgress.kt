package dev.agate.android.ui.components

import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors

/**
 * v44 进度条两种粗细：bar-track 8dp（额度/水位）/ mini-track 5dp（账号余额）。
 * track = insetBg + border 描边；fill 走语义色，350ms 缓动。
 */
@Composable
fun AgateProgress(
    fraction: Float,
    modifier: Modifier = Modifier,
    thick: Boolean = true,
    fillColor: Color? = null,
    label: String? = null,
) {
    val C = agateColors()
    val frac = fraction.coerceIn(0f, 1f)
    val animated by animateFloatAsState(frac, tween(350), label = "progress")
    val h = if (thick) 8.dp else 5.dp
    Column(modifier = modifier.fillMaxWidth()) {
        Box(
            Modifier
                .fillMaxWidth()
                .height(h)
                .background(C.insetBg, RoundedCornerShape(h / 2))
                .border(1.dp, C.border, RoundedCornerShape(h / 2))
        ) {
            Box(
                Modifier
                    .fillMaxWidth(animated)
                    .height(h)
                    .background(fillColor ?: C.accent, RoundedCornerShape(h / 2))
            )
        }
        if (label != null) {
            Text(
                text = label,
                style = AgateType.Caption,
                color = C.textDim,
                modifier = Modifier.padding(top = 4.dp),
            )
        }
    }
}
