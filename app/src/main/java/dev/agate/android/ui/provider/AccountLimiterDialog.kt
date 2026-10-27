package dev.agate.android.ui.provider

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.agateColors
import org.json.JSONObject

/**
 * 「额度设限」弹窗（口径照 3.0.8 的 `accounts-dialog-limiter`）。
 *
 * ── 两类规则 ────────────────────────────────────────────────
 * 内核 `limiter::LimiterRule` 支持两种判据，这里都做：
 *   - **余额**：读数低于阈值触发（与旧「余额不足处理」同一口径，严格小于）；
 *   - **Token 消耗**：窗口内累计消耗达到阈值触发（大于等于）。
 * 「不设限」= 规则停用（`enabled=false`）或整块清空 —— 与 3.0.8 一致：
 * **没有生效的规则就是不限制**。
 *
 * ── 可自定义的部分（照上游）─────────────────────────────────
 * 限制类型、阈值、触发动作（跳过 / 禁用）全部可改；Token 规则另有重置方式
 * （固定周期 / 自然日）与周期长度（30 分钟 ~ 24 小时，单位可切分钟 / 小时）。
 * 3.0.8 的编辑器就是这么给的 —— 不做任何档位写死。
 *
 * ── 保存 ────────────────────────────────────────────────────
 * 走 `PATCH /api/accounts` 的 `limiters`（整块替换），内核 `apply_patch`
 * 归一化校验后**顺手同步**旧 `lowBalance` 供降级兼容。客户端只发新键。
 */
@Composable
fun AccountLimiterDialog(
    accountName: String,
    /** 当前生效的规则列表（空 = 不限制）。 */
    current: List<LimiterRule>,
    onDismiss: () -> Unit,
    onSave: (List<LimiterRule>) -> Unit,
) {
    val C = agateColors()
    var draft by remember {
        mutableStateOf(
            current.firstOrNull() ?: LimiterRule(
                type = "balance",
                action = "skip",
                threshold = 1.0,
                reset = "fixed",
                period = 3600,
                enabled = true,
            ),
        )
    }
    var enabled by remember { mutableStateOf(current.isNotEmpty()) }
    var error by remember { mutableStateOf<String?>(null) }

    // 输入框用字符串状态：数字键盘上的中间态（空串 / 单个 "."）不该被解析成数字后回写
    var thresholdText by remember { mutableStateOf(fmtNumber(draft.threshold)) }
    var periodText by remember {
        mutableStateOf(
            if (draft.period % 3600L == 0L && draft.period >= 3600) (draft.period / 3600).toString()
            else (draft.period / 60).toString(),
        )
    }
    var periodUnitHours by remember {
        mutableStateOf(draft.period % 3600L == 0L && draft.period >= 3600)
    }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("额度设限") },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                Text(accountName, color = C.textSub, fontSize = 12.sp)
                Spacer(Modifier.height(10.dp))

                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text("启用限制", color = C.textMain, fontSize = 13.sp)
                        Text("达到条件时按下面的方式处理该账号", color = C.textDim, fontSize = 11.sp)
                    }
                    Switch(checked = enabled, onCheckedChange = { enabled = it })
                }

                if (!enabled) {
                    Spacer(Modifier.height(10.dp))
                    Text("已关闭：这个账号不参与任何限制（照常轮转）。", color = C.textSub, fontSize = 12.sp)
                    return@Column
                }

                // ── 限制类型 ──
                Spacer(Modifier.height(14.dp))
                Text("限制类型", color = C.textMain, fontSize = 13.sp)
                Spacer(Modifier.height(6.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    ChoiceChip(
                        label = "余额",
                        hint = "读数低于阈值时触发",
                        selected = draft.type == "balance",
                        onClick = { draft = draft.copy(type = "balance", reset = "fixed") },
                        modifier = Modifier.weight(1f),
                    )
                    ChoiceChip(
                        label = "Token 消耗",
                        hint = "窗口内累计消耗达到阈值",
                        selected = draft.type == "token",
                        onClick = { draft = draft.copy(type = "token") },
                        modifier = Modifier.weight(1f),
                    )
                }

                // ── 触发条件 ──
                Spacer(Modifier.height(14.dp))
                Text(
                    if (draft.type == "balance") "余额阈值" else "消耗阈值",
                    color = C.textMain, fontSize = 13.sp,
                )
                Spacer(Modifier.height(2.dp))
                Text(
                    if (draft.type == "balance") "读数低于这个数时触发（严格小于）"
                    else "窗口内累计消耗达到这个数时触发",
                    color = C.textDim, fontSize = 11.sp,
                )
                Spacer(Modifier.height(6.dp))
                OutlinedTextField(
                    value = thresholdText,
                    onValueChange = { thresholdText = it; error = null },
                    singleLine = true,
                    isError = error != null,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
                    modifier = Modifier.fillMaxWidth(),
                )

                // ── Token 规则的重置方式与周期 ──
                if (draft.type == "token") {
                    Spacer(Modifier.height(14.dp))
                    Text("重置方式", color = C.textMain, fontSize = 13.sp)
                    Spacer(Modifier.height(6.dp))
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        ChoiceChip(
                            label = "固定周期",
                            hint = "对齐自然时间，每 N 分钟 / 小时一轮",
                            selected = draft.reset == "fixed",
                            onClick = { draft = draft.copy(reset = "fixed") },
                            modifier = Modifier.weight(1f),
                        )
                        ChoiceChip(
                            label = "自然日",
                            hint = "每天本地 0 点重置（有的账号过了 0 点额度就回来）",
                            selected = draft.reset == "daily",
                            onClick = { draft = draft.copy(reset = "daily") },
                            modifier = Modifier.weight(1f),
                        )
                    }
                    if (draft.reset == "fixed") {
                        Spacer(Modifier.height(10.dp))
                        Text("周期长度", color = C.textMain, fontSize = 13.sp)
                        Spacer(Modifier.height(2.dp))
                        Text("30 分钟 ~ 24 小时", color = C.textDim, fontSize = 11.sp)
                        Spacer(Modifier.height(6.dp))
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            OutlinedTextField(
                                value = periodText,
                                onValueChange = { periodText = it; error = null },
                                singleLine = true,
                                isError = error != null,
                                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                                modifier = Modifier.weight(1f),
                            )
                            ChoiceChip(
                                label = if (periodUnitHours) "小时" else "分钟",
                                hint = "点按切换单位",
                                selected = true,
                                onClick = { periodUnitHours = !periodUnitHours },
                            )
                        }
                    }
                }

                // ── 触发动作 ──
                Spacer(Modifier.height(14.dp))
                Text("触发后", color = C.textMain, fontSize = 13.sp)
                Spacer(Modifier.height(6.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    ChoiceChip(
                        label = "跳过该账号",
                        hint = if (draft.type == "balance") "余额回升后自动恢复" else "窗口重置后自动恢复",
                        selected = draft.action == "skip",
                        onClick = { draft = draft.copy(action = "skip") },
                        modifier = Modifier.weight(1f),
                    )
                    ChoiceChip(
                        label = "禁用该账号",
                        hint = "重置也不恢复，需手动启用",
                        selected = draft.action == "disable",
                        onClick = { draft = draft.copy(action = "disable") },
                        modifier = Modifier.weight(1f),
                    )
                }

                error?.let {
                    Spacer(Modifier.height(8.dp))
                    Text(it, color = C.red, fontSize = 11.sp)
                }
            }
        },
        confirmButton = {
            TextButton(onClick = {
                if (!enabled) {
                    onSave(emptyList())
                    return@TextButton
                }
                val threshold = thresholdText.trim().toDoubleOrNull()
                if (threshold == null || !threshold.isFinite() || threshold <= 0.0) {
                    error = "阈值必须是大于 0 的数字"
                    return@TextButton
                }
                var period = 3600L
                if (draft.type == "token" && draft.reset == "fixed") {
                    val n = periodText.trim().toLongOrNull()
                    if (n == null || n <= 0) {
                        error = "周期长度必须是正整数"
                        return@TextButton
                    }
                    period = n * if (periodUnitHours) 3600L else 60L
                    if (period < 1800L || period > 86400L) {
                        error = "周期长度必须是 30 分钟 ~ 24 小时"
                        return@TextButton
                    }
                }
                onSave(listOf(draft.copy(threshold = threshold, period = period, enabled = true)))
            }) {
                Text("保存", color = C.accent)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("取消") } },
    )
}

/**
 * 一条限制规则（客户端侧形状，与内核 `limiter::LimiterRule::to_json` 逐键对齐）。
 *
 * `period` 只在「Token + 固定周期」时有意义；其余情况内核不带这个键，这里也照
 * 内核的口径序列化（不带），避免写一个内核不认的 0 进去。
 */
data class LimiterRule(
    /** `balance` | `token` */
    val type: String,
    /** `skip` | `disable` */
    val action: String,
    val threshold: Double,
    /** `fixed` | `daily`（余额规则恒 `fixed`） */
    val reset: String,
    /** 秒；仅 token + fixed 有效 */
    val period: Long,
    val enabled: Boolean,
) {
    fun toJson(): JSONObject = JSONObject()
        .put("type", type)
        .put("action", action)
        .put("threshold", threshold)
        .put("reset", if (type == "balance") "fixed" else reset)
        .put("enabled", enabled)
        .apply {
            if (type == "token" && reset == "fixed") put("period", period)
        }

    /** 一行摘要（账号卡上的入口用）。 */
    fun summary(): String {
        val what = when {
            type == "balance" -> "余额低于 ${fmtNumber(threshold)}"
            reset == "daily" -> "每日消耗达 ${fmtNumber(threshold)}"
            else -> "消耗达 ${fmtNumber(threshold)}"
        }
        val how = if (action == "disable") "禁用" else "跳过"
        return "$what 时$how"
    }
}

/** 从账号原始 JSON 里读出生效中的规则列表（空 = 不限制）。 */
fun limiterRulesOf(account: JSONObject): List<LimiterRule> {
    val rules = account.optJSONArray("limiters") ?: return emptyList()
    val out = ArrayList<LimiterRule>(rules.length())
    for (i in 0 until rules.length()) {
        val r = rules.optJSONObject(i) ?: continue
        if (!r.optBoolean("enabled", true)) continue
        val type = r.optString("type").takeIf { it == "balance" || it == "token" } ?: continue
        val threshold = r.opt("threshold")?.let { raw ->
            when (raw) {
                is Number -> raw.toDouble()
                is String -> raw.trim().toDoubleOrNull()
                else -> null
            }
        } ?: continue
        if (!threshold.isFinite() || threshold <= 0.0) continue
        out += LimiterRule(
            type = type,
            action = r.optString("action").takeIf { it == "disable" } ?: "skip",
            threshold = threshold,
            reset = r.optString("reset").takeIf { it == "daily" } ?: "fixed",
            period = r.optLong("period", 3600L).takeIf { it > 0 } ?: 3600L,
            enabled = true,
        )
    }
    return out
}

/** 数值展示：整数不带小数点，小数去掉尾零（`10.0` → `10`、`3500.50` → `3500.5`）。 */
private fun fmtNumber(v: Double): String =
    if (v == v.toLong().toDouble()) v.toLong().toString() else v.toString().trimEnd('0').trimEnd('.')

@Composable
private fun ChoiceChip(
    label: String,
    hint: String,
    selected: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    Column(
        modifier
            .background(if (selected) C.accent.copy(alpha = 0.12f) else C.insetBg, RoundedCornerShape(12.dp))
            .border(1.dp, if (selected) C.accent else C.border, RoundedCornerShape(12.dp))
            .clickable { onClick() }
            .padding(10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                label,
                color = if (selected) C.accent else C.textMain,
                fontSize = 12.sp,
                fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
            )
            if (selected) {
                Spacer(Modifier.size(6.dp))
                AgateBadge("已选", BadgeTone.BLUE)
            }
        }
        Spacer(Modifier.height(2.dp))
        Text(hint, color = C.textDim, fontSize = 10.sp)
    }
}

/** 账号卡上的「额度设限」入口行。 */
@Composable
fun LimiterEntryRow(
    summary: String,
    onOpen: () -> Unit,
) {
    val C = agateColors()
    Row(
        Modifier
            .fillMaxWidth()
            .clickable { onOpen() }
            .padding(vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text("额度设限", color = C.textSub, fontSize = 12.sp, modifier = Modifier.weight(1f))
        Text(summary, color = C.textDim, fontSize = 11.sp)
        Spacer(Modifier.size(4.dp))
        Text("›", color = C.textDim, fontSize = 13.sp)
    }
    Box(Modifier.fillMaxWidth().height(1.dp).background(C.border))
}
