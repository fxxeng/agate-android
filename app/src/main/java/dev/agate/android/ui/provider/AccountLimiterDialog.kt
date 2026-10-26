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

/**
 * 「额度设限」弹窗（照 3.0.8 的 `accounts-dialog-limiter` 口径，只做**余额**那一类）。
 *
 * ── 为什么只做余额规则 ──────────────────────────────────────
 * 3.0.8 的规则列表有「余额」与「Token」两类；Token 那一类要读窗口内累计消耗
 * （`tokenReadingForRule` 那套读数），内核已支持但客户端还没接用量流水。这里先
 * 把余额规则做完 —— 它正是「账号额度设限」这个需求的全部内容。
 *
 * ── 形态 ───────────────────────────────────────────────────
 * 一条余额规则 = `{type:"balance", action, threshold, reset:"fixed", enabled}`。
 * `action` 两档（内核 `LimiterAction`）：
 *   - `skip`：余额低于阈值时**跳过**这个账号（还有别的账号可用时继续换号）；
 *   - `disable`：直接**禁用**该账号（不再参与轮转，直到手动启用）。
 * 「不设限」= 规则停用（`enabled=false`）或整块清空 —— 与 3.0.8 取消「不处理」档
 * 的口径一致：**没有生效的规则就是不限制**。
 *
 * ── 与旧 `lowBalance` 的关系 ────────────────────────────────
 * 保存走 `limiters` 键（整块替换），内核 `apply_patch` 归一化校验后会**顺手同步**
 * 旧 `lowBalance` 供降级兼容 —— 客户端只发新键，不发旧键（两键同发时以 `limiters`
 * 为准，见那里的说明）。
 */
@Composable
fun AccountLimiterDialog(
    accountName: String,
    /** 当前生效的余额规则（`null` = 没有生效的规则，即不限制）。 */
    current: BalanceLimiter?,
    onDismiss: () -> Unit,
    onSave: (BalanceLimiter?) -> Unit,
) {
    val C = agateColors()
    var enabled by remember { mutableStateOf(current != null) }
    var action by remember { mutableStateOf(current?.action ?: "skip") }
    var threshold by remember { mutableStateOf(current?.threshold?.let { fmtThreshold(it) } ?: "10") }
    var error by remember { mutableStateOf<String?>(null) }

    val parsed = threshold.trim().toDoubleOrNull()

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
                        Text(
                            "余额低于阈值时按下面的方式处理该账号",
                            color = C.textDim, fontSize = 11.sp,
                        )
                    }
                    Switch(checked = enabled, onCheckedChange = { enabled = it })
                }

                if (!enabled) {
                    Spacer(Modifier.height(10.dp))
                    Text(
                        "已关闭：这个账号不参与余额限制（照常轮转）。",
                        color = C.textSub, fontSize = 12.sp,
                    )
                    return@Column
                }

                Spacer(Modifier.height(14.dp))
                Text("余额阈值", color = C.textMain, fontSize = 13.sp)
                Spacer(Modifier.height(2.dp))
                Text("读数低于这个数时触发（严格小于）", color = C.textDim, fontSize = 11.sp)
                Spacer(Modifier.height(6.dp))
                OutlinedTextField(
                    value = threshold,
                    onValueChange = { threshold = it; error = null },
                    singleLine = true,
                    isError = error != null,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
                    modifier = Modifier.fillMaxWidth(),
                )

                Spacer(Modifier.height(14.dp))
                Text("触发后", color = C.textMain, fontSize = 13.sp)
                Spacer(Modifier.height(6.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    ChoiceChip(
                        label = "跳过该账号",
                        hint = "还有别的账号可用时继续换号",
                        selected = action == "skip",
                        onClick = { action = "skip" },
                        modifier = Modifier.weight(1f),
                    )
                    ChoiceChip(
                        label = "禁用该账号",
                        hint = "不再参与轮转，需手动启用",
                        selected = action == "disable",
                        onClick = { action = "disable" },
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
                    onSave(null)
                    return@TextButton
                }
                if (parsed == null || !parsed.isFinite() || parsed <= 0.0) {
                    error = "余额阈值必须是大于 0 的数字"
                    return@TextButton
                }
                onSave(BalanceLimiter(action = action, threshold = parsed))
            }) {
                Text("保存", color = C.accent)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("取消") } },
    )
}

/** 一条余额限制规则（客户端侧的最小形状；序列化见 [toJson]）。 */
data class BalanceLimiter(val action: String, val threshold: Double) {
    fun toJson(): org.json.JSONObject = org.json.JSONObject()
        .put("type", "balance")
        .put("action", action)
        .put("threshold", threshold)
        // 余额规则恒 fixed（跟随余额读数，无窗口概念）—— 内核 `LimiterReset`
        .put("reset", "fixed")
        .put("enabled", true)
}

/** 从账号原始 JSON 里读出生效中的余额规则；没有 / 已停用 → null（= 不限制）。 */
fun balanceLimiterOf(account: org.json.JSONObject): BalanceLimiter? {
    val rules = account.optJSONArray("limiters") ?: return null
    for (i in 0 until rules.length()) {
        val rule = rules.optJSONObject(i) ?: continue
        if (rule.optString("type") != "balance") continue
        if (!rule.optBoolean("enabled", true)) continue
        val threshold = rule.opt("threshold")?.let { raw ->
            when (raw) {
                is Number -> raw.toDouble()
                is String -> raw.trim().toDoubleOrNull()
                else -> null
            }
        } ?: continue
        if (!threshold.isFinite() || threshold <= 0.0) continue
        return BalanceLimiter(
            action = rule.optString("action").takeIf { it == "disable" } ?: "skip",
            threshold = threshold,
        )
    }
    return null
}

/** 阈值展示：整数不带小数点，小数去掉尾零（`10.0` → `10`）。 */
private fun fmtThreshold(v: Double): String =
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

/** 弹窗入口行：账号卡上的「额度设限」按钮。 */
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
