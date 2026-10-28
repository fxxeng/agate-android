package dev.agate.android.data

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import org.json.JSONObject

/**
 * 签到中心数据源（任务中心 · 签到中心页）。
 *
 * ── 为什么是一条聚合端点 ─────────────────────────────────────
 * `GET /api/checkin-center` 一次给全四块：每日签到分组、一次性福利账号清单、
 * 自动签到设置、轮次台账。指标口径（谁该出现在「每日签到」里、
 * `checkedInToday` 怎么算）都在后端 —— 前端自己拼会抄一份判定逻辑，
 * 那正是「签到中心说可签 9 个、真点下去签了 8 个」这类分叉的起点。
 *
 * ── 这条端点零上游请求 ───────────────────────────────────────
 * 新手福利 / 活动套餐 / 成长任务的具体状态**不在**快照里：那些每查一个账号
 * 就要打一次上游。快照只给账号入口清单，展开或点「查询」时页面才按需调
 * `GET /api/accounts/{id}/onboarding`、`/zcode-claim/preview`、`/growth`
 * （见 [CheckinOps]）。所以进页面不会替用户问十几个上游接口。
 *
 * ── 「今日」是北京时间 ───────────────────────────────────────
 * 后端按固定 UTC+8 判（`core::beijing`）。手机时区与它不一致时，
 * 界面**不重算**这个布尔 —— 只格式化后端给的 `checkinAt` 时刻。
 */
object CheckinCenter {

    data class AccountRow(
        val id: String,
        val name: String,
        val available: Boolean,
        /** 上次签到时刻（ms；null = 从未签过）。 */
        val checkinAt: Long?,
        val checkedInToday: Boolean,
        /** 版本（cn / intl），WorkBuddy 国际版走日活链而不是签到。 */
        val edition: String?,
    ) {
        /** 国际版这一行要三颗按钮（保活 / 领取 / 保活+领取），不是一颗。 */
        val isIntl: Boolean get() = edition == "intl"
    }

    data class ProviderGroup(
        val id: String,
        val label: String,
        val accounts: List<AccountRow>,
        val doneCount: Int,
        val totalCount: Int,
    )

    /** 不参与每日签到的账号（按提供商 + 原因聚合，文案由后端给）。 */
    data class OutOfScopeRow(val label: String, val reason: String, val count: Int)

    /** 一次性福利的账号入口（Loomy / 小浣熊 / CodeArts / OfficeAce）。 */
    data class OnboardingAccount(
        val id: String,
        val name: String,
        val provider: String,
        /** 结算记忆存在 = 全部领完过一次（此后再查上游是白问）。 */
        val settled: Boolean,
        val settledAt: Long?,
    )

    /** ZCode 套餐账号（带本地领取台账 `claimPlans`：{planId: ms}）。 */
    data class PlanAccount(
        val id: String,
        val name: String,
        val claimAt: Long?,
        val claimPlans: Map<String, Long>,
    )

    /** WorkBuddy 国内版成长任务账号。 */
    data class GrowthAccount(val id: String, val name: String)

    /** 一轮批量签到的汇总（`checkinHistory`，最多 20 条，新的在前）。 */
    data class RoundRow(
        val at: Long,
        val date: String,
        val reason: String,
        val succeeded: Int,
        val active: Int,
        val total: Int,
        val skipped: Int,
        val failedCount: Int,
        val failed: List<String>,
    )

    /** 定时条目（`auto.schedules[]` 的一行，字段与后端 `auto_checkin::state` 同名）。 */
    data class ScheduleRow(
        val id: String,
        /** 后端归一化过的展示名（label 为空时退到 time）。 */
        val displayLabel: String,
        /** 原始 label（编辑回填用 —— 不能用 displayLabel，那是带兜底的）。 */
        val label: String,
        val time: String,
        val enabled: Boolean,
        val providers: List<String>,
        val lastFiredToday: Boolean,
        val nextRunAt: Long?,
        val lastResult: String?,
    )

    data class Snap(
        val fetchedAtMs: Long = 0,
        val groups: List<ProviderGroup> = emptyList(),
        val outOfScope: List<OutOfScopeRow> = emptyList(),
        val todayDone: Int = 0,
        val todayEligible: Int = 0,
        val onboarding: List<OnboardingAccount> = emptyList(),
        val plans: List<PlanAccount> = emptyList(),
        val growth: List<GrowthAccount> = emptyList(),
        val rounds: List<RoundRow> = emptyList(),
        val scheduleRows: List<ScheduleRow> = emptyList(),
        val keepalive: List<String> = emptyList(),
        val keepaliveDefault: List<String> = emptyList(),
        val autoEnabled: Boolean = false,
        val autoRunning: Boolean = false,
        val nextRunAt: Long? = null,
        /**
         * 可选签到范围（id → label），按内核 `CHECKIN_PROVIDERS` 注册顺序。
         * 来自聚合快照里 `auto.providerOptions` —— 是「哪些家支持签到」的**唯一**
         * 权威答案（= CHECKIN_PROVIDERS ∩ supports_checkin）。提供商页的
         * 「签到」能力标也读这一份，免得两处各维护一份清单。
         */
        val providerOptions: List<Pair<String, String>> = emptyList(),
        val schedulesTotal: Int = 0,
        val schedulesEnabled: Int = 0,
        val schedulesFiredToday: Int = 0,
    ) {
        /** 今日进度比例；0 个可签时给 0（不显示 NaN）。 */
        val progress: Float
            get() = if (todayEligible > 0) todayDone.toFloat() / todayEligible else 0f

        /** 一次性福利里还没结清的账号数（首页/总览卡上的「待领福利」读数）。 */
        val unsettledOnboarding: Int get() = onboarding.count { !it.settled }
    }

    private const val INTERVAL_MS = 10_000L

    private val _snapshot = MutableStateFlow(Snap())
    val snapshot: StateFlow<Snap> = _snapshot

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var pollJob: Job? = null

    fun configure(apiKey: String) {
        CoreTransport.configure(apiKey)
    }

    fun startPolling() {
        if (pollJob?.isActive == true) return
        pollJob = scope.launch {
            while (isActive) {
                refreshOnce()
                delay(INTERVAL_MS)
            }
        }
    }

    fun stopPolling() {
        pollJob?.cancel()
        pollJob = null
    }

    fun refreshOnce() {
        scope.launch {
            val data = CheckinOps.getData("/api/checkin-center")
            if (data == null) {
                _error.value = "无法连接内核"
                return@launch
            }
            _error.value = null
            _snapshot.value = parse(data)
        }
    }

    private fun parse(root: JSONObject): Snap {
        val daily = root.optJSONObject("daily")
        val extras = root.optJSONObject("extras")
        val auto = root.optJSONObject("auto")
        val keep = root.optJSONObject("keepalive")

        val groups = daily?.optJSONArray("providers")?.let { arr ->
            (0 until arr.length()).mapNotNull { i ->
                val g = arr.optJSONObject(i) ?: return@mapNotNull null
                val id = g.optString("id")
                if (id.isBlank()) return@mapNotNull null
                ProviderGroup(
                    id = id,
                    label = g.optString("label").ifBlank { PresetRegistry.labelOf(id) },
                    accounts = g.optJSONArray("accounts")?.let { list ->
                        (0 until list.length()).mapNotNull { j ->
                            val a = list.optJSONObject(j) ?: return@mapNotNull null
                            val aid = a.optString("id")
                            if (aid.isBlank()) return@mapNotNull null
                            AccountRow(
                                id = aid,
                                name = a.optString("name").ifBlank { aid },
                                available = a.optBoolean("available", true),
                                checkinAt = a.optLong("checkinAt", 0L).takeIf { it > 0 },
                                checkedInToday = a.optBoolean("checkedInToday"),
                                edition = a.optString("edition").takeIf { it.isNotBlank() },
                            )
                        }
                    } ?: emptyList(),
                    doneCount = g.optInt("doneCount"),
                    totalCount = g.optInt("totalCount"),
                )
            }
        } ?: emptyList()

        val outOfScope = daily?.optJSONArray("outOfScope")?.let { arr ->
            (0 until arr.length()).mapNotNull { i ->
                val o = arr.optJSONObject(i) ?: return@mapNotNull null
                OutOfScopeRow(
                    label = o.optString("label"),
                    reason = o.optString("reason"),
                    count = o.optInt("count"),
                )
            }
        } ?: emptyList()

        val onboarding = extras?.optJSONArray("onboarding")?.let { arr ->
            (0 until arr.length()).mapNotNull { i ->
                val o = arr.optJSONObject(i) ?: return@mapNotNull null
                val id = o.optString("id")
                if (id.isBlank()) return@mapNotNull null
                // settled 是「一次性福利的结算记忆」：null 或缺失 = 还没结清；
                // 对象 = 已结清（带 at 结算时刻）。
                val settled = o.optJSONObject("settled")
                OnboardingAccount(
                    id = id,
                    name = o.optString("name").ifBlank { id },
                    provider = o.optString("provider"),
                    settled = settled != null,
                    settledAt = settled?.optLong("at", 0L)?.takeIf { it > 0 }
                        ?: settled?.optLong("settledAt", 0L)?.takeIf { it > 0 },
                )
            }
        } ?: emptyList()

        val plans = extras?.optJSONArray("plans")?.let { arr ->
            (0 until arr.length()).mapNotNull { i ->
                val o = arr.optJSONObject(i) ?: return@mapNotNull null
                val id = o.optString("id")
                if (id.isBlank()) return@mapNotNull null
                val ledger = o.optJSONObject("claimPlans")
                val map = HashMap<String, Long>()
                ledger?.keys()?.forEach { key ->
                    val at = ledger.optLong(key, 0L)
                    if (at > 0) map[key] = at
                }
                PlanAccount(
                    id = id,
                    name = o.optString("name").ifBlank { id },
                    claimAt = o.optLong("claimAt", 0L).takeIf { it > 0 },
                    claimPlans = map,
                )
            }
        } ?: emptyList()

        val growth = extras?.optJSONArray("growth")?.let { arr ->
            (0 until arr.length()).mapNotNull { i ->
                val o = arr.optJSONObject(i) ?: return@mapNotNull null
                val id = o.optString("id")
                if (id.isBlank()) return@mapNotNull null
                GrowthAccount(id = id, name = o.optString("name").ifBlank { id })
            }
        } ?: emptyList()

        val rounds = root.optJSONArray("history")?.let { arr ->
            (0 until arr.length()).mapNotNull { i ->
                val r = arr.optJSONObject(i) ?: return@mapNotNull null
                val failed = r.optJSONArray("failed")?.let { list ->
                    (0 until list.length()).mapNotNull { j -> list.optString(j).takeIf { it.isNotBlank() } }
                } ?: emptyList()
                RoundRow(
                    at = r.optLong("at"),
                    date = r.optString("date"),
                    reason = r.optString("reason"),
                    succeeded = r.optInt("succeeded"),
                    active = r.optInt("active"),
                    total = r.optInt("total"),
                    skipped = r.optInt("skipped"),
                    failedCount = r.optInt("failedCount"),
                    failed = failed,
                )
            }
        } ?: emptyList()

        val schedules = auto?.optJSONArray("schedules")?.let { arr ->
            (0 until arr.length()).mapNotNull { i -> arr.optJSONObject(i) }
        } ?: emptyList()

        val scheduleRows = schedules.mapNotNull { s ->
            val id = s.optString("id")
            if (id.isBlank()) return@mapNotNull null
            val providers = s.optJSONArray("providers")?.let { ps ->
                (0 until ps.length()).mapNotNull { j -> ps.optString(j).takeIf { it.isNotBlank() } }
            } ?: emptyList()
            ScheduleRow(
                id = id,
                displayLabel = s.optString("displayLabel").ifBlank { s.optString("time") },
                label = s.optString("label"),
                time = s.optString("time"),
                enabled = s.optBoolean("enabled"),
                providers = providers,
                lastFiredToday = s.optBoolean("lastFiredToday"),
                nextRunAt = s.optLong("nextRunAt", 0L).takeIf { it > 0 },
                lastResult = s.optString("lastResult").takeIf { it.isNotBlank() },
            )
        }

        val keepModels = keep?.optJSONArray("models")?.let { arr ->
            (0 until arr.length()).mapNotNull { i -> arr.optString(i).takeIf { it.isNotBlank() } }
        } ?: emptyList()
        val keepDefault = keep?.optJSONArray("defaultModels")?.let { arr ->
            (0 until arr.length()).mapNotNull { i -> arr.optString(i).takeIf { it.isNotBlank() } }
        } ?: emptyList()

        return Snap(
            fetchedAtMs = System.currentTimeMillis(),
            groups = groups,
            outOfScope = outOfScope,
            todayDone = daily?.optInt("todayDone") ?: 0,
            todayEligible = daily?.optInt("todayEligible") ?: 0,
            onboarding = onboarding,
            plans = plans,
            growth = growth,
            rounds = rounds,
            scheduleRows = scheduleRows,
            keepalive = keepModels,
            keepaliveDefault = keepDefault,
            autoEnabled = auto?.optBoolean("enabled") ?: false,
            autoRunning = auto?.optBoolean("running") ?: false,
            nextRunAt = auto?.optLong("nextRunAt", 0L)?.takeIf { it > 0 },
            schedulesTotal = schedules.size,
            schedulesEnabled = schedules.count { it.optBoolean("enabled") },
            schedulesFiredToday = schedules.count {
                it.optBoolean("enabled") && it.optBoolean("lastFiredToday")
            },
            providerOptions = auto?.optJSONArray("providerOptions")?.let { arr ->
                (0 until arr.length()).mapNotNull { i ->
                    val o = arr.optJSONObject(i) ?: return@mapNotNull null
                    val id = o.optString("id")
                    if (id.isBlank()) return@mapNotNull null
                    id to o.optString("label").ifBlank { PresetRegistry.labelOf(id) }
                }
            } ?: emptyList(),
        )
    }
}
