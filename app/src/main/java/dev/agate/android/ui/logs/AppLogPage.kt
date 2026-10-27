package dev.agate.android.ui.logs

import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.GatewayRepository
import dev.agate.android.data.LogOps
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.AgateSearchBar
import dev.agate.android.ui.components.AgateSegmented
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateShape
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import dev.agate.core.GatewayPrefs
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * 应用日志页（原 M3 骨架，本次补实）—— 对齐 v44 `sub-reqlog`。
 *
 * App 自己的运行事件流：账号状态 / 签到 / 凭证刷新 / 换号 / 熔断 / 网关启停。
 * 与「调用记录」的分工：**这里是运维事件，那边是模型调用明细**。
 *
 * 三个筛选维度叠在一起用：
 *   · 分类 Tab（来自 `/api/logs/stats` 的 byCategory，带计数）
 *   · 级别（debug 及以上 / info / warn / error —— 内核按「该级及以上」过滤）
 *   · 关键词（消息体子串）
 *
 * ── 崩溃提示条的判据 ─────────────────────────────────────────
 * 来自 `/api/logs/crash`，内核只在**真的 panic** 时落痕（不是「进程没干净
 * 退出」）—— Android 上后者会把每次正常的后台回收都误报成崩溃。
 * 读一次即清，所以它是「上一次运行的现场」而不是一条常驻日志。
 */
@Composable
fun AppLogPage(
    onOpenDrawer: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val clipboard = LocalClipboardManager.current

    var entries by remember { mutableStateOf<List<LogOps.Entry>>(emptyList()) }
    var stats by remember { mutableStateOf<LogOps.Stats?>(null) }
    var category by remember { mutableStateOf<String?>(null) }
    var levelIndex by remember { mutableIntStateOf(1) }   // 1 = 信息及以上（默认）
    var keyword by remember { mutableStateOf("") }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var crash by remember { mutableStateOf<LogOps.Crash?>(null) }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    fun reload() {
        if (loading) return
        loading = true
        scope.launch {
            val p = LogOps.page(level = LEVELS[levelIndex], category = category, keyword = keyword)
            val s = LogOps.stats()
            loading = false
            if (p.ok && p.data != null) {
                entries = p.data.entries
                error = null
            } else {
                error = p.error ?: "读取失败"
            }
            if (s.ok) stats = s.data
        }
    }

    DisposableEffect(Unit) {
        GatewayPrefs.apiKey(ctx).let { GatewayRepository.configure(it) }
        onDispose { }
    }
    LaunchedEffect(Unit) {
        reload()
        // 崩溃痕迹只在进页面时读一次（读即清，重复读会拿到 null）
        crash = LogOps.takeCrash().takeIf { it.ok }?.data
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgatePageHeader(title = "应用日志", onOpenDrawer = onOpenDrawer) {
            Text(
                if (loading) "刷新中…" else "刷新",
                color = C.accent,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clickable(enabled = !loading) { reload() }
                    .padding(horizontal = 8.dp, vertical = 5.dp),
            )
            Text(
                "导出",
                color = C.textSub,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.clickable {
                    scope.launch {
                        val r = LogOps.download()
                        if (r.ok && r.data != null) {
                            clipboard.setText(AnnotatedString(r.data!!))
                            toast("已复制 ${r.data!!.lines().size} 行日志到剪贴板")
                        } else {
                            toast(r.error ?: "导出失败")
                        }
                    }
                }
                .padding(horizontal = 8.dp, vertical = 5.dp),
            )
            Text(
                "清空",
                color = C.red,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.clickable {
                    scope.launch {
                        val r = LogOps.clear(all = keyword.isBlank() && category == null)
                        if (r.ok) {
                            toast("已清空 ${r.data ?: 0L} 条日志")
                            reload()
                        } else {
                            toast(r.error ?: "清空失败")
                        }
                    }
                }
                .padding(horizontal = 8.dp, vertical = 5.dp),
            )
        }

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))

            // 崩溃提示条（有才显示）
            crash?.let { CrashBanner(it, onCopy = {
                clipboard.setText(AnnotatedString(it.dump()))
                toast("崩溃栈已复制，可直接发给开发者")
            }, onDismiss = {
                crash = null
                scope.launch { LogOps.dropCrash() }
            }) }

            // 分类 Tab（带计数）
            val cats = stats?.byCategory ?: emptyMap()
            val tabs = listOf(null to "全部") + cats.keys.sorted().map { it to it }
            Row(
                Modifier
                    .fillMaxWidth()
                    .horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                tabs.forEach { (value, label) ->
                    FilterChip(
                        text = label + (value?.let { cats[it]?.toString() }?.let { " · $it" } ?: ""),
                        active = category == value,
                        onClick = { category = value },
                    )
                }
            }

            Spacer(Modifier.height(8.dp))

            AgateSegmented(
                options = LEVELS.map { it.label },
                selected = levelIndex,
                onSelect = { levelIndex = it },
            )

            Spacer(Modifier.height(8.dp))

            AgateSearchBar(
                value = keyword,
                onValueChange = { keyword = it },
                hint = "搜索消息关键词…",
            )

            Spacer(Modifier.height(10.dp))

            val shown = entries.filter { entry ->
                (category == null || entry.category == category) &&
                    (keyword.isBlank() || entry.message.contains(keyword, ignoreCase = true))
            }

            Column(
                Modifier
                    .fillMaxWidth()
                    .agateCard().background(C.cardBg, RoundedCornerShape(16.dp))
                    .border(1.dp, C.border, RoundedCornerShape(16.dp))
                    .padding(12.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("运行日志", fontSize = 13.5.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
                    Spacer(Modifier.weight(1f))
                    Text(
                        "${shown.size} / ${stats?.total ?: entries.size} 条",
                        fontSize = 10.5.sp,
                        color = C.textDim,
                    )
                }
                Spacer(Modifier.height(4.dp))
                if (entries.isEmpty()) {
                    Text(
                        if (loading) "正在加载日志…" else "暂无日志",
                        fontSize = 12.sp,
                        color = C.textDim,
                        modifier = Modifier.padding(vertical = 14.dp),
                    )
                } else if (shown.isEmpty()) {
                    Text(
                        "没有符合筛选条件的日志",
                        fontSize = 12.sp,
                        color = C.textDim,
                        modifier = Modifier.padding(vertical = 14.dp),
                    )
                } else {
                    shown.forEach { entry ->
                        LogLine(entry)
                        Spacer(Modifier.height(2.dp))
                    }
                }
            }

            Spacer(Modifier.height(12.dp))
            Text(
                "记录 App 自身运行事件 · 模型调用明细见「调用记录」",
                fontSize = 11.sp,
                color = C.textDim,
            )
            Spacer(Modifier.height(20.dp))
        }
    }
}

/** 级别档位：与内核 LEVELS 同序，「该级及以上」。 */
internal val LEVELS = listOf(
    LogOps.Level.DEBUG,
    LogOps.Level.INFO,
    LogOps.Level.WARN,
    LogOps.Level.ERROR,
)

/** 崩溃痕迹的文本形态（复制给开发者用）。 */
private fun LogOps.Crash.dump(): String = buildString {
    appendLine("agate crash @ $at")
    if (thread.isNotBlank()) appendLine("thread: $thread")
    if (location.isNotBlank()) appendLine("at: $location")
    appendLine("message: $message")
}

/** 一条日志行：时间 + 分类标签 + 消息（按级别配色）。 */
@Composable
private fun LogLine(entry: LogOps.Entry) {
    val C = agateColors()
    val color = when (entry.level) {
        "error" -> C.red
        "warn" -> C.amber
        "debug" -> C.textDim
        else -> C.textSub
    }
    Row(
        Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp),
    ) {
        Text(
            text = dev.agate.android.ui.logs.fmtClock(entry.ts),
            fontSize = 10.5.sp,
            color = C.textDim,
            modifier = Modifier.width(62.dp),
        )
        Spacer(Modifier.width(6.dp))
        Box(
            Modifier
                .border(1.dp, C.border, RoundedCornerShape(4.dp))
                .background(C.textDim.copy(alpha = 0.12f), RoundedCornerShape(4.dp))
                .padding(horizontal = 5.dp, vertical = 1.dp),
        ) {
            Text(entry.category, fontSize = 9.sp, color = C.textSub)
        }
        Spacer(Modifier.width(6.dp))
        Text(
            text = entry.message,
            fontSize = 11.5.sp,
            color = color,
            lineHeight = 16.sp,
            modifier = Modifier.weight(1f),
        )
    }
}

/** 崩溃提示条。 */
@Composable
internal fun CrashBanner(
    crash: LogOps.Crash,
    onCopy: () -> Unit,
    onDismiss: () -> Unit,
) {
    val C = agateColors()
    Row(
        Modifier
            .fillMaxWidth()
            .background(C.amberBg, RoundedCornerShape(12.dp))
            .border(1.dp, C.amber.copy(alpha = 0.35f), RoundedCornerShape(12.dp))
            .padding(horizontal = 10.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            "检测到上次运行崩溃 · 点开复制给我们",
            fontSize = 12.sp,
            fontWeight = FontWeight.SemiBold,
            color = C.amber,
            modifier = Modifier.weight(1f).clickable(onClick = onCopy),
        )
        Text(
            "✕",
            fontSize = 14.sp,
            color = C.amber,
            modifier = Modifier.clickable(onClick = onDismiss).padding(horizontal = 6.dp),
        )
    }
    Spacer(Modifier.height(10.dp))
}
