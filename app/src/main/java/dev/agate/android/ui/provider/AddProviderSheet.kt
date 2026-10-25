package dev.agate.android.ui.provider

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.LoginKind
import dev.agate.android.data.ProviderPreset
import dev.agate.android.data.PresetRegistry
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.agateColors

/**
 * 添加提供商底弹（M3-3）：24 家官方预置卡片网格。
 *
 * - 已有账号的家显示「N 账号」徽标（可重复添加多账号）；
 * - 登录通道徽标：网页（WebView）/ 短信 / 粘贴凭证 —— 后两类 M3 后续版本接线；
 * - 25 张中转预置卡（custom providers）属自定义家体系，下一刀接 /api/…/custom。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AddProviderSheet(
    providerCounts: Map<String, Int>,
    onDismiss: () -> Unit,
    onPick: (ProviderPreset) -> Unit,
) {
    val C = agateColors()
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheetState,
        containerColor = C.bg,
    ) {
        Column(Modifier.padding(horizontal = 16.dp)) {
            Text(
                "添加提供商",
                color = C.textMain,
                fontSize = 18.sp,
                fontWeight = FontWeight.Bold,
            )
            Text(
                "官方 ${PresetRegistry.ALL.size} 家 · 登录后账号进入轮转队列",
                color = C.textDim,
                fontSize = 12.sp,
            )
            Spacer(Modifier.height(12.dp))
            LazyVerticalGrid(
                columns = GridCells.Fixed(2),
                modifier = Modifier
                    .fillMaxWidth()
                    .height(560.dp),
                horizontalArrangement = Arrangement.spacedBy(10.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                items(PresetRegistry.ALL) { preset ->
                    PresetCard(
                        preset = preset,
                        count = providerCounts[preset.id] ?: 0,
                        onClick = { onPick(preset) },
                    )
                }
            }
            Spacer(Modifier.height(16.dp))
        }
    }
}

@Composable
private fun PresetCard(
    preset: ProviderPreset,
    count: Int,
    onClick: () -> Unit,
) {
    val C = agateColors()
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(C.cardBg, RoundedCornerShape(14.dp))
            .clickable(onClick = onClick)
            .padding(12.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                preset.label,
                color = C.textMain,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f),
            )
            if (count > 0) {
                AgateBadge(text = "$count", tone = BadgeTone.BLUE)
            }
        }
        Spacer(Modifier.height(3.dp))
        Text(preset.desc, color = C.textDim, fontSize = 11.sp, maxLines = 1)
        Spacer(Modifier.height(7.dp))
        when (preset.kind) {
            LoginKind.WEBVIEW -> AgateBadge("网页登录", BadgeTone.GREEN)
            LoginKind.SMS -> AgateBadge("验证码 · 即将支持", BadgeTone.AMBER)
            LoginKind.PASTE -> AgateBadge("粘贴凭证 · 即将支持", BadgeTone.AMBER)
        }
    }
}
