package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

/** CheckinRun uncertain/reconciliation diagnostics. This API never retries a Provider claim. */
object CheckinRecovery {
    data class Pending(
        val runId: String,
        val accountId: String,
        val accountName: String,
        val provider: String,
        val accountStartedAt: Long,
        val lastError: String?,
        val source: String,
        val reason: String,
        val runStartedAt: Long,
    ) {
        val key: String get() = "$runId:$accountId"
    }

    data class Evidence(
        val evidenceId: Long,
        val provider: String,
        val verdict: String,
        val evidenceType: String,
        val observedAt: Long,
        val details: JSONObject,
        val resolved: Boolean,
    )

    data class PendingPage(val entries: List<Pending>, val total: Long)
    data class EvidencePage(val entries: List<Evidence>, val total: Long)

    data class ReconcileResult(
        val verdict: String,
        val reason: String?,
        val resolved: Boolean,
        val historyProjected: Boolean,
        val projectionError: String?,
        val evidenceId: Long?,
    )

    suspend fun queryPending(limit: Int = 100, offset: Int = 0): PendingPage? = withContext(Dispatchers.IO) {
        val data = CheckinOps.getData("/api/checkin-runs/uncertain?limit=${limit.coerceIn(1, 200)}&offset=${offset.coerceAtLeast(0)}")
            ?: return@withContext null
        val rows = data.optJSONArray("entries")
        val entries = if (rows == null) emptyList() else (0 until rows.length()).mapNotNull { index ->
            val row = rows.optJSONObject(index) ?: return@mapNotNull null
            val runId = row.optString("runId")
            val accountId = row.optString("accountId")
            if (runId.isBlank() || accountId.isBlank()) return@mapNotNull null
            Pending(
                runId = runId,
                accountId = accountId,
                accountName = row.optString("accountName").ifBlank { accountId },
                provider = row.optString("provider"),
                accountStartedAt = row.optLong("accountStartedAt"),
                lastError = row.optString("lastError").takeIf { it.isNotBlank() && it != "null" },
                source = row.optString("source"),
                reason = row.optString("reason"),
                runStartedAt = row.optLong("runStartedAt"),
            )
        }
        PendingPage(entries, data.optLong("total"))
    }

    suspend fun reconcile(item: Pending): ReconcileResult? = withContext(Dispatchers.IO) {
        val path = "/api/checkin-runs/reconcile?runId=${encode(item.runId)}&accountId=${encode(item.accountId)}"
        val result = CheckinOps.post(path, JSONObject())
        if (!result.ok) return@withContext null
        val data = result.data ?: return@withContext null
        val audit = data.optJSONObject("audit")
        ReconcileResult(
            verdict = data.optString("verdict", "unknown"),
            reason = data.optString("reason").takeIf { it.isNotBlank() && it != "null" },
            resolved = audit?.optBoolean("resolved") == true,
            historyProjected = audit?.optBoolean("historyProjected") == true,
            projectionError = audit?.optString("projectionError")?.takeIf { it.isNotBlank() && it != "null" },
            evidenceId = audit?.optLong("evidenceId")?.takeIf { it > 0 },
        )
    }

    suspend fun queryEvidence(item: Pending, limit: Int = 50): EvidencePage? = withContext(Dispatchers.IO) {
        val path = "/api/checkin-runs/evidence?runId=${encode(item.runId)}&accountId=${encode(item.accountId)}&limit=${limit.coerceIn(1, 100)}&offset=0"
        val data = CheckinOps.getData(path) ?: return@withContext null
        val rows = data.optJSONArray("entries")
        val entries = if (rows == null) emptyList() else (0 until rows.length()).mapNotNull { index ->
            val row = rows.optJSONObject(index) ?: return@mapNotNull null
            Evidence(
                evidenceId = row.optLong("evidenceId"),
                provider = row.optString("provider"),
                verdict = row.optString("verdict"),
                evidenceType = row.optString("evidenceType"),
                observedAt = row.optLong("observedAt"),
                details = row.optJSONObject("details") ?: JSONObject(),
                resolved = row.optBoolean("resolved"),
            )
        }
        EvidencePage(entries, data.optLong("total"))
    }

    private fun encode(value: String): String = java.net.URLEncoder.encode(value, "UTF-8")
}
