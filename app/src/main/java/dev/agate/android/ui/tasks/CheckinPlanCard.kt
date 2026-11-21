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

/** 一次待领取任务：账号 + 指定套餐 id（空 = 内核挑最高优先级）。 */
private data class PendingClaim(val accountId: String, val name: String, val planId: String)

/** 一份已取到的风控配置（弹滑块所需）。 */
private data class CaptchaCfg(val prefix: String, val region: String, val sceneId: String)

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
    // 待弹滑块的任务 + 其配置：非 null 时才显示 ZcodeCaptchaSheet
    var pending by remember { mutableStateOf<Pair<PendingClaim, CaptchaCfg>?>(null) }

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
                    val marketingError = r.data?.optString("marketingError").orEmpty()
                    onToast(
                        if (!deployed) "发现套餐失败：${marketingError.ifBlank { "本区未部署" }}"
                        else "${acc.name} 当前没有可领套餐"
                    )
                }
            } else {
                onToast("${acc.name} 探测失败：${r.error ?: "未知"}")
            }
        }
    }

    /** 真正发领取请求。`param` / `region` 来自滑块；空串 = 上游此刻不要验证码。 */
    fun doClaim(task: PendingClaim, param: String, region: String) {
        val key = "${task.accountId}:claim"
        onBusy(key)
        scope.launch {
            val r = CheckinOps.planClaim(
                task.accountId,
                task.planId,
                captchaVerifyParam = param,
                captchaRegion = region,
            )
            onBusy(null)
            // 失败也是 HTTP 200，靠 ok / failure 字段判；
            // already_claimed = 这份套餐已经到手（可能别处领的），与成功等价落定。
            val ok = r.data?.optBoolean("ok") ?: false
            val failure = r.data?.optString("failure").orEmpty()
            val settled = ok || failure == "already_claimed"
            if (settled) {
                val marketingError = r.data?.optString("marketingError").orEmpty()
                onToast(
                    if (ok) "${task.name} 已领取套餐"
                    else "${task.name} 这份今天已领过" +
                        if (marketingError.isNotBlank()) "（回执待补：$marketingError）" else ""
                )
                CheckinCenter.refreshOnce()
            } else {
                val label = r.data?.optString("failureLabel").orEmpty()
                val message = r.data?.optString("message").orEmpty()
                onToast(
                    "${task.name} 未领到：" +
                        (label.ifBlank { message }.ifBlank { r.error ?: "未知原因" })
                )
            }
        }
    }

    /** 领取前置：取风控配置，决定直接领还是先弹滑块。 */
    fun prepareClaim(acc: CheckinCenter.PlanAccount, planId: String) {
        if (busyKey != null) return
        val task = PendingClaim(acc.id, acc.name, planId)
        val key = "${acc.id}:config"
        onBusy(key)
        scope.launch {
            val r = CheckinOps.planCaptchaConfig(acc.id)
            onBusy(null)
            if (!r.ok) {
                // 配置取不到不该堵死领取：空码发一次，让内核/上游给最终判据
                doClaim(task, "", "")
                return@launch
            }
            val data = r.data
            val enabled = data?.optBoolean("enabled") ?: false
            val prefix = data?.optString("prefix").orEmpty()
            val region = data?.optString("region").orEmpty()
            val sceneId = data?.optString("sceneId").orEmpty()
            if (enabled && prefix.isNotBlank() && sceneId.isNotBlank()) {
                pending = task to CaptchaCfg(prefix, region, sceneId)
            } else {
                // 上游说不用码，或配置不全（弹也弹不出来）：直接空码领取
                doClaim(task, "", "")
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
                    if (busyKey == "${acc.id}:claim" || busyKey == "${acc.id}:config") "…" else "去领取",
                    primary = true,
                    enabled = busyKey == null,
                ) { prepareClaim(acc, "") }
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
                                val source = plan.optJSONArray("marketingCampaignIds")
                                if (source != null && source.length() > 0) {
                                    TaskTag("活动", C.amber)
                                }
                                val planId = plan.optString("planId")
                                Text(
                                    if (busyKey == "${acc.id}:claim") "…" else "领",
                                    color = C.accent,
                                    fontSize = 12.sp,
                                    fontWeight = FontWeight.SemiBold,
                                    modifier = Modifier
                                        .padding(start = 6.dp)
                                        .clickable(enabled = busyKey == null) {
                                            prepareClaim(acc, planId)
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
            "按钮始终可点，不按「今天领过没」堵死入口 —— 上游是按**套餐**判已领取的" +
                "（领了 A 之后 B 照样能领）。「去领取」领当前最高优先级的一份，套餐明细里每份可单独领。" +
                "领取要过阿里云滑块时会弹官方验证，验证串只用于本次领取；上游说不用码时直接领。"
        )
    }

    // 滑块弹层：解出不透明验证串后带着它领取；取消则清掉待办。
    pending?.let { (task, cfg) ->
        ZcodeCaptchaSheet(
            prefix = cfg.prefix,
            region = cfg.region,
            sceneId = cfg.sceneId,
            onDismiss = { pending = null },
            onParam = { param ->
                pending = null
                doClaim(task, param, cfg.region)
            },
        )
    }
}

