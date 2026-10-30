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
 * 自定义提供商的**模型清单 + 别名映射**登记器。
 *
 * ── 为什么需要它（不只是"锦上添花"）──
 * 内置家的清单由适配器给；自定义家的清单是**用户登记的运行期数据**，内核按它
 * 回答「这个模型名谁承载」。而在此之前客户端只有「拉取」一条路：上游不提供
 * `GET /models`（或暂时挂了、或那家根本不给）时，这一家就永远零模型 ——
 * 加进来也没法用，且界面上说不出原因。
 *
 * ── 别名映射为什么必须放在一起 ──────────────────────────────
 * 内核的 `mappings`（对外别名 → 上游真名）**只有自定义家有**：内置家的映射走
 * `model_rules`，那里已有独立页面。而自定义家的这条链此前在整个 App 里没有入口
 * —— 能建家、能登记模型，却没法把「客户端要用的名字」映射到「上游认的名字」，
 * 而这是中转类上游最常见的诉求（同一模型各家叫法不同）。
 * 两个清单本来就存在同一条记录的同一份写入里，所以放在同一个弹层。
 *
 * ── 顺序是真的会影响结果的，所以给了上移/下移 ────────────────
 * 内核 `custom::append_models_response` 按**数组顺序**把自定义家的模型追加到
 * `/v1/models` 末尾，整份清单不做二次排序 —— 也就是说这里的顺序就是客户端
 * 下拉里的顺序。常用模型放前面是有意义的，所以提供 ↑/↓。
 *
 * 为什么不用拖拽：弹层内部本身是可纵向滚动的容器，长按拖拽会和滚动抢手势，
 * 而行高还随「已停用」提示变高，落点判定更不稳。↑/↓ 在任意长度、任意行高下
 * 都是确定的 —— 这里的正确性比手势的顺手更重要。
 *
 * ── 保存是整表替换，因此**必须在既有条目上改** ────────────────
 * 内核 `POST /api/custom-providers/models` 拿到的 models/mappings 会整体覆盖。
 * 这里保留每个对象的原始字段（`enabled`、`reasoning`、能力位、映射的 enabled…），
 * 只做用户明确要求的动作：改启停、移除、追加。否则保存一次就把其他设置抹了。
 */
@Composable
internal fun ModelRegistryDialog(
    providerLabel: String,
    /** 基线：`models` 数组原样（每个对象至少含 `id`、`enabled`）。 */
    baseline: JSONArray,
    /** 基线：`mappings` 数组原样（每个对象含 `alias`、`target`）。 */
    mappingsBaseline: JSONArray,
    /** 保存中（按钮转圈 + 防重复点击）。 */
    saving: Boolean,
    onDismiss: () -> Unit,
    onSave: (models: JSONArray, mappings: JSONArray) -> Unit,
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
    var mappings by remember(mappingsBaseline) {
        mutableStateOf(
            (0 until mappingsBaseline.length()).mapNotNull { i ->
                mappingsBaseline.optJSONObject(i)?.takeIf { it.optString("alias").isNotBlank() }
            }
        )
    }
    var draft by remember { mutableStateOf("") }
    var aliasDraft by remember { mutableStateOf("") }
    var targetDraft by remember { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }
    var aliasError by remember { mutableStateOf<String?>(null) }

    fun addModel() {
        val id = draft.trim()
        if (id.isEmpty()) return
        if (id.length > NAME_MAX) {
            error = "模型名过长（上限 $NAME_MAX 字符）"
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

    fun addMapping() {
        val alias = aliasDraft.trim()
        val target = targetDraft.trim()
        when {
            alias.isEmpty() -> { aliasError = "映射名不能为空"; return }
            target.isEmpty() -> { aliasError = "要填目标上游模型名"; return }
            alias.length > NAME_MAX || target.length > NAME_MAX -> {
                aliasError = "名字过长（上限 $NAME_MAX 字符）"
                return
            }
            mappings.any {
                it.optString("alias").equals(alias, ignoreCase = true) &&
                    it.optString("target").equals(target, ignoreCase = true)
            } -> { aliasError = "这条映射已经存在"; return }
        }
        mappings = mappings + JSONObject()
            .put("alias", alias)
            .put("target", target)
            .put("enabled", true)
        aliasDraft = ""
        targetDraft = ""
        aliasError = null
    }

    val knownIds = entries.map { it.optString("id") }.toSet()

    Dialog(onDismissRequest = { if (!saving) onDismiss() }) {
        Column(
            Modifier
                .fillMaxWidth()
                .background(C.cardBg, RoundedCornerShape(18.dp))
                .border(1.dp, C.border, RoundedCornerShape(18.dp))
                .padding(16.dp),
        ) {
            Text(
                "登记模型与别名",
                color = C.textMain,
                fontSize = 15.sp,
                fontWeight = FontWeight.SemiBold,
            )
            Spacer(Modifier.height(3.dp))
            Text(
                "$providerLabel · 这里填的名字就是客户端要用的名字（登记后需启用才会下发）",
                color = C.textDim,
                fontSize = 11.sp,
            )

            // ── 模型清单 ──
            Spacer(Modifier.height(12.dp))
            Text("模型清单（${entries.size}）", color = C.textSub, fontSize = 12.sp)
            Spacer(Modifier.height(6.dp))
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
                        .heightIn(max = 190.dp)
                        .verticalScroll(rememberScrollState()),
                ) {
                    entries.forEachIndexed { index, entry ->
                        val id = entry.optString("id")
                        val enabled = entry.optBoolean("enabled", true)
                        Row(
                            Modifier.fillMaxWidth().padding(vertical = 4.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(Modifier.weight(1f)) {
                                Text(
                                    id,
                                    color = if (enabled) C.textMain else C.textDim,
                                    fontSize = 12.5.sp,
                                )
                                if (!enabled) {
                                    Text(
                                        "已停用 · 不会下发给客户端",
                                        color = C.textDim,
                                        fontSize = 10.sp,
                                    )
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
                            // 上移 / 下移：这个顺序就是客户端看到的下发顺序（见文件头）
                            Text(
                                "↑",
                                color = if (index == 0 || saving) C.textDim else C.accent,
                                fontSize = 13.sp,
                                modifier = Modifier
                                    .clickable(enabled = index > 0 && !saving) {
                                        entries = entries.toMutableList().also {
                                            val moved = it.removeAt(index)
                                            it.add(index - 1, moved)
                                        }
                                    }
                                    .padding(horizontal = 5.dp, vertical = 4.dp),
                            )
                            Text(
                                "↓",
                                color = if (index == entries.lastIndex || saving) C.textDim
                                else C.accent,
                                fontSize = 13.sp,
                                modifier = Modifier
                                    .clickable(enabled = index < entries.lastIndex && !saving) {
                                        entries = entries.toMutableList().also {
                                            val moved = it.removeAt(index)
                                            it.add(index + 1, moved)
                                        }
                                    }
                                    .padding(horizontal = 5.dp, vertical = 4.dp),
                            )
                            Spacer(Modifier.width(2.dp))
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
            Spacer(Modifier.height(6.dp))
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
                        .clickable(enabled = draft.isNotBlank() && !saving) { addModel() }
                        .padding(horizontal = 8.dp, vertical = 6.dp),
                )
            }
            error?.let {
                Spacer(Modifier.height(4.dp))
                Text(it, color = C.red, fontSize = 11.sp)
            }

            // ── 别名映射 ──
            Spacer(Modifier.height(14.dp))
            Text("别名映射（${mappings.size}）", color = C.textSub, fontSize = 12.sp)
            Spacer(Modifier.height(2.dp))
            Text(
                "客户端请求「别名」时，网关改成向上游要「目标模型」。中转类上游各家叫法不同时用它。",
                color = C.textDim,
                fontSize = 10.5.sp,
            )
            Spacer(Modifier.height(6.dp))
            if (mappings.isNotEmpty()) {
                Column(
                    Modifier
                        .fillMaxWidth()
                        .heightIn(max = 130.dp)
                        .verticalScroll(rememberScrollState()),
                ) {
                    mappings.forEachIndexed { index, entry ->
                        val alias = entry.optString("alias")
                        val target = entry.optString("target")
                        val targetKnown = target in knownIds
                        Row(
                            Modifier.fillMaxWidth().padding(vertical = 3.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(Modifier.weight(1f)) {
                                Text("$alias → $target", color = C.textMain, fontSize = 12.sp)
                                if (!targetKnown) {
                                    // 不拦：可以先把映射建好、之后补模型；但必须提示，
                                    // 否则他只会在客户端报错时一脸茫然
                                    Text(
                                        "目标模型还没在清单里登记，这条现在不会生效",
                                        color = C.amber,
                                        fontSize = 10.sp,
                                    )
                                }
                            }
                            Text(
                                "移除",
                                color = C.red,
                                fontSize = 11.sp,
                                modifier = Modifier
                                    .clickable(enabled = !saving) {
                                        mappings = mappings.filterIndexed { i, _ -> i != index }
                                    }
                                    .padding(horizontal = 4.dp, vertical = 4.dp),
                            )
                        }
                    }
                }
            }
            Spacer(Modifier.height(6.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    AgateFormInput(
                        value = aliasDraft,
                        onValueChange = { aliasDraft = it; aliasError = null },
                        hint = "别名",
                        singleLine = true,
                        minLines = 1,
                    )
                }
                Spacer(Modifier.width(6.dp))
                Column(Modifier.weight(1f)) {
                    AgateFormInput(
                        value = targetDraft,
                        onValueChange = { targetDraft = it; aliasError = null },
                        hint = "目标模型",
                        singleLine = true,
                        minLines = 1,
                    )
                }
                Spacer(Modifier.width(6.dp))
                Text(
                    "添加",
                    color = if (aliasDraft.isBlank() || targetDraft.isBlank() || saving) C.textDim
                    else C.accent,
                    fontSize = 12.5.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .clickable(
                            enabled = aliasDraft.isNotBlank() &&
                                targetDraft.isNotBlank() && !saving,
                        ) { addMapping() }
                        .padding(horizontal = 6.dp, vertical = 6.dp),
                )
            }
            aliasError?.let {
                Spacer(Modifier.height(4.dp))
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
                            val outModels = JSONArray()
                            entries.forEach { outModels.put(it) }
                            val outMappings = JSONArray()
                            mappings.forEach { outMappings.put(it) }
                            onSave(outModels, outMappings)
                        }
                        .padding(horizontal = 14.dp, vertical = 8.dp),
                )
            }
        }
    }
}

/** 与内核 `MAX_MODEL_ID_CHARS` / `MAX_ALIAS_CHARS` 一致（都是 128）。 */
private const val NAME_MAX = 128
