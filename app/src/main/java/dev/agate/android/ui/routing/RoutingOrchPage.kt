package dev.agate.android.ui.routing

import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.RoutingWorkspaceOps
import dev.agate.android.ui.components.AgateSubPageHeader
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject

/**
 * 路由编排（R63 批5）：分组列表 → 选中组成员调序 → 追踪时间线。
 *
 * ── 为什么是列表式、不是画布 ────────────────────────────────────
 * 上游桌面端的路由画布（canvas + drag + edges，47 文件）在 411dp 竖屏上放不下
 * 节点图，触屏拖拽精度也不足。手机端用三张卡串起来：点组 → 看成员 → ↑↓ 调序。
 * 调序走「按钮交换相邻」而不是拖拽，是因为长列表拖拽在触屏上极易误触。
 *
 * ── 数据层边界 ──────────────────────────────────────────────────
 * 本页只读三个现成端点（groups / saveGroup / trace），不新增任何 Rust 端点。
 * 组保存是整块替换语义（`PUT /api/routing/groups`），所以 members 必须
 * 从 `group` JSONObject 里原样取回、改完顺序再原样放回 —— 不能映射成
 * Kotlin 数据类再拼（会静默丢字段）。
 */
@Composable
fun RoutingOrchPage(
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    var snapshot by remember { mutableStateOf<RoutingWorkspaceOps.GroupsSnapshot?>(null) }
    var selectedId by remember { mutableStateOf<String?>(null) }
    var trace by remember { mutableStateOf<RoutingWorkspaceOps.TraceSnapshot?>(null) }
    var loading by remember { mutableStateOf(false) }
    var saving by remember { mutableStateOf(false) }
    val members = remember { mutableStateListOf<JSONObject>() }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    fun pickGroup(id: String?, groups: List<RoutingWorkspaceOps.GroupRow>) {
        selectedId = id
        members.clear()
        val row = groups.firstOrNull { it.group.optString("id") == id } ?: return
        val arr = row.group.optJSONArray("members") ?: JSONArray()
        for (i in 0 until arr.length()) arr.optJSONObject(i)?.let { members.add(it) }
    }

    fun reload() {
        if (loading) return
        loading = true
        scope.launch {
            val g = RoutingWorkspaceOps.groups()
            val t = RoutingWorkspaceOps.trace(limit = 20)
            loading = false
            if (g.ok) {
                snapshot = g.data
                val groups = g.data?.groups ?: emptyList()
                val target = selectedId?.takeIf { id ->
                    groups.any { it.group.optString("id") == id }
                } ?: groups.firstOrNull()?.group?.optString("id")
                pickGroup(target, groups)
            }
            if (t.ok) trace = t.data
        }
    }

    LaunchedEffect(Unit) { reload() }

    fun move(from: Int, to: Int) {
        if (from !in members.indices || to !in members.indices || from == to) return
        members.add(to, members.removeAt(from))
    }

    fun saveOrder() {
        val row = snapshot?.groups?.firstOrNull {
            it.group.optString("id") == selectedId
        } ?: return
        if (saving) return
        saving = true
        scope.launch {
            val group = JSONObject(row.group.toString())
            val arr = JSONArray()
            members.forEach { arr.put(it) }
            group.put("members", arr)
            val r = RoutingWorkspaceOps.saveGroup(row.group.optString("id"), group, row.revision)
            saving = false
            if (r.ok) {
                snapshot = r.data
                pickGroup(selectedId, r.data?.groups ?: emptyList())
                toast("已保存，下一请求生效")
            } else {
                toast(r.error ?: "保存失败")
            }
        }
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg)) {
        AgateSubPageHeader(title = "路由编排", onBack = onBack) {
            Text(
                if (loading) "刷新中…" else "刷新",
                color = C.accent,
                fontSize = 13.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clickable(enabled = !loading) { reload() }
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

            // ── 卡1 分组列表 ──
            val groups = snapshot?.groups ?: emptyList()
            Column(
                Modifier
                    .fillMaxWidth()
                    .background(C.cardBg, RoundedCornerShape(12.dp))
                    .border(1.dp, C.border, RoundedCornerShape(12.dp))
                    .padding(14.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("路由分组", fontSize = 14.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
                    Spacer(Modifier.width(8.dp))
                    Text("${groups.size} 组", fontSize = 10.5.sp, color = C.textDim)
                    Spacer(Modifier.weight(1f))
                    Text(
                        "＋ 新建组",
                        fontSize = 11.sp,
                        color = C.accent,
                        modifier = Modifier.clickable { toast("新建组编辑器在批 6 落地") },
                    )
                }
                Spacer(Modifier.height(8.dp))
                if (groups.isEmpty()) {
                    Text("还没有路由组，点右上角新建", fontSize = 12.sp, color = C.textDim)
                } else {
                    groups.forEach { row ->
                        val id = row.group.optString("id")
                        val name = row.group.optString("name").ifBlank { id }
                        val strategy = when (row.group.optString("strategy")) {
                            "rotate" -> "负载均衡"
                            "provider" -> "同家轮询"
                            else -> "顺序优先"
                        }
                        val model = row.group.optString("model").ifBlank { "不限模型" }
                        val count = row.group.optJSONArray("members")?.length() ?: 0
                        val sel = id == selectedId
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier
                                .fillMaxWidth()
                                .clickable { pickGroup(id, groups) }
                                .background(
                                    if (sel) C.accent.copy(alpha = 0.10f) else C.cardBg,
                                    RoundedCornerShape(10.dp),
                                )
                                .border(
                                    1.dp,
                                    if (sel) C.accent.copy(alpha = 0.45f) else C.border,
                                    RoundedCornerShape(10.dp),
                                )
                                .padding(horizontal = 11.dp, vertical = 8.dp),
                        ) {
                            Column(Modifier.weight(1f)) {
                                Text(
                                    name,
                                    fontSize = 13.sp,
                                    fontWeight = FontWeight.SemiBold,
                                    color = if (sel) C.accent else C.textMain,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                                Text(
                                    "$model · $count 成员",
                                    fontSize = 10.5.sp,
                                    color = C.textDim,
                                )
                            }
                            Spacer(Modifier.width(8.dp))
                            Text(
                                strategy,
                                fontSize = 9.5.sp,
                                color = C.textSub,
                                modifier = Modifier
                                    .background(C.cardBg, RoundedCornerShape(50))
                                    .border(1.dp, C.border, RoundedCornerShape(50))
                                    .padding(horizontal = 6.dp, vertical = 3.dp),
                            )
                            Spacer(Modifier.width(10.dp))
                            Text("›", fontSize = 13.sp, color = C.textDim)
                        }
                        Spacer(Modifier.height(6.dp))
                    }
                }
            }

            Spacer(Modifier.height(12.dp))

            // ── 卡2 选中组详情：成员调序 ──
            val selRow = snapshot?.groups?.firstOrNull {
                it.group.optString("id") == selectedId
            }
            if (selRow != null) {
                val gid = selRow.group.optString("id")
                val gname = selRow.group.optString("name").ifBlank { gid }
                val strategyLabel = when (selRow.group.optString("strategy")) {
                    "rotate" -> "负载均衡"
                    "provider" -> "同家轮询"
                    else -> "顺序优先"
                }
                val stickyLabel = when (selRow.group.optString("sticky")) {
                    "session" -> "按会话"
                    "turn" -> "按轮次"
                    "off" -> "关闭"
                    else -> "自动"
                }
                Column(
                    Modifier
                        .fillMaxWidth()
                        .background(C.cardBg, RoundedCornerShape(12.dp))
                        .border(1.dp, C.border, RoundedCornerShape(12.dp))
                        .padding(14.dp),
                ) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(gname, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
                        Spacer(Modifier.width(8.dp))
                        Text(
                            "$strategyLabel · 粘滞$stickyLabel",
                            fontSize = 10.5.sp,
                            color = C.textDim,
                        )
                        Spacer(Modifier.weight(1f))
                        Text(
                            "删除组",
                            fontSize = 11.sp,
                            color = C.red,
                            modifier = Modifier.clickable { toast("删除组在批 6 落地") },
                        )
                    }
                    Spacer(Modifier.height(10.dp))
                    Text("对外模型", fontSize = 10.5.sp, color = C.textDim)
                    Spacer(Modifier.height(4.dp))
                    Text(
                        selRow.group.optString("model").ifBlank { "不限模型" },
                        fontSize = 13.sp,
                        fontWeight = FontWeight.SemiBold,
                        color = C.textMain,
                    )
                    Spacer(Modifier.height(10.dp))
                    Text("成员顺序（决定选号先后）", fontSize = 10.5.sp, color = C.textDim)
                    Spacer(Modifier.height(6.dp))
                    if (members.isEmpty()) {
                        Text("这个组还没有成员", fontSize = 12.sp, color = C.textDim)
                    } else {
                        members.forEachIndexed { index, member ->
                            val included = member.optBoolean("included", true)
                            val model = member.optString("model")
                            val provider = member.optString("provider")
                            val name = if (model.isNotBlank()) model else provider
                            val sub = if (included) "$provider · 参与" else "$provider · 未参与"
                            val stateColor = if (included) C.green else C.textDim
                            Row(
                                verticalAlignment = Alignment.CenterVertically,
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .background(C.insetBg, RoundedCornerShape(10.dp))
                                    .border(1.dp, C.border, RoundedCornerShape(10.dp))
                                    .padding(horizontal = 10.dp, vertical = 8.dp),
                            ) {
                                Row(
                                    verticalAlignment = Alignment.CenterVertically,
                                    horizontalArrangement = Arrangement.Center,
                                    modifier = Modifier
                                        .size(22.dp)
                                        .background(
                                            if (index == 0) C.accent else C.cardBg,
                                            RoundedCornerShape(7.dp),
                                        )
                                        .border(
                                            1.dp,
                                            if (index == 0) C.accent else C.border,
                                            RoundedCornerShape(7.dp),
                                        ),
                                ) {
                                    Text(
                                        "${index + 1}",
                                        fontSize = 11.sp,
                                        fontWeight = FontWeight.Bold,
                                        color = if (index == 0) C.onAccent else C.textSub,
                                    )
                                }
                                Spacer(Modifier.width(9.dp))
                                Column(Modifier.weight(1f)) {
                                    Text(
                                        name,
                                        fontSize = 12.5.sp,
                                        fontWeight = FontWeight.SemiBold,
                                        color = C.textMain,
                                        maxLines = 1,
                                        overflow = TextOverflow.Ellipsis,
                                    )
                                    Text(sub, fontSize = 10.sp, color = stateColor)
                                }
                                Spacer(Modifier.width(6.dp))
                                val upEnabled = index > 0
                                val downEnabled = index < members.lastIndex
                                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                                    Text(
                                        "↑",
                                        fontSize = 12.sp,
                                        color = if (upEnabled) C.textSub else C.border,
                                        modifier = Modifier
                                            .size(26.dp)
                                            .background(C.cardBg, RoundedCornerShape(8.dp))
                                            .border(1.dp, C.border, RoundedCornerShape(8.dp))
                                            .clickable(enabled = upEnabled) { move(index, index - 1) }
                                            .padding(top = 6.dp),
                                    )
                                    Text(
                                        "↓",
                                        fontSize = 12.sp,
                                        color = if (downEnabled) C.textSub else C.border,
                                        modifier = Modifier
                                            .size(26.dp)
                                            .background(C.cardBg, RoundedCornerShape(8.dp))
                                            .border(1.dp, C.border, RoundedCornerShape(8.dp))
                                            .clickable(enabled = downEnabled) { move(index, index + 1) }
                                            .padding(top = 6.dp),
                                    )
                                }
                            }
                            Spacer(Modifier.height(6.dp))
                        }
                    }
                    Spacer(Modifier.height(8.dp))
                    Text(
                        if (saving) "保存中…" else "保存顺序（下一请求生效）",
                        fontSize = 12.5.sp,
                        fontWeight = FontWeight.SemiBold,
                        color = C.onAccent,
                        modifier = Modifier
                            .fillMaxWidth()
                            .background(C.accent, RoundedCornerShape(10.dp))
                            .clickable(enabled = !saving) { saveOrder() }
                            .padding(vertical = 9.dp),
                    )
                }

                Spacer(Modifier.height(12.dp))
            }

            // ── 卡3 追踪时间线 ──
            Column(
                Modifier
                    .fillMaxWidth()
                    .background(C.cardBg, RoundedCornerShape(12.dp))
                    .border(1.dp, C.border, RoundedCornerShape(12.dp))
                    .padding(14.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("路由追踪", fontSize = 14.sp, fontWeight = FontWeight.SemiBold, color = C.textMain)
                    Spacer(Modifier.width(8.dp))
                    Text("近 30 天", fontSize = 10.5.sp, color = C.textDim)
                    Spacer(Modifier.weight(1f))
                    Text("全部记录 ›", fontSize = 11.sp, color = C.accent)
                }
                Spacer(Modifier.height(8.dp))
                val events = trace?.entries ?: emptyList()
                if (events.isEmpty()) {
                    Text("还没有请求记录", fontSize = 12.sp, color = C.textDim)
                } else {
                    events.forEachIndexed { index, row ->
                        val ok = row.status in 200..299
                        val dot = if (ok) C.green else if (row.status == 0L) C.amber else C.red
                        val stickyHit = runCatching {
                            JSONObject(row.route).optBoolean("stickyHit")
                        }.getOrDefault(false)
                        val time = runCatching {
                            val fmt = java.text.SimpleDateFormat("HH:mm", java.util.Locale.CHINA)
                            fmt.format(java.util.Date(row.ts))
                        }.getOrDefault("--:--")
                        Row(
                            verticalAlignment = Alignment.Top,
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Column(horizontalAlignment = Alignment.CenterHorizontally) {
                                Spacer(Modifier.height(4.dp))
                                Spacer(
                                    Modifier
                                        .size(8.dp)
                                        .background(dot, RoundedCornerShape(50)),
                                )
                                if (index < events.lastIndex) {
                                    Spacer(
                                        Modifier
                                            .width(1.5.dp)
                                            .height(38.dp)
                                            .background(C.border),
                                    )
                                }
                            }
                            Spacer(Modifier.width(10.dp))
                            Column(Modifier.weight(1f)) {
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    Text(time, fontSize = 10.5.sp, color = C.textDim)
                                    Spacer(Modifier.width(8.dp))
                                    Text(
                                        "${row.model} → ${row.accountName.ifBlank { row.accountId }}",
                                        fontSize = 12.sp,
                                        fontWeight = FontWeight.SemiBold,
                                        color = C.textMain,
                                        maxLines = 1,
                                        overflow = TextOverflow.Ellipsis,
                                        modifier = Modifier.weight(1f),
                                    )
                                    if (stickyHit) {
                                        Text(
                                            "粘滞",
                                            fontSize = 9.sp,
                                            color = C.accent,
                                            modifier = Modifier
                                                .background(C.accent.copy(alpha = 0.10f), RoundedCornerShape(50))
                                                .border(1.dp, C.accent.copy(alpha = 0.45f), RoundedCornerShape(50))
                                                .padding(horizontal = 5.dp, vertical = 2.dp),
                                        )
                                    }
                                }
                                Spacer(Modifier.height(3.dp))
                                val detail = if (row.error != null) {
                                    "${row.status} · ${row.durationMs}ms · ${row.error}"
                                } else {
                                    "${row.status} · ${row.durationMs}ms"
                                }
                                Text(
                                    detail,
                                    fontSize = 10.5.sp,
                                    color = C.textSub,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            }
                        }
                        Spacer(Modifier.height(10.dp))
                    }
                }
            }

            Spacer(Modifier.height(16.dp))
        }
    }
}
