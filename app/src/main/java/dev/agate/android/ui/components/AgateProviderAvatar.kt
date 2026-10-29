package dev.agate.android.ui.components

import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import dev.agate.android.R
import dev.agate.android.data.CustomProviderOps
import dev.agate.android.data.PresetApiRegistry

/**
 * 提供商官方 logo（取自上游 3.0.8 的 `assets/provider-icons`，16 家官方 Agent 全有）。
 * 自定义 / 中转上游没有官方图标 → 回落为两字母头像。
 */
object ProviderIcons {
    private val map: Map<String, Int> = mapOf(
        "workbuddy" to R.drawable.provider_workbuddy,
        "raccoon" to R.drawable.provider_raccoon,
        "catpaw" to R.drawable.provider_catpaw,
        "autoclaw" to R.drawable.provider_autoclaw,
        "qoder" to R.drawable.provider_qoder,
        "accio" to R.drawable.provider_accio,
        "zcode" to R.drawable.provider_zcode,
        "codearts" to R.drawable.provider_codearts,
        "trae" to R.drawable.provider_trae,
        "loomy" to R.drawable.provider_loomy,
        "kuku" to R.drawable.provider_kuku,
        "monkeycode" to R.drawable.provider_monkeycode,
        "commandcode" to R.drawable.provider_commandcode,
        "antigravity" to R.drawable.provider_antigravity,
        "officeace" to R.drawable.provider_officeace,
        "orcaterm" to R.drawable.provider_orcaterm,
        "cline" to R.drawable.provider_cline,
    )

    /**
     * 变体归一：同一产品线的国际版 / 国内版 / 双池共用同一图标
     * （`workbuddy-intl` → `workbuddy`、`accio-cn` → `accio`、`cline-free` → `cline`）。
     */
    private fun normalize(providerId: String): String = when {
        providerId.endsWith("-intl") -> providerId.removeSuffix("-intl")
        providerId.endsWith("-cn") -> providerId.removeSuffix("-cn")
        providerId.startsWith("cline") -> "cline"
        else -> providerId
    }

    fun resOf(providerId: String): Int? = map[normalize(providerId)] ?: map[providerId]

    /**
     * 自定义提供商的图标：按**名称**反查「预置 API」模板的图标。
     *
     * ── 为什么需要这一层 ─────────────────────────────────────────
     * 从「预置 API」卡片建出来的家，落库后身份是 `custom-xxxx`（内核
     * `custom_providers::ID_PREFIX`），上面那张 map 里当然没有它 —— 于是商汤
     * 日日新、OpenAI、DeepSeek 这些明明有官方 logo 的模板，建完就只剩一个
     * 两字母色块。模板的图标其实一直在手边（`PresetApiRegistry.ALL[].iconRes`），
     * 缺的只是「名称 → 图标」这一跳。
     *
     * ── 为什么按名称而不是新增字段 ───────────────────────────────
     * 「已建过」的判据本来就是**名称相等**（上游 `matchesPresetName` 的口径，
     * 见 `PresetApiRegistry` 的模块头）。用户改名后那家就与模板脱钩 —— 这是
     * 刻意的语义，图标也跟着脱落才是对的（它已经不再是「那家」了）。
     * 在提供商记录里加一个 iconKey 字段会引入第三种状态：改了名、图标还留着
     * 旧模板的，看起来比现在更乱。
     *
     * 匹配**忽略大小写与多余空白**：用户手打「openai」也该认出来。
     */
    fun resOfCustomName(name: String): Int? {
        val key = name.trim().lowercase()
        if (key.isEmpty()) return null
        return PresetApiRegistry.ALL
            .firstOrNull { it.name.trim().lowercase() == key }
            ?.iconRes
    }
}

/**
 * 提供商头像：有官方 logo 画 logo，否则用两字母头像兜底。
 *
 * [customName] 只给**自定义提供商**（`custom-xxxx`）用：它们的官方图标不在
 * 内置 map 里，但「从预置 API 卡片建出来的」家名字仍然对得上模板，能按名字
 * 反查出同一张 logo（见 [ProviderIcons.resOfCustomName] 的说明）。
 * 传空串 = 不是自定义家，行为与之前逐字一致。
 */
@Composable
fun AgateProviderAvatar(
    providerId: String,
    label: String,
    size: Dp,
    corner: Dp = 13.dp,
    customName: String = "",
) {
    // 顺序不能颠倒：内置 id 优先（`workbuddy` 这类永远走 map）；只有**自定义家**
    // （`custom-xxxx`）才拿名字去撞预置模板 —— 内置家没图标就是没图标，不该
    // 因为名字碰巧像某张模板而借用别人的 logo。
    val res = ProviderIcons.resOf(providerId)
        ?: if (CustomProviderOps.isCustomProviderId(providerId)) {
            ProviderIcons.resOfCustomName(customName.ifBlank { label })
        } else {
            null
        }
    if (res != null) {
        Image(
            painter = painterResource(res),
            contentDescription = label,
            modifier = Modifier.size(size).clip(RoundedCornerShape(corner)),
        )
    } else {
        AgateAvatar(
            name = label,
            size = size,
            corner = corner,
            letters = initialsOfLabel(label),
        )
    }
}

/** 名称 → 两字母（供无 logo 的兜底使用）。 */
internal fun initialsOfLabel(label: String): String {
    val words = label.split(' ', '　', '-', '_').filter { it.isNotBlank() }
    return if (words.size >= 2) {
        words.take(2).map { it.first().uppercaseChar() }.joinToString("")
    } else {
        label.take(2).uppercase()
    }
}