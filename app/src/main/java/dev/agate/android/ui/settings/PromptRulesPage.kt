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
import androidx.compose.foundation.text.BasicTextField
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
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.PromptRulesOps
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateSubPageHeader
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateColors
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch

/**
 * 提示词规则：按 家/客户端/模型 条件换提示词。
 *
 * ── 与「设置 → 提示词」的关系 ────────────────────────────────
 * 那边是全局单一提示词；这里是「满足条件就换一份」的规则表，两者共用回落落点。
 * 规则有序、首条命中即止，所以 PUT 整份替换（规则 + fallback + revision）。
 *
 * 条件的完整结构（field/op/values[]）较复杂，界面把每条规则的条件只读摊开，
 * 可编辑名称/启用/动作模式/正文，fallback 全字段可编辑。需要新增复杂条件的场景
 * 由配置端/文档负责，避免移动端手搓条件树出错。
 */
@Composable
fun PromptRulesPage(onBack: () -> Unit) {
    val C = agateColors()
    val scope = rememberCoroutineScope()

    var view by remember { mutableStateOf<PromptRulesOps.View?>(null) }
    var rules by remember { mutableStateOf<List<PromptRulesOps.Rule>>(emptyList()) }
    var fallback by remember { mutableStateOf<PromptRulesOps.Fallback?>(null) }
    var dirty by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var notice by remember { mutableStateOf<String?>(null) }

    fun adopt(v: PromptRulesOps.View) {
        view = v; rules = v.rules.map { it.rule }; fallback = v.fallback; dirty = false
    }
    fun reload() {
        scope.launch {
            val r = PromptRulesOps.load()
            if (r.ok && r.data != null) { adopt(r.data!!); error = null }
            else error = r.error ?: "读取规则失败"
        }
    }
    LaunchedEffect(Unit) { reload() }

    fun save() {
        val v = view ?: return
        val fb = fallback ?: return
        busy = true; error = null; notice = null
        scope.launch {
            val r = PromptRulesOps.save(rules, fb, v.revision)
            if (r.ok && r.data != null) { adopt(r.data!!); notice = "已保存" }
            else error = r.error ?: "保存失败（规则可能被别处改过）"
            busy = false
        }
    }

    Column(Modifier.fillMaxSize().background(C.bg)) {
        AgateSubPageHeader("提示词规则", onBack = onBack) {
            view?.let { AgateBadge("${it.rules.size} 条", BadgeTone.BLUE) }
        }
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 16.dp)) {
            Spacer(Modifier.height(10.dp))
            Text("满足条件就换一份提示词；从上到下匹配，首条命中即止。旧「提示词」组管的是全局单份。",
                style = AgateType.Caption, color = C.textSub)
            Spacer(Modifier.height(10.dp))
            if (error != null) { PrBanner(error!!, C.red, C.redBg); Spacer(Modifier.height(8.dp)) }
            if (notice != null) { PrBanner(notice!!, C.green, C.greenBg); Spacer(Modifier.height(8.dp)) }

            if (view == null) {
                PrCard { Text("读取中…", style = AgateType.BodyS, color = C.textDim) }
            } else {
                view!!.error.takeIf { it.isNotBlank() }?.let {
                    Spacer(Modifier.height(6.dp)); PrBanner(it, C.amber, C.amberBg)
                }
                Text("规则（${rules.size}）", style = AgateType.SectionTitle, color = C.textMain, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.height(8.dp))
                if (rules.isEmpty()) {
                    PrCard { Text("没有规则 —— 全部走回落提示词。", style = AgateType.BodyS, color = C.textDim) }
                } else {
                    rules.forEachIndexed { index, rule ->
                        val rv = view!!.rules.getOrNull(index)
                        PrCard { PrRuleEditor(rule, rv, index, C) { next ->
                            rules = rules.toMutableList().also { it[index] = next }.toList(); dirty = true
                        } }
                        Spacer(Modifier.height(10.dp))
                    }
                }

                fallback?.let { fb ->
                    Text("回落项（都没命中时用）", style = AgateType.SectionTitle, color = C.textMain, fontWeight = FontWeight.SemiBold)
                    Spacer(Modifier.height(8.dp))
                    PrCard {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Column(Modifier.weight(1f)) {
                                Text("模式", style = AgateType.BodyS, color = C.textMain)
                                Text("passthrough / replace / append", style = AgateType.Caption, color = C.textDim)
                            }
                        }
                        Spacer(Modifier.height(4.dp))
                        PrTextField(fb.mode, "passthrough") { fallback = copyFb(fb, mode = it); dirty = true }
                        Spacer(Modifier.height(8.dp))
                        Text("正文（inline 原文，留空 = 用内置默认）", style = AgateType.Caption, color = C.textDim)
                        Spacer(Modifier.height(4.dp))
                        PrTextField(fb.text, "提示词正文", singleLine = false, minLines = 3) { fallback = copyFb(fb, text = it); dirty = true }
                    }
                    Spacer(Modifier.height(16.dp))
                }

                PrBtn(if (busy) "保存中…" else "保存整份", primary = true, enabled = !busy && dirty) { save() }
                if (!dirty && notice == null) {
                    Spacer(Modifier.height(6.dp))
                    Text("改动任一字段后保存才可用。", style = AgateType.Caption, color = C.textDim)
                }
            }
            Spacer(Modifier.height(24.dp))
        }
    }
}

@Composable
private fun PrRuleEditor(rule: PromptRulesOps.Rule, rv: PromptRulesOps.RuleView?, index: Int,
                         C: AgateColors, onEdit: (PromptRulesOps.Rule) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    Row(Modifier.fillMaxWidth().clickable { expanded = !expanded }, verticalAlignment = Alignment.CenterVertically) {
        Text("${index + 1}. ${rule.name.ifBlank { rule.id }}", style = AgateType.BodyS, color = C.textMain,
            fontWeight = FontWeight.Medium, modifier = Modifier.weight(1f))
        if (rule.enabled) AgateBadge("启用", BadgeTone.GREEN) else AgateBadge("停用", BadgeTone.GRAY)
        Text(if (expanded) "▴" else "▾", color = C.textDim, modifier = Modifier.padding(start = 6.dp))
    }
    Spacer(Modifier.height(3.dp))
    val cond = rule.conditions.joinToString(" · ") { c -> "${c.field} ${c.op} ${c.values.joinToString(",")}" }
    Text(cond.ifBlank { "无条件（始终命中）" }, style = AgateType.Caption, color = C.textDim)
    rv?.takeIf { it.source.isNotBlank() }?.let {
        Spacer(Modifier.height(2.dp))
        Text("正文来源：${it.source} · ${it.lines} 行${it.fileError.takeIf { e -> e.isNotBlank() }?.let { " · $it" } ?: ""}",
            style = AgateType.Caption, color = if (it.fileError.isNotBlank()) C.red else C.textDim)
    }
    if (expanded) {
        Spacer(Modifier.height(10.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) { Text("启用", style = AgateType.BodyS, color = C.textMain) }
            Switch(checked = rule.enabled, onCheckedChange = { onEdit(copyRule(rule, enabled = it)) })
        }
        Spacer(Modifier.height(8.dp))
        Text("规则名", style = AgateType.Caption, color = C.textDim)
        PrTextField(rule.name, "规则名") { onEdit(copyRule(rule, name = it)) }
        Spacer(Modifier.height(8.dp))
        Text("动作模式（passthrough / replace / append）", style = AgateType.Caption, color = C.textDim)
        PrTextField(rule.mode, "passthrough") { onEdit(copyRule(rule, mode = it)) }
        Spacer(Modifier.height(8.dp))
        Text("提示词正文", style = AgateType.Caption, color = C.textDim)
        PrTextField(rule.text, "正文", singleLine = false, minLines = 3) { onEdit(copyRule(rule, text = it)) }
    }
}

private fun copyRule(r: PromptRulesOps.Rule,
                     id: String = r.id, name: String = r.name, enabled: Boolean = r.enabled,
                     logic: String = r.logic, conditions: List<PromptRulesOps.Condition> = r.conditions,
                     mode: String = r.mode, file: String = r.file, text: String = r.text) =
    PromptRulesOps.Rule(id, name, enabled, logic, conditions, mode, file, text)

private fun copyFb(f: PromptRulesOps.Fallback, mode: String = f.mode, file: String = f.file, text: String = f.text) =
    PromptRulesOps.Fallback(mode, file, text)

@Composable
private fun PrTextField(value: String, hint: String, singleLine: Boolean = true, minLines: Int = 1, onChange: (String) -> Unit) {
    val C = agateColors()
    BasicTextField(value = value, onValueChange = onChange, singleLine = singleLine, minLines = minLines,
        textStyle = TextStyle(color = C.textMain, fontSize = 13.sp), cursorBrush = SolidColor(C.accent),
        decorationBox = { inner ->
            Box(Modifier.fillMaxWidth().background(C.insetBg, RoundedCornerShape(9.dp))
                .border(1.dp, C.border, RoundedCornerShape(9.dp)).padding(horizontal = 10.dp, vertical = 8.dp)) {
                if (value.isEmpty()) Text(hint, style = AgateType.Caption, color = C.textDim)
                inner()
            }
        })
}

@Composable
private fun PrCard(content: @Composable ColumnScope.() -> Unit) {
    val C = agateColors()
    Column(Modifier.fillMaxWidth().agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
        .border(1.dp, C.border, RoundedCornerShape(20.dp)).padding(horizontal = 14.dp, vertical = 13.dp), content = content)
}

@Composable
private fun PrBanner(text: String, fg: Color, bg: Color) {
    Row(Modifier.fillMaxWidth().background(bg, RoundedCornerShape(10.dp)).padding(horizontal = 12.dp, vertical = 8.dp)) {
        Text(text, style = AgateType.Caption, color = fg)
    }
}

@Composable
private fun PrBtn(text: String, primary: Boolean, enabled: Boolean, onClick: () -> Unit) {
    val C = agateColors()
    Box(Modifier.fillMaxWidth()
        .background(if (!enabled) C.btnBg.copy(alpha = 0.5f) else if (primary) C.accent else C.btnBg, RoundedCornerShape(11.dp))
        .then(if (enabled) Modifier.clickable(onClick = onClick) else Modifier)
        .padding(vertical = 12.dp), contentAlignment = Alignment.Center) {
        Text(text, style = AgateType.BodyS, fontWeight = FontWeight.SemiBold,
            color = if (!enabled) C.textDim else if (primary) C.onAccent else C.textMain)
    }
}
