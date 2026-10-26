package dev.agate.android.data.dto

import org.json.JSONObject

/**
 * 账号与提供商 DTO（M3-3 从 GatewayRepository 拆出，方案 §7.1 单一数据源）。
 *
 * 字段口径与内核 account_store::store_view 对齐：
 * - /api/session 的 `accounts` 字段是对象信封 {currentAccountId, currentAccountIds,
 *   accounts[], providers[]}，不是裸数组（M3-2 曾按裸数组解析：零账号时恰好都对，
 *   有账号后 KPI 恒 0 —— 本文件修正，解析统一收口到这里）。
 * - 每条账号是各家公开形态（字段集各家不同）+ 内核统一注入的跨家事实
 *   （hasCredentials / chatSupported / enabled / priority …，见 store_view 的注入说明）。
 */

/** 一条账号（跨家通用视图：展示名按各家常见字段链取，不做逐家适配）。 */
data class AccountDto(
    val id: String,
    val provider: String,
    val displayName: String,
    val enabled: Boolean,
    val hasCredentials: Boolean,
    val chatSupported: Boolean,
    val priority: Int,
    /** 有地区概念的家才有（qoder/zcode/accio…），界面用于副标题。 */
    val edition: String?,
    val lowBalance: Boolean,
    /** 原始 JSON：详情页按家取专有字段时用，列表页不碰。 */
    val raw: JSONObject,
)

/** /api/session → accounts 信封的整体视图。 */
data class AccountsSnapshot(
    val accounts: List<AccountDto>,
    /** provider id → 账号总数（含禁用，与分组标题「N 个账号」口径一致）。 */
    val providerCounts: Map<String, Int>,
    /** provider id → 该家队首账号 id（界面 ★ 判据；全局 currentAccountId 不分家，不用）。 */
    val currentAccountIds: Map<String, String>,
)

object AccountDtos {

    /** 展示名字段链：nameCustom（用户自定义）优先，其余按各家公开形态常见字段兜底。 */
    private val NAME_FIELDS = listOf(
        "nameCustom", "name", "nickname", "loginName", "uid", "userId", "phone", "label",
    )

    /**
     * 从字段链里取展示名。
     *
     * ⚠️ 不能用 `optString(key)` 直接判空：`org.json` 的 `optString` 会把
     * 布尔/对象类型 `toString()` 出来 —— 某个家把 `name` 写成 `false` 时，
     * 界面就会显示成「false」（真机踩到过：WorkBuddy 国际版账号名变成 false）。
     * 这里只接受「非空字符串」与「非 0 数字」，其余类型一律跳过。
     */
    private fun pickName(acc: JSONObject): String? {
        for (key in NAME_FIELDS) {
            when (val value = acc.opt(key)) {
                is String -> value.trim().takeIf { it.isNotEmpty() }?.let { return it }
                is Number -> value.toLong().takeIf { it != 0L }?.toString()?.let { return it }
                else -> Unit // Boolean / JSONObject.NULL / 对象：跳过
            }
        }
        return null
    }

    /** session 为 null（本次未拉到）返回空视图；调用方按「沿用上一份快照」处理。 */
    fun parseSession(session: JSONObject?): AccountsSnapshot {
        if (session == null) return AccountsSnapshot(emptyList(), emptyMap(), emptyMap())
        val envelope = session.optJSONObject("accounts")
            ?: return AccountsSnapshot(emptyList(), emptyMap(), emptyMap())
        val arr = envelope.optJSONArray("accounts")
            ?: return AccountsSnapshot(emptyList(), emptyMap(), emptyMap())

        val list = ArrayList<AccountDto>(arr.length())
        for (i in 0 until arr.length()) {
            val acc = arr.optJSONObject(i) ?: continue
            val id = acc.optString("id")
            if (id.isBlank()) continue
            val name = pickName(acc) ?: id.takeLast(8)
            list += AccountDto(
                id = id,
                provider = acc.optString("provider"),
                displayName = name,
                enabled = acc.optBoolean("enabled", true),
                hasCredentials = acc.optBoolean("hasCredentials"),
                chatSupported = acc.optBoolean("chatSupported"),
                priority = acc.optInt("priority", 0),
                edition = acc.optString("edition").ifBlank { null },
                lowBalance = acc.optBoolean("lowBalance"),
                raw = acc,
            )
        }

        val counts = HashMap<String, Int>()
        envelope.optJSONArray("providers")?.let { ps ->
            for (i in 0 until ps.length()) {
                val p = ps.optJSONObject(i) ?: continue
                val pid = p.optString("id")
                if (pid.isNotBlank()) counts[pid] = p.optInt("count", 0)
            }
        }

        val heads = HashMap<String, String>()
        envelope.optJSONObject("currentAccountIds")?.let { obj ->
            for (key in obj.keys()) {
                val v = obj.optString(key)
                if (v.isNotBlank()) heads[key] = v
            }
        }
        return AccountsSnapshot(list, counts, heads)
    }
}
