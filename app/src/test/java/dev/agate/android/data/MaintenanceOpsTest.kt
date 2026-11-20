package dev.agate.android.data

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** 维护记录解析的纯测试（不依赖 Android / CoreTransport）。 */
class MaintenanceOpsTest {

    private fun runJson(): JSONObject = JSONObject().apply {
        put("id", "1700000000-abc")
        put("taskId", "credential_maintenance")
        put("source", "manual")
        put("startedAt", 1700000000000L)
        put("finishedAt", 1700000003000L)
        put("durationMs", 3000L)
        put("status", "partial")
        put("refreshed", 1)
        put("skipped", 2)
        put("failed", 1)
        put("total", 4)
    }

    private fun ok(data: JSONObject): CoreTransport.Reply.Ok =
        CoreTransport.Reply.Ok(200, JSONObject().put("success", true).put("data", data).toString())

    @Test
    fun parses_run_fields_and_nullable() {
        val run = MaintenanceOps.Run.from(runJson())
        assertEquals("1700000000-abc", run.id)
        assertEquals("credential_maintenance", run.taskId)
        assertEquals("partial", run.status)
        assertEquals(1, run.refreshed)
        assertEquals(4, run.total)
        assertTrue(!run.isRunning)
        assertNull(run.retryOf)
        assertNull(run.error)
    }

    @Test
    fun parses_detail_with_items() {
        val data = JSONObject().apply {
            put("id", "r1")
            put("taskId", "model_refresh")
            put("status", "completed")
            put("results", JSONArray().put(JSONObject().apply {
                put("id", "0")
                put("provider", "openai")
                put("providerLabel", "OpenAI")
                put("status", "refreshed")
                put("credentialChanged", true)
                put("forced", false)
            }))
        }
        val outcome = MaintenanceOps.parseReply(ok(data), MaintenanceOps.Detail::from)
        assertTrue(outcome is MaintenanceOps.Outcome.Ok)
        val detail = (outcome as MaintenanceOps.Outcome.Ok).value
        assertEquals(1, detail.results.size)
        assertEquals("openai", detail.results[0].provider)
        assertEquals(true, detail.results[0].credentialChanged)
    }

    @Test
    fun parses_overview_page_and_latest() {
        val data = JSONObject().apply {
            put("records", JSONObject().apply {
                put("items", JSONArray().put(runJson()))
                put("total", 7)
                put("offset", 0)
                put("limit", 20)
            })
            put("latest", JSONObject().put("credential_maintenance", runJson()))
            put("retentionDays", 30)
            put("maxRecords", 2000)
        }
        val outcome = MaintenanceOps.parseReply(ok(data)) { obj ->
            val page = MaintenanceOps.Page.from(obj.optJSONObject("records")!!)
            val latest = HashMap<String, MaintenanceOps.Run>()
            obj.optJSONObject("latest")?.let { l ->
                val keys = l.keys()
                while (keys.hasNext()) {
                    val k = keys.next()
                    latest[k] = MaintenanceOps.Run.from(l.getJSONObject(k))
                }
            }
            MaintenanceOps.Overview(page, obj.optInt("retentionDays"), obj.optInt("maxRecords"), latest)
        }
        assertTrue(outcome is MaintenanceOps.Outcome.Ok)
        val ov = (outcome as MaintenanceOps.Outcome.Ok).value
        assertEquals(7, ov.records.total)
        assertEquals(1, ov.records.items.size)
        assertEquals(30, ov.retentionDays)
        assertEquals(1, ov.latest.size)
    }

    @Test
    fun management_error_envelope_maps_to_err() {
        val reply = CoreTransport.Reply.Ok(
            400,
            JSONObject().put("success", false).put("error", "未知的维护任务").toString(),
        )
        val outcome = MaintenanceOps.parseReply(reply, MaintenanceOps.Detail::from)
        assertTrue(outcome is MaintenanceOps.Outcome.Err)
        assertEquals("未知的维护任务", (outcome as MaintenanceOps.Outcome.Err).message)
    }

    @Test
    fun down_reply_maps_to_err() {
        val outcome = MaintenanceOps.parseReply(
            CoreTransport.Reply.Down("内核未就绪"),
            MaintenanceOps.Detail::from,
        )
        assertTrue(outcome is MaintenanceOps.Outcome.Err)
        assertEquals("内核未就绪", (outcome as MaintenanceOps.Outcome.Err).message)
    }
}
