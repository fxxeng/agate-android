package dev.agate.android.ui.provider

import android.annotation.SuppressLint
import android.content.Intent
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.view.ViewGroup
import android.webkit.JavascriptInterface
import android.webkit.WebChromeClient
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import dev.agate.android.data.LoginClient
import dev.agate.android.data.ProviderPreset
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.agateColors
import dev.agate.core.AgateForegroundService
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import org.json.JSONObject

/**
 * AutoClaw 国际版的网页登录（Zai / Google）。
 *
 * ── 为什么要单独一页，而不是普通的「内嵌 / 浏览器登录」────────────
 * 官方主登录方式是 OAuth，而它的授权地址**必须先在浏览器端过一次阿里云滑块
 * 风控**才拿得到（内核 `/api/session/login/start` 对 AutoClaw 一律 400，指向
 * `/api/session/login/oauth/start`，见 `api/session.rs`）。所以这里是三段：
 *   ① 取风控配置（prefix / sceneId）→ ② WebView 跑滑块拿 captchaVerifyParam
 *   → ③ 用验证串换授权地址，交系统浏览器打开并轮询等待。
 *
 * 滑块必须跑在浏览器环境（阿里云 SDK 是 JS），Android 这里用内嵌 WebView；
 * JS → Kotlin 用 `addJavascriptInterface` 把不透明验证串交回来（与桌面壳的
 * bridge 同一职责，只是壳换成 Compose）。
 */
@Composable
internal fun AutoClawOAuthForm(
    preset: ProviderPreset,
    /** 网关是否在监听。OAuth 回调走浏览器打 `localhost:<port>`，不能用 JNI 直调顶替。 */
    gatewayUp: Boolean?,
    onDone: (Boolean, String) -> Unit,
) {
    val providerId = preset.id
    val label = preset.label
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    var busy by remember { mutableStateOf(false) }
    var hint by remember { mutableStateOf<String?>(null) }
    var config by remember { mutableStateOf<LoginClient.CaptchaConfig?>(null) }
    var pendingVendor by remember { mutableStateOf<String?>(null) }

    var state by remember { mutableStateOf<String?>(null) }
    var waiting by remember { mutableStateOf(false) }
    var seconds by remember { mutableStateOf(0) }

    fun openBrowser(url: String) {
        runCatching {
            ctx.startActivity(
                Intent(Intent.ACTION_VIEW, Uri.parse(url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            )
        }.onFailure { hint = "无法打开浏览器：${it.message}" }
    }

    // 等回调：与 ExternalLoginForm 同一套轮询，state 是内核任务的唯一句柄
    LaunchedEffect(waiting, state) {
        if (!waiting) return@LaunchedEffect
        while (isActive && waiting) {
            delay(2_000L)
            seconds += 2
            val current = state ?: break
            when (val r = LoginClient.wait(current)) {
                is LoginClient.WaitResult.Done -> {
                    waiting = false
                    onDone(true, "已添加 $label 账号")
                }
                is LoginClient.WaitResult.Failed -> {
                    waiting = false
                    hint = r.reason
                }
                else -> Unit
            }
        }
    }

    DisposableEffect(Unit) {
        onDispose { state?.let { s -> scope.launch { LoginClient.cancel(s) } } }
    }

    Text(
        "官方登录需要先在浏览器完成一次滑块风控，再由系统浏览器打开 Zai / Google 授权页。" +
            "回调地址是 localhost:<网关端口>，必须先运行网关，否则浏览器会得到「拒绝连接」。",
        color = C.textSub, fontSize = 12.sp,
    )
    Spacer(Modifier.height(10.dp))
    if (gatewayUp == false) {
        // 复用添加页同一套 amber 卡片口径：网关没跑时明确拦一道，别让用户白跑一趟。
        Column(
            Modifier
                .fillMaxWidth()
                .background(C.amber.copy(alpha = 0.10f), RoundedCornerShape(10.dp))
                .border(1.dp, C.amber.copy(alpha = 0.45f), RoundedCornerShape(10.dp))
                .padding(10.dp),
        ) {
            Text("网关未运行：现在发起会在授权完成后卡在 localhost 回调。", color = C.textMain, fontSize = 12.sp)
            Spacer(Modifier.height(8.dp))
            Button(
                onClick = { AgateForegroundService.start(ctx) },
                colors = ButtonDefaults.buttonColors(containerColor = C.accent, contentColor = C.onAccent),
            ) { Text("运行网关", fontWeight = FontWeight.SemiBold) }
        }
        Spacer(Modifier.height(10.dp))
    }

    if (waiting) {
        Row(
            Modifier
                .fillMaxWidth()
                .background(C.insetBg, RoundedCornerShape(10.dp))
                .padding(horizontal = 10.dp, vertical = 9.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("等待授权 · 已等待 ${seconds}s", color = C.accent, fontSize = 13.sp, modifier = Modifier.weight(1f))
            Text(
                "取消",
                color = C.red,
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.clickable {
                    waiting = false
                    state?.let { s -> scope.launch { LoginClient.cancel(s) } }
                    state = null
                },
            )
        }
    } else {
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            listOf("zai" to "Zai 登录", "google" to "Google 登录").forEach { (vendor, labelText) ->
                Button(
                    onClick = {
                        if (busy) return@Button
                        // 网关未运行直接拦：OAuth 回调落在网关端口，点了也只会卡在 localhost。
                        if (gatewayUp == false) {
                            hint = "请先运行网关，再发起网页登录（回调落在 localhost:<网关端口>）"
                            return@Button
                        }
                        busy = true
                        hint = null
                        scope.launch {
                            // ① 取风控配置 → ② 弹滑块宿主页（下方 AliyunCaptchaSheet）
                            val result = LoginClient.captchaConfig(providerId)
                            if (result.isSuccess) {
                                val cfg = result.getOrThrow()
                                if (!cfg.enabled) {
                                    hint = "这家当前不支持网页登录，请改用「粘贴凭证」"
                                } else if (cfg.prefix.isBlank() || cfg.sceneId.isBlank()) {
                                    hint = "风控验证配置不完整，请稍后重试"
                                } else {
                                    pendingVendor = vendor
                                    config = cfg
                                }
                            } else {
                                hint = result.exceptionOrNull()?.message ?: "取风控配置失败"
                            }
                            busy = false
                        }
                    },
                    modifier = Modifier.weight(1f),
                    colors = ButtonDefaults.buttonColors(containerColor = C.accent, contentColor = C.onAccent),
                ) { Text(if (busy) "准备中…" else labelText, fontWeight = FontWeight.SemiBold) }
            }
        }
    }

    hint?.let {
        Spacer(Modifier.height(10.dp))
        Text(it, color = C.red, fontSize = 12.sp)
    }

    val cfg = config
    val vendor = pendingVendor
    if (cfg != null && vendor != null) {
        AliyunCaptchaSheet(
            config = cfg,
            onDismiss = {
                config = null
                pendingVendor = null
            },
            onParam = { param ->
                config = null
                pendingVendor = null
                busy = true
                scope.launch {
                    // ③ 用不透明验证串换授权地址 → 系统浏览器打开 → 复用 wait 轮询
                    when (val r = LoginClient.oauthStart(providerId, vendor, param)) {
                        is LoginClient.StartResult.Started -> {
                            r.warning?.let { hint = it }
                            state = r.state
                            seconds = 0
                            waiting = true
                            openBrowser(r.authUrl)
                        }
                        is LoginClient.StartResult.Failed -> hint = r.reason
                    }
                    busy = false
                }
            },
        )
    }
}

/**
 * 阿里云滑块：内嵌 WebView 跑官方 SDK，验证串经 JS bridge 回传。
 *
 * ── 为什么用 `addJavascriptInterface` ─────────────────────────
 * 桌面端在页面里直接调 bridge 方法，Android 没有那条 IPC；滑块的
 * `captchaVerifyCallback` 只拿到一个不透明字符串，交给 Kotlin 原样发去内核即可
 * （不解析、不改写，空串会被上游当「没过风控」拒掉）。
 */
@SuppressLint("SetJavaScriptEnabled")
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun AliyunCaptchaSheet(
    config: LoginClient.CaptchaConfig,
    onDismiss: () -> Unit,
    onParam: (String) -> Unit,
) {
    val C = agateColors()
    val context = LocalContext.current
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)

    // 回调发生在 WebView 的后台线程，切回主线程再动 Compose 状态
    val main = remember { Handler(Looper.getMainLooper()) }
    val bridge = remember(onParam, onDismiss) {
        object {
            @JavascriptInterface
            fun onParam(param: String) {
                main.post { onParam(param) }
            }

            @JavascriptInterface
            fun onError(message: String) {
                main.post { Toast.makeText(context, "风控验证失败：$message", Toast.LENGTH_LONG).show() }
            }

            @JavascriptInterface
            fun onCancel() {
                main.post { onDismiss() }
            }
        }
    }

    val html = remember(config) {
        // 所有注入值都用 JSONObject.quote 转义：sceneId / prefix 哪天带引号或反斜杠，
        // 裸拼进 JS 会让整段脚本语法错误，表现就是「滑块一直不出现」。
        captchaPage(
            region = JSONObject.quote(config.region),
            prefix = JSONObject.quote(config.prefix),
            sceneId = JSONObject.quote(config.sceneId),
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
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("风控验证", color = C.textMain, fontSize = 16.sp, fontWeight = FontWeight.Bold)
                Spacer(Modifier.weight(1f))
                AgateBadge(text = "阿里云滑块", tone = BadgeTone.BLUE)
            }
            Spacer(Modifier.height(6.dp))
            Text("请在下方完成官方滑块验证，验证串只用于换取本次授权地址", color = C.textSub, fontSize = 12.sp)
            Spacer(Modifier.height(10.dp))
            // ── WebView 视口必须是「设备宽度」而不是桌面宽度 ──────────
            // useWideViewPort=true + loadWithOverviewMode=true 会让这条 data: 页按
            // 980px 布局再整体缩小，阿里云的 fixed popup 于是按 980px 定位 ——
            // 在窄屏上左右被裁（「完成安全验证」只剩半个字）。HTML 里已有
            // width=device-width,initial-scale=1，两边一起保证 popup 按实宽渲染。
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
                        // popup 里的「确认」按钮可能由 SDK 用 window.open 触发；
                        // 不给多窗口会把那一步静默吞掉，看起来就是滑块点不动。
                        settings.javaScriptCanOpenWindowsAutomatically = true
                        webChromeClient = WebChromeClient()
                        webViewClient = WebViewClient()
                        addJavascriptInterface(bridge, "AgateCaptcha")
                        loadDataWithBaseURL("https://www.aliyun.com/", html, "text/html", "utf-8", null)
                    }
                },
            )
            Spacer(Modifier.height(10.dp))
            Text(
                "取消",
                color = C.textSub,
                fontSize = 13.sp,
                modifier = Modifier.clickable { onDismiss() },
            )
        }
    }
}

/**
 * 极简宿主页：只提供阿里云 SDK 要求的 `window.AliyunCaptchaConfig`、挂载点与
 * V2 回调，其余全交给官方脚本（不抄一份 SDK 实现，上游改了也不会漂移）。
 *
 * `getInstance` 后延时 2.1s 再 `show()` 与桌面端同款 —— SDK 初始化后立刻弹会在
 * 部分机型上出现空白面板。
 */
internal fun captchaPage(region: String, prefix: String, sceneId: String): String = """
<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<style>html,body{margin:0;padding:0;background:#fff}#captcha-element{position:relative;z-index:2147483000}</style></head><body>
<div id="captcha-element"></div>
<button id="captcha-trigger" type="button" tabindex="-1" aria-hidden="true"
  style="position:fixed;width:1px;height:1px;opacity:0;pointer-events:none"></button>
<script>
window.AliyunCaptchaConfig = { region: $region, prefix: $prefix };
function fail(e){ window.AgateCaptcha && window.AgateCaptcha.onError(String((e && e.message) || e || '验证失败')); }
var s = document.createElement('script');
s.src = 'https://o.alicdn.com/captcha-frontend/aliyunCaptcha/AliyunCaptcha.js';
s.async = true;
s.onerror = function(){ fail('验证码组件加载失败，请检查网络后重试'); };
s.onload = function(){
  try {
    window.initAliyunCaptcha({
      SceneId: $sceneId,
      mode: 'popup',
      element: '#captcha-element',
      button: '#captcha-trigger',
      language: 'cn',
      showErrorTip: true,
      zIndex: 2147483000,
      getInstance: function(instance){
        setTimeout(function(){ instance.show(); }, 2100);
      },
      captchaVerifyCallback: function(param){
        // 不在这里等业务：bizResult 只表示「给了验证串」，业务失败由 Kotlin 侧
        // 报错并让用户重来（面板先收起，避免两处状态错乱）。
        window.AgateCaptcha.onParam(String(param || ''));
        return { captchaResult: true, bizResult: true };
      },
      onBizResultCallback: function(){},
      fail: fail,
      onError: fail
    });
  } catch (e) { fail(e); }
};
document.head.appendChild(s);
</script></body></html>
""".trimIndent()
