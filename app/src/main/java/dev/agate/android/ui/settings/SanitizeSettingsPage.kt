package dev.agate.android.ui.settings

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
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Icon
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
import androidx.compose.ui.unit.dp
import dev.agate.android.data.SettingsOps
import dev.agate.android.ui.components.AgateAccordion
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateFormInput
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch

/**
 * 设置页 · 脱敏分组：内置规则集开关 + 零宽空格词表。
 *
 * ── 为什么这两项在同一组 ────────────────────────────────────
 * 它们是**两层**脱敏，改的是同一件事（发给上游的请求体），只是手法不同：
 * 内置规则集处理「已知的具体指纹」（表头键值、几句模板、错误码），封闭、
 * 随版本走；零宽空格词表处理「用户自己知道的词」，开放、随用户走。
 * 两层在核心里先后执行、开关各自独立，所以并排放，读数也并排。
 *
 * ── 为什么词表用「加一条 / 删一条」而接口是整份 PUT ──────────
 * 界面上一次只改一条是自然的（用户心里想的是「我要加这个词」），而落盘是
 * 整份替换 —— 两者之间由本页持有当前列表、每次改动整份提交。这样既避免
 * 了逐词接口那四种操作各自的冲突语义，界面行为也不受影响。
 */
@Composable
fun SanitizeSettingsPage(
    onOpenDrawer: () -> Unit = {},
    onBack: () -> Unit = {},
) {
    val C = agateColors()
    val scope = rememberCoroutineScope()

    var loading by remember { mutableStateOf(true) }
    var sanitizeOn by remember { mutableStateOf(true) }
    var words by remember { mutableStateOf(listOf<String>()) }
    var draft by remember { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }
    var notice by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }

    // 首次进入读一次真值（内核是唯一真源，不在本地留一份默认值当缓存）
    LaunchedEffect(Unit) {
        loading = true
        error = null
        val guards = SettingsOps.guards()
        if (guards.ok) {
            guards.data?.let { sanitizeOn = it.sanitize }
        }
        val list = SettingsOps.zwspWords()
        if (list.ok) {
            words = list.data.orEmpty()
        } else {
            error = list.error
        }
        loading = false
    }

    fun submit(next: List<String>, onOk: () -> Unit) {
        if (busy) return
        busy = true
        error = null
        notice = null
        scope.launch {
            val result = SettingsOps.saveZwspWords(next)
            if (result.ok) {
                words = next
                onOk()
            } else {
                error = result.error
            }
            busy = false
        }
    }

    Column(modifier = Modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "设置", onOpenDrawer = onOpenDrawer, onBack = onBack)
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 14.dp)
                .padding(top = 12.dp)
                .navigationBarsPadding(),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            // ── 内置规则集 ──────────────────────────────────
            AgateAccordion(title = "指纹脱敏（内置规则集）", initiallyExpanded = true) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text("剥离上游审核黑名单指纹", style = AgateType.Body, color = C.textMain)
                        Spacer(Modifier.height(4.dp))
                        Text(
                            "表头键值整段删除、承载语义的模板句最小改写（每句只换一个词）。" +
                                "关掉它等于把客户端 system 模板原样发给上游 —— 那正是模板句被误拦的原因。",
                            style = AgateType.Caption,
                            color = C.textSub,
                        )
                    }
                    Spacer(Modifier.width(12.dp))
                    Switch(
                        checked = sanitizeOn,
                        onCheckedChange = { want ->
                            val before = sanitizeOn
                            sanitizeOn = want
                            scope.launch {
                                val result = SettingsOps.saveSanitize(want)
                                if (!result.ok) {
                                    sanitizeOn = before // 保存失败要弹回去，不能让人以为生效了
                                    error = result.error
                                }
                            }
                        },
                    )
                }
            }

            // ── 零宽空格词表 ────────────────────────────────
            AgateAccordion(
                title = "零宽空格脱敏（自定义词表）",
                initiallyExpanded = true,
                badge = {
                    AgateBadge(
                        text = if (words.isEmpty()) "未启用" else "${words.size} 个词",
                        tone = if (words.isEmpty()) BadgeTone.GRAY else BadgeTone.BLUE,
                    )
                },
            ) {
                Text(
                    "这里填「我知道会被拦、但内置规则里没有」的词。命中的每一处都会在词尾插入一个" +
                        "零宽空格（U+200B）：人眼看着是同一个词，而上游的逐字精确匹配对不上了。",
                    style = AgateType.Caption,
                    color = C.textSub,
                )
                Spacer(Modifier.height(10.dp))
                Text(
                    "与上面那一层的区别：那一层是封闭的、随版本走；这一层由你维护，改完下一个请求就生效。",
                    style = AgateType.Caption,
                    color = C.textDim,
                )

                Spacer(Modifier.height(14.dp))

                // 加词
                Row(verticalAlignment = Alignment.CenterVertically) {
                    AgateFormInput(
                        value = draft,
                        onValueChange = { draft = it; error = null },
                        hint = "要打断的词（最多 ${SettingsOps.ZWSP_MAX_WORD_CHARS} 字）",
                        modifier = Modifier.weight(1f),
                    )
                    Spacer(Modifier.width(8.dp))
                    val check = SettingsOps.validateZwspWord(draft)
                    val canAdd = draft.isNotBlank() && check.isSuccess &&
                        words.size < SettingsOps.ZWSP_MAX_WORDS && !busy &&
                        !words.contains(check.getOrNull())
                    Box(
                        modifier = Modifier
                            .size(40.dp)
                            .background(
                                if (canAdd) C.accent else C.insetBg,
                                RoundedCornerShape(12.dp),
                            )
                            .clickable(enabled = canAdd) {
                                val word = check.getOrNull() ?: return@clickable
                                submit(words + word) {
                                    draft = ""
                                    notice = "已加入「$word」"
                                }
                            },
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(
                            Icons.Filled.Add,
                            contentDescription = "加入词表",
                            tint = if (canAdd) C.onAccent else C.textDim,
                            modifier = Modifier.size(18.dp),
                        )
                    }
                }

                // 输入即时反馈：只在「非空且有问题」时显示，空输入不唠叨
                val inline = if (draft.isBlank()) null else SettingsOps.validateZwspWord(draft)
                    .exceptionOrNull()?.message
                val duplicate = draft.isNotBlank() && words.contains(draft.trim())
                if (inline != null) {
                    Spacer(Modifier.height(6.dp))
                    Text(inline, style = AgateType.Caption, color = C.red)
                } else if (duplicate) {
                    Spacer(Modifier.height(6.dp))
                    Text("这个词已经在表里了", style = AgateType.Caption, color = C.textDim)
                }

                if (words.size >= SettingsOps.ZWSP_MAX_WORDS) {
                    Spacer(Modifier.height(6.dp))
                    Text(
                        "已到上限 ${SettingsOps.ZWSP_MAX_WORDS} 条 —— 这是「自己的业务词」的上限，" +
                            "超过这个规模说明该用内置规则集而不是这里。",
                        style = AgateType.Caption,
                        color = C.amber,
                    )
                }

                // 词条列表
                if (words.isNotEmpty()) {
                    Spacer(Modifier.height(14.dp))
                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        words.forEach { word ->
                            Row(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .background(C.insetBg, RoundedCornerShape(12.dp))
                                    .border(1.dp, C.border, RoundedCornerShape(12.dp))
                                    .padding(horizontal = 12.dp, vertical = 10.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Text(
                                    text = word,
                                    style = AgateType.Body,
                                    color = C.textMain,
                                    modifier = Modifier.weight(1f),
                                )
                                Box(
                                    modifier = Modifier
                                        .size(28.dp)
                                        .clickable(enabled = !busy) {
                                            submit(words - word) {
                                                notice = "已移除「$word」"
                                            }
                                        },
                                    contentAlignment = Alignment.Center,
                                ) {
                                    Icon(
                                        Icons.Filled.Close,
                                        contentDescription = "移除",
                                        tint = C.textDim,
                                        modifier = Modifier.size(16.dp),
                                    )
                                }
                            }
                        }
                    }
                } else if (!loading) {
                    Spacer(Modifier.height(14.dp))
                    Text(
                        "词表是空的 —— 这一层当前不生效，只有上面那层内置规则集在工作。",
                        style = AgateType.Caption,
                        color = C.textDim,
                    )
                }

                // 反馈区
                error?.let {
                    Spacer(Modifier.height(10.dp))
                    Notice(text = it, color = C.red, bg = C.redBg)
                }
                notice?.let {
                    Spacer(Modifier.height(10.dp))
                    Notice(text = it, color = C.green, bg = C.greenBg)
                }

                if (loading) {
                    Spacer(Modifier.height(10.dp))
                    Text("正在读取…", style = AgateType.Caption, color = C.textDim)
                }
            }

            // ── 说明：为什么不做成「替换成别的词」 ──────────
            AgateAccordion(title = "这一层会改动什么", initiallyExpanded = false) {
                Text(
                    "它在你发出的文本里插入不可见字符，不是替换成别的词。所以：",
                    style = AgateType.Caption,
                    color = C.textSub,
                )
                Spacer(Modifier.height(8.dp))
                Bullet("语义不变：模型读到的还是同一个词，只是中间/末尾多了一个不可见字符。", C)
                Bullet("可读性不变：人眼看不到 U+200B（它是零宽的）。", C)
                Bullet("长度会变：每命中一次多 3 个字节（UTF-8 下 U+200B 占 3 字节），token 数可能微增。", C)
                Bullet("只作用于 messages 里的文本，不碰系统提示词与工具声明。", C)
                Spacer(Modifier.height(8.dp))
                Text(
                    "注意：它对纯数字串无效 —— 上游会把数字归一化，零宽空格会被吃掉。" +
                        "数字类指纹属于内置规则集的职责（那里做的是真正的字面改写）。",
                    style = AgateType.Caption,
                    color = C.amber,
                )
            }

            Spacer(Modifier.height(16.dp))
        }
    }
}

@Composable
private fun Notice(text: String, color: androidx.compose.ui.graphics.Color, bg: androidx.compose.ui.graphics.Color) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(bg, RoundedCornerShape(10.dp))
            .padding(horizontal = 12.dp, vertical = 8.dp),
    ) {
        Text(text, style = AgateType.Caption, color = color)
    }
}

@Composable
private fun Bullet(text: String, C: dev.agate.android.ui.theme.AgateColors) {
    Row(modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp)) {
        Text("· ", style = AgateType.Caption, color = C.textDim)
        Text(text, style = AgateType.Caption, color = C.textSub)
    }
}
