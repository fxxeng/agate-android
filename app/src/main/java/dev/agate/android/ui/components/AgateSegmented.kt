package dev.agate.android.ui.components

import androidx.compose.animation.animateColorAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors

/**
 * v44 分段控件：seg-bg 底 + 4dp 内边距 + 8dp 圆角；
 * 选中项 seg-active-bg 底 + textMain 文字，未选 textDim。
 */
@Composable
fun AgateSegmented(
    options: List<String>,
    selected: Int,
    onSelect: (Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    Row(
        modifier = modifier
            .fillMaxWidth()
            .background(C.segBg, RoundedCornerShape(12.dp))
            .border(1.dp, C.border, RoundedCornerShape(12.dp))
            .padding(4.dp),
    ) {
        options.forEachIndexed { i, label ->
            val active = i == selected
            val bg by animateColorAsState(if (active) C.segActiveBg else C.segBg, label = "seg")
            Column(modifier = Modifier.weight(1f)) {
                Box(
                    Modifier
                        .fillMaxWidth()
                        .then(if (active) Modifier.shadow(2.dp, RoundedCornerShape(8.dp)) else Modifier)
                        .background(bg, RoundedCornerShape(8.dp))
                        .clickable { onSelect(i) }
                        .padding(vertical = 8.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        text = label,
                        style = AgateType.BodyS,
                        fontWeight = FontWeight.SemiBold,
                        color = if (active) C.accent else C.textDim,
                    )
                }
            }
        }
    }
}
