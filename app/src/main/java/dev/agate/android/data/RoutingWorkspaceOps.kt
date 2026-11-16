package dev.agate.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/**
 * 路由工作台与路由组的数据层（「模型路由」页 / 「路由组」页）。
 *
 * 内核路由（`agate-server/src/server/http.rs` 的 `/api/routing/*` 注册块）：
 * - `GET  /api/routing/workspace?model=`  模型覆盖 + 参与名单 + 队列 + 粘滞计数
 * - `PUT  /api/routing/profile`           保存 / 恢复继承 / 调账号顺序（同端点）
 * - `GET  /api/routing/sticky?model=`     会话粘滞快照（谁被钉在哪）
 * - `POST /api/routing/sticky/clear`      忘掉指定会话（空数组 = 全清）
 * - `GET  /api/routing/groups`            组列表 + 新建修订号 + 成员目录
 * - `PUT  /api/routing/groups`            保存或删除一个路由组
 * - `POST /api/routing/preview`           零副作用预演（拖拽时实时看队列）
 *
 * ── 为什么 profile / group 原样留在 JSONObject 里 ──────────────────
 * 这两个结构是内核的**演进中**配置（`RoutingProfile` 8 个键、
 * `RoutingGroup` 7 个键），增键是常态。全量映射成 Kotlin 数据类的代价是：
 * 内核每加一个字段，Kotlin 要改数据类、改序列化、改测试三处；漏改的表现是
 * 「界面保存后那个字段被悄悄清空」—— 两个 PUT 都是**整块替换**语义，
 * 漏一个键就是一次静默的数据损坏，且只在真机上偶发、极难定位。
 *
 * 另一层原因在内核侧：`RoutingProfile` / `RoutingGroup` 都带
 * `deny_unknown_fields`。原样往返时多余的键会被内核 400 拦下；而「本地展开
 * 再拼回去」的写法要求 Kotlin 这边的字段集与内核逐字同步 —— 那正是上面那个
 * 缺陷的来源。所以这里只解界面渲染**必须**的标量（模型名、计数、修订号、
 * 状态），复杂嵌套对象整个交给界面：读出来改一个字段，再原样提交回去。
 *
 * ── 修订号不是礼貌，是并发安全 ──────────────────────────────────
 * 两个 PUT 都带 `revision`（sha256 十六进制 64 位）。内核拿它做乐观并发：
 * 不匹配就回 409 且不落库，不会覆盖别人（另一个窗口 / 另一个客户端）
 * 刚存的配置。因此 revision 必须来自**本次界面会话最近一次读到的响应**，
 * 不能是缓存里的旧值；409 的 error 串本身已是中文，可直接上屏。
 *
 * ── 与 [RoutingOps] 的分工 ─────────────────────────────────────
 * [RoutingOps] 管「全局九项策略 + 候选队列」；本对象管「逐模型 / 逐组的工作台」。
 * 两边读到的 `settings` 是**同一份** `routing_json` 形状 —— 需要现成的九项语义时
 * 套 `RoutingOps.Settings(...)` 即可，不必在这里再写一份映射：多一份就多一处漂移，
 * 而九项里「熔断默认关」「恢复阈值默认 2」这类兜底值写反了，界面会显示与
 * 实际相反的状态。
 */
object RoutingWorkspaceOps {

    /**
     * 参与名单里的一行提供商。
     *
     * [available] 是「此刻能承载本模型的请求」（内核用 `forwarding_providers`
     * 判据算的 eligible 集合）；注意响应里的行是
     * **eligible ∪ 生效档案里点到的家** —— 也就是说 [available] 为 false 的行
     * 恰恰是「档案里点了但当前不可用」的那几家，界面要显示它们（并标灰），
     * 不能过滤掉：过滤了就变成「用户的设置凭空消失」。
     */
    class ProviderRow(o: JSONObject) {
        val id: String = o.optString("id")
        val label: String = o.optString("label").ifBlank { id }
        val accountCount: Long = o.optLong("accountCount", 0L)

        /** 自定义提供商（用户自己填的 baseUrl，不是内置那几家）。 */
        val custom: Boolean = o.optBoolean("custom", false)
        val available: Boolean = o.optBoolean("available", false)
    }

    /**
     * 队列里的一位（可用或被跳过，都带原因）。
     *
     * 字段形状与 [RoutingOps.QueueRow] 对齐，但**独立定义**：两个端点的行字段
     * 本就不一样（预演的行多了 `candidateId` / `memberId` / `model` 且带
     * `providerLabel`，工作台的行没有 `providerLabel`），绑成一个类会让一边的
     * 缺键污染另一边的语义。独立定义后哪天要按端点分别演进，也不用动对方。
     */
    class QueueRow(o: JSONObject) {
        val id: String = o.optString("id")
        val name: String = o.optString("name").ifBlank { id }
        val provider: String = o.optString("provider")

        /**
         * 工作台的 `entry_json` **不发**这个键（预演才发），这里回落到
         * [provider] 的原始 id：宁可显示 `openai-compat` 这种 id，
         * 也不要显示空白 —— 空白会被读成「没有提供商」。
         */
        val providerLabel: String = o.optString("providerLabel").ifBlank { provider }
        val state: String = o.optString("state")
        val reason: String = o.optString("reason")

        /**
         * 冷却结束时刻（毫秒）。`0` = 没有冷却 —— 工作台在无冷却时发的是
         * **null** 而不是 0，`optLong` 会落到默认值，两种形态在这里同义。
         */
        val resetAt: Long = o.optLong("resetAt", 0L)

        /** 工作台与预演都不发该键，恒为 -1（保留只为与 [RoutingOps.QueueRow] 同形）。 */
        val priority: Long = o.optLong("priority", -1L)

        /** 能真正承载请求（`tried` 是预演里「模拟失败」的那条，不算）。 */
        val usable: Boolean get() = state == "usable"
    }

    /**
     * 队列快照：谁排第一 + 可用行 + 被跳过行。
     *
     * 两个端点给的队列都来自同一个 `routing::plan_route` —— 界面画成什么样，
     * 转发时基本就怎么走（预演的 `cursor` / `sticky` 置空只影响「轮转位次」，
     * 不影响排序判据）。
     */
    class QueueSnapshot(o: JSONObject) {
        /**
         * 队首 id。预演在队列为空时发 **null**，工作台发**空串**；
         * 两种都折成空串，界面判空只需比 `""`。
         */
        val pickedId: String = if (o.isNull("pickedId")) "" else o.optString("pickedId")
        val ordered: List<QueueRow> = jsonObjects(o, "ordered").map { QueueRow(it) }
        val blocked: List<QueueRow> = jsonObjects(o, "blocked").map { QueueRow(it) }

        /** 一个人都排不上（既没有可用行也没有被跳过行）—— 界面空态的判据。 */
        val isEmpty: Boolean get() = ordered.isEmpty() && blocked.isEmpty()
    }

    /**
     * 工作台一份完整快照（内核 `routing_workspace::workspace_json` 的形状，
     * 只取渲染所需）。
     *
     * 刻意没解的内核字段：
     * - `accounts`：账号全集（每个都在几十个键上下，账号页已有自己的数据源）；
     * - `accountConnections`：连接数仪表用，工作台只需画队列；
     * - `gateway`：端口 / 地址，网关状态另有观察通道；
     * - `configuredModels`：[models] 已经并进去了（内核把 advertised 列表与
     *   逐模型档案的键合并去重后才发出来）。
     */
    class Workspace(o: JSONObject) {
        /**
         * 请求里没带 `model` 时内核回 **null**（= 全局视角），这里折成 null
         * 而不是空串：界面据此区分「全局」与「某个模型」，混成 `""` 会后端
         * 语义错位（`?model=` 空值等于不带）。
         */
        val model: String? = if (o.isNull("model")) null else o.optString("model").ifBlank { null }

        /** 该模型有独立档案（不含「被路由组接管」这一种）。 */
        val customized: Boolean = o.optBoolean("customized", false)

        /** 全局默认档案存在 —— 即全局也被改过，不是出厂状态。 */
        val globalCustomized: Boolean = o.optBoolean("globalCustomized", false)

        /** 可选模型列表（已按小写排序去重，路由组接管的模型不在其中）。 */
        val models: List<String> = jsonStrings(o, "models")
        val providers: List<ProviderRow> = jsonObjects(o, "providers").map { ProviderRow(it) }

        /**
         * 当前**实际生效**的档案（全局默认 + 模型覆盖 + 组投影后再按可用性
         * materialize 过）。这也是「保存覆盖」要提交的形状。
         */
        val profile: JSONObject = o.optJSONObject("profile") ?: JSONObject()

        /**
         * 「把这个模型的覆盖去掉」之后会长什么样 —— 界面上「恢复继承」按钮旁
         * 的那段预览就是它。不需要恢复的场景下与 [profile] 相同。
         */
        val inheritedProfile: JSONObject = o.optJSONObject("inheritedProfile") ?: JSONObject()

        /** 64 位十六进制修订号，两个 PUT 的并发依据（见模块头）。 */
        val revision: String = o.optString("revision")

        /** 当前模型生效的九项（`routing_api::routing_json` 形状）。 */
        val settings: JSONObject = o.optJSONObject("settings") ?: JSONObject()

        /** 全局默认的九项（「这模型改过什么」的对照物）。 */
        val globalSettings: JSONObject = o.optJSONObject("globalSettings") ?: JSONObject()
        val queue: QueueSnapshot = QueueSnapshot(o.optJSONObject("queue") ?: JSONObject())

        /**
         * 活跃粘滞会话条数。内核按当前模型的粘滞 TTL 过滤，但**不按模型过滤**：
         * 这是全量视图，别在某模型的页面上把它当「本模型的会话数」。
         */
        val stickyCount: Long = o.optLong("stickyCount", 0L)

        /**
         * 账号 id → `{model, resetAt, limitedModels}`（原样保留，因为键就是
         * 账号 id，展开成 List 只会让界面多一次查找）。`resetAt` 0 = 无冷却，
         * `limitedModels` 是该账号当前处于限额的模型条数。
         */
        val accountStates: JSONObject = o.optJSONObject("accountStates") ?: JSONObject()

        /** 内核毫秒时间戳（`logging::now_ms`）—— 不是秒，格式化前别无脑 ×1000。 */
        val updatedAt: Long = o.optLong("updatedAt", 0L)
    }

    /**
     * 一个路由组：[group] 原样（`id` / `name` / `model` / `description` /
     * `strategy` / `sticky` / `members`），[revision] 是该组**当前**的修订号。
     *
     * 成员（成员 → 成员内账号名单）不在 Kotlin 里展开：编辑一个成员就是改
     * `group` 里的 `members[i]`，提交时整块交回即可（`deny_unknown_fields`
     * 会替我们校验键名，展开反而要自己保证「展开时没丢字段」）。
     */
    class GroupRow(o: JSONObject) {
        val group: JSONObject = o.optJSONObject("group") ?: JSONObject()
        val revision: String = o.optString("revision")
    }

    /**
     * 组列表 + 新建修订号 + 成员目录（编辑器一次拉全）。
     *
     * [newRevision] 是「新建一个组」要用的修订号：新建没有既有 group 可比，
     * 内核用一个固定的空组哈希当基线。改既有组时用 [GroupRow.revision]，
     * 两者不能混 —— 混了就是 409。
     */
    class GroupsSnapshot(o: JSONObject) {
        val groups: List<GroupRow> = jsonObjects(o, "groups").map { GroupRow(it) }
        val newRevision: String = o.optString("newRevision")
        val catalog: List<CatalogRow> = jsonObjects(o, "catalog").map { CatalogRow(it) }
    }

    /**
     * 建组时可加的成员（内核 `routing_groups::choices` 的一行）。
     *
     * [capabilities] 是生效后的能力表（6 个键：`maxInputTokens` /
     * `maxOutputTokens` / `supportsToolCall` / `supportsImages` /
     * `supportsVideo` / `supportsReasoning`），原样给出。值为 **null** 表示
     * 「没有声明这一项」，不是「不支持」—— 拿它做过滤时把 null 当宽松处理，
     * 否则会把没写能力表的模型全挡在门外。
     */
    class CatalogRow(o: JSONObject) {
        val provider: String = o.optString("provider")
        val providerLabel: String = o.optString("providerLabel").ifBlank { provider }
        val model: String = o.optString("model")
        val name: String = o.optString("name").ifBlank { model }
        val capabilities: JSONObject = o.optJSONObject("capabilities") ?: JSONObject()
        val accountCount: Long = o.optLong("accountCount", 0L)
    }

    /**
     * 一条被钉住的会话。
     *
     * [label] 是内核给的短标签（键过长时取末尾 12 字符并前置省略号）——
     * 直接用，别在界面上再截一次，否则会出现「…abc…abc」这种双重省略。
     */
    class StickySession(o: JSONObject) {
        /** 会话键（客户端身份 + 会话标识的派生），清粘滞时原样交回。 */
        val key: String = o.optString("key")
        val label: String = o.optString("label").ifBlank { key }
        val accountId: String = o.optString("accountId")
        val accountLabel: String = o.optString("accountLabel").ifBlank { accountId }

        /** 最后使用时刻（毫秒）。超过该模型粘滞 TTL 的会话内核已经不再返回。 */
        val lastUsedAt: Long = o.optLong("lastUsedAt", 0L)
    }

    /**
     * 粘滞快照（`GET /api/routing/sticky`）。
     *
     * 响应里的 `updatedAt` 没解：每条会话各自的 [StickySession.lastUsedAt]
     * 才是界面要显示的时间，整份快照的采样时刻没有展示位。
     */
    class StickySnapshot(o: JSONObject) {
        /**
         * 生效的粘滞模式：`auto` / `session` / `turn` / `off`。缺键按 `auto`
         * （与 `StickyMode` 的默认一致）。
         */
        val sticky: String = o.optString("sticky").ifBlank { "auto" }
        val sessions: List<StickySession> = jsonObjects(o, "sessions").map { StickySession(it) }

        val isEmpty: Boolean get() = sessions.isEmpty()
    }

    /**
     * 清粘滞的结果。
     *
     * [cleared] 是内核**真正忘掉**的条数：传了已过期或不存在的 key 不计入，
     * 所以「勾了 3 条，清了 2 条」是正常的（第三条早就过期了），不是失败。
     */
    class ClearResult(o: JSONObject) {
        /** 缺键按 true：能构造出这个对象，说明信封已经是 2xx `success` 了。 */
        val ok: Boolean = o.optBoolean("ok", true)
        val cleared: Long = o.optLong("cleared", 0L)
    }

    /**
     * 预演结果。
     *
     * 响应里还有一个 `preview: true`（恒真，只是给调用方的自证标记）——
     * 没解：走到这里本来就一定是预演请求。
     */
    class PreviewResult(o: JSONObject) {
        val model: String = o.optString("model")

        /** 组命中 或 该模型有独立档案。 */
        val customized: Boolean = o.optBoolean("customized", false)

        /** 命中的路由组（草稿或已存的）；按模型独立档案预演时为 null。 */
        val group: JSONObject? = o.optJSONObject("group")

        /** 生效设置：组会把自己的策略 / 粘滞叠在基础设置之上。 */
        val settings: JSONObject = o.optJSONObject("settings") ?: JSONObject()
        val queue: QueueSnapshot = QueueSnapshot(o.optJSONObject("queue") ?: JSONObject())
    }

    /**
     * 预演时模拟的请求能力需求（内核 `RequestNeeds`）。
     *
     * 单独做一个类而不是让界面拼 JSONObject：那个结构是
     * `deny_unknown_fields` 的，多一个键就 400，而三个布尔也不值得让每个
     * 调用点各写一遍（写错一个键名只会得到一句 400，不会得到编译错误）。
     */
    class Needs(
        val tools: Boolean = false,
        val images: Boolean = false,
        val video: Boolean = false,
    ) {
        fun toJson(): JSONObject = JSONObject()
            .put("tools", tools)
            .put("images", images)
            .put("video", video)
    }

    // ── 读 ──────────────────────────────────────────────────────

    /**
     * 工作台快照。`model` 为 null / 空白 = 全局视角（不带查询参数），
     * 与内核「model 可省」同一语义；空串不会当成模型名发出去。
     */
    suspend fun workspace(model: String? = null): OpResult<Workspace> {
        val data = get(withModel("/api/routing/workspace", model))
            ?: return OpResult(false, null, "无法连接内核")
        return OpResult(true, Workspace(data), null)
    }

    /**
     * 会话粘滞快照。
     *
     * `model` 只影响读到的**粘滞模式**（逐模型覆盖可能改了 `sticky`）与由此
     * 推出的 TTL；`sessions` 本身是全局的 —— 内核 `sticky_sessions` 不带模型
     * 过滤。所以别在某模型的页面上把条数当「本模型的会话」。
     */
    suspend fun sticky(model: String? = null): OpResult<StickySnapshot> {
        val data = get(withModel("/api/routing/sticky", model))
            ?: return OpResult(false, null, "无法连接内核")
        return OpResult(true, StickySnapshot(data), null)
    }

    /**
     * 路由组列表 + 成员目录，一次拉全。
     *
     * 不分两次请求：目录（能加哪些成员）与组列表（已经存了哪些）如果来自两个
     * 时刻，编辑器就会允许「加一个刚被删掉的账号」—— 那种错误要等到保存时才
     * 以一句「账号不存在，请刷新路由」暴露出来。
     */
    suspend fun groups(): OpResult<GroupsSnapshot> {
        val data = get("/api/routing/groups")
            ?: return OpResult(false, null, "无法连接内核")
        return OpResult(true, GroupsSnapshot(data), null)
    }

    // ── 写 ──────────────────────────────────────────────────────

    /**
     * 保存路由档案。一个端点承载三种写意图（内核按 body 形状分派）：
     *
     * 1. **保存覆盖**：`profile` 非 null + `revision` → 乐观并发写入。
     *    `model` 为空 = 写全局默认档案；
     * 2. **恢复继承**：`profile` 传 null + `revision` → 删掉该模型的覆盖，
     *    回落到全局 / 出厂；
     * 3. **调账号顺序**：`profile` + `orderChange`，且 `model` **必须为空**
     *    （内核只在全局分支里处理 `orderChange`，非空 model 会让这个键被静默忽略）。
     *
     * [orderChange] 的形状：`{"kind":"provider"}` 调各家之间的先后，
     * `{"kind":"account","provider":"xxx"}` 调某家内部账号顺序。**顺序本身不在这里传**
     * —— 内核按 `profile.providers` / `profile.accountOrder[provider]` 重排账号表，
     * 所以那份 profile 必须是用户拖拽后的结果，否则「提交的顺序」和「界面上的顺序」
     * 会对不上，而且内核只能在校验失败时才告诉你。
     *
     * 两个坑写在这里，免得每个调用点各踩一次：
     * - `revision` 与 `orderChange` 同时给时，内核走 revision 分支、顺序修改被忽略
     *   且**不报错**。这里不替调用方猜，按界面动作二选一传。
     * - 全局档案保存时内核会清掉 `accountOrder`（全局顺序由账号表自己承载）。
     *   也就是说全局 profile 读回来的 `accountOrder` 永远是空的，别拿它当「全局顺序」。
     * - 全局档案里的 `strategy` / `sticky` 也**不是**档案字段：内核把它们提升成
     *   全局路由设置（落 `routingStrategy` / `routingSticky`），档案本体只剩参与范围。
     *   保存后再读 [Workspace.profile] 看不到这两个键是正常的 —— 它们体现在
     *   [Workspace.globalSettings] 里。
     *
     * 省略 `revision` = 不做并发校验（内核直接写）。只应在用户明确选择
     * 「丢弃别处的修改」时这样用。
     *
     * 失败文案可直接上屏：400 是校验原因（「未知提供商」「账号不存在，请刷新路由」），
     * 409 是别处改过（「路由配置已在其他位置修改」），都是后端拟好的中文。
     */
    suspend fun saveProfile(
        model: String? = null,
        profile: JSONObject? = null,
        revision: String? = null,
        orderChange: JSONObject? = null,
    ): OpResult<Workspace> {
        val body = JSONObject()
            .put("model", model?.takeIf { it.isNotBlank() } ?: JSONObject.NULL)
            .put("profile", profile ?: JSONObject.NULL)
        revision?.let { body.put("revision", it) }
        orderChange?.let { body.put("orderChange", it) }
        return write("PUT", "/api/routing/profile", body).mapData { Workspace(it) }
    }

    /**
     * 忘掉指定会话的粘滞。
     *
     * [keys] 为空 = **全清**（内核把空数组当「没有限制」）。这不是「什么都不做」：
     * 想不动就别调这个函数，不要用空数组表达「不清」。
     *
     * 只影响下一次选路：已经在跑的请求不会被打断。
     */
    suspend fun clearSticky(keys: List<String> = emptyList()): OpResult<ClearResult> {
        val array = JSONArray()
        keys.forEach { array.put(it) }
        val body = JSONObject().put("keys", array)
        return write("POST", "/api/routing/sticky/clear", body).mapData { ClearResult(it) }
    }

    /**
     * 保存或删除一个路由组（同一端点：`group` 为 null 即删除）。
     *
     * [revision]：新建用 [GroupsSnapshot.newRevision]，改既有组用
     * [GroupRow.revision]。不匹配内核回 409 且不落库。
     *
     * 组内成员的账号名单必须与当前账号全集一致（内核 `validate_members` 会拒绝
     * 「基于过期视图」的名单），所以保存前不要拿几秒前缓存的账号列表拼。
     *
     * 保存成功后内核会**全清粘滞**（组变更后旧粘滞可能指向已移除的成员），
     * 界面据此把 [sticky] 读到的列表当作已失效，不必再拉一次判断。
     */
    suspend fun saveGroup(
        id: String,
        group: JSONObject?,
        revision: String,
    ): OpResult<GroupsSnapshot> {
        val body = JSONObject()
            .put("id", id)
            .put("group", group ?: JSONObject.NULL)
            .put("revision", revision)
        return write("PUT", "/api/routing/groups", body).mapData { GroupsSnapshot(it) }
    }

    /**
     * 零副作用预演：把草稿组 / 草稿档案丢进去，看队列会长什么样。
     *
     * 内核用的是与转发同一个 `plan_route`，但 `cursor = None`（不推进轮转位次）、
     * `sticky_account = None`（不读粘滞）—— 拖一百次也不会改变下一次真实请求
     * 的选路，所以这个端点可以跟着拖拽实时调。
     *
     * [failFirst] 为 true 时把当前首选放进排除表再算一遍，模拟「首选挂了轮到谁」；
     * 被模拟排除的那条 `state` 是 `tried`，`reason` 是「模拟首选成员失败，未实际发送」。
     *
     * [group] 与 [model] 的模型 ID 必须一致（内核校验，不一致直接 400）。
     * [global] 只在「该模型还没有独立档案」时才有意义：它表示草稿要按全局默认
     * 档案预演。
     */
    suspend fun preview(
        model: String = "",
        group: JSONObject? = null,
        profile: JSONObject? = null,
        global: Boolean = false,
        needs: Needs? = null,
        failFirst: Boolean = false,
    ): OpResult<PreviewResult> {
        val body = JSONObject().put("model", model)
        group?.let { body.put("group", it) }
        profile?.let { body.put("profile", it) }
        // false 与缺省在 serde 侧等价，不塞进请求体：少一个键就少一次
        // 「deny_unknown_fields 挡住了谁」的排查。
        if (global) body.put("global", true)
        needs?.let { body.put("needs", it.toJson()) }
        if (failFirst) body.put("failFirst", true)
        return write("POST", "/api/routing/preview", body).mapData { PreviewResult(it) }
    }

    // ── 链路 ────────────────────────────────────────────────────

    /**
     * 读端点：解 `{success, data}` 信封（与 [RoutingOps] 同一条解包路径）。
     *
     * 失败一律 null —— 读端点不产生业务错误，调用方统一报「无法连接内核」。
     * 写端点不能用它：那边要把内核的中文 error（400 / 409）原样带给用户，
     * 所以走 [write]。
     */
    private suspend fun get(path: String): JSONObject? {
        val reply = withContext(Dispatchers.IO) { CoreTransport.get(path) }
        if (reply !is CoreTransport.Reply.Ok) return null
        val body = runCatching { JSONObject(reply.text) }.getOrNull() ?: return null
        if (!body.optBoolean("success")) return null
        return body.optJSONObject("data") ?: body
    }

    /**
     * 写端点：成功交出 `data`，失败交出可直接上屏的 error。
     *
     * 非 2xx 时读 `error` 字段（内核管理面错误统一是
     * `{success:false, error:"…"}`，文案已是中文），空则给兜底文案 —— 不能把
     * 空串交给界面，否则用户看到的是一个没有任何信息的错误条。
     */
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

    /** 把写请求的 `data` 再套一层解析；失败原样透出 error，不吞文案。 */
    private fun <T> OpResult<JSONObject>.mapData(build: (JSONObject) -> T): OpResult<T> {
        val payload = data
        return if (ok && payload != null) {
            OpResult<T>(true, build(payload), null)
        } else {
            OpResult<T>(false, null, error)
        }
    }

    private fun withModel(path: String, model: String?): String =
        if (model.isNullOrBlank()) path else "$path?model=${model.urlEnc()}"

    private fun String.urlEnc(): String = java.net.URLEncoder.encode(this, "UTF-8")

    class OpResult<T>(val ok: Boolean, val data: T?, val error: String?)
}

/**
 * `data[key]` 是对象数组时逐项取出。缺键给空列表；项不是对象就跳过 ——
 * 不让一行的形状问题炸掉整页（内核将来往数组里塞别的东西时，界面还能用）。
 */
private fun jsonObjects(o: JSONObject, key: String): List<JSONObject> =
    o.optJSONArray(key)?.let { arr ->
        (0 until arr.length()).mapNotNull { arr.optJSONObject(it) }
    } ?: emptyList()

/** `data[key]` 是字符串数组时取出非空项（内核会发空串占位的场景一并兜掉）。 */
private fun jsonStrings(o: JSONObject, key: String): List<String> =
    o.optJSONArray(key)?.let { arr ->
        (0 until arr.length()).map { arr.optString(it) }.filter { it.isNotBlank() }
    } ?: emptyList()
