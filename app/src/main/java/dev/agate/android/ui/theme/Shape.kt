package dev.agate.android.ui.theme

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * v44 形状令牌（对齐 123_v44.html，含 v18 增量覆盖）：
 * - 卡片 `.card` 圆角 var(--r-card) = 20px
 * - KPI 单元 16px（.kpi）
 * - 分段/搜索/表单 12px
 * - 小按钮 9px、头像 13px、胶囊 999px
 */
object AgateShape {
    val card = RoundedCornerShape(20.dp)
    val kpi = RoundedCornerShape(16.dp)
    val control = RoundedCornerShape(12.dp)
    val small = RoundedCornerShape(9.dp)
    val avatar = RoundedCornerShape(13.dp)
    val chip = RoundedCornerShape(999.dp)
}

/** 卡片阴影（v18 --shadow-card：1px 2px + 6px 16px-8px），卡片统一走它。 */
val AgateCardShadow: Dp = 3.dp
