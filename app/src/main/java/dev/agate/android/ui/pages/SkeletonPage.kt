package dev.agate.android.ui.pages

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors

/**
 * M2 二级页通用骨架：M3/M4 填真数据，M2 先立路由与视觉占位。
 * milestone 标签注明该页归属（M3 核心链路 / M4 高阶页面）。
 */
@Composable
fun SkeletonPage(
    title: String,
    milestone: String,
    hint: String,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    Column(
        modifier = modifier
            .fillMaxSize()
            .background(C.bg)
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 16.dp),
    ) {
        Spacer(Modifier.height(16.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(title, style = AgateType.PageTitle, color = C.textMain)
            Spacer(Modifier.weight(1f))
            AgateBadge(milestone, BadgeTone.BLUE)
        }
        Spacer(Modifier.height(10.dp))
        Text(hint, style = AgateType.Body, color = C.textSub)
        Spacer(Modifier.height(14.dp))
        // 视觉占位卡（组件形态与真实页一致，走查布局观感）
        repeat(3) { i ->
            Column(
                Modifier
                    .fillMaxWidth()
                    .padding(bottom = 10.dp)
                    .background(C.cardBg, RoundedCornerShape(18.dp))
                    .border(1.dp, C.border, RoundedCornerShape(18.dp))
                    .padding(14.dp),
            ) {
                Text(
                    "占位区块 ${i + 1} · 待接内核 API",
                    style = AgateType.SectionTitle,
                    color = C.textSub,
                )
                Spacer(Modifier.height(6.dp))
                Text(
                    "本页在 $milestone 落地真实数据源（Loopback HTTP /api/*），" +
                        "M2 仅保证路由、主题、组件与深浅色表现就绪。",
                    style = AgateType.BodyS,
                    color = C.textDim,
                )
            }
        }
        Spacer(Modifier.height(24.dp))
    }
}
