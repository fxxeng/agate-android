package dev.agate.android

import android.content.pm.PackageManager
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.enableEdgeToEdge
import androidx.activity.compose.setContent
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import dev.agate.android.navigation.AgateNavHost
import dev.agate.android.ui.theme.AgateTheme
import dev.agate.android.ui.theme.AgateThemeMode
import dev.agate.core.GatewayPrefs

/**
 * M2 起为 Compose 骨架：AgateTheme（三档主题）+ AgateNavHost
 * （4 主 Tab + 侧抽屉 + 22 二级页路由）。M1 网关控制迁入「网关控制」页常驻。
 */
class MainActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        // 系统的 preview 窗口按 manifest theme + **系统**夜间模式绘制，读不到
        // 用户手动选的档位。跟随系统档由 values-night 自动分流；手动档在这里
        // 于窗口绘制前切到固定色 style，把 Activity 自己的第一帧纠正过来。
        // 之后由 AgateTheme 的 SideEffect 按主题态实时接管（见 Theme.kt）。
        when (runCatching { AgateThemeMode.valueOf(GatewayPrefs.themeMode(this)) }
            .getOrDefault(AgateThemeMode.FOLLOW_SYSTEM)) {
            AgateThemeMode.LIGHT -> setTheme(R.style.Theme_Agate_Light)
            AgateThemeMode.DARK -> setTheme(R.style.Theme_Agate_Dark)
            AgateThemeMode.FOLLOW_SYSTEM -> Unit
        }
        super.onCreate(savedInstanceState)
        // 显式开启边到边，让各版本表现一致。
        //
        // targetSdk = 35 → Android 15 **本来就强制** edge-to-edge，而外壳已经
        // 按这个前提写好了（`Scaffold(contentWindowInsets = 0)` + 页头
        // statusBarsPadding + 底栏 navigationBarsPadding，见 AgateNavHost 的注释）。
        // 不调用这个 API 的话，API 26~34 上是「系统帮忙让开」，35+ 是「自己让开」，
        // 两套行为靠运气对齐 —— 一旦哪天在低版本上看到一个多出来的白边，很难判断
        // 是布局写错还是系统版本差异。显式开启后只有一套行为。
        enableEdgeToEdge()
        requestNotificationPermission()
        setContent {
            // F13：主题档持久化到 GatewayPrefs（冷启动读档；rememberSaveable
            // 兜底进程内配置变更）。脏值（老版本枚举名变更等）回退跟随系统。
            var themeMode by rememberSaveable {
                mutableStateOf(
                    runCatching { AgateThemeMode.valueOf(GatewayPrefs.themeMode(this@MainActivity)) }
                        .getOrDefault(AgateThemeMode.FOLLOW_SYSTEM)
                )
            }
            AgateTheme(mode = themeMode) {
                AgateNavHost(
                    themeMode = themeMode,
                    onThemeModeChange = {
                        themeMode = it
                        GatewayPrefs.setThemeMode(this@MainActivity, it.name)
                    },
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
