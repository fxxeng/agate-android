package dev.agate.android.ui.settings

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.AccountSortOps
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateSubPageHeader
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateColors
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject

/**
 * 账号自动排序：按额度到期 / 余额 / 加入时间给各家的账号排优先级。
 *
 * ── 与路由档案里的 accountOrder 不同 ─────────────────────────
 * 那边是单条路由里手写的账号顺序；这里是「自动排序规则」——每家最多三条，
 * 顺序即优先级。内核会算出 providerOrder / accountOrder 供路由使用。
 *
 * 规则是 tagged enum：`{"key":"credit|balance|added","on":bool,"dir":"..."}`，
 * 这里按原样透传 JSON（不建 DTO），内核加一维规则客户端不必跟着改。
 * 保存是按家整份替换（传该家的规则数组），所以每家一个「保存」按钮。
 */
@Composable
fun AccountSortPage(onBack: () -> Unit) {
    val C = agateColors()
    val scope = rememberCoroutineScope()

    var sorting by remember { mutableStateOf<AccountSortOps.Sorting?>(null) }
    // 正在编辑的家 → 规则数组副本（可改）；未编辑的家读 sorting.rules
    var drafts by remember { mutableStateOf<Map<String, JSONArray>>(emptyMap()) }
    var dirty by remember { mutableStateOf<Set<String>>(emptySet()) }
    var busy by remember { mutableStateOf<String?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var notice by remember { mutableStateOf<String?>(null) }

    fun reload() {
        scope.launch {
            val r = AccountSortOps.load()
            if (r.ok && r.data != null) {
                sorting = r.data
                drafts = r.data!!.rules.mapValues { (_, v) -> copyArr(v) }
                dirty = emptySet()
                error = null
            } else {
                error = r.error ?: "读取排序规则失败"
            }
        }
    }
    LaunchedEffect(Unit) { reload() }

    fun save(provider: String) {
        val arr = drafts[provider] ?: return
        busy = provider; error = null; notice = null
        scope.launch {
            val r = AccountSortOps.save(provider, arr, onlyIfUnset = false)
            if (r.ok && r.data != null) {
                sorting = r.data
                drafts = r.data!!.rules.mapValues { (_, v) -> copyArr(v) }
                dirty = dirty - provider
                notice = "「$provider」排序规则已保存"
            } else {
                error = r.error ?: "保存失败"
            }
            busy = null
        }
    }

    fun toggleRule(provider: String, index: Int) {
        val arr = copyArr(drafts[provider] ?: JSONArray())
        val rule = arr.optJSONObject(index) ?: return
        rule.put("on", !rule.optBoolean("on", false))
        arr.put(index, rule)
        drafts = drafts + (provider to arr)
        dirty = dirty + provider
    }

    fun flipDir(provider: String, index: Int) {
        val arr = copyArr(drafts[provider] ?: JSONArray())
        val rule = arr.optJSONObject(index) ?: return
        val dir = rule.optString("dir")
        rule.put("dir", if (dir == "desc") "asc" else "desc")
        arr.put(index, rule)
        drafts = drafts + (provider to arr)
        dirty = dirty + provider
    }

    fun reset(provider: String) {
        busy = provider; error = null
        scope.launch {
            val r = AccountSortOps.save(provider, null, onlyIfUnset = false)
            if (r.ok && r.data != null) {
                sorting = r.data
                drafts = r.data!!.rules.mapValues { (_, v) -> copyArr(v) }
                dirty = dirty - provider
                notice = "「$provider」已恢复默认排序"
            } else {
                error = r.error ?: "恢复失败"
            }
            busy = null
        }
    }

    Column(Modifier.fillMaxSize().background(C.bg)) {
        AgateSubPageHeader("账号自动排序", onBack = onBack) {
            sorting?.let { AgateBadge("${it.configured.size} 家已配", BadgeTone.BLUE) }
        }
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 16.dp)) {
            Spacer(Modifier.height(10.dp))
            Text("按额度到期 / 余额 / 加入时间给每家的账号排优先级，顺序即优先级。改动按家单独保存并即时生效。",
                style = AgateType.Caption, color = C.textSub)
            Spacer(Modifier.height(10.dp))
            if (error != null) { AsBanner(error!!, C.red, C.redBg); Spacer(Modifier.height(8.dp)) }
            if (notice != null) { AsBanner(notice!!, C.green, C.greenBg); Spacer(Modifier.height(8.dp)) }

            val s = sorting
            if (s == null) {
                AsCard { Text("读取中…", style = AgateType.BodyS, color = C.textDim) }
            } else {
                val providers = s.rules.keys.toList()
                if (providers.isEmpty()) {
                    AsCard { Text("还没有账号分组。先去「提供商」添加账号后再来排序。",
                        style = AgateType.BodyS, color = C.textDim) }
                } else {
                    providers.forEach { provider ->
                        val arr = drafts[provider] ?: JSONArray()
                        AsCard {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(provider, style = AgateType.SectionTitle, color = C.textMain,
                                    fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                                if (s.configured.containsKey(provider)) AgateBadge("已配置", BadgeTone.BLUE)
                                else AgateBadge("默认", BadgeTone.GRAY)
                            }
                            Spacer(Modifier.height(8.dp))
                            if (arr.length() == 0) {
                                Text("该家没有排序规则（使用默认顺序）。", style = AgateType.Caption, color = C.textDim)
                            } else {
                                (0 until arr.length()).forEach { i ->
                                    val rule = arr.optJSONObject(i) ?: return@forEach
                                    RuleLine(rule, C,
                                        onToggle = { toggleRule(provider, i) },
                                        onFlipDir = { flipDir(provider, i) })
                                }
                            }
                            // 该家账号排好的顺序（真值，只读）
                            s.accountOrder[provider]?.let { order ->
                                if (order.isNotEmpty()) {
                                    Spacer(Modifier.height(8.dp))
                                    Text("当前顺序：${order.take(5).joinToString(" › ")}${if (order.size > 5) " …" else ""}",
                                        style = AgateType.Caption, color = C.textDim)
                                }
                            }
                            Spacer(Modifier.height(10.dp))
                            Row(horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(8.dp)) {
                                AsBtn("恢复默认", enabled = busy == null && s.configured.containsKey(provider)) { reset(provider) }
                                AsBtn(if (busy == provider) "保存中…" else "保存", primary = true,
                                    enabled = busy == null && dirty.contains(provider)) { save(provider) }
                            }
                        }
                        Spacer(Modifier.height(10.dp))
                    }
                }
                if (s.providerOrder.isNotEmpty()) {
                    Spacer(Modifier.height(4.dp))
                    Text("提供商显示顺序：${s.providerOrder.joinToString(" › ")}", style = AgateType.Caption, color = C.textDim)
                }
            }
            Spacer(Modifier.height(24.dp))
        }
    }
}

private fun copyArr(src: JSONArray): JSONArray {
    val out = JSONArray()
    (0 until src.length()).forEach { i -> out.put(src.opt(i)) }
    return out
}

@Composable
private fun RuleLine(rule: JSONObject, C: AgateColors, onToggle: () -> Unit, onFlipDir: () -> Unit) {
    val key = rule.optString("key")
    val label = when (key) { "credit" -> "额度到期"; "balance" -> "余额"; "added" -> "加入时间"; else -> key }
    val dir = rule.optString("dir")
    Row(Modifier.fillMaxWidth().padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(label, style = AgateType.BodyS, color = C.textMain, modifier = Modifier.weight(1f))
        Text(if (dir == "desc") "降序" else "升序", fontSize = 12.sp, color = C.accent,
            modifier = Modifier.clickable(onClick = onFlipDir).padding(horizontal = 6.dp))
        Switch(checked = rule.optBoolean("on", false), onCheckedChange = { onToggle() })
    }
}

@Composable
private fun AsCard(content: @Composable ColumnScope.() -> Unit) {
    val c = agateColors()
    Column(Modifier.fillMaxWidth().agateCard().background(c.cardBg, RoundedCornerShape(20.dp))
        .border(1.dp, c.border, RoundedCornerShape(20.dp)).padding(horizontal = 14.dp, vertical = 13.dp), content = content)
}

@Composable
private fun AsBanner(text: String, fg: Color, bg: Color) {
    Row(Modifier.fillMaxWidth().background(bg, RoundedCornerShape(10.dp)).padding(horizontal = 12.dp, vertical = 8.dp)) {
        Text(text, style = AgateType.Caption, color = fg)
    }
}

@Composable
private fun AsBtn(text: String, primary: Boolean = false, enabled: Boolean, onClick: () -> Unit) {
    val c = agateColors()
    Box(Modifier.background(if (!enabled) c.btnBg.copy(alpha = 0.5f) else if (primary) c.accent else c.btnBg, RoundedCornerShape(10.dp))
        .then(if (enabled) Modifier.clickable(onClick = onClick) else Modifier)
        .padding(horizontal = 14.dp, vertical = 9.dp), contentAlignment = Alignment.Center) {
        Text(text, style = AgateType.BodyS, fontWeight = FontWeight.SemiBold,
            color = if (!enabled) c.textDim else if (primary) c.onAccent else c.textMain)
    }
}
