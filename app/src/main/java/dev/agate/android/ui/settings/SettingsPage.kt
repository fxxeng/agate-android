package dev.agate.android.ui.settings

import android.widget.Toast
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import dev.agate.android.data.KeepalivePrefs
import dev.agate.android.data.LogOps
import dev.agate.android.data.SettingsOps
import dev.agate.android.ui.components.AgateAccordion
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateFormInput
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.AgateColors
import dev.agate.android.ui.theme.AgateThemeMode
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import dev.agate.core.AgateForegroundService
import dev.agate.core.GatewayPrefs
import kotlinx.coroutines.launch

/**
 * 应用设置页（十组手风琴）。
 *
 * ── 这一页的设计前提 ────────────────────────────────────────
 * ① **只做内核真有端点的项**。监听端口 / 无 Key 调用这类归「网关控制」页
 *    （或根本不给入口）—— 放进来就是点了没反应的死按钮。
 * ② **折叠态也要给读数**。设置页最高频的问题是「我现在是多少」，不是「我要改」，
 *    所以每组标题右侧挂一行从内核读回来的真实值，不展开也能判断。
 * ③ **开关即时生效、数字弹层单项提交**。开关停在错误位置（显示开了其实没开）
 *    比报错严重得多，所以失败必须回弹；数字组内核是整单校验，本地先挡一遍。
 *
 * ── 分组与端点对照 ──────────────────────────────────────────
 * | 组            | 端点                                            |
 * | 服务接入      | GET/PUT /api/config                             |
 * | 外观          | GatewayPrefs.themeMode（壳层偏好，不进内核）     |
 * | 流式与输出    | /api/keepalive-strip · /api/auto-continue ·      |
 * |               | /api/debug                                      |
 * | 排队与超时    | /api/queue · /api/timeouts                      |
 * | 重试          | /api/retry（含 noRetryStatusCodes 数组）         |
 * | 脱敏与安全    | /api/sanitize · /api/cors ·                     |
 * |               | /api/developer-role-compat · /api/zwsp          |
 * | 提示词        | GET/PUT /api/prompt                             |
 * | 数据管理      | /api/storage · /api/retention                   |
 * | 备份与恢复    | /api/backup/export · /api/backup/import         |
 * | 关于          | 包信息 + 配置目录 + 诊断信息                     |
 *
 * ── 一次读全还是分组懒加载 ──────────────────────────────────
 * 分组懒加载看着省，但用户点开一组要等一次往返，而每组展开都要重绘摘要行 ——
 * 摘要行本来就得先读。所以首屏并发读全，任一失败只让该组显示「读取失败」，
 * 不整页报错（十组是十条独立端点，一组挂了不该拖死整页）。
 */
@Composable
fun SettingsPage(
    themeMode: AgateThemeMode = AgateThemeMode.FOLLOW_SYSTEM,
    onThemeModeChange: (AgateThemeMode) -> Unit = {},
    onOpenDrawer: () -> Unit = {},
    onBack: () -> Unit = {},
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    // ── 内核读数（null = 还没读到 / 读失败）──────────────────
    var cfg by remember { mutableStateOf<SettingsOps.Config?>(null) }
    var stream by remember { mutableStateOf<SettingsOps.Stream?>(null) }
    var guards by remember { mutableStateOf<SettingsOps.Guards?>(null) }
    var timeouts by remember { mutableStateOf<Map<String, Long>>(emptyMap()) }
    var queue by remember { mutableStateOf<Map<String, Long>>(emptyMap()) }
    var retry by remember { mutableStateOf<Map<String, Long>>(emptyMap()) }
    var retention by remember { mutableStateOf<Map<String, Long>>(emptyMap()) }
    var codes by remember { mutableStateOf<List<Int>>(emptyList()) }
    var prompt by remember { mutableStateOf<SettingsOps.Prompt?>(null) }
    var storage by remember { mutableStateOf<SettingsOps.Storage?>(null) }
    var words by remember { mutableStateOf<List<String>>(emptyList()) }

    var loading by remember { mutableStateOf(true) }
    var errors by remember { mutableStateOf<Map<String, String>>(emptyMap()) }
    var notices by remember { mutableStateOf<Map<String, String>>(emptyMap()) }
    // 正在保存的开关 key（禁用 + 半透明），防止连点
    var pending by remember { mutableStateOf<Set<String>>(emptySet()) }

    fun setError(group: String, message: String?) {
        errors = if (message == null) errors - group else errors + (group to message)
    }

    fun setNotice(group: String, message: String?) {
        notices = if (message == null) notices - group else notices + (group to message)
    }

    fun reload() {
        scope.launch {
            loading = true
            val failed = LinkedHashMap<String, String>()

            val c = SettingsOps.config()
            if (c.ok) cfg = c.data else failed["接入"] = c.error ?: "读取失败"

            val s = SettingsOps.stream()
            if (s.ok) stream = s.data else failed["流式"] = s.error ?: "读取失败"

            val g = SettingsOps.guards()
            if (g.ok) guards = g.data else failed["安全"] = g.error ?: "读取失败"

            val t = SettingsOps.timeouts()
            if (t.ok) timeouts = t.data.orEmpty() else failed["超时"] = t.error ?: "读取失败"

            val q = SettingsOps.queue()
            if (q.ok) queue = q.data.orEmpty() else failed["超时"] = "排队设置读取失败"

            val r = SettingsOps.retry()
            if (r.ok) retry = r.data.orEmpty() else failed["重试"] = r.error ?: "读取失败"

            val n = SettingsOps.noRetryCodes()
            if (n.ok) codes = n.data.orEmpty()

            val ret = SettingsOps.retention()
            if (ret.ok) retention = ret.data.orEmpty() else failed["数据"] = ret.error ?: "读取失败"

            val p = SettingsOps.prompt()
            if (p.ok) prompt = p.data else failed["提示词"] = p.error ?: "读取失败"

            val st = SettingsOps.storage()
            if (st.ok) storage = st.data else failed["数据"] = st.error ?: "读取失败"

            val w = SettingsOps.zwspWords()
            if (w.ok) words = w.data.orEmpty() else failed["安全"] = w.error ?: "词表读取失败"

            errors = failed
            loading = false
        }
    }

    LaunchedEffect(Unit) { reload() }

    /**
     * 开关的统一保存路径：乐观置位 → 失败回弹并留原因。
     *
     * `group` 必须显式传，不能从 `key` 里推：key 是端点路径（`/api/cors`），
     * 而错误是按用户看得见的组名（「安全」）存的。早先写成
     * `key.substringAfter('/')` 得到的是 `api/cors`，于是一个失败的开关
     * **回弹了却没有任何提示** —— 用户只看到开关自己弹回去。
     */
    fun toggle(
        key: String,
        group: String,
        want: Boolean,
        revert: () -> Unit,
        save: suspend () -> SettingsOps.OpResult<*>,
    ) {
        if (key in pending) return
        pending = pending + key
        setNotice(group, null)
        setError(group, null)
        scope.launch {
            val result = save()
            if (!result.ok) {
                revert()
                setError(group, result.error ?: "保存失败")
            }
            pending = pending - key
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
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            SectionHeader(
                title = "应用设置",
                subtitle = "十一组 · 读数来自内核，改完即生效（无需重启）",
                trailing = versionName(ctx),
            )

            // ══ 1. 服务接入 ══════════════════════════════════
            AgateAccordion(
                title = "服务接入",
                badge = {
                    AgateBadge(
                        text = accessSummary(cfg),
                        tone = if (cfg == null) BadgeTone.GRAY else BadgeTone.BLUE,
                    )
                },
            ) {
                val base = "http://127.0.0.1:${GatewayPrefs.port(ctx)}/v1"
                ValueRow(
                    label = "本机地址 · OpenAI 兼容",
                    value = base,
                    hint = "客户端把 Base URL 填这里，路径照旧（/chat/completions）",
                    action = "复制",
                    onAction = { copyToClipboard(ctx, base); toast(ctx, "已复制") },
                )
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    listOf("chat", "responses", "messages", "models").forEach {
                        AgateBadge(it, BadgeTone.GRAY)
                    }
                }

                Spacer(Modifier.height(14.dp))
                GroupError(errors["接入"], C) { reload() }

                // API Key
                var keyDraft by remember { mutableStateOf("") }
                var editingKey by remember { mutableStateOf(false) }
                ItemRow(
                    label = "API Key",
                    desc = "掩码只用来显示「已经有了」，值拿不到也不该拿到 · 清除后接口恢复免鉴权",
                ) {
                    if (editingKey) {
                        Column {
                            AgateFormInput(
                                value = keyDraft,
                                onValueChange = { keyDraft = it },
                                hint = "至少 ${SettingsOps.MIN_API_KEY_LENGTH} 个字符",
                                modifier = Modifier.width(190.dp),
                            )
                            Spacer(Modifier.height(6.dp))
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                MiniButton("保存", enabled = "/api/config" !in pending) {
                                    if (keyDraft.length < SettingsOps.MIN_API_KEY_LENGTH) {
                                        toast(ctx, "至少 ${SettingsOps.MIN_API_KEY_LENGTH} 个字符")
                                        return@MiniButton
                                    }
                                    scope.launch {
                                        if ("/api/config" in pending) return@launch
                                        pending = pending + "/api/config"
                                        val r = SettingsOps.saveApiKey(keyDraft)
                                        if (r.ok) {
                                            cfg = r.data
                                            editingKey = false
                                            keyDraft = ""
                                            toast(ctx, "已保存")
                                            setError("接入", null)
                                        } else {
                                            setError("接入", r.error)
                                        }
                                        pending = pending - "/api/config"
                                    }
                                }
                                MiniButton("取消", filled = false) { editingKey = false }
                            }
                        }
                    } else {
                        Column(horizontalAlignment = Alignment.End) {
                            Text(
                                text = if (cfg?.apiKeySet == true) {
                                    cfg?.apiKeyMasked?.ifBlank { "已设置" } ?: "已设置"
                                } else {
                                    "未设置"
                                },
                                style = AgateType.BodyS,
                                color = if (cfg?.apiKeySet == true) C.textSub else C.amber,
                            )
                            Spacer(Modifier.height(4.dp))
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                MiniButton("改") { editingKey = true }
                                if (cfg?.apiKeySet == true) {
                                    MiniButton("清除", filled = false, enabled = "/api/config" !in pending) {
                                        scope.launch {
                                            if ("/api/config" in pending) return@launch
                                            pending = pending + "/api/config"
                                            val r = SettingsOps.saveApiKey(null)
                                            if (r.ok) {
                                                cfg = r.data
                                                toast(ctx, "已清除，接口恢复免鉴权")
                                                setError("接入", null)
                                            } else {
                                                setError("接入", r.error)
                                            }
                                            pending = pending - "/api/config"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // 计费语言
                ItemRow(
                    label = "计费语言",
                    desc = "影响余额 / 用量文案的语言（内核默认 ${"zh-CN"}）",
                ) {
                    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        listOf("zh-CN" to "中文", "en-US" to "English").forEach { (value, label) ->
                            val active = cfg?.locale == value
                            Box(
                                Modifier
                                    .background(
                                        if (active) C.accent.copy(alpha = 0.14f) else C.insetBg,
                                        RoundedCornerShape(10.dp),
                                    )
                                    .border(
                                        1.dp,
                                        if (active) C.accent else C.border,
                                        RoundedCornerShape(10.dp),
                                    )
                                    .clickable {
                                        scope.launch {
                                            val r = SettingsOps.saveLocale(value)
                                            if (r.ok) cfg = r.data else setError("接入", r.error)
                                        }
                                    }
                                    .padding(horizontal = 10.dp, vertical = 5.dp),
                            ) {
                                Text(
                                    label,
                                    style = AgateType.Caption,
                                    color = if (active) C.accent else C.textSub,
                                )
                            }
                        }
                    }
                }

                // 默认模型（只读）
                ItemRow(
                    label = "默认模型",
                    desc = "客户端不指定 model 时用哪个 · 改它去「模型 → 映射」",
                ) {
                    AgateBadge(cfg?.defaultModel?.ifBlank { "auto" } ?: "auto", BadgeTone.GRAY)
                }
            }

            // ══ 2. 外观 ══════════════════════════════════════
            AgateAccordion(
                title = "外观",
                badge = { AgateBadge(themeLabel(themeMode), BadgeTone.GRAY) },
            ) {
                Text(
                    "三档主题存在壳层偏好里（不进内核），冷启动即按此渲染。",
                    style = AgateType.Caption,
                    color = C.textSub,
                )
                Spacer(Modifier.height(10.dp))
                AgateSegmented(
                    options = listOf("跟随系统", "浅色", "深色"),
                    selected = themeMode.ordinal.coerceIn(0, 2),
                    onSelect = { index ->
                        val next = AgateThemeMode.values().getOrElse(index) {
                            AgateThemeMode.FOLLOW_SYSTEM
                        }
                        onThemeModeChange(next)
                    },
                )
            }

            // ══ 3. 流式与输出 ════════════════════════════════
            AgateAccordion(
                title = "流式与输出",
                badge = {
                    AgateBadge(
                        text = streamSummary(stream),
                        tone = if (stream == null) BadgeTone.GRAY else BadgeTone.BLUE,
                    )
                },
            ) {
                GroupError(errors["流式"], C) { reload() }
                val s = stream
                var keepalive by remember(s) { mutableStateOf(s?.stripKeepalive ?: true) }
                var autoContinue by remember(s) { mutableStateOf(s?.autoContinue ?: false) }
                var rounds by remember(s) { mutableStateOf(s?.autoContinueRounds ?: 3) }
                var debug by remember(s) { mutableStateOf(s?.debug ?: false) }

                SwitchRow(
                    label = "丢弃纯换行的保活分片",
                    desc = "防止 markdown 把一句话拆成一行一个词 · 模型确实单独输出过的空行会一并丢弃",
                    checked = keepalive,
                    enabled = "/api/keepalive-strip" !in pending,
                    onToggle = { want ->
                        val before = keepalive
                        keepalive = want
                        toggle("/api/keepalive-strip", "流式", want, { keepalive = before }) {
                            val result = SettingsOps.saveStripKeepalive(want)
                            // 这条 PUT 只回被改的那个键，快照得自己改对 ——
                            // 见 SettingsOps.Stream 的文档
                            if (result.ok) stream = stream?.copy(stripKeepalive = want)
                            result
                        }
                    },
                )
                SwitchRow(
                    label = "断流续写",
                    desc = "上游说到一半就停时，回放半截追一句「请继续输出」再打一次 · 会多消耗配额",
                    checked = autoContinue,
                    enabled = "/api/auto-continue" !in pending,
                    onToggle = { want ->
                        val before = autoContinue
                        autoContinue = want
                        toggle("/api/auto-continue", "流式", want, { autoContinue = before }) {
                            val result = SettingsOps.saveAutoContinue(enabled = want)
                            if (result.ok) stream = stream?.copy(autoContinue = want)
                            result
                        }
                    },
                )
                if (autoContinue) {
                    NumberRow(
                        label = "续写轮数上限",
                        desc = "只在续写开着时有意义",
                        value = rounds.toLong(),
                        unit = "轮",
                        enabled = "/api/auto-continue" !in pending,
                        onPick = { picked ->
                            // NumberRow 走 Long（与超时/重试共用一套弹层），
                            // 这一项在内核里是 Int —— 窄化前先按范围夹一次。
                            val next = picked.coerceIn(
                                SettingsOps.AUTO_CONTINUE_MIN_ROUNDS.toLong(),
                                SettingsOps.AUTO_CONTINUE_MAX_ROUNDS.toLong(),
                            ).toInt()
                            val before = rounds
                            rounds = next
                            scope.launch {
                                val r = SettingsOps.saveAutoContinue(rounds = next)
                                if (!r.ok) {
                                    rounds = before
                                    setError("流式", r.error)
                                }
                            }
                        },
                        field = SettingsOps.Field(
                            "autoContinueMaxRounds",
                            "续写轮数上限",
                            SettingsOps.AUTO_CONTINUE_MIN_ROUNDS.toLong(),
                            SettingsOps.AUTO_CONTINUE_MAX_ROUNDS.toLong(),
                            " 轮",
                            "只在续写开着时有意义。轮数越多，多耗的配额越多。",
                            SettingsOps.AUTO_CONTINUE_DEFAULT_ROUNDS.toLong(),
                        ),
                    )
                }
                SwitchRow(
                    label = "调试模式（报文采集）",
                    desc = "完整保存发给上游的请求与响应 · 只保留最近 ${s?.debugLimit ?: 500} 条 · " +
                        "凭据类头落盘前已脱敏 · 只对新请求生效",
                    checked = debug,
                    enabled = "/api/debug" !in pending,
                    onToggle = { want ->
                        val before = debug
                        debug = want
                        toggle("/api/debug", "流式", want, { debug = before }) {
                            val result = SettingsOps.saveDebug(want)
                            if (result.ok) stream = stream?.copy(debug = want)
                            result
                        }
                    },
                )
                if (debug) {
                    ReadRow(
                        label = "出网取证",
                        desc = "已采集 ${s?.debugRows ?: 0} 条，去「调用记录」页按调试标记查看",
                        value = "${s?.debugRows ?: 0} 条",
                    )
                }
                // 开发者角色兼容只放在「脱敏与安全」组里。
                // 设计预览把它写进了两组，但它是同一个配置键 —— 两处开关会各有
                // 一份本地状态，一处改完另一处还显示旧值，看起来像保存失败。
                SetFeedback("流式", errors, notices, C)
            }

            // ══ 4. 排队与超时 ════════════════════════════════
            AgateAccordion(
                title = "排队与超时",
                badge = {
                    AgateBadge(
                        text = queueSummary(queue, timeouts),
                        tone = if (timeouts.isEmpty()) BadgeTone.GRAY else BadgeTone.BLUE,
                    )
                },
            ) {
                GroupError(errors["超时"], C) { reload() }
                SettingsOps.QUEUE_FIELDS.forEach { field ->
                    NumberRow(
                        label = field.label,
                        desc = field.desc,
                        value = queue[field.key] ?: field.default,
                        unit = field.unit.trim(),
                        field = field,
                        enabled = "/api/queue" !in pending,
                        onPick = { picked ->
                            scope.launch {
                                val r = SettingsOps.saveQueue(mapOf(field.key to picked))
                                if (r.ok) {
                                    queue = queue + (field.key to picked)
                                    setError("超时", null)
                                } else {
                                    setError("超时", r.error)
                                }
                            }
                        },
                    )
                }
                Spacer(Modifier.height(10.dp))
                SettingsOps.TIMEOUT_FIELDS.forEach { field ->
                    NumberRow(
                        label = field.label,
                        desc = field.desc,
                        value = timeouts[field.key] ?: field.default,
                        unit = field.unit.trim(),
                        field = field,
                        enabled = "/api/timeouts" !in pending,
                        onPick = { picked ->
                            scope.launch {
                                val r = SettingsOps.saveTimeouts(mapOf(field.key to picked))
                                if (r.ok) {
                                    timeouts = timeouts + (field.key to picked)
                                    setError("超时", null)
                                } else {
                                    setError("超时", r.error)
                                }
                            }
                        },
                    )
                }
                Hint(
                    "改完对下一个请求立即生效，无需重启。排队发生在首字节之前，吃的是「等待响应」那份预算。",
                    C,
                )
                SetFeedback("超时", errors, notices, C)
            }

            // ══ 5. 重试 ══════════════════════════════════════
            AgateAccordion(
                title = "重试",
                badge = {
                    AgateBadge(
                        text = retrySummary(retry, codes.size),
                        tone = if (retry.isEmpty()) BadgeTone.GRAY else BadgeTone.BLUE,
                    )
                },
            ) {
                GroupError(errors["重试"], C) { reload() }
                SettingsOps.RETRY_FIELDS.forEach { field ->
                    NumberRow(
                        label = field.label,
                        desc = field.desc,
                        value = retry[field.key] ?: field.default,
                        unit = field.unit.trim(),
                        field = field,
                        enabled = "/api/retry" !in pending,
                        onPick = { picked ->
                            scope.launch {
                                val r = SettingsOps.saveRetry(mapOf(field.key to picked))
                                if (r.ok) {
                                    retry = retry + (field.key to picked)
                                    setError("重试", null)
                                } else {
                                    setError("重试", r.error)
                                }
                            }
                        },
                    )
                }
                var editingCodes by remember { mutableStateOf(false) }
                ItemRow(
                    label = "直接换号的状态码",
                    desc = "撞上这些码时不在同一账号重发，直接换下一个 · 空 = 一律照常重试",
                ) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        if (codes.isEmpty()) {
                            Text("未设置", style = AgateType.Caption, color = C.textDim)
                        } else {
                            codes.take(3).forEach {
                                AgateBadge("$it", BadgeTone.AMBER)
                                Spacer(Modifier.width(4.dp))
                            }
                            if (codes.size > 3) AgateBadge("+${codes.size - 3}", BadgeTone.GRAY)
                        }
                        Spacer(Modifier.width(8.dp))
                        MiniButton("编辑", filled = false) { editingCodes = true }
                    }
                }
                Hint(
                    "内核会排序去重后落库；任何一项越界都是整单 400 不落盘，所以界面先本地挡一遍。",
                    C,
                )
                SetFeedback("重试", errors, notices, C)

                if (editingCodes) {
                    CodesDialog(
                        initial = codes,
                        onDismiss = { editingCodes = false },
                        onSave = { next ->
                            scope.launch {
                                val r = SettingsOps.saveNoRetryCodes(next)
                                if (r.ok) {
                                    codes = r.data.orEmpty()
                                    editingCodes = false
                                    setError("重试", null)
                                } else {
                                    setError("重试", r.error)
                                }
                            }
                        },
                    )
                }
            }

            // ══ 6. 脱敏与安全 ════════════════════════════════
            AgateAccordion(
                title = "脱敏与安全",
                badge = {
                    AgateBadge(
                        text = guardSummary(guards, words.size),
                        tone = when {
                            guards == null -> BadgeTone.GRAY
                            guards?.cors == true && cfg?.apiKeySet != true -> BadgeTone.RED
                            else -> BadgeTone.BLUE
                        },
                    )
                },
            ) {
                GroupError(errors["安全"], C) { reload() }

                SwitchRow(
                    label = "上游审核指纹剥离（内置规则集）",
                    desc = "出站前剥离审核黑名单指纹：表头键值整段删除、承载语义的模板句最小改写。" +
                        "关掉 = 客户端 system 模板原样发出，那正是被误拦（400 code=11-128）的原因。",
                    checked = guards?.sanitize ?: true,
                    enabled = "/api/sanitize" !in pending,
                    onToggle = { want ->
                        val before = guards
                        guards = before?.let { SettingsOps.Guards(want, it.cors, it.developerRoleCompat) }
                        toggle("/api/sanitize", "安全", want, { guards = before }) {
                            val result = SettingsOps.saveSanitize(want)
                            if (result.ok) guards = guards?.copy(sanitize = want)
                            result
                        }
                    },
                )
                SwitchRow(
                    label = "网关跨域访问（CORS）",
                    desc = "开着时 /v1/* 应答跨源请求（来源 *，预检直接 204）· " +
                        "开着又没配 Key 时，任何网页都能借本机网关打上游",
                    checked = guards?.cors ?: false,
                    enabled = "/api/cors" !in pending,
                    onToggle = { want ->
                        val before = guards
                        guards = before?.let { SettingsOps.Guards(it.sanitize, want, it.developerRoleCompat) }
                        toggle("/api/cors", "安全", want, { guards = before }) {
                            val result = SettingsOps.saveCors(want)
                            if (result.ok) guards = guards?.copy(cors = want)
                            result
                        }
                    },
                )
                SwitchRow(
                    label = "开发者角色兼容",
                    desc = "developer → system 的固定改写，语义无损 · 与「流式与输出」里那一项是同一个开关",
                    checked = guards?.developerRoleCompat ?: true,
                    enabled = "/api/developer-role-compat" !in pending,
                    onToggle = { want ->
                        val before = guards
                        guards = before?.let { SettingsOps.Guards(it.sanitize, it.cors, want) }
                        toggle("/api/developer-role-compat", "安全", want, { guards = before }) {
                            val result = SettingsOps.saveDeveloperRoleCompat(want)
                            if (result.ok) guards = guards?.copy(developerRoleCompat = want)
                            result
                        }
                    },
                )
                if (guards?.cors == true && cfg?.apiKeySet != true) {
                    Spacer(Modifier.height(8.dp))
                    Banner(
                        "跨域开着、且 API Key 为空 —— 这两个条件同时成立时，同一网段的任何网页都能" +
                            "免鉴权调用本机网关。要么关掉跨域，要么立刻设一个 Key。",
                        C.red,
                        C.redBg,
                    )
                }

                // ── 零宽空格词表（第二层脱敏）─────────────────
                Spacer(Modifier.height(14.dp))
                Divider(C)
                Spacer(Modifier.height(12.dp))
                ZwspBlock(
                    words = words,
                    busy = pending.any { it.startsWith("/api/zwsp") },
                    onSave = { next, ok ->
                        scope.launch {
                            if ("/api/zwsp" in pending) return@launch
                            pending = pending + "/api/zwsp"
                            val r = SettingsOps.saveZwspWords(next)
                            if (r.ok) {
                                words = next
                                setError("安全", null)
                                ok()
                            } else {
                                setError("安全", r.error)
                            }
                            pending = pending - "/api/zwsp"
                        }
                    },
                )
                SetFeedback("安全", errors, notices, C)
            }

            // ══ 7. 提示词 ════════════════════════════════════
            AgateAccordion(
                title = "提示词",
                badge = {
                    AgateBadge(
                        text = promptSummary(prompt),
                        tone = when {
                            prompt == null -> BadgeTone.GRAY
                            prompt?.degradeActive == true -> BadgeTone.AMBER
                            else -> BadgeTone.BLUE
                        },
                    )
                },
            ) {
                GroupError(errors["提示词"], C) { reload() }
                val p = prompt
                var mode by remember(p) { mutableStateOf(p?.mode ?: "passthrough") }
                var fileDraft by remember(p) { mutableStateOf(p?.file ?: "") }
                var editingText by remember { mutableStateOf(false) }

                Text(
                    "客户端 CLI 会注入几十句固定模板，上游逐字审核命中即整单 400 · 只改发给上游的副本",
                    style = AgateType.Caption,
                    color = C.textSub,
                )
                Spacer(Modifier.height(10.dp))
                AgateSegmented(
                    options = listOf("透传", "替换", "追加"),
                    selected = SettingsOps.PROMPT_MODES.indexOf(mode).coerceAtLeast(0),
                    onSelect = { index ->
                        val next = SettingsOps.PROMPT_MODES.getOrElse(index) { "passthrough" }
                        val before = mode
                        mode = next
                        scope.launch {
                            val r = SettingsOps.savePrompt(mode = SettingsOps.Opt.Set(next))
                            if (r.ok) {
                                prompt = r.data
                                setError("提示词", null)
                            } else {
                                mode = before
                                setError("提示词", r.error)
                            }
                        }
                    },
                )
                Spacer(Modifier.height(12.dp))

                ItemRow(
                    label = "生效正文来源",
                    desc = "优先级：界面正文 > 文件 > 内置默认",
                ) {
                    AgateBadge(
                        p?.sourceLabel ?: "—",
                        if (p?.source == "none") BadgeTone.GRAY else BadgeTone.GREEN,
                    )
                }

                // 文件路径：只有在用自有提示词时才有意义
                ItemRow(
                    label = "提示词文件",
                    desc = if (p?.usesOwnPrompt == true) {
                        "留空 = 不用文件 · 读不到时会回落内置默认并在这里给原因"
                    } else {
                        "当前模式不读文件，切换成「替换」或「追加」后才生效"
                    },
                ) {
                    AgateFormInput(
                        value = fileDraft,
                        onValueChange = { fileDraft = it },
                        hint = "绝对路径（可留空）",
                        singleLine = false,
                        minLines = 1,
                        modifier = Modifier.width(200.dp),
                    )
                }
                if (fileDraft.isNotBlank() && fileDraft != (p?.file ?: "")) {
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        MiniButton("保存路径") {
                            scope.launch {
                                val r = SettingsOps.savePrompt(file = SettingsOps.Opt.Set(fileDraft.trim()))
                                if (r.ok) {
                                    prompt = r.data
                                    setError("提示词", null)
                                } else {
                                    setError("提示词", r.error)
                                }
                            }
                        }
                        MiniButton("清空路径", filled = false) {
                            scope.launch {
                                val r = SettingsOps.savePrompt(file = SettingsOps.Opt.Clear)
                                if (r.ok) {
                                    prompt = r.data
                                    fileDraft = r.data?.file.orEmpty()
                                    setError("提示词", null)
                                } else {
                                    setError("提示词", r.error)
                                }
                            }
                        }
                    }
                    Spacer(Modifier.height(8.dp))
                }
                if (p?.fileError?.isNotBlank() == true) {
                    Banner("文件读不到，已回落内置默认：${p.fileError}", C.amber, C.amberBg)
                }

                ItemRow(
                    label = "提示词正文",
                    desc = "${p?.lines ?: 0} 行 · 点开全屏编辑（正文动辄几百行，弹层装不下）",
                ) {
                    MiniButton("编辑") { editingText = true }
                }

                if ((p?.providerOverrides ?: 0) > 0) {
                    ReadRow(
                        label = "按提供商覆盖",
                        desc = "每家的模式与正文可以不同（稀疏表：没配过的家沿用上面这份）",
                        value = "${p?.providerOverrides} 家",
                    )
                }

                // 降级状态
                if (p?.degradeActive == true) {
                    Spacer(Modifier.height(6.dp))
                    Banner(
                        "撞审降级中，至 ${p.degradeUntilText.ifBlank { "次日 00:00" }} · " +
                            "期间所有请求直接带中性提示词出门，不再先撞一次 400",
                        C.amber,
                        C.amberBg,
                    )
                    Spacer(Modifier.height(8.dp))
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            "改好提示词了？点这里立刻恢复本模式自己的提示词，不必等到零点。",
                            style = AgateType.Caption,
                            color = C.textSub,
                            modifier = Modifier.weight(1f),
                        )
                        Spacer(Modifier.width(8.dp))
                        MiniButton("立即解除") {
                            scope.launch {
                                val r = SettingsOps.savePrompt(clearDegrade = true)
                                if (r.ok) {
                                    prompt = r.data
                                    setError("提示词", null)
                                } else {
                                    setError("提示词", r.error)
                                }
                            }
                        }
                    }
                }
                SetFeedback("提示词", errors, notices, C)

                if (editingText) {
                    PromptTextDialog(
                        initial = p?.text.orEmpty(),
                        modeLabel = p?.modeLabel ?: "透传",
                        onDismiss = { editingText = false },
                        onSave = { text ->
                            scope.launch {
                                val r = if (text.isBlank()) {
                                    SettingsOps.savePrompt(text = SettingsOps.Opt.Clear)
                                } else {
                                    SettingsOps.savePrompt(text = SettingsOps.Opt.Set(text))
                                }
                                if (r.ok) {
                                    prompt = r.data
                                    editingText = false
                                    setError("提示词", null)
                                } else {
                                    setError("提示词", r.error)
                                }
                            }
                        },
                    )
                }
            }

            // ══ 8. 数据管理 ══════════════════════════════════
            AgateAccordion(
                title = "数据管理",
                badge = {
                    AgateBadge(
                        text = storageSummary(storage),
                        tone = when {
                            storage == null -> BadgeTone.GRAY
                            storage?.available != true -> BadgeTone.RED
                            else -> BadgeTone.BLUE
                        },
                    )
                },
            ) {
                GroupError(errors["数据"], C) { reload() }
                val st = storage
                if (st != null && st.available) {
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .background(C.insetBg, RoundedCornerShape(12.dp))
                            .border(1.dp, C.border, RoundedCornerShape(12.dp))
                            .padding(horizontal = 12.dp, vertical = 10.dp),
                        horizontalArrangement = Arrangement.spacedBy(18.dp),
                    ) {
                        Column {
                            Text("库文件", style = AgateType.Caption, color = C.textDim)
                            Text(humanBytes(st.bytes), style = AgateType.Body, color = C.textMain)
                        }
                        Column(modifier = Modifier.weight(1f)) {
                            Text("位置", style = AgateType.Caption, color = C.textDim)
                            Text(
                                st.file.ifBlank { "—" },
                                style = AgateType.Caption,
                                color = C.textSub,
                            )
                        }
                    }
                    Spacer(Modifier.height(10.dp))
                    if (st.tables.isEmpty()) {
                        Hint("库里还没有数据。", C)
                    } else {
                        st.tables.forEach { (label, count) ->
                            ReadRow(label = label, desc = "", value = group(count))
                        }
                    }
                } else if (st != null) {
                    Banner(
                        "库不可用 —— 各表计数都读不到。这不是「没有数据」，而是打不开库文件。",
                        C.red,
                        C.redBg,
                    )
                }

                Spacer(Modifier.height(12.dp))
                Divider(C)
                Spacer(Modifier.height(12.dp))
                Text("保留天数", style = AgateType.Body, fontWeight = FontWeight.SemiBold, color = C.textMain)
                Spacer(Modifier.height(4.dp))
                Text(
                    "改小会立刻裁剪既有数据 —— 不是「以后的才按新规则」，所以点保存前先看这句。",
                    style = AgateType.Caption,
                    color = C.amber,
                )
                Spacer(Modifier.height(8.dp))
                SettingsOps.RETENTION_FIELDS.forEach { field ->
                    val current = retention[field.key] ?: field.default
                    NumberRow(
                        label = field.label,
                        desc = field.desc,
                        value = current,
                        unit = "天",
                        field = field,
                        valueText = if (current >= SettingsOps.RETENTION_MAX_DAYS) "永久" else "$current 天",
                        enabled = "/api/retention" !in pending,
                        onPick = { picked ->
                            scope.launch {
                                val r = SettingsOps.saveRetention(mapOf(field.key to picked))
                                if (r.ok) {
                                    retention = r.data.orEmpty()
                                    setError("数据", null)
                                    setNotice("数据", "已保存，裁剪已按新规则执行")
                                } else {
                                    setError("数据", r.error)
                                }
                            }
                        },
                    )
                }
                Hint(
                    "想换库的位置只能改环境变量 AGATE_PROXY_HOME 后重启 —— 这一页只展示，" +
                        "不给「更改」按钮：一个点了必然报错的按钮比没有按钮更糟。",
                    C,
                )
                SetFeedback("数据", errors, notices, C)
            }

            // ══ 9. 备份与恢复 ════════════════════════════════
            AgateAccordion(title = "备份与恢复") {
                var exportPath by remember {
                    mutableStateOf(buildString { append(defaultBackupPath(ctx)) })
                }
                var includeLogs by remember { mutableStateOf(true) }
                var importPath by remember { mutableStateOf("") }
                var confirmImport by remember { mutableStateOf(false) }
                var busyBackup by remember { mutableStateOf(false) }
                /**
                 * 刚恢复完、内核内存态还是旧库 → 提示并给一键重启。
                 *
                 * 为什么不自动重启：恢复完成时进程里还有别的动作在跑（页面 reload、
                 * 轮询协程），此刻强杀内核会把它们抛在半路，用户看到的是「恢复成功
                 * 但整页报错」。让用户按一下、明确知道要中断，比替他抢一步更可靠。
                 */
                var needsRestart by remember { mutableStateOf(false) }
                var restarting by remember { mutableStateOf(false) }

                /**
                 * SAF 导出：内核只认「绝对路径 + .sql」，它不会往 content:// 写。
                 * 所以流程是「内核写到缓存文件 → 我们把字节拷进用户选的位置 → 删缓存」。
                 * 中间那份缓存是必要的：备份由后端直读写（大文件不过 HTTP），
                 * 让它在 SAF Uri 上写是不行的。
                 */
                val exportToFile = rememberLauncherForActivityResult(
                    ActivityResultContracts.CreateDocument("application/sql"),
                ) { uri ->
                    if (uri == null) return@rememberLauncherForActivityResult
                    busyBackup = true
                    scope.launch {
                        val tmp = java.io.File(ctx.cacheDir, "agate-export.sql")
                        val r = SettingsOps.exportBackup(tmp.absolutePath, includeLogs)
                        if (!r.ok) {
                            setError("备份", r.error)
                            busyBackup = false
                            return@launch
                        }
                        val written = runCatching {
                            val out = ctx.contentResolver.openOutputStream(uri)
                                ?: error("无法写入所选位置")
                            out.use { sink -> tmp.inputStream().use { it.copyTo(sink) } }
                        }
                        tmp.delete()
                        if (written.isSuccess) {
                            setNotice("备份", "已保存到所选位置 · ${humanBytes(r.data?.bytes ?: 0)}")
                            setError("备份", null)
                        } else {
                            setError("备份", "写入失败：${written.exceptionOrNull()?.message ?: "未知"}")
                        }
                        busyBackup = false
                    }
                }

                /**
                 * 分享备份：导出到 `cache/backup/` → FileProvider → 系统分享面板。
                 *
                 * 与「导出到文件」的分工：那个是**存到本机某个位置**；这个是
                 * **直接发给另一台设备/人**（微信、邮件、隔空投送、网盘）。换机时
                 * 走分享比「先存再找」少两步。
                 *
                 * 只写 FileProvider 声明的那个子目录（见 res/xml/file_paths.xml）：
                 * 备份是全库明文，能分享的范围越小越好。分享完就删本地副本 ——
                 * 留在 cache 里等于多一份可被其它 App 读到的明文凭证。
                 */
                fun shareBackup() {
                    if (busyBackup) return
                    busyBackup = true
                    scope.launch {
                        val dir = java.io.File(ctx.cacheDir, "backup").apply { mkdirs() }
                        val stamp = java.text.SimpleDateFormat(
                            "yyyyMMdd-HHmm",
                            java.util.Locale.US,
                        ).format(java.util.Date())
                        val file = java.io.File(dir, "agate-backup-$stamp.sql")
                        val r = SettingsOps.exportBackup(file.absolutePath, includeLogs)
                        busyBackup = false
                        if (!r.ok) {
                            setError("备份", r.error)
                            return@launch
                        }
                        val shared = runCatching {
                            val uri = androidx.core.content.FileProvider.getUriForFile(
                                ctx,
                                "${ctx.packageName}.fileprovider",
                                file,
                            )
                            val intent = android.content.Intent(android.content.Intent.ACTION_SEND).apply {
                                type = "application/sql"
                                putExtra(android.content.Intent.EXTRA_STREAM, uri)
                                putExtra(
                                    android.content.Intent.EXTRA_SUBJECT,
                                    "Agate 备份 ${humanBytes(r.data?.bytes ?: 0)}",
                                )
                                addFlags(android.content.Intent.FLAG_GRANT_READ_URI_PERMISSION)
                            }
                            ctx.startActivity(
                                android.content.Intent.createChooser(intent, "分享备份文件"),
                            )
                        }
                        // 无论系统面板是否打开成功，本地副本都删掉：它是明文全库。
                        // 分享面板读到的是 FileProvider 的 fd，删除后已打开的那份
                        // 仍可读（Linux 语义），不必担心「刚分享就没了」。
                        file.delete()
                        if (shared.isSuccess) {
                            setNotice("备份", "已交给系统分享 · ${humanBytes(r.data?.bytes ?: 0)}")
                            setError("备份", null)
                        } else {
                            setError(
                                "备份",
                                "调起分享失败：${shared.exceptionOrNull()?.message ?: "未知"}",
                            )
                        }
                    }
                }

                /**
                 * SAF 导入：同样要落成本地文件再说 —— 内核按路径读。
                 * 读完**不直接恢复**：仍然走下面那条「覆盖式恢复」确认弹层，
                 * 选完文件就静默覆盖是不可接受的。
                 */
                val importFromFile = rememberLauncherForActivityResult(
                    ActivityResultContracts.OpenDocument(),
                ) { uri ->
                    if (uri == null) return@rememberLauncherForActivityResult
                    busyBackup = true
                    scope.launch {
                        val tmp = java.io.File(ctx.cacheDir, "agate-import.sql")
                        val copied = runCatching {
                            val inp = ctx.contentResolver.openInputStream(uri)
                                ?: error("无法读取所选文件")
                            inp.use { src -> tmp.outputStream().use { src.copyTo(it) } }
                        }
                        busyBackup = false
                        if (copied.isSuccess) {
                            importPath = tmp.absolutePath
                            confirmImport = true
                        } else {
                            setError("备份", "读取失败：${copied.exceptionOrNull()?.message ?: "未知"}")
                        }
                    }
                }

                // 首选入口：存到用户能拿到的地方。下面那条「填路径」留给进阶用法
                // （导出到内核可写的任意绝对路径 / 从已知路径恢复）。
                ItemRow(
                    label = "导出到文件",
                    desc = "选手机里的位置（下载目录、网盘等）· 也可之后用系统分享发出",
                ) {
                    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        MiniButton(if (busyBackup) "导出中…" else "选择位置", enabled = !busyBackup) {
                            if (!busyBackup) {
                                val stamp = java.text.SimpleDateFormat(
                                    "yyyyMMdd-HHmm",
                                    java.util.Locale.US,
                                ).format(java.util.Date())
                                exportToFile.launch("agate-backup-$stamp.sql")
                            }
                        }
                        MiniButton("分享", enabled = !busyBackup) { shareBackup() }
                    }
                }
                Spacer(Modifier.height(10.dp))
                ItemRow(
                    label = "导出整库",
                    desc = "SQL 文本落到指定路径 · 路径由后端直读写，大文件不过 HTTP",
                ) {
                    MiniButton(if (busyBackup) "导出中…" else "导出", enabled = !busyBackup) {
                        if (busyBackup) return@MiniButton
                        busyBackup = true
                        scope.launch {
                            val r = SettingsOps.exportBackup(exportPath.trim(), includeLogs)
                            if (r.ok) {
                                val info = r.data
                                setNotice(
                                    "备份",
                                    "已导出 ${humanBytes(info?.bytes ?: 0)}" +
                                        if (info != null && info.tables.isNotEmpty()) {
                                            " · " + info.tables.take(3)
                                                .joinToString("、") { "${it.first} ${it.second}" }
                                        } else ""
                                )
                                setError("备份", null)
                            } else {
                                setError("备份", r.error)
                            }
                            busyBackup = false
                        }
                    }
                }
                Spacer(Modifier.height(8.dp))
                AgateFormInput(
                    value = exportPath,
                    onValueChange = { exportPath = it },
                    hint = "绝对路径，以 .sql 结尾",
                    singleLine = false,
                    minLines = 1,
                )
                Spacer(Modifier.height(8.dp))
                SwitchRow(
                    label = "包含日志与报文",
                    desc = "关掉 = 只导账号与配置，文件小很多（只想换机器带走凭证时用它）",
                    checked = includeLogs,
                    enabled = !busyBackup,
                    onToggle = { includeLogs = it },
                )

                Spacer(Modifier.height(12.dp))
                Divider(C)
                Spacer(Modifier.height(12.dp))
                ItemRow(
                    label = "从备份恢复",
                    desc = "整体覆盖主库 · 账号、Key、设置全部被替换 · 进行中的请求会断开",
                ) {
                    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        MiniButton("选择文件", enabled = !busyBackup) {
                            if (!busyBackup) importFromFile.launch(arrayOf("application/sql", "*/*"))
                        }
                        MiniButton("恢复", enabled = importPath.isNotBlank()) {
                            confirmImport = true
                        }
                    }
                }
                Spacer(Modifier.height(8.dp))
                AgateFormInput(
                    value = importPath,
                    onValueChange = { importPath = it },
                    hint = "要恢复的 .sql 绝对路径",
                    singleLine = false,
                    minLines = 1,
                )
                Hint(
                    "恢复是覆盖式的：先导出当前库再恢复。备份文件必须是绝对路径且以 .sql 结尾 —— " +
                        "内核只认自家格式。",
                    C,
                )
                if (needsRestart) {
                    Spacer(Modifier.height(8.dp))
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .background(C.amber.copy(alpha = 0.10f), RoundedCornerShape(10.dp))
                            .border(1.dp, C.amber.copy(alpha = 0.40f), RoundedCornerShape(10.dp))
                            .padding(horizontal = 10.dp, vertical = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            "已换库，但内核内存里还是旧数据 —— 重启内核后新库才真正生效。",
                            color = C.amber,
                            fontSize = 11.sp,
                            modifier = Modifier.weight(1f),
                        )
                        MiniButton(
                            if (restarting) "重启中…" else "重启内核",
                            enabled = !restarting,
                        ) {
                            if (restarting) return@MiniButton
                            restarting = true
                            scope.launch {
                                // 走前台服务：它是网关的唯一载体（App 侧没有直接
                                // Gateway.start 的入口，见 KeepaliveGroup 的说明）
                                AgateForegroundService.stop(ctx)
                                kotlinx.coroutines.delay(1_200L)
                                val ok = AgateForegroundService.start(ctx)
                                restarting = false
                                if (ok) {
                                    needsRestart = false
                                    setNotice("备份", "内核已重启，新库已生效")
                                    setError("备份", null)
                                } else {
                                    setError(
                                        "备份",
                                        "内核没能自动重启（系统限制了后台启动）—— 到「网关控制」手动启动",
                                    )
                                }
                            }
                        }
                    }
                }
                SetFeedback("备份", errors, notices, C)

                if (confirmImport) {
                    ConfirmDialog(
                        title = "从备份恢复整库？",
                        danger = "这会用备份文件整体覆盖当前数据库，不可撤销：",
                        bullets = listOfNotNull(
                            storage?.accounts?.let { "当前 $it 个账号与其凭证" },
                            storage?.requests?.let { "当前 ${group(it)} 条请求明细与报表聚合" },
                            "API Key、提示词、全部设置项",
                        ),
                        note = "进行中的请求会被断开。恢复完成后建议重启内核让缓存对齐。",
                        confirmText = "确认覆盖",
                        onDismiss = { confirmImport = false },
                        onConfirm = {
                            confirmImport = false
                            scope.launch {
                                val r = SettingsOps.importBackup(importPath.trim())
                                if (r.ok) {
                                    setNotice("备份", r.data ?: "恢复完成")
                                    setError("备份", null)
                                    needsRestart = true
                                    reload()
                                } else {
                                    setError("备份", r.error)
                                }
                            }
                        },
                    )
                }
            }

            // ══ 10. 保活与通知 ══════════════════════════════
            //
            // 这一组混了两种东西，界面上必须能分出来：
            //   · 两颗开关 —— 真的能改（偏好落 SharedPreferences，服务侧读同一个键）；
            //   · 三行实况 —— 改不了，由系统授予或由网关自身状态决定，只能看 + 去授权。
            // 把后者做成开关会误导（点了只会跳出去，或干脆是关掉网关）。
            KeepaliveGroup(ctx)

            // ══ 11. 关于 ════════════════════════════════════
            AgateAccordion(title = "关于") {
                ReadRow(
                    label = "当前版本",
                    desc = "",
                    value = "${versionName(ctx)} (${versionCode(ctx)})",
                )
                ReadRow(
                    label = "内核状态",
                    desc = "经 JNI 直调管理面的连通性",
                    value = if (cfg != null || guards != null) "已连接" else "未连接",
                )
                ItemRow(label = "配置目录", desc = "库文件所在位置") {
                    MiniButton("复制") {
                        copyToClipboard(ctx, storage?.configDir.orEmpty())
                        toast(ctx, "已复制")
                    }
                }
                if (storage?.configDir?.isNotBlank() == true) {
                    Text(
                        storage?.configDir.orEmpty(),
                        style = AgateType.Caption,
                        color = C.textDim,
                    )
                    Spacer(Modifier.height(6.dp))
                }
                ItemRow(label = "导出诊断信息", desc = "版本 + 存储概况 + 最近 200 条日志，反馈问题时贴出来") {
                    MiniButton("导出") {
                        scope.launch {
                            val text = buildDiagnostics(ctx, storage)
                            copyToClipboard(ctx, text)
                            toast(ctx, "诊断信息已复制到剪贴板")
                        }
                    }
                }
                Spacer(Modifier.height(10.dp))
                Text(
                    "个人开发开源维护，不收集任何账号数据。",
                    style = AgateType.Caption,
                    color = C.textDim,
                )
            }

            Spacer(Modifier.height(8.dp))
            Text(
                if (loading) "正在读取…" else "以上读数来自内核，改动均为即时生效。",
                style = AgateType.Caption,
                color = C.textDim,
                modifier = Modifier.fillMaxWidth().padding(bottom = 20.dp),
            )
        }
    }
}

// ══════════════════════════════════════════════════════════════
//  子块：零宽空格词表（第二层脱敏）
// ══════════════════════════════════════════════════════════════

/**
 * 零宽空格词表。
 *
 * ── 为什么与上面那层并排但分块 ──────────────────────────────
 * 两层脱敏改的是同一件事（发给上游的请求体），手法不同：内置规则集处理
 * 「已知的具体指纹」（表头键值、几句模板、错误码），封闭、随版本走；这一层
 * 处理「用户自己知道的词」，开放、随用户走。开关各自独立，所以并排、读数并排。
 *
 * ── 为什么界面是「加一条 / 删一条」而接口是整份 PUT ──────────
 * 界面上一次只改一条是自然的（用户心里想的是「我要加这个词」），落盘却是整份
 * 替换 —— 两者之间由这里持有当前列表、每次改动整份提交。这样避免了逐词接口
 * 那四种操作各自的冲突语义，界面行为也不受影响。
 */
@Composable
private fun ZwspBlock(
    words: List<String>,
    busy: Boolean,
    onSave: (List<String>, () -> Unit) -> Unit,
) {
    val C = agateColors()
    var draft by remember { mutableStateOf("") }
    var notice by remember { mutableStateOf<String?>(null) }

    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(
            "零宽空格脱敏（自定义词表）",
            style = AgateType.Body,
            fontWeight = FontWeight.SemiBold,
            color = C.textMain,
            modifier = Modifier.weight(1f),
        )
        AgateBadge(
            text = if (words.isEmpty()) "未启用" else "${words.size} 个词",
            tone = if (words.isEmpty()) BadgeTone.GRAY else BadgeTone.BLUE,
        )
    }
    Spacer(Modifier.height(6.dp))
    Text(
        "这里填「我知道会被拦、但内置规则里没有」的词。命中的每一处都会在词尾插入一个" +
            "零宽空格（U+200B）：人眼看着是同一个词，而上游的逐字精确匹配对不上了。",
        style = AgateType.Caption,
        color = C.textSub,
    )
    Spacer(Modifier.height(6.dp))
    Text(
        "与上面那一层的区别：那一层是封闭的、随版本走；这一层由你维护，改完下一个请求就生效。",
        style = AgateType.Caption,
        color = C.textDim,
    )
    Spacer(Modifier.height(6.dp))
    Text(
        "对纯数字串无效 —— 上游会把数字归一化，零宽空格会被吃掉；数字类指纹仍归内置规则集" +
            "（那里做的是真正的字面改写）。每命中一次多 3 个字节（UTF-8 下 U+200B 占 3 字节），" +
            "token 数可能微增，但语义不变。",
        style = AgateType.Caption,
        color = C.amber,
    )

    Spacer(Modifier.height(12.dp))
    Row(verticalAlignment = Alignment.CenterVertically) {
        AgateFormInput(
            value = draft,
            onValueChange = { draft = it; notice = null },
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
                .background(if (canAdd) C.accent else C.insetBg, RoundedCornerShape(12.dp))
                .clickable(enabled = canAdd) {
                    val word = check.getOrNull() ?: return@clickable
                    onSave(words + word) {
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

    val inline = if (draft.isBlank()) {
        null
    } else {
        SettingsOps.validateZwspWord(draft).exceptionOrNull()?.message
    }
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

    if (words.isEmpty()) {
        Spacer(Modifier.height(10.dp))
        Text(
            "词表是空的 —— 这一层当前不生效，只有内置规则集在工作。",
            style = AgateType.Caption,
            color = C.textDim,
        )
    } else {
        Spacer(Modifier.height(12.dp))
        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
            words.forEach { word ->
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .background(C.insetBg, RoundedCornerShape(12.dp))
                        .border(1.dp, C.border, RoundedCornerShape(12.dp))
                        .padding(horizontal = 12.dp, vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        text = word,
                        style = AgateType.BodyS,
                        color = C.textMain,
                        modifier = Modifier.weight(1f),
                    )
                    Box(
                        modifier = Modifier
                            .size(28.dp)
                            .clickable(enabled = !busy) {
                                onSave(words - word) { notice = "已移除「$word」" }
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
    }
    notice?.let {
        Spacer(Modifier.height(8.dp))
        Text(it, style = AgateType.Caption, color = C.green)
    }

    // ── 效果预览 ────────────────────────────────────────────
    // 词表生效与否在界面上原本完全不可见：用户加完词只能「发一条请求祈祷它生效」。
    // 这里给一段样例文本、把内核算出来的结果摊开 —— 尤其是**把不可见字符显式
    // 画出来**（⟨ZW⟩），否则前后两段看起来一模一样，反而更像没生效。
    Spacer(Modifier.height(14.dp))
    Divider(C)
    Spacer(Modifier.height(12.dp))
    ZwspPreviewBlock(savedWords = words, busy = busy)
}

/**
 * 词表效果预览：输入一句样例，看内核把它改成什么样。
 *
 * ── 三个刻意的决定 ──────────────────────────────────────────
 *  1. **由内核算**（`POST /api/zwsp/preview`），不在界面里做字符串替换。
 *     插零宽空格有两处容易做错且做错后看不出来（已打断的位置不重复插、
 *     多词按顺序叠加），界面自己实现一份就会与实际出站分叉，而预览的全部
 *     意义是「所见即所发」。
 *  2. **把不可见字符画出来**（⟨ZW⟩）：不这么做，改前改后两段文本看起来
 *     完全一样，用户会以为没生效。
 *  3. **字数与字节数都给**：U+200B 是「1 字符 / 3 字节」，两个增量不同 ——
 *     否则「只多了一个字符怎么大了 3 字节」会被当成 bug 报上来。
 */
@Composable
private fun ZwspPreviewBlock(savedWords: List<String>, busy: Boolean) {
    val C = agateColors()
    val scope = rememberCoroutineScope()
    var sample by remember { mutableStateOf("") }
    var result by remember { mutableStateOf<SettingsOps.ZwspPreview?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var running by remember { mutableStateOf(false) }

    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(
            "效果预览",
            style = AgateType.Body,
            fontWeight = FontWeight.SemiBold,
            color = C.textMain,
            modifier = Modifier.weight(1f),
        )
        AgateBadge(
            text = if (savedWords.isEmpty()) "无词可预览" else "按当前 ${savedWords.size} 个词",
            tone = BadgeTone.GRAY,
        )
    }
    Spacer(Modifier.height(6.dp))
    AgateFormInput(
        value = sample,
        onValueChange = { sample = it; error = null; result = null },
        hint = "输入一句样例文本（最多 4000 字）",
        singleLine = false,
        minLines = 2,
    )
    Spacer(Modifier.height(8.dp))
    Row(verticalAlignment = Alignment.CenterVertically) {
        MiniButton(if (running) "计算中…" else "预览", enabled = !busy && !running && sample.isNotBlank()) {
            running = true
            scope.launch {
                // 带上当前词表：预览的就是「已保存的这套规则」，不掺未提交的草稿
                val r = SettingsOps.previewZwsp(sample, savedWords)
                running = false
                if (r.ok) {
                    result = r.data
                    error = null
                } else {
                    error = r.error
                    result = null
                }
            }
        }
    }
    error?.let {
        Spacer(Modifier.height(6.dp))
        Text(it, style = AgateType.Caption, color = C.red)
    }
    result?.let { p ->
        Spacer(Modifier.height(8.dp))
        Text(
            "命中 ${p.inserted} 处 · 字数 ${p.charsBefore}→${p.charsAfter} · " +
                "字节 ${p.bytesBefore}→${p.bytesAfter}",
            style = AgateType.Caption,
            color = if (p.inserted > 0) C.green else C.amber,
        )
        Spacer(Modifier.height(6.dp))
        Box(
            Modifier
                .fillMaxWidth()
                .background(C.insetBg, RoundedCornerShape(10.dp))
                .border(1.dp, C.border, RoundedCornerShape(10.dp))
                .padding(10.dp),
        ) {
            Text(
                // 把零宽空格替换成可见标记：真发出去时它是不可见的
                text = p.transformed.replace(SettingsOps.ZWSP, "⟨ZW⟩"),
                style = AgateType.BodyS,
                color = C.textSub,
            )
        }
        Spacer(Modifier.height(4.dp))
        Text(
            "⟨ZW⟩ = 零宽空格（U+200B）。它真的会被插进发往上游的文本里，只是肉眼看不见。",
            style = AgateType.Caption,
            color = C.textDim,
        )
    }
}

// ══════════════════════════════════════════════════════════════
//  子块：数字编辑弹层 / 状态码编辑 / 提示词正文 / 二次确认
// ══════════════════════════════════════════════════════════════

/**
 * 数字编辑弹层。
 *
 * 只提交这一项 —— 内核允许部分更新，而整组提交时「用户改一项、另一项恰好被
 * 别人改过」会被自己的旧值覆盖回去。范围与默认值全部来自 [SettingsOps.Field]，
 * 越界时保存按钮禁用、下方给具体原因，不把判定推给内核往返。
 */
@Composable
private fun NumberEditDialog(
    field: SettingsOps.Field,
    current: Long,
    onDismiss: () -> Unit,
    onSave: (Long) -> Unit,
) {
    val C = agateColors()
    var text by remember { mutableStateOf(current.toString()) }
    val parsed = text.trim().toLongOrNull()
    val valid = parsed != null && field.accepts(parsed)
    val reason = when {
        parsed == null -> "要一个整数"
        !field.accepts(parsed) -> "必须在 ${field.rangeText} 之间（收到 $parsed）"
        else -> null
    }

    Dialog(onDismissRequest = onDismiss) {
        Column(
            Modifier
                .fillMaxWidth()
                .background(C.cardBg, RoundedCornerShape(18.dp))
                .border(1.dp, C.border, RoundedCornerShape(18.dp))
                .padding(16.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    field.label,
                    style = AgateType.Body,
                    fontWeight = FontWeight.SemiBold,
                    color = C.textMain,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    "✕",
                    color = C.textDim,
                    fontSize = 15.sp,
                    modifier = Modifier.clickable { onDismiss() }.padding(4.dp),
                )
            }
            Spacer(Modifier.height(4.dp))
            Text(field.desc, style = AgateType.Caption, color = C.textDim)
            Spacer(Modifier.height(12.dp))

            Row(verticalAlignment = Alignment.CenterVertically) {
                AgateFormInput(
                    value = text,
                    onValueChange = { text = it.filter { ch -> ch.isDigit() } },
                    hint = field.default.toString(),
                    keyboardType = KeyboardType.Number,
                    modifier = Modifier.weight(1f),
                )
                if (field.unit.isNotBlank()) {
                    Spacer(Modifier.width(8.dp))
                    Text(field.unit.trim(), style = AgateType.BodyS, color = C.textSub)
                }
            }
            Spacer(Modifier.height(6.dp))
            Text(
                reason ?: "范围 ${field.rangeText} · 默认 ${field.default}${field.unit}",
                style = AgateType.Caption,
                color = if (reason == null) C.textDim else C.red,
            )

            Spacer(Modifier.height(10.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                listOf(-10L, -1L, 1L, 10L).forEach { delta ->
                    MiniButton(
                        if (delta > 0) "+$delta" else "$delta",
                        filled = false,
                    ) {
                        val base = parsed ?: current
                        text = (base + delta).coerceIn(field.min, field.max).toString()
                    }
                }
                MiniButton("默认", filled = false) { text = field.default.toString() }
            }

            Spacer(Modifier.height(14.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                MiniButton("取消", filled = false, modifier = Modifier.weight(1f)) { onDismiss() }
                MiniButton("保存", enabled = valid, modifier = Modifier.weight(1f)) {
                    parsed?.let(onSave)
                }
            }
            Spacer(Modifier.height(6.dp))
            Text(
                "保存只提交这一项，其余字段保持原值（内核允许部分更新）。",
                style = AgateType.Caption,
                color = C.textDim,
            )
        }
    }
}

/** 「指定状态码直接换号」编辑器：逗号/空格/换行分隔，本地校验区间与条数。 */
@Composable
private fun CodesDialog(
    initial: List<Int>,
    onDismiss: () -> Unit,
    onSave: (List<Int>) -> Unit,
) {
    val C = agateColors()
    var text by remember { mutableStateOf(initial.joinToString(", ")) }
    val parsed = remember(text) {
        text.split(',', ' ', '\n', '，')
            .map { it.trim() }
            .filter { it.isNotEmpty() }
            .mapNotNull { it.toIntOrNull() }
    }
    val outOfRange = parsed.filter {
        it < SettingsOps.RETRY_CODE_MIN || it > SettingsOps.RETRY_CODE_MAX
    }
    val tooMany = parsed.size > SettingsOps.RETRY_MAX_NO_RETRY_CODES
    val valid = outOfRange.isEmpty() && !tooMany

    Dialog(onDismissRequest = onDismiss) {
        Column(
            Modifier
                .fillMaxWidth()
                .background(C.cardBg, RoundedCornerShape(18.dp))
                .border(1.dp, C.border, RoundedCornerShape(18.dp))
                .padding(16.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "直接换号的状态码",
                    style = AgateType.Body,
                    fontWeight = FontWeight.SemiBold,
                    color = C.textMain,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    "✕",
                    color = C.textDim,
                    fontSize = 15.sp,
                    modifier = Modifier.clickable { onDismiss() }.padding(4.dp),
                )
            }
            Spacer(Modifier.height(4.dp))
            Text(
                "逗号分隔，例如 401, 402, 429 · 撞上即不在同一账号重发",
                style = AgateType.Caption,
                color = C.textDim,
            )
            Spacer(Modifier.height(10.dp))
            AgateFormInput(
                value = text,
                onValueChange = { text = it },
                hint = "${SettingsOps.RETRY_CODE_MIN}–${SettingsOps.RETRY_CODE_MAX}",
                singleLine = false,
                minLines = 3,
                keyboardType = KeyboardType.Number,
            )
            Spacer(Modifier.height(6.dp))
            Text(
                when {
                    outOfRange.isNotEmpty() -> "越界：${outOfRange.joinToString(", ")}"
                    tooMany -> "最多 ${SettingsOps.RETRY_MAX_NO_RETRY_CODES} 个"
                    else -> "共 ${parsed.size} 个 · 保存后内核会排序去重"
                },
                style = AgateType.Caption,
                color = if (valid) C.textDim else C.red,
            )
            Spacer(Modifier.height(14.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                MiniButton("取消", filled = false, modifier = Modifier.weight(1f)) { onDismiss() }
                MiniButton("保存", enabled = valid, modifier = Modifier.weight(1f)) {
                    onSave(parsed)
                }
            }
        }
    }
}

/**
 * 提示词正文全屏编辑。
 *
 * 为什么单独一个全屏层而不是弹层：正文动辄几百行，弹层装不下；而它是这一组
 * 唯一需要「看全、改多处」的东西，塞在小框里只能看到十几行。
 */
@Composable
private fun PromptTextDialog(
    initial: String,
    modeLabel: String,
    onDismiss: () -> Unit,
    onSave: (String) -> Unit,
) {
    val C = agateColors()
    var text by remember { mutableStateOf(initial) }
    Dialog(
        onDismissRequest = onDismiss,
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        Column(
            Modifier
                .fillMaxSize()
                .background(C.bg),
        ) {
            AgatePageHeader(title = "提示词正文", onOpenDrawer = {}, onBack = onDismiss)
            Row(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                AgateBadge(modeLabel, BadgeTone.BLUE)
                Spacer(Modifier.width(8.dp))
                Text(
                    "清空正文 = 回落到文件 / 内置默认，不报错",
                    style = AgateType.Caption,
                    color = C.textDim,
                )
            }
            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .padding(horizontal = 14.dp),
            ) {
                AgateFormInput(
                    value = text,
                    onValueChange = { text = it },
                    hint = "在这里写系统提示词…",
                    singleLine = false,
                    minLines = 18,
                    modifier = Modifier.fillMaxSize(),
                )
            }
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 14.dp, vertical = 10.dp)
                    .navigationBarsPadding(),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    "${text.lines().count()} 行 · ${text.length} 字",
                    style = AgateType.Caption,
                    color = C.textDim,
                    modifier = Modifier.weight(1f),
                )
                MiniButton("撤销", filled = false) { text = initial }
                Spacer(Modifier.width(8.dp))
                MiniButton("保存") { onSave(text) }
            }
        }
    }
}

/** 危险动作的二次确认：必须复述「会丢什么」（带具体数字）。 */
@Composable
private fun ConfirmDialog(
    title: String,
    danger: String,
    bullets: List<String>,
    note: String,
    confirmText: String,
    onDismiss: () -> Unit,
    onConfirm: () -> Unit,
) {
    val C = agateColors()
    Dialog(onDismissRequest = onDismiss) {
        Column(
            Modifier
                .fillMaxWidth()
                .background(C.cardBg, RoundedCornerShape(18.dp))
                .border(1.dp, C.border, RoundedCornerShape(18.dp))
                .padding(16.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    title,
                    style = AgateType.Body,
                    fontWeight = FontWeight.SemiBold,
                    color = C.textMain,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    "✕",
                    color = C.textDim,
                    fontSize = 15.sp,
                    modifier = Modifier.clickable { onDismiss() }.padding(4.dp),
                )
            }
            Spacer(Modifier.height(8.dp))
            Text(danger, style = AgateType.Caption, color = C.red)
            Spacer(Modifier.height(8.dp))
            bullets.forEach { line ->
                Row(modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp)) {
                    Text("· ", style = AgateType.Caption, color = C.red)
                    Text(line, style = AgateType.Caption, color = C.textSub)
                }
            }
            if (note.isNotBlank()) {
                Spacer(Modifier.height(8.dp))
                Text(note, style = AgateType.Caption, color = C.textDim)
            }
            Spacer(Modifier.height(14.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                MiniButton("取消", filled = false, modifier = Modifier.weight(1f)) { onDismiss() }
                MiniButton(confirmText, danger = true, modifier = Modifier.weight(1f)) { onConfirm() }
            }
        }
    }
}

// ══════════════════════════════════════════════════════════════
//  通用行与小件
// ══════════════════════════════════════════════════════════════

@Composable
private fun SectionHeader(title: String, subtitle: String, trailing: String) {
    val C = agateColors()
    Row(verticalAlignment = Alignment.CenterVertically) {
        Column(modifier = Modifier.weight(1f)) {
            Text(title, style = AgateType.PageTitle, color = C.textMain)
            Spacer(Modifier.height(2.dp))
            Text(subtitle, style = AgateType.Caption, color = C.textDim)
        }
        AgateBadge(trailing, BadgeTone.GRAY)
    }
}

/** 标签 + 说明 + 右侧控件。所有组的主要行都用它，保证纵向节奏一致。 */
@Composable
private fun ItemRow(
    label: String,
    desc: String,
    trailing: @Composable () -> Unit,
) {
    val C = agateColors()
    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(label, style = AgateType.BodyS, color = C.textMain)
            if (desc.isNotBlank()) {
                Spacer(Modifier.height(2.dp))
                Text(desc, style = AgateType.Caption, color = C.textDim)
            }
        }
        Spacer(Modifier.width(10.dp))
        trailing()
    }
}

/** 只读行：值以灰字呈现，且不加点击态 —— 可点与不可点必须能区分。 */
@Composable
private fun ReadRow(label: String, desc: String, value: String) {
    val C = agateColors()
    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(label, style = AgateType.BodyS, color = C.textMain)
            if (desc.isNotBlank()) {
                Spacer(Modifier.height(2.dp))
                Text(desc, style = AgateType.Caption, color = C.textDim)
            }
        }
        Text(value, style = AgateType.BodyS, color = C.textSub)
    }
}

/** 值 + 动作（复制之类）：值本身是常态文本，动作用小按钮，避免整行可点的歧义。 */
@Composable
private fun ValueRow(
    label: String,
    value: String,
    hint: String,
    action: String,
    onAction: () -> Unit,
) {
    val C = agateColors()
    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(label, style = AgateType.BodyS, color = C.textMain)
            Spacer(Modifier.height(3.dp))
            Text(value, style = AgateType.BodyS, color = C.accent)
            if (hint.isNotBlank()) {
                Spacer(Modifier.height(2.dp))
                Text(hint, style = AgateType.Caption, color = C.textDim)
            }
        }
        Spacer(Modifier.width(10.dp))
        MiniButton(action, filled = false) { onAction() }
    }
}

@Composable
private fun SwitchRow(
    label: String,
    desc: String,
    checked: Boolean,
    enabled: Boolean,
    onToggle: (Boolean) -> Unit,
) {
    val C = agateColors()
    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(label, style = AgateType.BodyS, color = if (enabled) C.textMain else C.textSub)
            Spacer(Modifier.height(2.dp))
            Text(desc, style = AgateType.Caption, color = C.textDim)
        }
        Spacer(Modifier.width(10.dp))
        Switch(
            checked = checked,
            onCheckedChange = { if (enabled) onToggle(it) },
            enabled = enabled,
        )
    }
}

/** 数字行：点值开弹层。显示【当前值 + 单位】，不改原值。 */
@Composable
private fun NumberRow(
    field: SettingsOps.Field,
    label: String,
    desc: String,
    value: Long,
    unit: String,
    enabled: Boolean,
    onPick: (Long) -> Unit,
    valueText: String? = null,
) {
    val C = agateColors()
    var open by remember { mutableStateOf(false) }
    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(label, style = AgateType.BodyS, color = C.textMain)
            if (desc.isNotBlank()) {
                Spacer(Modifier.height(2.dp))
                Text(desc, style = AgateType.Caption, color = C.textDim)
            }
        }
        Spacer(Modifier.width(10.dp))
        Box(
            modifier = Modifier
                .background(
                    if (enabled) C.insetBg else C.insetBg.copy(alpha = 0.5f),
                    RoundedCornerShape(10.dp),
                )
                .border(1.dp, C.border, RoundedCornerShape(10.dp))
                .clickable(enabled = enabled) { open = true }
                .padding(horizontal = 10.dp, vertical = 5.dp),
        ) {
            Text(
                valueText ?: (value.toString() + if (unit.isNotBlank()) " $unit" else ""),
                style = AgateType.BodyS,
                color = if (enabled) C.accent else C.textDim,
            )
        }
    }
    if (open) {
        // 范围与默认值一律来自入参 field —— 早先按 label 反查过一遍，那是隐患：
        // 两组里出现同名的行（比如两个「最多等几次」）就会静默取到另一组的区间。
        NumberEditDialog(
            field = field,
            current = value,
            onDismiss = { open = false },
            onSave = {
                open = false
                onPick(it)
            },
        )
    }
}

@Composable
private fun MiniButton(
    text: String,
    filled: Boolean = true,
    enabled: Boolean = true,
    danger: Boolean = false,
    modifier: Modifier = Modifier,
    onClick: () -> Unit,
) {
    val C = agateColors()
    val bg = when {
        !enabled -> C.insetBg
        danger -> C.red
        filled -> C.accent
        else -> C.insetBg
    }
    val fg = when {
        !enabled -> C.textDim
        danger -> Color.White
        filled -> C.onAccent
        else -> C.textSub
    }
    Box(
        modifier = modifier
            .background(bg, RoundedCornerShape(10.dp))
            .border(
                1.dp,
                if (filled || danger) Color.Transparent else C.border,
                RoundedCornerShape(10.dp),
            )
            .clickable(enabled = enabled) { onClick() }
            .padding(horizontal = 12.dp, vertical = 6.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(text, style = AgateType.Caption, fontWeight = FontWeight.SemiBold, color = fg)
    }
}

@Composable
private fun Hint(text: String, C: AgateColors) {
    Spacer(Modifier.height(8.dp))
    Text(text, style = AgateType.Caption, color = C.textDim)
}

@Composable
private fun Banner(text: String, fg: Color, bg: Color) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(bg, RoundedCornerShape(10.dp))
            .padding(horizontal = 12.dp, vertical = 8.dp),
    ) {
        Text(text, style = AgateType.Caption, color = fg)
    }
}

@Composable
private fun Divider(C: AgateColors) {
    Box(Modifier.fillMaxWidth().height(1.dp).background(C.border))
}

@Composable
private fun GroupError(message: String?, C: AgateColors, onRetry: () -> Unit) {
    if (message == null) return
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(C.redBg, RoundedCornerShape(10.dp))
            .padding(horizontal = 12.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(message, style = AgateType.Caption, color = C.red, modifier = Modifier.weight(1f))
        Spacer(Modifier.width(8.dp))
        MiniButton("重试", filled = false) { onRetry() }
    }
    Spacer(Modifier.height(8.dp))
}

/** 每组的失败 / 成功提示各一条；失败优先（失败时成功文案已经过期）。 */
@Composable
private fun SetFeedback(
    group: String,
    errors: Map<String, String>,
    notices: Map<String, String>,
    C: AgateColors,
) {
    val error = errors[group]
    val notice = notices[group]
    if (error != null) {
        Spacer(Modifier.height(8.dp))
        Banner(error, C.red, C.redBg)
    } else if (notice != null) {
        Spacer(Modifier.height(8.dp))
        Banner(notice, C.green, C.greenBg)
    }
}

// ══════════════════════════════════════════════════════════════
//  摘要文案（折叠态的读数，全部来自内核真值）
// ══════════════════════════════════════════════════════════════

// 这些函数之所以是 internal 而不是 private：它们全是「看一眼觉得对、实际会错」
// 的判断（千分位换挡、99999 天要显示「永久」、读不到要显示「未读取」而不是 0），
// 而 Compose 的 @Composable 函数在 JVM 单测里跑不起来。把判断留在纯函数里、
// 让它可测，是这一页唯一能钉住这些细节的方式。

internal fun accessSummary(cfg: SettingsOps.Config?): String {
    if (cfg == null) return "未读取"
    val key = if (cfg.apiKeySet) "Key 已设" else "无 Key"
    val locale = cfg.locale.ifBlank { "zh-CN" }
    return "$key · $locale"
}

internal fun themeLabel(mode: AgateThemeMode): String = when (mode) {
    AgateThemeMode.FOLLOW_SYSTEM -> "跟随系统"
    AgateThemeMode.LIGHT -> "浅色"
    AgateThemeMode.DARK -> "深色"
}

internal fun streamSummary(s: SettingsOps.Stream?): String {
    if (s == null) return "未读取"
    return "保活丢弃${state(s.stripKeepalive)} · 续写${state(s.autoContinue)} · 调试${state(s.debug)}"
}

internal fun state(on: Boolean): String = if (on) "开" else "关"

internal fun queueSummary(
    queue: Map<String, Long>,
    timeouts: Map<String, Long>,
): String {
    if (queue.isEmpty() && timeouts.isEmpty()) return "未读取"
    val waits = SettingsOps.QUEUE_FIELDS.map { queue[it.key] ?: it.default }
    val secs = SettingsOps.TIMEOUT_FIELDS.map { timeouts[it.key] ?: it.default }
    return "等${waits.firstOrNull() ?: 0} 次 · ${secs.joinToString("/")} 秒"
}

internal fun retrySummary(retry: Map<String, Long>, codeCount: Int): String {
    if (retry.isEmpty()) return "未读取"
    val values = SettingsOps.RETRY_FIELDS.map { retry[it.key] ?: it.default }
    val suffix = if (codeCount > 0) " · $codeCount 个码直接换号" else ""
    return "原地 ${values[0]} 次 · 换号 ${values[1]} 个 · 间隔 ${values[2]} 秒$suffix"
}

internal fun guardSummary(guards: SettingsOps.Guards?, wordCount: Int): String {
    if (guards == null) return "未读取"
    val words = if (wordCount > 0) " · 词表 $wordCount" else ""
    return "指纹剥离${state(guards.sanitize)} · 跨域${state(guards.cors)}$words"
}

internal fun promptSummary(p: SettingsOps.Prompt?): String {
    if (p == null) return "未读取"
    val base = "${p.modeLabel} · 来源：${p.sourceLabel}"
    return if (p.degradeActive) "$base · 降级中" else base
}

internal fun storageSummary(s: SettingsOps.Storage?): String {
    if (s == null) return "未读取"
    if (!s.available) return "库不可用"
    return "库 ${humanBytes(s.bytes)} · 请求 ${group(s.requests)} 条"
}

// ══════════════════════════════════════════════════════════════
//  纯函数（格式化 / 系统信息 / 剪贴板）
// ══════════════════════════════════════════════════════════════

/**
 * 千分位。设置页的条数动辄上万，裸数字读起来要数位。
 *
 * 规则是**标准的那条**（千位起加逗号，1000 → `1,000`），不是「5 位才加」。
 * 早先按「4 位不加」写会造出同一页两种读法：`3214 条` 与 `10,000 条` 并存，
 * 用户没法从数字形态判断量级。分界放在千位没有争议。
 */
internal fun group(value: Long): String {
    val digits = value.toString()
    if (digits.length <= 3) return digits
    val sb = StringBuilder()
    digits.reversed().forEachIndexed { index, ch ->
        if (index > 0 && index % 3 == 0) sb.append(',')
        sb.append(ch)
    }
    return sb.reverse().toString()
}

/** 人类可读的字节数。用 1024 进制，与 `/api/storage` 的 bytes 口径一致。 */
internal fun humanBytes(bytes: Long): String {
    if (bytes <= 0) return "0 B"
    val units = listOf("B", "KB", "MB", "GB")
    var value = bytes.toDouble()
    var index = 0
    while (value >= 1024 && index < units.lastIndex) {
        value /= 1024
        index++
    }
    return if (index == 0) "${bytes} B" else String.format("%.1f %s", value, units[index])
}

internal fun versionName(ctx: android.content.Context): String = runCatching {
    ctx.packageManager.getPackageInfo(ctx.packageName, 0).versionName ?: "—"
}.getOrDefault("—")

internal fun versionCode(ctx: android.content.Context): Long = runCatching {
    val info = ctx.packageManager.getPackageInfo(ctx.packageName, 0)
    if (android.os.Build.VERSION.SDK_INT >= 28) info.longVersionCode
    else @Suppress("DEPRECATION") info.versionCode.toLong()
}.getOrDefault(0L)

/**
 * 备份文件的默认落点。
 *
 * 用 `filesDir` 而不是外部存储：内核（同一进程）对这个目录有写权限，而
 * `getExternalFilesDir` 在部分机型上会被厂商的存储重定向拦下 —— 路径看着对、
 * 写的时候才 400。用户仍可改成任意绝对路径。
 */
internal fun defaultBackupPath(ctx: android.content.Context): String {
    val stamp = java.text.SimpleDateFormat("yyyyMMdd-HHmm", java.util.Locale.US)
        .format(java.util.Date())
    return "${ctx.filesDir.absolutePath}/agate-backup-$stamp.sql"
}

/**
 * 诊断信息：版本 + 存储概况 + 最近若干条日志。
 *
 * 为什么拼成一段文本而不是打包文件：用户反馈问题的动作是「贴给我」，
 * 一段能直接粘进对话框的文本比一个要他先找到再上传的 zip 有用得多。
 */
private suspend fun buildDiagnostics(
    ctx: android.content.Context,
    storage: SettingsOps.Storage?,
): String {
    val page = LogOps.page(limit = 200)
    val entries = page.data?.entries.orEmpty()
    val sb = StringBuilder()
    sb.appendLine("# Agate 诊断信息")
    sb.appendLine("版本: ${versionName(ctx)} (${versionCode(ctx)})")
    sb.appendLine(
        "时间: " + java.text.SimpleDateFormat("yyyy-MM-dd HH:mm:ss", java.util.Locale.US)
            .format(java.util.Date())
    )
    sb.appendLine()
    sb.appendLine("## 存储")
    if (storage == null) {
        sb.appendLine("读取失败")
    } else {
        sb.appendLine("可用: ${storage.available}")
        sb.appendLine("库文件: ${storage.file} (${humanBytes(storage.bytes)})")
        sb.appendLine(
            "账号 ${storage.accounts} · 请求 ${storage.requests} · " +
                "日志 ${storage.logs} · 调试 ${storage.debugRows}"
        )
    }
    sb.appendLine()
    sb.appendLine("## 最近日志（${entries.size} 条）")
    entries.forEach { entry ->
        val at = java.text.SimpleDateFormat("MM-dd HH:mm:ss", java.util.Locale.US)
            .format(java.util.Date(entry.ts))
        sb.appendLine("$at [${entry.level}] ${entry.category}: ${entry.message}")
    }
    if (!page.ok) sb.appendLine("（日志读取失败：${page.error}）")
    return sb.toString()
}

private fun copyToClipboard(ctx: android.content.Context, text: String) {
    val manager = ctx.getSystemService(android.content.Context.CLIPBOARD_SERVICE)
        as? android.content.ClipboardManager ?: return
    manager.setPrimaryClip(android.content.ClipData.newPlainText("agate", text))
}

private fun toast(ctx: android.content.Context, message: String) {
    Toast.makeText(ctx, message, Toast.LENGTH_SHORT).show()
}

/* ══════════ 第 10 组：保活与通知 ══════════ */

/**
 * 「保活与通知」组（v44 `.sgroup` 保活与通知）。
 *
 * ── 为什么两组不同性质的行混在一张卡里 ──────────────────────
 * 上游把四项平铺成开关，但其中三项在 Android 上根本不能由 App 开关：
 *   · 前台服务 —— 它是网关的**唯一载体**（`Gateway.start` 在 app 侧零调用，
 *     三个入口全走 `AgateForegroundService.start`），关掉 = 关掉网关；
 *   · 忽略电池优化 / 精确闹钟 —— 系统授予的权限，App 只能申请，不能设值。
 * 所以这里是「两颗真开关 + 三行只读实况」。只读行的右侧是可点的动作
 * （跳网关控制 / 去授权），不是开关 —— 让人点一下能到达该去的地方。
 *
 * ── 读数为什么每次进入才刷新 ───────────────────────────────
 * `ignoringBatteryOptimization` / `canExactAlarm` 读的是系统状态，
 * 用户在系统弹窗里授权完返回时值就变了。用 `remember` + 每次组合重读
 * 会在滚动重组时反复打系统调用；折中做法是拿一个 `refresh` 计数，
 * 点过授权或从后台回来时 +1 重读。这里只在进出页面时读一次
 * （本组位于长列表底部，滚动重组频率低），配合「去授权」后自动 +1。
 */
@Composable
private fun KeepaliveGroup(ctx: android.content.Context) {
    val C = agateColors()
    var refresh by remember { mutableStateOf(0) }

    // 三行系统实况 + 两颗开关的初值。key = refresh 让它可被手动作废重读。
    val running = remember(refresh) { KeepalivePrefs.serviceRunning() }
    val ignoring = remember(refresh) { KeepalivePrefs.ignoringBatteryOptimization(ctx) }
    val canExact = remember(refresh) { KeepalivePrefs.canExactAlarm(ctx) }
    var checkinNotify by remember(refresh) { mutableStateOf(KeepalivePrefs.checkinNotify(ctx)) }
    var alertNotify by remember(refresh) { mutableStateOf(KeepalivePrefs.alertNotify(ctx)) }

    // 徽章：跑起来了且白名单给了才算「健康」，否则标黄让人去处理
    val tone = when {
        running && ignoring -> BadgeTone.GREEN
        running || ignoring -> BadgeTone.AMBER
        else -> BadgeTone.GRAY
    }
    val badgeText = when {
        running && ignoring -> "运行中 · 已加白"
        running -> "运行中 · 未加白"
        ignoring -> "已加白 · 未运行"
        else -> "未运行"
    }

    AgateAccordion(
        title = "保活与通知",
        badge = { AgateBadge(text = badgeText, tone = tone) },
    ) {
        // ── 只读：前台服务 ──
        ItemRow(
            label = "前台服务常驻",
            desc = "网关由前台服务承载，关掉服务等于关掉网关 · 启停请到「网关控制」",
            ) {
            MiniButton(
                text = if (running) "运行中" else "未运行",
                filled = false,
                onClick = { refresh++ },
            )
        }

        // ── 只读：电池优化 ──
        ItemRow(
            label = "忽略电池优化",
            desc = "加白后深睡期网络照常，定时签到的轮次才打得出去",
        ) {
            MiniButton(
                text = if (ignoring) "已授权" else "去授权",
                filled = !ignoring,
                onClick = {
                    if (ignoring) {
                        toast(ctx, "已在白名单内")
                    } else {
                        // requestIfNeeded 拉起系统弹窗后会立即返回 true；
                        // 用户授权完回到 App 时刷新一次读数
                        KeepalivePrefs.requestIgnoreBatteryOptimization(ctx)
                        refresh++
                    }
                },
            )
        }

        // ── 只读：精确闹钟 ──
        ItemRow(
            label = "精确闹钟授权",
            desc = "Android 12+ 定时刻唤醒所需 · 未授权时定点签到会退化成系统窗口内随机",
        ) {
            MiniButton(
                text = if (canExact) "已授权" else "去授权",
                filled = !canExact,
                onClick = {
                    if (canExact) {
                        toast(ctx, "已授权精确闹钟")
                    } else {
                        KeepalivePrefs.requestExactAlarm(ctx)
                        refresh++
                    }
                },
            )
        }

        // ── 真开关：签到结果通知 ──
        SwitchRow(
            label = "签到结果通知",
            desc = "定时 / 手动签到完成后系统通知 · 只报本轮汇总，不逐账号刷屏",
            checked = checkinNotify,
            enabled = true,
            onToggle = { want ->
                checkinNotify = want
                KeepalivePrefs.setCheckinNotify(ctx, want)
            },
        )

        // ── 真开关：异常告警 ──
        SwitchRow(
            label = "服务异常告警",
            desc = "网关启动失败时提醒 · 常驻通知会一直挂着失败原因，但你在别的页面时看不到",
            checked = alertNotify,
            enabled = true,
            onToggle = { want ->
                alertNotify = want
                KeepalivePrefs.setAlertNotify(ctx, want)
            },
        )

        Spacer(Modifier.height(8.dp))
        Hint(
            "前两行是系统权限与网关状态，只能看不能改；后两行是本机偏好，" +
                "改完立即生效，不需要重启网关。",
            C,
        )
    }
}
