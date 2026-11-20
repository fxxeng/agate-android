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
import androidx.compose.foundation.layout.width
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
import dev.agate.android.data.VisionCompatOps
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.AgateSubPageHeader
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateColors
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch

/**
 * 图片兼容：发图给看不了图的模型时怎么降级。
 *
 * ── assist vs reroute ────────────────────────────────────────
 * - assist：另调一只视觉模型把图读成文字，替换后交回原模型（原模型的回答风格保留）；
 * - reroute：整单改交给视觉模型（原模型完全不参与）。
 *
 * 配置是整份有序规则表 + revision 防并发覆盖：首条命中即止，所以一次 PUT 全量替换，
 * 不能逐条改。界面把每条规则的核心字段摊开可编辑，改完点「保存整份」一次性提交。
 * 模型引用 `provider/model` 用文本输入（家 id + 上游模型 id），与内核 ModelRef 对齐。
 */
@Composable
fun VisionCompatPage(onBack: () -> Unit) {
    val C = agateColors()
    val scope = rememberCoroutineScope()

    var view by remember { mutableStateOf<VisionCompatOps.View?>(null) }
    var enabled by remember { mutableStateOf(true) }
    var rules by remember { mutableStateOf<List<VisionCompatOps.Rule>>(emptyList()) }
    var timeout by remember { mutableStateOf(30L) }
    var unknown by remember { mutableStateOf("try_original") }
    var dirty by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var notice by remember { mutableStateOf<String?>(null) }

    fun adopt(v: VisionCompatOps.View) {
        view = v
        enabled = v.config.enabled
        rules = v.config.rules
        timeout = v.config.timeoutSeconds
        unknown = v.config.unknownCapability
        dirty = false
    }

    fun reload() {
        scope.launch {
            val r = VisionCompatOps.load()
            if (r.ok && r.data != null) {
                adopt(r.data!!); error = null
            } else {
                error = r.error ?: "读取配置失败"
            }
        }
    }

    LaunchedEffect(Unit) { reload() }

    fun save() {
        val v = view ?: return
        busy = true
        error = null; notice = null
        val cfg = VisionCompatOps.Config(enabled, rules, unknown, timeout)
        scope.launch {
            val r = VisionCompatOps.save(cfg, v.revision)
            if (r.ok && r.data != null) {
                adopt(r.data!!)
                notice = "已保存（改完对下一个请求立即生效）"
            } else {
                error = r.error ?: "保存失败（配置可能被别处改过，请重进页面）"
            }
            busy = false
        }
    }

    Column(Modifier.fillMaxSize().background(C.bg)) {
        AgateSubPageHeader("图片兼容", onBack = onBack) {
            view?.let { AgateBadge(if (it.config.enabled) "已启用" else "已关闭",
                if (it.config.enabled) BadgeTone.GREEN else BadgeTone.GRAY) }
        }
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 16.dp)) {
            Spacer(Modifier.height(10.dp))
            Text(
                "把图片发给看不了图的模型时的降级规则。规则按从上到下匹配，首条命中即止；" +
                    "改动是整份替换，点「保存整份」才生效。",
                style = AgateType.Caption, color = C.textSub,
            )
            Spacer(Modifier.height(10.dp))

            if (error != null) { VcBanner(error!!, C.red, C.redBg); Spacer(Modifier.height(8.dp)) }
            if (notice != null) { VcBanner(notice!!, C.green, C.greenBg); Spacer(Modifier.height(8.dp)) }

            if (view == null) {
                VcCard { Text("读取中…", style = AgateType.BodyS, color = C.textDim) }
            } else {
                VcCard {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text("总开关", style = AgateType.BodyS, color = C.textMain)
                            Text("关掉 = 不做任何兼容，图片照原样发给模型", style = AgateType.Caption, color = C.textDim)
                        }
                        Switch(checked = enabled, enabled = !busy, onCheckedChange = { enabled = it; dirty = true })
                    }
                    Spacer(Modifier.height(8.dp))
                    VcDivider(C)
                    Spacer(Modifier.height(8.dp))
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text("未知能力时", style = AgateType.BodyS, color = C.textMain)
                            Text("模型没声明能否看图时怎么办", style = AgateType.Caption, color = C.textDim)
                        }
                    }
                    Spacer(Modifier.height(6.dp))
                    val unknownOpts = listOf("try_original" to "先照原样发", "compat" to "按兼容处理")
                    val idx = unknownOpts.indexOfFirst { it.first == unknown }.coerceAtLeast(0)
                    AgateSegmented(unknownOpts.map { it.second }, idx, onSelect = {
                        unknown = unknownOpts[it].first; dirty = true
                    })
                    Spacer(Modifier.height(10.dp))
                    VcDivider(C)
                    Spacer(Modifier.height(8.dp))
                    VcNumberRow("识读超时（秒）", "另调视觉模型时的等待上限", timeout, view!!.limits.maxTimeoutSeconds) {
                        timeout = it; dirty = true
                    }
                }

                Spacer(Modifier.height(14.dp))
                Text("规则（${rules.size}）", style = AgateType.SectionTitle, color = C.textMain, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.height(8.dp))
                if (rules.isEmpty()) {
                    VcCard { Text("还没有规则。兼容不会触发 —— 先在桌面/配置端添加规则，或告诉我你要的降级场景。",
                        style = AgateType.BodyS, color = C.textDim) }
                } else {
                    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                        rules.forEachIndexed { index, rule ->
                            VcCard {
                                RuleEditor(
                                    rule = rule,
                                    index = index,
                                    C = C,
                                    onEdit = { next -> rules = rules.toMutableList().also { it[index] = next }.toList(); dirty = true },
                                    onMoveUp = if (index > 0) { {
                                        rules = rules.toMutableList().also { java.util.Collections.swap(it, index, index - 1) }.toList(); dirty = true
                                    } } else null,
                                )
                            }
                        }
                    }
                }

                Spacer(Modifier.height(16.dp))
                VcBtn(if (busy) "保存中…" else "保存整份", primary = true, enabled = !busy && dirty) { save() }
                if (!dirty && notice == null) {
                    Spacer(Modifier.height(6.dp))
                    Text("改动任一字段后这里会亮起；未改动时保存是禁用的。", style = AgateType.Caption, color = C.textDim)
                }
            }
            Spacer(Modifier.height(24.dp))
        }
    }
}

@Composable
private fun RuleEditor(
    rule: VisionCompatOps.Rule,
    index: Int,
    C: AgateColors,
    onEdit: (VisionCompatOps.Rule) -> Unit,
    onMoveUp: (() -> Unit)?,
) {
    var expanded by remember { mutableStateOf(false) }
    Row(Modifier.fillMaxWidth().clickable { expanded = !expanded }, verticalAlignment = Alignment.CenterVertically) {
        Text("${index + 1}. ${rule.name.ifBlank { rule.id }}", style = AgateType.BodyS, color = C.textMain, fontWeight = FontWeight.Medium,
            modifier = Modifier.weight(1f))
        AgateBadge(if (rule.mode == "reroute") "reroute" else "assist",
            if (rule.mode == "reroute") BadgeTone.AMBER else BadgeTone.BLUE)
        if (rule.enabled) AgateBadge("启用", BadgeTone.GREEN) else AgateBadge("停用", BadgeTone.GRAY)
        Text(if (expanded) "▴" else "▾", color = C.textDim, modifier = Modifier.padding(start = 6.dp))
    }
    if (expanded) {
        Spacer(Modifier.height(8.dp))
        VcDivider(C)
        Spacer(Modifier.height(8.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("启用此规则", style = AgateType.BodyS, color = C.textMain)
                Text("停用后这条不参与匹配", style = AgateType.Caption, color = C.textDim)
            }
            Switch(checked = rule.enabled, onCheckedChange = {
                onEdit(rule.copy(enabled = it))
            })
        }
        Spacer(Modifier.height(8.dp))
        Text("模式", style = AgateType.Caption, color = C.textDim)
        Spacer(Modifier.height(4.dp))
        val modes = listOf("assist" to "识读替换", "reroute" to "整体改交")
        AgateSegmented(modes.map { it.second }, modes.indexOfFirst { it.first == rule.mode }.coerceAtLeast(0), onSelect = {
            onEdit(rule.copy(mode = modes[it].first))
        })
        Spacer(Modifier.height(10.dp))
        Text("目标视觉模型（provider/model，可留空用规则自带目标）", style = AgateType.Caption, color = C.textDim)
        Spacer(Modifier.height(4.dp))
        val targetDraft = rule.target?.let { "${it.provider}/${it.model}" } ?: ""
        VcTextField(targetDraft, "例如 openai/gpt-4o") { text ->
            val parts = text.split("/", limit = 2)
            val t = if (parts.size == 2 && parts[0].isNotBlank() && parts[1].isNotBlank()) {
                VisionCompatOps.VisionTarget(parts[0], parts[1], rule.target?.reasoning ?: "")
            } else null
            onEdit(rule.copy(target = t))
        }
        Spacer(Modifier.height(8.dp))
        Text("匹配模型（每行一个 provider/model，留空 = 规则里已定义的全部）",
            style = AgateType.Caption, color = C.textDim)
        Spacer(Modifier.height(4.dp))
        VcTextField(rule.models.joinToString("\n") { "${it.provider}/${it.model}" }, "provider/model", singleLine = false, minLines = 2) { text ->
            val parsed = text.lines().mapNotNull { line ->
                val p = line.trim().split("/", limit = 2)
                if (p.size == 2 && p[0].isNotBlank() && p[1].isNotBlank()) VisionCompatOps.ModelRef(p[0], p[1]) else null
            }
            onEdit(rule.copy(models = parsed))
        }
        Spacer(Modifier.height(8.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("复用识读结果", style = AgateType.BodyS, color = C.textMain)
                Text("同一张图短时间内不重复调视觉模型", style = AgateType.Caption, color = C.textDim)
            }
            Switch(checked = rule.reuseResults, onCheckedChange = { onEdit(rule.copy(reuseResults = it)) })
        }
        if (onMoveUp != null) {
            Spacer(Modifier.height(8.dp))
            VcBtn("上移（提高优先级）", primary = false, enabled = true, onClick = onMoveUp)
        }
    }
}

/** 用构造参数逐项复制 Rule（普通 class 没有 copy）。 */
private fun VisionCompatOps.Rule.copy(
    id: String = this.id,
    name: String = this.name,
    enabled: Boolean = this.enabled,
    mode: String = this.mode,
    models: List<VisionCompatOps.ModelRef> = this.models,
    target: VisionCompatOps.VisionTarget? = this.target,
    reuseResults: Boolean = this.reuseResults,
    preferCurrentRoute: Boolean = this.preferCurrentRoute,
    fallback: String = this.fallback,
    targets: List<VisionCompatOps.VisionTarget> = this.targets,
) = VisionCompatOps.Rule(id, name, enabled, mode, models, target, reuseResults, preferCurrentRoute, fallback, targets)

@Composable
private fun VcNumberRow(label: String, desc: String, value: Long, max: Long, onChange: (Long) -> Unit) {
    val C = agateColors()
    var draft by remember(value) { mutableStateOf(value.toString()) }
    Row(verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(label, style = AgateType.BodyS, color = C.textMain)
            Text(desc, style = AgateType.Caption, color = C.textDim)
        }
        BasicTextField(value = draft, onValueChange = { d ->
            draft = d.filter { it.isDigit() }.take(4)
            d.filter { it.isDigit() }.toLongOrNull()?.let { onChange(it.coerceIn(1, max)) }
        }, singleLine = true, textStyle = TextStyle(color = C.accent, fontSize = 14.sp),
            cursorBrush = SolidColor(C.accent),
            modifier = Modifier.background(C.insetBg, RoundedCornerShape(9.dp)).padding(horizontal = 10.dp, vertical = 6.dp))
    }
}

@Composable
private fun VcTextField(value: String, hint: String, singleLine: Boolean = true, minLines: Int = 1, onChange: (String) -> Unit) {
    val C = agateColors()
    BasicTextField(value = value, onValueChange = onChange, singleLine = singleLine, minLines = minLines,
        textStyle = TextStyle(color = C.textMain, fontSize = 13.sp), cursorBrush = SolidColor(C.accent),
        decorationBox = { inner ->
            Box(Modifier.background(C.insetBg, RoundedCornerShape(9.dp)).border(1.dp, C.border, RoundedCornerShape(9.dp))
                .padding(horizontal = 10.dp, vertical = 8.dp)) {
                if (value.isEmpty()) Text(hint, style = AgateType.Caption, color = C.textDim)
                inner()
            }
        }, modifier = Modifier.fillMaxWidth())
}

@Composable
private fun VcCard(content: @Composable ColumnScope.() -> Unit) {
    val C = agateColors()
    Column(Modifier.fillMaxWidth().agateCard().background(C.cardBg, RoundedCornerShape(20.dp))
        .border(1.dp, C.border, RoundedCornerShape(20.dp)).padding(horizontal = 14.dp, vertical = 13.dp), content = content)
}

@Composable
private fun VcDivider(C: AgateColors) = Box(Modifier.fillMaxWidth().height(1.dp).background(C.border.copy(alpha = 0.55f)))

@Composable
private fun VcBanner(text: String, fg: Color, bg: Color) {
    Row(Modifier.fillMaxWidth().background(bg, RoundedCornerShape(10.dp)).padding(horizontal = 12.dp, vertical = 8.dp)) {
        Text(text, style = AgateType.Caption, color = fg)
    }
}

@Composable
private fun VcBtn(text: String, primary: Boolean, enabled: Boolean, onClick: () -> Unit) {
    val C = agateColors()
    Box(Modifier.fillMaxWidth()
        .background(if (!enabled) C.btnBg.copy(alpha = 0.5f) else if (primary) C.accent else C.btnBg, RoundedCornerShape(11.dp))
        .then(if (enabled) Modifier.clickable(onClick = onClick) else Modifier)
        .padding(vertical = 12.dp), contentAlignment = Alignment.Center) {
        Text(text, style = AgateType.BodyS, fontWeight = FontWeight.SemiBold,
            color = if (!enabled) C.textDim else if (primary) C.onAccent else C.textMain)
    }
}
