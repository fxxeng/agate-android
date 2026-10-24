package dev.agate.android

import android.content.pm.PackageManager
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import dev.agate.android.navigation.AgateNavHost
import dev.agate.android.ui.theme.AgateTheme
import dev.agate.android.ui.theme.AgateThemeMode

/**
 * M2 起为 Compose 骨架：AgateTheme（三档主题）+ AgateNavHost
 * （4 主 Tab + 侧抽屉 + 22 二级页路由）。M1 网关控制迁入「网关控制」页常驻。
 */
class MainActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        requestNotificationPermission()
        setContent {
            var themeMode by rememberSaveable { mutableStateOf(AgateThemeMode.FOLLOW_SYSTEM) }
            AgateTheme(mode = themeMode) {
                AgateNavHost(
                    themeMode = themeMode,
                    onThemeModeChange = { themeMode = it },
                )
            }
        }
    }

    private fun requestNotificationPermission() {
        if (android.os.Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(android.Manifest.permission.POST_NOTIFICATIONS), 1)
        }
    }
}
