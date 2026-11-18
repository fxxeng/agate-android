package dev.agate.android.ui.tasks

import android.widget.Toast
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
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
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
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
import dev.agate.android.data.CheckinCenter
import dev.agate.android.data.CheckinOps
import dev.agate.android.data.PresetRegistry
import dev.agate.core.GatewayPrefs
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.AgateSubPageHeader
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.components.StatCell
import dev.agate.android.ui.components.AgateStatRow
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject

/* ── 新手福利 ── */

@Composable
internal fun OnboardingCard(
    snap: CheckinCenter.Snap,
    busyKey: String?,
    onBusy: (String?) -> Unit,
    onToast: (String) -> Unit,
) {
    val C = agateColors()
    val scope = rememberCoroutineScope()
    // 每个账号的查询结果（只在点「查询」后才填，展开不自动查）
    val results = remember { mutableStateMapOf<String, JSONObject>() }
    val names = remember(snap.onboarding) { snap.onboarding.associate { it.id to it.name } }

    var queryingAll by remember { mutableStateOf(false) }

    fun query(acc: CheckinCenter.OnboardingAccount, refresh: Boolean) {
        val key = "${acc.id}:onboarding"
        if (busyKey != null) return
        onBusy(key)
        scope.launch {
            val data = CheckinOps.onboardingStatus(acc.id, refresh)
            onBusy(null)
            if (data != null) results[acc.id] = data
            else onToast("${acc.name} 查询失败")
        }
    }

    fun claim(acc: CheckinCenter.OnboardingAccount) {
        val key = "${acc.id}:claim"
        if (busyKey != null) return
        onBusy(key)
        scope.launch {
            val r = CheckinOps.onboardingClaim(acc.id)
            onBusy(null)
            if (r.ok) {
                val claimed = r.data?.optInt("claimed", 0) ?: 0
                onToast("${acc.name} 已领取 $claimed 项")
                // 领取后响应里带最新状态，直接用它回填
                r.data?.let { results[acc.id] = it }
                CheckinCenter.refreshOnce()
            } else {
                onToast("${acc.name} 领取失败：${r.error ?: "未知"}")
            }
        }
    }

    TaskCard {
        TaskCardHeader(
            title = "新手福利",
            subtitle = "一次性福利 · 领取后记在本机，不再查上游",
        ) {
            if (queryingAll) {
                AgateBadge("查询中", BadgeTone.AMBER)
            } else if (snap.onboarding.isNotEmpty()) {
                TaskBtn("全部查询", outlined = true, enabled = busyKey == null) {
                    queryingAll = true
                    scope.launch {
                        snap.onboarding.forEach { acc ->
                            CheckinOps.onboardingStatus(acc.id, refresh = true)
                                ?.let { results[acc.id] = it }
                        }
                        queryingAll = false
                        CheckinCenter.refreshOnce()
                    }
                }
            }
        }

        if (snap.onboarding.isEmpty()) {
            Spacer(Modifier.height(6.dp))
            Text("没有支持一次性福利的账号", style = AgateType.BodyS, color = C.textDim)
        }

        snap.onboarding.forEach { acc ->
            val data = results[acc.id]
            val tasks = data?.optJSONArray("tasks")
            val unclaimed = data?.optInt("unclaimed", 0) ?: 0
            val earned = data?.optInt("earned", 0) ?: 0
            val total = data?.optInt("total", 0) ?: 0
            val settledFromData = data?.optBoolean("settled") ?: acc.settled
            val settledAtMs = data?.optLong("settledAt", 0L)?.takeIf { it > 0 } ?: acc.settledAt

            Column {
                TaskRow(
                    title = acc.name,
                    avatarColorKey = acc.provider,
                    letters = PresetRegistry.labelOf(acc.provider).take(1),
                    // 兜底头像的 label 尽量取产品名。`labelOf` 是**精确匹配**，
                    // 遇到 `workbuddy-intl` 这类变体会回落成 id 本身（小写英文），
                    // 那比账号名的观感还差 —— 所以猜不到就退回账号名，不硬显 id。
                    providerId = acc.provider,
                    label = PresetRegistry.byId(acc.provider)?.label ?: acc.name,
                    subtitle = buildString {
                        append(PresetRegistry.labelOf(acc.provider))
                        when {
                            settledFromData && settledAtMs != null ->
                                append(" · 已结清（${beijingStamp(settledAtMs)}）")
                            data != null -> append(" · 已领 $earned/$total · 待领 $unclaimed")
                            acc.settled -> append(" · 记录显示已结清")
                            else -> append(" · 尚未查询")
                        }
                    },
                ) {
                    val acting = busyKey == "${acc.id}:onboarding" || busyKey == "${acc.id}:claim"
                    TaskBtn(
                        if (busyKey == "${acc.id}:onboarding") "…" else "查询",
                        outlined = true,
                        enabled = busyKey == null,
                    ) { query(acc, refresh = true) }
                    if (unclaimed > 0) {
                        TaskBtn(
                            if (busyKey == "${acc.id}:claim") "…" else "一键领取",
                            primary = true,
                            enabled = busyKey == null,
                        ) { claim(acc) }
                    } else if (data != null && settledFromData) {
                        TaskTag("已领完", C.green)
                    }
                    if (acting) AgateBadge("…", BadgeTone.AMBER)
                }

                if (tasks != null && tasks.length() > 0) {
                    Column(
                        verticalArrangement = Arrangement.spacedBy(5.dp),
                        modifier = Modifier.padding(start = 8.dp, bottom = 4.dp, end = 2.dp),
                    ) {
                        for (i in 0 until tasks.length()) {
                            val t = tasks.optJSONObject(i) ?: continue
                            val done = t.optBoolean("done")
                            val blocked = t.optBoolean("blocked")
                            val points = t.optInt("points", 0)
                            TaskSubRow {
                                Text(
                                    if (done) "✓" else "○",
                                    color = if (done) C.green else C.textDim,
                                    fontSize = 12.sp,
                                )
                                Text(
                                    t.optString("title").ifBlank { t.optString("key") },
                                    color = if (done) C.textDim else C.textMain,
                                    fontSize = 12.sp,
                                    modifier = Modifier.weight(1f),
                                )
                                if (points > 0) {
                                    Text("+$points", style = AgateType.Caption, color = C.textSub)
                                }
                                Text(
                                    when {
                                        done -> "已领取"
                                        blocked -> "需先完成前置"
                                        else -> "可领取"
                                    },
                                    style = AgateType.Caption,
                                    color = when {
                                        done -> C.green
                                        blocked -> C.textDim
                                        else -> C.amber
                                    },
                                )
                            }
                        }
                    }
                }
            }
        }

        Spacer(Modifier.height(6.dp))
        TaskFold(
            "「已结清」是后端落盘的结算记忆：领完之后连查询都不再发上游 —— " +
                "手动点「查询」是唯一「重新问一次」的入口。"
        )
    }
}

