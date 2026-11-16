package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

/**
 * 模型页的两块路由配置：标题生成路由 + 上下文压缩路由。
 *
 * 内核端点（`api/title_routes.rs` / `api/compact_routes.rs`）：
 * - `GET/PUT /api/models/title-routes`    标题生成路由（按客户端分流到低成本模型）
 * - `GET/PUT /api/models/compact-routes`  上下文压缩路由（按客户端+来源模型映射）
 *
 * ── 为什么 config 用 JSONObject 原样保留 ─────────────────────────
 * 两块配置都是「整块 GET + 整块 PUT + revision 防覆盖」，且内核带
 * `deny_unknown_fields` —— 本地展开再拼回去，漏一个键就是静默清空。
 * 界面编辑时直接改 JSONObject 再提交，不做全量数据类映射。
 *
 * ── 写入语义 ─────────────────────────────────────────────────────
 * PUT body = {config: JSONObject, revision: String}。
 * revision 对不上内核返回 409（中文原文可直接上屏）。
 */
object ModelRoutesOps {

    /** 一块路由配置的 GET 快照。 */
    class RouteConfig(o: JSONObject) {
        /** 配置本体（原样保留，编辑时直接改这里再提交）。 */
        val config: JSONObject = o.optJSONObject("config") ?: JSONObject()
        /** 修订号（64 位 hex，PUT 时必传）。 */
        val revision: String = o.optString("revision")
        /** 可选客户端列表。 */
        val clients: List<Client> = o.optJSONArray("clients")?.let { arr ->
            (0 until arr.length()).mapNotNull { arr.optJSONObject(it)?.let(::Client) }
        } ?: emptyList()
        /** 可选模型列表（id + name，界面下拉用）。 */
        val models: List<Model> = o.optJSONArray("models")?.let { arr ->
            (0 until arr.length()).mapNotNull { arr.optJSONObject(it)?.let(::Model) }
        } ?: emptyList()
        /** 规则条数上限（compact 有，title 没有 → 0）。 */
        val maxRules: Int = o.optInt("maxRules", 0)
        /** 总开关（从 config 里取，缺省 true）。 */
        val enabled: Boolean = config.optBoolean("enabled", true)
        /** 规则条数。 */
        val ruleCount: Int = config.optJSONArray("rules")?.length() ?: 0
    }

    class Client(o: JSONObject) {
        val id: String = o.optString("id")
        val label: String = o.optString("label").ifBlank { id }
        val supported: Boolean = o.optBoolean("supported", true)
    }

    class Model(o: JSONObject) {
        val id: String = o.optString("id")
        val name: String = o.optString("name").ifBlank { id }
    }

    // ── 标题生成路由 ──────────────────────────────────────────────

    suspend fun titleRoutes(): OpResult<RouteConfig> {
        val data = get("/api/models/title-routes") ?: return OpResult(false, null, "无法连接内核")
        return OpResult(true, RouteConfig(data), null)
    }

    suspend fun saveTitleRoutes(config: JSONObject, revision: String): OpResult<RouteConfig> {
        val body = JSONObject().put("config", config).put("revision", revision)
        val r = write("PUT", "/api/models/title-routes", body)
        return if (r.ok && r.data != null) OpResult(true, RouteConfig(r.data), null)
        else OpResult(false, null, r.error)
    }

    // ── 上下文压缩路由 ────────────────────────────────────────────

    suspend fun compactRoutes(): OpResult<RouteConfig> {
        val data = get("/api/models/compact-routes") ?: return OpResult(false, null, "无法连接内核")
        return OpResult(true, RouteConfig(data), null)
    }

    suspend fun saveCompactRoutes(config: JSONObject, revision: String): OpResult<RouteConfig> {
        val body = JSONObject().put("config", config).put("revision", revision)
        val r = write("PUT", "/api/models/compact-routes", body)
        return if (r.ok && r.data != null) OpResult(true, RouteConfig(r.data), null)
        else OpResult(false, null, r.error)
    }

    // ── 通道（与 RoutingWorkspaceOps 同一模式）────────────────────

    private suspend fun get(path: String): JSONObject? {
        val reply = withContext(Dispatchers.IO) { CoreTransport.get(path) }
        if (reply !is CoreTransport.Reply.Ok) return null
        val body = runCatching { JSONObject(reply.text) }.getOrNull() ?: return null
        if (!body.optBoolean("success")) return null
        return body.optJSONObject("data") ?: body
    }

    private suspend fun write(
        method: String,
        path: String,
        body: JSONObject,
    ): OpResult<JSONObject> {
        val reply = withContext(Dispatchers.IO) {
            CoreTransport.call(method, path, body.toString())
        }
        if (reply !is CoreTransport.Reply.Ok) {
            return OpResult(false, null, (reply as? CoreTransport.Reply.Down)?.message ?: "链路错误")
        }
        val parsed = runCatching { JSONObject(reply.text) }.getOrNull()
        if (reply.status !in 200..299) {
            return OpResult(false, null, parsed?.optString("error").orEmpty().ifBlank { "保存失败" })
        }
        val data = parsed?.optJSONObject("data")
            ?: return OpResult(false, null, parsed?.optString("error").orEmpty().ifBlank { "内核响应异常" })
        return OpResult(true, data, null)
    }

    class OpResult<T>(val ok: Boolean, val data: T?, val error: String?)
}
