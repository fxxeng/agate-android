package dev.agate.android.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors

/** KPI 单元：label（副色小字）+ value（tnum 大数字，语义色可选）。 */
data class KpiCell(
    val label: String,
    val value: String,
    val valueColor: Color? = null,
)

/**
 * v44 KPI 四联卡：1fr×4 网格、16dp 圆角、居中排布；数字一律 tnum 等宽。
 */
@Composable
fun AgateKpiStrip(
    cells: List<KpiCell>,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    Row(modifier = modifier.fillMaxWidth()) {
        cells.forEach { cell ->
            Column(
                modifier = Modifier
                    .weight(1f)
                    .padding(horizontal = 4.dp)
                    .background(C.cardBg, RoundedCornerShape(16.dp))
                    .border(1.dp, C.border, RoundedCornerShape(16.dp))
                    .padding(vertical = 12.dp, horizontal = 4.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text(
                    text = cell.value,
                    style = AgateType.NumL,
                    color = cell.valueColor ?: C.textMain,
                )
                Text(
                    text = cell.label,
                    style = AgateType.Caption,
                    color = C.textDim,
                )
            }
        }
    }
}
