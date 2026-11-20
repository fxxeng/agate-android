package dev.agate.android.ui.settings

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
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
import dev.agate.android.data.ClientIdentificationOps
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateSubPageHeader
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateColors
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch

/**
 * 客户端识别（显示识别，不是行为识别）。
 *
 * ── 最重要的一条分界 ────────────────────────────────────────
 * 这里的规则只影响「统计列归类」与「提示词规则匹配」。协议兼容（Codex 推理折叠等）
 * 走内核 classify 的固定表，**不读这里任何一项**。改名字不会改变请求形状 ——
 * 所以界面明确提示，并提供 preview 并排显示 strict（行为真值）与 value（显示值），
 * 两者不一致是设计常态。
 *
 * 配置整份替换 + revision。规则有序，首条命中即止。
 */
@Composable
fun ClientIdentificationPage(onBack: () -> Unit) {
    val C = agateColors()
    val scope = rememberCoroutineScope()

    var view by remember { mutableStateOf<ClientIdentificationOps.View?>(null) }
    var accept by remember { mutableStateOf(false) }
    var rules by remember { mutableStateOf<List<ClientIdentificationOps.ClientRule>>(emptyList()) }
    var dirty by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var notice by remember { mutableStateOf<String?>(null) }

    // preview 输入
    var previewUa by remember { mutableStateOf("") }
    var previewHeader by remember { mutableStateOf("") }
    var previewResult by remember { mutableStateOf<ClientIdentificationOps.Preview?>(null) }
    var previewing by remember { mutableStateOf(false) }

    fun adopt(v: ClientIdentificationOps.View) {
        view = v; accept = v.config.acceptDeclarations; rules = v.config.rules; dirty = false
    }

    fun reload() {
        scope.launch {
            val r = ClientIdentificationOps.load()
            if (r.ok && r.data != null) { adopt(r.data!!); error = null }
            else error = r.error ?: "读取配置失败"
        }
    }

    LaunchedEffect(Unit) { reload() }

    fun save() {
        val v = view ?: return
        busy = true; error = null; notice = null
        val cfg = ClientIdentificationOps.Config(accept, v.config.clients, rules)
        scope.launch {
            val r = ClientIdentificationOps.save(cfg, v.revision)
            if (r.ok && r.data != null) { adopt(r.data!!); notice = "已保存" }
            else error = r.error ?: "保存失败（配置可能被别处改过）"
            busy = false
        }
    }

    fun preview() {
        val v = view ?: return
        previewing = true; error = null
        val headers = if (previewHeader.isBlank()) emptyList() else previewHeader.lines().mapNotNull { line ->
            val p = line.split(":", limit = 2)
            if (p.size == 2) p[0].trim() to p[1].trim() else null
        }
        val cfg = ClientIdentificationOps.Config(accept, v.config.clients, rules)
        scope.launch {
            val r = ClientIdentificationOps.preview(cfg, previewUa, headers)
            if (r.ok && r.data != null) previewResult = r.data
            else error = r.error ?: "预览失败"
            previewing = false
        }
    }

    Column(Modifier.fillMaxSize().background(C.bg)) {
        AgateSubPageHeader("客户端识别", onBack = onBack) {
            view?.let { AgateBadge("${it.config.rules.size} 条规则", BadgeTone.BLUE) }
        }
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 16.dp)) {
            Spacer(Modifier.height(10.dp))
            Text(
                "按请求头把客户端归类到统计列，并驱动提示词规则匹配。改这里不改变发给上游的协议形状。",
                style = AgateType.Caption, color = C.textSub,
            )
            Spacer(Modifier.height(10.dp))
            if (error != null) { CiBanner(error!!, C.red, C.redBg); Spacer(Modifier.height(8.dp)) }
            if (notice != null) { CiBanner(notice!!, C.green, C.greenBg); Spacer(Modifier.height(8.dp)) }

            if (view == null) {
                CiCard { Text("读取中…", style = AgateType.BodyS, color = C.textDim) }
            } else {
                CiCard {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text("接受客户端声明头", style = AgateType.BodyS, color = C.textMain)
                            Text("允许用 ${view!!.declarationHeader.ifBlank { "客户端声明头" }} 参与识别", style = AgateType.Caption, color = C.textDim)
                        }
                        Switch(checked = accept, enabled = !busy, onCheckedChange = { accept = it; dirty = true })
                    }
                }
                Spacer(Modifier.height(14.dp))
                Text("识别规则（${rules.size} · 首条命中即止）", style = AgateType.SectionTitle,
                    color = C.textMain, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.height(8.dp))
                if (rules.isEmpty()) {
                    CiCard { Text("没有自定义规则 —— 使用内置客户端目录识别。", style = AgateType.BodyS, color = C.textDim) }
                } else {
                    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                        rules.forEachIndexed { index, rule ->
                            CiCard {
                                CiRuleEditor(rule, index, C) { next ->
                                    rules = rules.toMutableList().also { it[index] = next }.toList(); dirty = true
                                }
                            }
                        }
                    }
                }

                Spacer(Modifier.height(16.dp))
                CiBtn(if (busy) "保存中…" else "保存整份", primary = true, enabled = !busy && dirty) { save() }
                Spacer(Modifier.height(20.dp))

                Text("预览识别结果", style = AgateType.SectionTitle, color = C.textMain, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.height(8.dp))
                CiCard {
                    Text("User-Agent", style = AgateType.Caption, color = C.textDim)
                    CiTextField(previewUa, "粘贴一个客户端 UA") { previewUa = it }
                    Spacer(Modifier.height(8.dp))
                    Text("附加请求头（每行 Name: Value，可留空）", style = AgateType.Caption, color = C.textDim)
                    CiTextField(previewHeader, "例如 X-Client-Product: codex", singleLine = false, minLines = 2) { previewHeader = it }
                    Spacer(Modifier.height(10.dp))
                    CiBtn(if (previewing) "识别中…" else "预览", primary = false, enabled = !previewing) { preview() }
                    previewResult?.let { p ->
                        Spacer(Modifier.height(12.dp))
                        CiDivider(C)
                        Spacer(Modifier.height(10.dp))
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text("协议行为（strict）", style = AgateType.Caption, color = C.textDim, modifier = Modifier.weight(1f))
                            AgateBadge(p.strict.ifBlank { "unknown" }, BadgeTone.AMBER)
                        }
                        Spacer(Modifier.height(6.dp))
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text("显示归类（value）", style = AgateType.Caption, color = C.textDim, modifier = Modifier.weight(1f))
                            AgateBadge(p.label.ifBlank { p.value }, BadgeTone.BLUE)
                        }
                        if (p.strict.isNotBlank() && p.value.isNotBlank() && p.strict != p.value) {
                            Spacer(Modifier.height(8.dp))
                            Text("两者不一致是设计常态：strict 决定协议兼容，value 只影响统计与提示词匹配。",
                                style = AgateType.Caption, color = C.amber)
                        }
                    }
                }
            }
            Spacer(Modifier.height(24.dp))
        }
    }
}

@Composable
private fun CiRuleEditor(rule: ClientIdentificationOps.ClientRule, index: Int, C: AgateColors,
                         onEdit: (ClientIdentificationOps.ClientRule) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    Row(Modifier.fillMaxWidth().clickable { expanded = !expanded }, verticalAlignment = Alignment.CenterVertically) {
        Text("${index + 1}. ${rule.id}", style = AgateType.BodyS, color = C.textMain, fontWeight = FontWeight.Medium,
            modifier = Modifier.weight(1f))
        if (rule.enabled) AgateBadge("启用", BadgeTone.GREEN) else AgateBadge("停用", BadgeTone.GRAY)
        Text(if (expanded) "▴" else "▾", color = C.textDim, modifier = Modifier.padding(start = 6.dp))
    }
    if (expanded) {
        Spacer(Modifier.height(8.dp)); CiDivider(C); Spacer(Modifier.height(8.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) { Text("启用", style = AgateType.BodyS, color = C.textMain) }
            Switch(checked = rule.enabled, onCheckedChange = { onEdit(copyRule(rule, enabled = it)) })
        }
        Spacer(Modifier.height(8.dp))
        CiField("匹配头", rule.header) { onEdit(copyRule(rule, header = it)) }
        Spacer(Modifier.height(6.dp))
        CiField("匹配方式（product / contains / exact）", rule.matchType) { onEdit(copyRule(rule, matchType = it)) }
        Spacer(Modifier.height(6.dp))
        CiField("匹配值", rule.pattern) { onEdit(copyRule(rule, pattern = it)) }
        Spacer(Modifier.height(6.dp))
        CiField("归入客户端 id", rule.clientId) { onEdit(copyRule(rule, clientId = it)) }
    }
}

private fun copyRule(r: ClientIdentificationOps.ClientRule,
                     id: String = r.id, enabled: Boolean = r.enabled, header: String = r.header,
                     matchType: String = r.matchType, pattern: String = r.pattern, clientId: String = r.clientId) =
    ClientIdentificationOps.ClientRule(id, enabled, header, matchType, pattern, clientId)

@Composable
private fun CiField(label: String, value: String, onChange: (String) -> Unit) {
    val C = agateColors()
    Text(label, style = AgateType.Caption, color = C.textDim)
    Spacer(Modifier.height(3.dp))
    CiTextField(value, label) { onChange(it) }
}

@Composable
private fun CiTextField(value: String, hint: String, singleLine: Boolean = true, minLines: Int = 1, onChange: (String) -> Unit) {
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
private fun CiCard(content: @Composable ColumnScope.() -> Unit) {
    val C = agateColors()
    Column(Modifier.fillMaxWidth().agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
        .border(1.dp, C.border, RoundedCornerShape(20.dp)).padding(horizontal = 14.dp, vertical = 13.dp), content = content)
}

@Composable
private fun CiDivider(C: AgateColors) = Box(Modifier.fillMaxWidth().height(1.dp).background(C.border.copy(alpha = 0.55f)))

@Composable
private fun CiBanner(text: String, fg: Color, bg: Color) {
    Row(Modifier.fillMaxWidth().background(bg, RoundedCornerShape(10.dp)).padding(horizontal = 12.dp, vertical = 8.dp)) {
        Text(text, style = AgateType.Caption, color = fg)
    }
}

@Composable
private fun CiBtn(text: String, primary: Boolean, enabled: Boolean, onClick: () -> Unit) {
    val C = agateColors()
    Box(Modifier.fillMaxWidth()
        .background(if (!enabled) C.btnBg.copy(alpha = 0.5f) else if (primary) C.accent else C.btnBg, RoundedCornerShape(11.dp))
        .then(if (enabled) Modifier.clickable(onClick = onClick) else Modifier)
        .padding(vertical = 12.dp), contentAlignment = Alignment.Center) {
        Text(text, style = AgateType.BodyS, fontWeight = FontWeight.SemiBold,
            color = if (!enabled) C.textDim else if (primary) C.onAccent else C.textMain)
    }
}
