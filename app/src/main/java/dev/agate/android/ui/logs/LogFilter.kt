package dev.agate.android.ui.logs

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateShape
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors

/** 色调 → 前景 / 背景 / 描边（与 [dev.agate.android.ui.components.BadgeTone] 同色板）。 */
internal data class ChipTone(val fg: Color, val bg: Color, val border: Color)

@Composable
internal fun chipToneOf(tone: dev.agate.android.ui.components.BadgeTone): ChipTone {
    val C = agateColors()
    return when (tone) {
        dev.agate.android.ui.components.BadgeTone.GREEN -> ChipTone(C.badgeGreenText, C.greenBg, Color(0x4D22C55E))
        dev.agate.android.ui.components.BadgeTone.BLUE -> ChipTone(C.badgeBlueText, C.blueBg, Color(0x4D3B82F6))
        dev.agate.android.ui.components.BadgeTone.RED -> ChipTone(C.badgeRedText, C.redBg, Color(0x4DF43F5E))
        dev.agate.android.ui.components.BadgeTone.AMBER -> ChipTone(C.badgeAmberText, C.amberBg, Color(0x4DF59E0B))
        dev.agate.android.ui.components.BadgeTone.GRAY -> ChipTone(C.badgeGrayText, C.badgeGrayText.copy(alpha = 0.10f), C.border)
    }
}

/** 筛选 chip：`标签：值 ▾`，选中时 accent 底。 */
@Composable
internal fun FilterChip(
    text: String,
    active: Boolean,
    onClick: () -> Unit,
    tone: dev.agate.android.ui.components.BadgeTone =
        dev.agate.android.ui.components.BadgeTone.GRAY,
    danger: Boolean = false,
) {
    val C = agateColors()
    val base = chipToneOf(tone)
    val fg = when {
        danger -> C.red
        active -> C.accent
        else -> C.textSub
    }
    val bg = if (danger) C.redBg else if (active) C.accent.copy(alpha = 0.12f) else C.insetBg
    val border = if (danger) C.red.copy(alpha = 0.35f) else if (active) C.accent.copy(alpha = 0.35f) else C.border
    Box(
        Modifier
            .clip(RoundedCornerShape(9.dp))
            .background(bg)
            .border(1.dp, border, RoundedCornerShape(9.dp))
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 5.dp),
    ) {
        Text(text, fontSize = 11.5.sp, color = fg, maxLines = 1)
    }
}

/** 单选筛选弹层；`options` 第一项传 null 表示「全部」。 */
@Composable
internal fun FilterSheet(
    title: String,
    options: List<Pair<String?, String>>,
    selected: String?,
    onPick: (String?) -> Unit,
    onDismiss: () -> Unit,
) {
    val C = agateColors()
    Box(
        Modifier
            .fillMaxSize()
            .background(Color(0x99000000))
            .clickable(onClick = onDismiss),
        contentAlignment = Alignment.BottomCenter,
    ) {
        Column(
            Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(topStart = 18.dp, topEnd = 18.dp))
                .background(C.cardBg)
                .clickable(enabled = false, onClick = {})
                .padding(16.dp),
        ) {
            Text(title, fontSize = 15.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
            Spacer(Modifier.height(10.dp))
            Column(Modifier.heightIn(max = 340.dp).verticalScroll(rememberScrollState())) {
                options.forEach { (value, label) ->
                    val active = value == selected
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .clip(RoundedCornerShape(10.dp))
                            .background(if (active) C.accent.copy(alpha = 0.12f) else Color.Transparent)
                            .clickable {
                                onPick(value)
                                onDismiss()
                            }
                            .padding(horizontal = 10.dp, vertical = 11.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            label,
                            fontSize = 13.sp,
                            color = if (active) C.accent else C.textMain,
                            modifier = Modifier.weight(1f),
                        )
                        if (active) Text("✓", fontSize = 13.sp, color = C.accent)
                    }
                }
            }
        }
    }
}

/** 日志清理折叠组：抹掉原文 / 删除明细 / 压缩数据库。 */
@Composable
internal fun ClearGroup(
    busy: Boolean,
    onClear: (mode: String) -> Unit,
    onCompact: () -> Unit,
    expanded: Boolean = false,
    onExpandedChange: (Boolean) -> Unit = {},
) {
    val C = agateColors()
    val open = expanded
    var confirm by remember { mutableStateOf<String?>(null) }

    Column(
        Modifier
            .fillMaxWidth()
            .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
            .border(1.dp, C.border, RoundedCornerShape(16.dp)),
    ) {
        Row(
            Modifier
                .fillMaxWidth()
                .clickable { onExpandedChange(!open) }
                .padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("日志清理", fontSize = 13.5.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
            Spacer(Modifier.width(8.dp))
            Text(
                "抹掉原文 · 删除明细 · 压缩",
                fontSize = 11.sp,
                color = C.textDim,
                modifier = Modifier.weight(1f),
            )
            Text(if (open) "⌃" else "⌄", fontSize = 13.sp, color = C.textDim)
        }

        if (open) {
            Box(Modifier.fillMaxWidth().height(1.dp).background(C.border))
            Column(Modifier.fillMaxWidth().padding(12.dp)) {
                ClearRow(
                    name = "仅清空报文原文",
                    desc = "保留统计行与报表，只抹请求 / 响应正文 · 清理后详情「预览对话」无原文可看",
                    action = "抹掉",
                    danger = false,
                    enabled = !busy,
                    onConfirm = { confirm = "raw" },
                )
                ClearRow(
                    name = "全部删除",
                    desc = "有筛选时只删命中的条目，按天报表不受影响 · 无法恢复",
                    action = "删除",
                    danger = true,
                    enabled = !busy,
                    onConfirm = { confirm = "all" },
                )
                ClearRow(
                    name = "压缩数据库",
                    desc = "checkpoint + VACUUM 后台执行，回收已删数据占用的磁盘空间",
                    action = "压缩",
                    danger = false,
                    enabled = !busy,
                    onConfirm = { onCompact() },
                )
            }
        }
    }

    // 用局部快照承接：委托属性（by remember）无法智能转换，直接 `confirm!!`
    // 会被判成 Any，而 ClearDialog 要的是 String。
    val pending = confirm
    if (pending != null) {
        val mode = pending
        ClearDialog(
            title = if (mode == "raw") "抹掉报文原文" else "清理调用记录",
            message = if (mode == "raw") {
                "只清空请求 / 响应的正文，统计行与按天报表保留。"
            } else {
                "无筛选时清空全部并重置报表；有筛选时只删命中项。此方式无法恢复。"
            },
            confirmText = if (mode == "raw") "抹掉" else "执行清理",
            danger = mode == "all",
            onDismiss = { confirm = null },
            onConfirm = {
                confirm = null
                onClear(mode)
            },
        )
    }
}

@Composable
private fun ClearRow(
    name: String,
    desc: String,
    action: String,
    danger: Boolean,
    enabled: Boolean,
    onConfirm: () -> Unit,
) {
    val C = agateColors()
    Row(
        Modifier
            .fillMaxWidth()
            .padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(name, fontSize = 13.sp, color = C.textMain)
            Text(desc, fontSize = 11.sp, color = C.textDim, lineHeight = 15.sp)
        }
        Spacer(Modifier.width(8.dp))
        Text(
            action,
            fontSize = 12.sp,
            fontWeight = FontWeight.SemiBold,
            color = if (!enabled) C.textDim else if (danger) C.red else C.accent,
            modifier = Modifier
                .border(
                    1.dp,
                    if (danger) C.red.copy(alpha = 0.35f) else C.border,
                    RoundedCornerShape(8.dp),
                )
                .clickable(enabled = enabled, onClick = onConfirm)
                .padding(horizontal = 11.dp, vertical = 6.dp),
        )
    }
}

/** 通用确认弹层。 */
@Composable
internal fun ClearDialog(
    title: String,
    message: String,
    confirmText: String,
    danger: Boolean,
    onDismiss: () -> Unit,
    onConfirm: () -> Unit,
) {
    val C = agateColors()
    Box(
        Modifier
            .fillMaxSize()
            .background(Color(0x99000000))
            .clickable(onClick = onDismiss),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            Modifier
                .fillMaxWidth()
                .padding(horizontal = 28.dp)
                .clip(RoundedCornerShape(18.dp))
                .background(C.cardBg)
                .border(1.dp, C.border, RoundedCornerShape(18.dp))
                .clickable(enabled = false, onClick = {})
                .padding(16.dp),
        ) {
            Text(title, fontSize = 15.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
            Spacer(Modifier.height(8.dp))
            Text(message, fontSize = 12.5.sp, color = C.textSub, lineHeight = 17.sp)
            Spacer(Modifier.height(14.dp))
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                Text(
                    "取消",
                    fontSize = 13.sp,
                    color = C.textSub,
                    modifier = Modifier
                        .clickable(onClick = onDismiss)
                        .padding(horizontal = 12.dp, vertical = 7.dp),
                )
                Spacer(Modifier.width(6.dp))
                Text(
                    confirmText,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = if (danger) C.red else C.accent,
                    modifier = Modifier
                        .clickable(onClick = onConfirm)
                        .padding(horizontal = 12.dp, vertical = 7.dp),
                )
            }
        }
    }
}
