package dev.agate.android.ui.tasks

import android.annotation.SuppressLint
import android.os.Handler
import android.os.Looper
import android.view.ViewGroup
import android.webkit.JavascriptInterface
import android.webkit.WebChromeClient
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import dev.agate.android.ui.provider.captchaPage
import dev.agate.android.ui.theme.agateColors
import org.json.JSONObject

/**
 * ZCode 领取专用的阿里云无痕滑块弹层。
 *
 * ── 为什么复用 AutoClaw 的 `captchaPage`，而不是另写一份 SDK 注入 ──
 * 两者都是阿里云验证码 V3 的无痕验证，宿主页（SDK URL、初始化参数、V2 回调
 * `captchaVerifyCallback`、JS bridge 名字）必须逐字一致才不会漂移。
 * `captchaPage` 是 internal 顶层函数（同 module 可见），这里只负责容器与回传。
 *
 * ── 验证串怎么流转 ────────────────────────────────────────────
 * SDK 解出的不透明串经 `AgateCaptcha.onParam` 回传 → Kotlin 原样交给
 * `CheckinOps.planClaim` → 内核把它当 `X-Aliyun-Captcha-Verify-Param` 转发。
 * 本层不解析、不改写（空串会被上游当「没过风控」拒掉）。
 */
@SuppressLint("SetJavaScriptEnabled")
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ZcodeCaptchaSheet(
    prefix: String,
    region: String,
    sceneId: String,
    onDismiss: () -> Unit,
    onParam: (String) -> Unit,
) {
    val C = agateColors()
    val context = LocalContext.current
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)

    // WebView 回调发生在后台线程，切回主线程再动 Compose 状态
    val main = remember { Handler(Looper.getMainLooper()) }
    val bridge = remember(onParam, onDismiss) {
        object {
            @JavascriptInterface
            fun onParam(param: String) {
                main.post { onParam(param) }
            }

            @JavascriptInterface
            fun onError(message: String) {
                main.post {
                    Toast.makeText(context, "风控验证失败：$message", Toast.LENGTH_LONG).show()
                }
            }

            @JavascriptInterface
            fun onCancel() {
                main.post { onDismiss() }
            }
        }
    }

    val html = remember(prefix, region, sceneId) {
        // 所有注入值用 JSONObject.quote 转义：sceneId/prefix 哪天带引号或反斜杠，
        // 裸拼进 JS 会让整段脚本语法错误，表现就是「滑块一直不出现」。
        captchaPage(
            region = JSONObject.quote(region),
            prefix = JSONObject.quote(prefix),
            sceneId = JSONObject.quote(sceneId),
        )
    }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheetState,
        containerColor = C.cardBg,
    ) {
        Column(
            Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp)
                .padding(bottom = 16.dp),
        ) {
            Text("领取验证", color = C.textMain, fontSize = 16.sp)
            Spacer(Modifier.height(6.dp))
            Text(
                "完成官方滑块验证后自动领取套餐，验证串只用于本次领取",
                color = C.textSub,
                fontSize = 12.sp,
            )
            Spacer(Modifier.height(10.dp))
            // 视口必须是「设备宽度」：useWideViewPort=true 会让 data: 页按 980px
            // 布局再缩小，阿里云 fixed popup 在窄屏被左右裁切（同 AutoClaw 修复）。
            AndroidView(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(420.dp)
                    .background(C.insetBg, RoundedCornerShape(10.dp)),
                factory = { ctx ->
                    WebView(ctx).apply {
                        layoutParams = ViewGroup.LayoutParams(
                            ViewGroup.LayoutParams.MATCH_PARENT,
                            ViewGroup.LayoutParams.MATCH_PARENT,
                        )
                        settings.javaScriptEnabled = true
                        settings.domStorageEnabled = true
                        settings.useWideViewPort = false
                        settings.loadWithOverviewMode = false
                        settings.javaScriptCanOpenWindowsAutomatically = true
                        webChromeClient = WebChromeClient()
                        webViewClient = WebViewClient()
                        addJavascriptInterface(bridge, "AgateCaptcha")
                        loadDataWithBaseURL("https://www.aliyun.com/", html, "text/html", "utf-8", null)
                    }
                },
            )
        }
    }
}
