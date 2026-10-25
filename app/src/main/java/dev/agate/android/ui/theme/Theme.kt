package dev.agate.android.ui.theme

import android.app.Activity
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import android.os.Build
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalView
import androidx.core.view.WindowCompat

/** 主题三档：跟随系统 / 浅色 / 深色（不做更多主题）。 */
enum class AgateThemeMode { FOLLOW_SYSTEM, LIGHT, DARK }

val LocalAgateColors = staticCompositionLocalOf { AgateColors.Dark }

/** 组件取语义色的唯一入口：`val C = agateColors()`。 */
@Composable
fun agateColors(): AgateColors = LocalAgateColors.current

@Composable
fun AgateTheme(
    mode: AgateThemeMode = AgateThemeMode.FOLLOW_SYSTEM,
    content: @Composable () -> Unit,
) {
    val dark = when (mode) {
        AgateThemeMode.FOLLOW_SYSTEM -> isSystemInDarkTheme()
        AgateThemeMode.LIGHT -> false
        AgateThemeMode.DARK -> true
    }
    val C = if (dark) AgateColors.Dark else AgateColors.Light

    // Material3 ColorScheme 映射（让 M3 组件默认贴合 v44 视觉；组件内仍优先取 AgateColors）
    val scheme = if (dark) {
        darkColorScheme(
            primary = C.accent,
            onPrimary = C.onAccent,
            background = C.bg,
            onBackground = C.textMain,
            surface = C.cardBg,
            onSurface = C.textMain,
            surfaceVariant = C.insetBg,
            onSurfaceVariant = C.textSub,
            outline = C.border,
            outlineVariant = C.border,
            error = C.red,
            onError = C.onAccent,
            secondary = C.accent,
            onSecondary = C.onAccent,
            tertiary = C.green,
        )
    } else {
        lightColorScheme(
            primary = C.accent,
            onPrimary = C.onAccent,
            background = C.bg,
            onBackground = C.textMain,
            surface = C.cardBg,
            onSurface = C.textMain,
            surfaceVariant = C.insetBg,
            onSurfaceVariant = C.textSub,
            outline = C.border,
            outlineVariant = C.border,
            error = C.red,
            onError = C.onAccent,
            secondary = C.accent,
            onSecondary = C.onAccent,
            tertiary = C.green,
        )
    }

    // 系统栏跟随主题态：与页头/底栏同色（chromeBg），消除分界缝
    val view = LocalView.current
    if (!view.isInEditMode) {
        SideEffect {
            val window = (view.context as? Activity)?.window ?: return@SideEffect
            @Suppress("DEPRECATION")
            window.statusBarColor = C.chromeBg.toArgb()
            // 导航栏颜色交由 AgateNavHost 按「一级页/二级页」动态设置（二级页无底栏，
            // 需与页面底色一致）；这里只关掉系统对比度蒙层，否则手势条区域会多一条色带。
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                window.isNavigationBarContrastEnforced = false
            }
            WindowCompat.getInsetsController(window, view).apply {
                isAppearanceLightStatusBars = !dark
                isAppearanceLightNavigationBars = !dark
            }
        }
    }

    CompositionLocalProvider(LocalAgateColors provides C) {
        MaterialTheme(
            colorScheme = scheme,
            typography = AgateTypography,
            content = content,
        )
    }
}
