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
}

/** 提供商头像：有官方 logo 画 logo，否则用两字母头像兜底。 */
@Composable
fun AgateProviderAvatar(
    providerId: String,
    label: String,
    size: Dp,
    corner: Dp = 13.dp,
) {
    val res = ProviderIcons.resOf(providerId)
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