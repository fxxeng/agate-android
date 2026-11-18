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

/* ── 活动福利 ── */

@Composable
internal fun PlanCard(
    snap: CheckinCenter.Snap,
    busyKey: String?,
    onBusy: (String?) -> Unit,
    onToast: (String) -> Unit,
) {
    val C = agateColors()
    val scope = rememberCoroutineScope()
    val previews = remember { mutableStateMapOf<String, JSONArray>() }

    fun preview(acc: CheckinCenter.PlanAccount) {
        val key = "${acc.id}:preview"
        if (busyKey != null) return
        onBusy(key)
        scope.launch {
            val r = CheckinOps.planPreview(acc.id)
            onBusy(null)
            if (r.ok) {
                val plans = r.data?.optJSONArray("plans") ?: JSONArray()
                previews[acc.id] = plans
                if (plans.length() == 0) {
                    val deployed = r.data?.optBoolean("deployed") ?: true
                    onToast(
                        if (!deployed) "这套接口在本区没部署"
                        else "${acc.name} 当前没有可领套餐"
                    )
                }
            } else {
                onToast("${acc.name} 探测失败：${r.error ?: "未知"}")
            }
        }
    }

    fun claim(acc: CheckinCenter.PlanAccount) {
        val key = "${acc.id}:claim"
        if (busyKey != null) return
        onBusy(key)
        scope.launch {
            val r = CheckinOps.planClaim(acc.id)
            onBusy(null)
            // 注意：失败也是 HTTP 200，要靠 failure 字段判
            val ok = r.data?.optBoolean("ok") ?: false
            when {
                ok -> {
                    onToast("${acc.name} 已领取套餐")
                    CheckinCenter.refreshOnce()
                }
                else -> {
                    val label = r.data?.optString("failureLabel").orEmpty()
                    val message = r.data?.optString("message").orEmpty()
                    onToast(
                        "${acc.name} 未领到：" +
                            (label.ifBlank { message }.ifBlank { r.error ?: "未知原因" })
                    )
                }
            }
        }
    }

    /** 今天领过几份：本地台账 `claimPlans` 按北京时间日界逐份比对。 */
    fun claimedToday(acc: CheckinCenter.PlanAccount): Int {
        val today = beijingDateKey(System.currentTimeMillis())
        return acc.claimPlans.values.count { beijingDateKey(it) == today }
    }

    TaskCard {
        TaskCardHeader(
            title = "活动福利",
            subtitle = "限时体验套餐 · 逐份查看与领取",
        )
        if (snap.plans.isEmpty()) {
            Spacer(Modifier.height(6.dp))
            Text("没有 ZCode 账号", style = AgateType.BodyS, color = C.textDim)
        }
        snap.plans.forEach { acc ->
            val claimed = claimedToday(acc)
            TaskRow(
                title = acc.name,
                avatarColorKey = "zcode",
                letters = "ZC",
                providerId = "zcode",
                label = PresetRegistry.labelOf("zcode"),
                subtitle = PresetRegistry.labelOf("zcode") + " · " +
                    if (claimed > 0) "今天已领 $claimed 份" else "今天未领",
            ) {
                TaskBtn(
                    if (busyKey == "${acc.id}:preview") "…" else "套餐明细",
                    outlined = true,
                    enabled = busyKey == null,
                ) { preview(acc) }
                TaskBtn(
                    if (busyKey == "${acc.id}:claim") "…" else "去领取",
                    primary = true,
                    enabled = busyKey == null,
                ) { claim(acc) }
            }
            previews[acc.id]?.let { plans ->
                if (plans.length() > 0) {
                    Column(
                        verticalArrangement = Arrangement.spacedBy(5.dp),
                        modifier = Modifier.padding(start = 8.dp, bottom = 4.dp, end = 2.dp),
                    ) {
                        for (i in 0 until plans.length()) {
                            val plan = plans.optJSONObject(i) ?: continue
                            val ents = plan.optJSONArray("entitlements")
                            val first = ents?.optJSONObject(0)
                            TaskSubRow {
                                Column(Modifier.weight(1f)) {
                                    Text(
                                        plan.optString("name").ifBlank { plan.optString("planId") },
                                        color = C.textMain,
                                        fontSize = 12.sp,
                                    )
                                    if (first != null) {
                                        Text(
                                            first.optString("showName") + " " +
                                                "${first.optInt("grantUnits")} ${first.optString("unitType")}",
                                            style = AgateType.Caption,
                                            color = C.textDim,
                                        )
                                    }
                                }
                                TaskTag("优先级 ${plan.optInt("priority")}", C.blue)
                            }
                        }
                    }
                }
            }
        }
        Spacer(Modifier.height(6.dp))
        TaskFold(
            "按钮始终可点，不按「今天领过没」堵死入口 —— 上游是按**套餐**判已领取的" +
                "（领了 A 之后 B 照样能领）。行上那句话只说今天领了几份，由本地台账按北京时间判。" +
                "领取要过阿里云滑块时本版**不做过码**，会如实显示「验证码未通过」。"
        )
    }
}

