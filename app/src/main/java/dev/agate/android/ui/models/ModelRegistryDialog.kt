package dev.agate.android.ui.models

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Switch
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
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import dev.agate.android.ui.components.AgateFormInput
import dev.agate.android.ui.theme.agateColors
import org.json.JSONArray
import org.json.JSONObject

/**
 * 自定义提供商的**模型清单登记器**。
 *
 * ── 为什么需要它（不只是"锦上添花"）──
 * 内置家的清单由适配器给；自定义家的清单是**用户登记的那份运行期数据**，
 * 内核按它回答「这个模型名谁承载」。而在此之前客户端只有「拉取」一条路：
 * 上游不提供 `GET /models`（或暂时挂了、或那家根本不给）时，这一家就永远
 * 零模型 —— 加进来也没法用，且界面上没有任何说得出原因的提示。
 *
 * ── 为什么保留 `enabled` 而不是全删重建 ──────────────────────
 * 内核 `/api/custom-providers/models` 是**整表替换**，所以这里交上去的是完整
 * 清单。既有条目一律**原地保留**（连同它的 enabled 与将来可能出现的额外字段），
 * 只做三件用户明确要求的动作：改启停、移除、追加。这样保存一次不会把用户的
 * 其他设置悄悄抹掉。
 */
@Composable
internal fun ModelRegistryDialog(
    providerLabel: String,
    /** 基线：`models` 数组原样（每个对象至少含 `id`、`enabled`）。 */
    baseline: JSONArray,
    /** 保存中（按钮转圈 + 防重复点击）。 */
    saving: Boolean,
    onDismiss: () -> Unit,
    onSave: (JSONArray) -> Unit,
) {
    val C = agateColors()
    // 编辑副本：对话框关掉即丢弃，不做"边改边存"—— 整表替换的接口不适合那样用
    var entries by remember(baseline) {
        mutableStateOf(
            (0 until baseline.length()).mapNotNull { i ->
                baseline.optJSONObject(i)?.takeIf { it.optString("id").isNotBlank() }
            }
        )
    }
    var draft by remember { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }

    fun add() {
        val id = draft.trim()
        if (id.isEmpty()) return
        if (id.length > 200) {
            error = "模型名过长（上限 200 字符）"
            return
        }
        if (entries.any { it.optString("id").equals(id, ignoreCase = true) }) {
            error = "「$id」已经登记过了"
            return
        }
        entries = entries + JSONObject().put("id", id).put("enabled", true)
        draft = ""
        error = null
    }

    Dialog(onDismissRequest = { if (!saving) onDismiss() }) {
        Column(
            Modifier
                .fillMaxWidth()
                .background(C.cardBg, RoundedCornerShape(18.dp))
                .border(1.dp, C.border, RoundedCornerShape(18.dp))
                .padding(16.dp),
        ) {
            Text(
                "登记模型",
                color = C.textMain,
                fontSize = 15.sp,
                fontWeight = FontWeight.SemiBold,
            )
            Spacer(Modifier.height(3.dp))
            Text(
                "$providerLabel · 这里登记的模型名就是客户端要用的名字（登记后需启用才能下发）",
                color = C.textDim,
                fontSize = 11.sp,
            )
            Spacer(Modifier.height(12.dp))

            // 已有条目
            if (entries.isEmpty()) {
                Text(
                    "还没有登记任何模型。可以在下面手填，或回到「拉取模型」从上游拉一份。",
                    color = C.textDim,
                    fontSize = 12.sp,
                )
            } else {
                Column(
                    Modifier
                        .fillMaxWidth()
                        .heightIn(max = 280.dp)
                        .verticalScroll(rememberScrollState()),
                ) {
                    entries.forEachIndexed { index, entry ->
                        val id = entry.optString("id")
                        val enabled = entry.optBoolean("enabled", true)
                        Row(
                            Modifier.fillMaxWidth().padding(vertical = 5.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(Modifier.weight(1f)) {
                                Text(
                                    id,
                                    color = if (enabled) C.textMain else C.textDim,
                                    fontSize = 12.5.sp,
                                )
                                if (!enabled) {
                                    Text("已停用 · 不会下发给客户端", color = C.textDim, fontSize = 10.sp)
                                }
                            }
                            Switch(
                                checked = enabled,
                                onCheckedChange = { want ->
                                    entries = entries.toMutableList().also {
                                        it[index] = JSONObject(entry.toString()).put("enabled", want)
                                    }
                                },
                                modifier = Modifier.size(width = 42.dp, height = 24.dp),
                                enabled = !saving,
                            )
                            Spacer(Modifier.width(4.dp))
                            Text(
                                "移除",
                                color = C.red,
                                fontSize = 11.sp,
                                modifier = Modifier
                                    .clickable(enabled = !saving) {
                                        entries = entries.filterIndexed { i, _ -> i != index }
                                    }
                                    .padding(horizontal = 4.dp, vertical = 4.dp),
                            )
                        }
                    }
                }
            }

            Spacer(Modifier.height(10.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    AgateFormInput(
                        value = draft,
                        onValueChange = {
                            draft = it
                            if (error != null) error = null
                        },
                        hint = "模型名，如 gpt-4o-mini",
                        singleLine = true,
                        minLines = 1,
                    )
                }
                Spacer(Modifier.width(8.dp))
                Text(
                    "添加",
                    color = if (draft.isBlank() || saving) C.textDim else C.accent,
                    fontSize = 12.5.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .clickable(enabled = draft.isNotBlank() && !saving) { add() }
                        .padding(horizontal = 8.dp, vertical = 6.dp),
                )
            }
            error?.let {
                Spacer(Modifier.height(6.dp))
                Text(it, color = C.red, fontSize = 11.sp)
            }

            Spacer(Modifier.height(14.dp))
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                Text(
                    "取消",
                    color = C.textSub,
                    fontSize = 12.5.sp,
                    modifier = Modifier
                        .clickable(enabled = !saving) { onDismiss() }
                        .padding(horizontal = 10.dp, vertical = 8.dp),
                )
                Spacer(Modifier.width(6.dp))
                Text(
                    if (saving) "保存中…" else "保存",
                    color = C.onAccent,
                    fontSize = 12.5.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .background(
                            if (saving) C.accent.copy(alpha = 0.5f) else C.accent,
                            RoundedCornerShape(10.dp),
                        )
                        .clickable(enabled = !saving) {
                            val out = JSONArray()
                            entries.forEach { out.put(it) }
                            onSave(out)
                        }
                        .padding(horizontal = 14.dp, vertical = 8.dp),
                )
            }
        }
    }
}
