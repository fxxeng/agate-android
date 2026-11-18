package dev.agate.android.ui.tasks

import android.widget.Toast
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.CheckinCenter
import dev.agate.android.data.CheckinOps
import dev.agate.android.data.PresetRegistry
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateSubPageHeader
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.StatCell
import dev.agate.android.ui.components.AgateStatRow
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject

/**
 * 导航卡外壳：与页面既有卡同色（`cardBg` 底 + `border` 描边 + 14dp 内边距）。
 *
 * 圆角按本批规格取 14dp，没有复用 TaskKit 的 `TaskCard`（那是 20dp 圆角）——
 * 规格要求 12~16dp，而本批禁止改 TaskKit；如需四张卡 + 导航卡完全同半径，
 * 应在 TaskKit 里统一调，而不是各自写一份。
 */
@Composable
internal fun NavCard(content: @Composable ColumnScope.() -> Unit) {
    val C = agateColors()
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(14.dp))
            .border(1.dp, C.border, RoundedCornerShape(14.dp))
            .padding(14.dp),
        content = content,
    )
}

/** 行首小标签（`任务档` / `功能页`）：10.5sp + `textDim`，只做视觉分组。 */
@Composable
internal fun NavRowLabel(text: String) {
    Text(text, color = agateColors().textDim, fontSize = 10.5.sp)
}

/**
 * 导航卡胶囊（第一行档位切换 + 第二行功能页跳转 + 每日签到卡内的 provider 筛选共用）。
 *
 * 选中态：`accent@10%` 底 + `accent@45%` 描边 + `accent` 粗字；未选中：`insetBg` 底 +
 * `border` 描边 + `textSub` 字。颜色全部走语义色，页面里不出现任何色值字面量。
 * `badge` 是可选的数量尾巴（`textDim` 9.5sp、距文字 6dp），只有 provider 筛选
 * 传真实计数，导航卡两行不传（数字属于 provider 行，不是档位）。
 */
@Composable
internal fun NavChip(
    text: String,
    selected: Boolean,
    badge: String? = null,
    onClick: () -> Unit,
) {
    val C = agateColors()
    val shape = RoundedCornerShape(50)
    Row(
        modifier = Modifier
            .clickable(onClick = onClick)
            .background(if (selected) C.accent.copy(alpha = 0.10f) else C.insetBg, shape)
            .border(1.dp, if (selected) C.accent.copy(alpha = 0.45f) else C.border, shape)
            .padding(horizontal = 9.dp, vertical = 5.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text,
            color = if (selected) C.accent else C.textSub,
            fontSize = 11.5.sp,
            fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
            // provider label 长短不一：长名不许把计数挤出去，也不许换行撑高胶囊
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f, fill = false),
        )
        if (badge != null) {
            Text(
                badge,
                color = C.textDim,
                fontSize = 9.5.sp,
                modifier = Modifier.padding(start = 6.dp),
            )
        }
    }
}

/** provider 徽标：只有拿到正数才显示；0 视为「这家没账号」，不出数字。 */
internal fun countBadge(n: Int): String? = if (n > 0) n.toString() else null

