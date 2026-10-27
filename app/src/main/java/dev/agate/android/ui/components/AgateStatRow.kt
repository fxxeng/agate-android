package dev.agate.android.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors

/** 紧凑统计单元：`今日 86` —— label 与 value 同行。 */
data class StatCell(
    val label: String,
    val value: String,
    val valueColor: Color? = null,
)

/**
 * 紧凑统计条 —— 一整张卡里横向排 N 项，用 1px 竖线分隔。
 *
 * ── 为什么不再用四张独立小卡 ─────────────────────────────────
 * 原来的四联是「四张带边框的圆角卡并排」，纵向被计了两遍：每张卡自己的
 * 12dp 上下内边距 + 边框 + 卡间缝隙，四个读数在首屏吃掉近 70dp。
 *
 * 改成**一整张卡 + 内部竖线分隔**之后：
 *   · 外框只有一份，纵向内边距 12dp → 9dp；
 *   · label 与 value 同行，不再上下两行；
 *   · 数字 20sp → 15sp（仍是 tnum 等宽，读数照样一眼可辨）。
 * 总高约 46dp，比原来省三分之一，信息量不变。
 */
@Composable
fun AgateStatRow(
    cells: List<StatCell>,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    Row(
        modifier = modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(14.dp))
        .border(1.dp, C.border, RoundedCornerShape(14.dp))
        .padding(vertical = 9.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        cells.forEachIndexed { index, cell ->
            if (index > 0) {
                Box(
                    Modifier
                        .height(20.dp)
                        .width(1.dp)
                        .background(C.border),
                )
            }
            Row(
                modifier = Modifier
                    .weight(1f)
                    .padding(horizontal = 4.dp),
                horizontalArrangement = Arrangement.Center,
                verticalAlignment = Alignment.Bottom,
            ) {
                Text(text = cell.label, fontSize = 10.sp, color = C.textDim)
                Spacer(Modifier.width(4.dp))
                Text(
                    text = cell.value,
                    style = AgateType.Num.copy(fontSize = 15.sp, lineHeight = 18.sp),
                    color = cell.valueColor ?: C.textMain,
                )
            }
        }
    }
}
