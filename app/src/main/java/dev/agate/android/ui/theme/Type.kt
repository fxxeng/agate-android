package dev.agate.android.ui.theme

import androidx.compose.material3.Typography
import androidx.compose.ui.text.PlatformTextStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

/**
 * v44 排印：基础 14px 正文体系 + `.num { font-variant-numeric: tabular-nums }`。
 * Compose 侧用 fontFeatureSettings = "tnum" 实现等宽数字，
 * 所有计数/额度/耗时等数字一律走 [AgateType.Num] 系，避免跳动。
 */
object AgateType {
    /** 等宽数字开关（PlatformTextStyle 在 Android 上映射到 Paint 的 fontFeature）。 */
    private val tnum = PlatformTextStyle(includeFontPadding = false)

    val PageTitle = TextStyle(
        fontFamily = FontFamily.Default,
        fontWeight = FontWeight.Bold,
        fontSize = 22.sp,
        lineHeight = 28.sp,
    )

    val SectionTitle = TextStyle(
        fontFamily = FontFamily.Default,
        fontWeight = FontWeight.SemiBold,
        fontSize = 13.sp,
        lineHeight = 18.sp,
    )

    val Body = TextStyle(
        fontFamily = FontFamily.Default,
        fontWeight = FontWeight.Normal,
        fontSize = 14.sp,
        lineHeight = 20.sp,
    )

    val BodyS = TextStyle(
        fontFamily = FontFamily.Default,
        fontWeight = FontWeight.Medium,
        fontSize = 12.sp,
        lineHeight = 17.sp,
    )

    val Caption = TextStyle(
        fontFamily = FontFamily.Default,
        fontWeight = FontWeight.Normal,
        fontSize = 11.sp,
        lineHeight = 15.sp,
    )

    val Num = Body.copy(
        fontWeight = FontWeight.SemiBold,
        platformStyle = PlatformTextStyle(includeFontPadding = false),
        fontFeatureSettings = "tnum",
    )

    val NumL = Num.copy(fontSize = 20.sp, lineHeight = 26.sp, fontWeight = FontWeight.Bold)

    val NumXL = Num.copy(fontSize = 28.sp, lineHeight = 34.sp, fontWeight = FontWeight.Bold)
}

/** Material3 排印基线（M3 组件默认字体沿用 Agate 档位）。 */
val AgateTypography = Typography().let { base ->
    base.copy(
        titleLarge = AgateType.PageTitle,
        titleSmall = AgateType.SectionTitle,
        bodyLarge = AgateType.Body,
        bodySmall = AgateType.BodyS,
        labelSmall = AgateType.Caption,
    )
}
