package dev.agate.android.ui.tasks

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.BeijingDay
import dev.agate.android.ui.components.AgateAvatar
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * 任务中心四页共用的小件。
 *
 * ── 为什么单独抽一份 ────────────────────────────────────────
 * 四页的卡片外壳、行结构、小按钮是同一套（v44 `.card` / `.row` / `.btn-ghost`）。
 * 各页各写一份的话，「行高的 7dp 上下内边距」这种细节会在四页里漂成四种，
 * 翻页时一眼能看出不齐。
 *
 * ── 时间一律按北京时间格式化 ─────────────────────────────────
 * 签到记录的 `date` 是后端的北京日期键，`ts` 是毫秒。格式化时把时间戳
 * 平移到 UTC+8 再用 UTC 格式化 = 稳定输出北京时刻，**不受手机时区影响**。
 * 跟手机时区走会让海外设备看到与内核落账不同的一天（issue #138 同一类错位）。
 */

/** 卡片外壳（v44 `.card`：cardBg + 20dp 圆角 + 1dp 描边 + 3dp 投影）。 */
@Composable
fun TaskCard(
    modifier: Modifier = Modifier,
    content: @Composable ColumnScope.() -> Unit,
) {
    val C = agateColors()
    Column(
        modifier = modifier
            .fillMaxWidth()
            .agateCard()
            .background(C.cardBg, RoundedCornerShape(20.dp))
            .border(1.dp, C.border, RoundedCornerShape(20.dp))
            .padding(horizontal = 14.dp, vertical = 13.dp),
        content = content,
    )
}

/** 卡片标题行：标题 + 右侧动作槽（`.card-h`）。 */
@Composable
fun TaskCardHeader(
    title: String,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    actions: @Composable RowScope.() -> Unit = {},
) {
    val C = agateColors()
    Row(modifier = modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(
                title,
                style = AgateType.SectionTitle,
                fontWeight = FontWeight.SemiBold,
                color = C.textMain,
            )
            if (subtitle != null) {
                Text(subtitle, style = AgateType.Caption, color = C.textDim)
            }
        }
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            content = actions,
        )
    }
}

/** 卡内说明行（`.card-s`：12px textDim）。 */
@Composable
fun TaskHint(text: String, modifier: Modifier = Modifier, color: Color? = null) {
    val C = agateColors()
    Text(text, style = AgateType.Caption, color = color ?: C.textDim, modifier = modifier)
}

/** 一条行：头像 / 标题 + 副标 / 右侧槽（v44 `.row`）。 */
@Composable
fun TaskRow(
    title: String,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    letters: String? = null,
    avatarColorKey: String = title,
    onClick: (() -> Unit)? = null,
    trailing: @Composable RowScope.() -> Unit = {},
) {
    val C = agateColors()
    Row(
        modifier = modifier
            .fillMaxWidth()
            .then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier)
            .padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        AgateAvatar(
            name = avatarColorKey,
            size = 34.dp,
            corner = 11.dp,
            letters = letters ?: title.take(2).uppercase(),
        )
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            Text(title, color = C.textMain, fontSize = 14.sp, fontWeight = FontWeight.Medium)
            if (subtitle != null) {
                Text(subtitle, color = C.textSub, fontSize = 12.sp, lineHeight = 16.sp)
            }
        }
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(5.dp),
            content = trailing,
        )
    }
}

/** 缩进子行（展开后的账号明细，`.sub-r`）。 */
@Composable
fun TaskSubRow(
    modifier: Modifier = Modifier,
    content: @Composable RowScope.() -> Unit,
) {
    val C = agateColors()
    Row(
        modifier = modifier
            .fillMaxWidth()
            .background(C.insetBg.copy(alpha = 0.55f), RoundedCornerShape(9.dp))
            .padding(horizontal = 10.dp, vertical = 7.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(7.dp),
        content = content,
    )
}

/** 小按钮（`.btn` 三种：primary / ghost / outlined）。 */
@Composable
fun TaskBtn(
    text: String,
    modifier: Modifier = Modifier,
    primary: Boolean = false,
    outlined: Boolean = false,
    danger: Boolean = false,
    enabled: Boolean = true,
    onClick: () -> Unit,
) {
    val C = agateColors()
    val fg = when {
        !enabled -> C.textDim
        danger -> C.red
        primary -> C.onAccent
        outlined -> C.accent
        else -> C.textMain
    }
    val bg = when {
        !enabled -> C.btnBg.copy(alpha = 0.45f)
        primary -> C.accent
        outlined -> C.accent.copy(alpha = 0.10f)
        else -> C.btnBg
    }
    Text(
        text,
        color = fg,
        fontSize = 12.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = modifier
            .background(bg, RoundedCornerShape(9.dp))
            .then(
                if (outlined && enabled) Modifier.border(1.dp, C.accent.copy(alpha = 0.35f), RoundedCornerShape(9.dp))
                else Modifier
            )
            .then(if (enabled) Modifier.clickable(onClick = onClick) else Modifier)
            .padding(horizontal = 10.dp, vertical = 6.dp),
    )
}

/** 标签（键 + 值，`.tag`）。 */
@Composable
fun TaskTag(text: String, color: Color, modifier: Modifier = Modifier) {
    Text(
        text,
        color = color,
        fontSize = 10.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = modifier
            .background(color.copy(alpha = 0.14f), RoundedCornerShape(999.dp))
            .border(1.dp, color.copy(alpha = 0.30f), RoundedCornerShape(999.dp))
            .padding(horizontal = 7.dp, vertical = 2.dp),
    )
}

/** 卡片间垂直间隔。 */
@Composable
fun TaskGap(height: Int = 10) {
    Spacer(Modifier.height(height.dp))
}

/** 细分割线。 */
@Composable
fun TaskDivider(modifier: Modifier = Modifier) {
    val C = agateColors()
    Box(modifier.fillMaxWidth().height(1.dp).background(C.border.copy(alpha = 0.55f)))
}

/** 折叠说明（`<div class="fold">`：inset 底，位置在卡尾）。 */
@Composable
fun TaskFold(text: String, modifier: Modifier = Modifier) {
    val C = agateColors()
    Text(
        text,
        style = AgateType.Caption,
        color = C.textDim,
        lineHeight = 16.sp,
        modifier = modifier
            .fillMaxWidth()
            .background(C.insetBg.copy(alpha = 0.6f), RoundedCornerShape(9.dp))
            .padding(horizontal = 9.dp, vertical = 7.dp),
    )
}

/* ── 时间格式化（北京时间，不跟手机时区） ───────────────────── */

private const val OFFSET_MS = 8 * 3600_000L

/** 毫秒 → 北京时间的 `HH:mm`。 */
fun beijingClock(ms: Long): String =
    SimpleDateFormat("HH:mm", Locale.US).format(Date(ms + OFFSET_MS))

/** 毫秒 → 北京时间的 `MM-dd`。 */
fun beijingMonthDay(ms: Long): String =
    SimpleDateFormat("MM-dd", Locale.US).format(Date(ms + OFFSET_MS))

/** 毫秒 → 北京时间的 `MM-dd HH:mm`。 */
fun beijingStamp(ms: Long): String =
    SimpleDateFormat("MM-dd HH:mm", Locale.US).format(Date(ms + OFFSET_MS))

/** 毫秒 → 北京日期键 `yyyy-MM-dd`（与后端 `date` 字段同口径）。 */
fun beijingDateKey(ms: Long): String = BeijingDay.dateKey(ms)

/**
 * 相对时间（「约 3h42m 后」/「2 分钟前」）。
 *
 * `prefix` 决定方向：`null` = 无前缀，`"下次"` = 未来向，`"上次"` = 过去向。
 */
fun relTime(target: Long, now: Long = System.currentTimeMillis(), prefix: String? = null): String {
    if (target <= 0) return "—"
    val diff = target - now
    val future = diff > 0
    val abs = kotlin.math.abs(diff)
    val mins = abs / 60_000
    val body = when {
        mins < 1 -> "不到 1 分钟"
        mins < 60 -> "${mins} 分钟"
        abs < 86_400_000L -> {
            val h = mins / 60
            val m = mins % 60
            if (m > 0) "${h}h${m}m" else "${h} 小时"
        }
        else -> "${abs / 86_400_000L} 天"
    }
    // 注意花括号：汉字是合法的 Kotlin 标识符字符，`"$prefix约"` 会被解析成
    // 插值一个叫 `prefix约` 的变量（报 Unresolved reference），而不是
    // 「prefix 的值 + 字面量『约』」。凡是变量后紧跟汉字都必须写 ${...}。
    val dir = if (future) "后" else "前"
    return if (prefix != null) "${prefix}约 $body$dir" else "约 $body$dir"
}
