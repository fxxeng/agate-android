package dev.agate.android.ui.components

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.unit.dp
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors

/**
 * v44 24h 双轴图：柱 = 每小时请求量（accent 35% 透明），
 * 折线 = 成功率（green，0..1 归一），底部 0/6/12/18/24 刻度（tnum）。
 * 柱高按 volumes.max 归一；两轴量纲不同，图例注明。
 */
@Composable
fun AgateDualAxisChart(
    volumes: List<Int>,
    successRates: List<Float>,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val n = 24
    val vols = List(n) { volumes.getOrNull(it) ?: 0 }
    val rates = List(n) { (successRates.getOrNull(it) ?: 0f).coerceIn(0f, 1f) }
    val maxV = (vols.maxOrNull() ?: 1).coerceAtLeast(1)

    Column(modifier = modifier.fillMaxWidth()) {
        Canvas(
            Modifier
                .fillMaxWidth()
                .height(120.dp)
        ) {
            val w = size.width
            val h = size.height
            val bottomPad = 6.dp.toPx()
            val chartH = h - bottomPad
            val slot = w / n
            val barW = slot * 0.55f

            // 柱（左轴：请求量）
            vols.forEachIndexed { i, v ->
                val barH = if (v == 0) 0f else (v.toFloat() / maxV) * (chartH * 0.85f)
                if (barH > 0f) {
                    drawRoundRect(
                        color = C.accent.copy(alpha = 0.35f),
                        topLeft = Offset(i * slot + (slot - barW) / 2f, chartH - barH),
                        size = Size(barW, barH),
                        cornerRadius = CornerRadius(4.dp.toPx(), 4.dp.toPx()),
                    )
                }
            }

            // 网格线（三条虚线）
            val dash = PathEffect.dashPathEffect(floatArrayOf(6f, 6f))
            listOf(0.25f, 0.5f, 0.75f).forEach { f ->
                drawLine(
                    color = C.border.copy(alpha = 0.5f),
                    start = Offset(0f, chartH * f),
                    end = Offset(w, chartH * f),
                    strokeWidth = 1f,
                    pathEffect = dash,
                )
            }

            // 折线（右轴：成功率 0..1 → 全高）
            val path = Path()
            rates.forEachIndexed { i, r ->
                val x = i * slot + slot / 2f
                val y = chartH * (1f - r)
                if (i == 0) path.moveTo(x, y) else path.lineTo(x, y)
            }
            drawPath(
                path = path,
                color = C.green,
                style = Stroke(width = 2.dp.toPx(), cap = StrokeCap.Round),
            )
            rates.forEachIndexed { i, r ->
                drawCircle(
                    color = C.green,
                    radius = 2.5.dp.toPx(),
                    center = Offset(i * slot + slot / 2f, chartH * (1f - r)),
                )
            }
        }
        // 刻度行（0/6/12/18/24 均匀分布，tnum）
        Row(Modifier.fillMaxWidth()) {
            listOf("0", "6", "12", "18", "24").forEach { t ->
                Text(
                    text = t,
                    style = AgateType.Num.copy(fontSize = AgateType.Caption.fontSize),
                    color = C.textDim,
                    modifier = Modifier.weight(1f),
                )
            }
        }
        Row(Modifier.padding(top = 6.dp)) {
            LegendSwab(C.accent.copy(alpha = 0.35f), "请求量")
            Spacer(Modifier.width(10.dp))
            LegendSwab(C.green, "成功率")
        }
    }
}

@Composable
private fun LegendSwab(color: Color, label: String) {
    val C = agateColors()
    Row {
        Canvas(Modifier.width(10.dp).height(10.dp).padding(1.dp)) {
            drawRoundRect(color = color, cornerRadius = CornerRadius(3f, 3f))
        }
        Spacer(Modifier.width(4.dp))
        Text(label, style = AgateType.Caption, color = C.textSub)
    }
}
