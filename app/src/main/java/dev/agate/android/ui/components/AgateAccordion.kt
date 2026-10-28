package dev.agate.android.ui.components

import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors

/**
 * v44 手风琴卡：card 底 + 18dp 圆角；标题行（13px 半粗 + 展开箭头），
 * animateContentSize 平滑开合；内容槽完全开放。
 */
@Composable
fun AgateAccordion(
    title: String,
    modifier: Modifier = Modifier,
    initiallyExpanded: Boolean = false,
    badge: (@Composable () -> Unit)? = null,
    content: @Composable () -> Unit,
) {
    val C = agateColors()
    var expanded by remember { mutableStateOf(initiallyExpanded) }
    Column(
        modifier = modifier
            .fillMaxWidth()
            .animateContentSize()
            .background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(14.dp),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .clickable { expanded = !expanded },
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = title,
                style = AgateType.SectionTitle,
                fontWeight = FontWeight.SemiBold,
                color = C.textMain,
                modifier = Modifier.weight(1f),
            )
            if (badge != null) badge()
            Icon(
                imageVector = if (expanded) Icons.Filled.ExpandLess else Icons.Filled.ExpandMore,
                contentDescription = if (expanded) "收起" else "展开",
                tint = C.textDim,
            )
        }
        if (expanded) {
            // ⚠️ 这里是 `Column` 而不是 `Box`：内容槽**天然是多子项的**。
            // 十组设置里每组都传了一串平级子元素（ValueRow + 四个徽标 + ItemRow…），
            // 用 Box 会让它们全部叠在左上角 —— 实测就是设置页「服务接入」里
            // 「未设置 / messages / models / 自动 / English」互相压字。
            // Box 曾经看着没问题，只因为组件走查页（ShowcasePage）每组只塞了一个 Text。
            Column(Modifier.padding(top = 10.dp)) { content() }
        }
    }
}
