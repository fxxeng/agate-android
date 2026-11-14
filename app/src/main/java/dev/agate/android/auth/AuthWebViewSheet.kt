package dev.agate.android.auth

import android.annotation.SuppressLint
import android.net.http.SslError
import android.webkit.ConsoleMessage
import android.webkit.CookieManager
import android.webkit.SslErrorHandler
import android.webkit.WebChromeClient
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebStorage
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import dev.agate.android.data.LoginClient
import dev.agate.android.data.PresetRegistry
import dev.agate.android.ui.components.AgateBadge
import dev.agate.android.ui.components.BadgeTone
import dev.agate.android.ui.theme.agateColors
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/**
 * Cookie 串里有没有**登录态**（`names` 之一存在且非空）。
 *
 * 名单由各家在 `ProviderPreset.sessionCookieNames` 里声明（豆包是 `sessionid` 系、
 * KukuAI 是百度通行证的 `BDUSS`）—— 写死一份会让「换了家就永远判不成功」：
 * 用户在登录页里明明登进去了，客户端却一直认为没登录，最后等到 5 分钟超时。
 *
 * 客户端先判一次只是为了「别在用户还没登录完时就交回空 Cookie」，真正的判定
 * 仍在内核（那边是唯一权威，见 `login::ensure_has_session` 与各家的
 * `complete_login`）。
 */
private fun hasSessionCookie(cookie: String, names: List<String>): Boolean {
    if (names.isEmpty()) return false
    return cookie.split(';').any { part ->
        val name = part.substringBefore('=').trim()
        val value = part.substringAfter('=', "").trim()
        name in names && value.isNotEmpty()
    }
}

/**
 * 按 [urls] 顺序读 Cookie，返回**第一个含登录态 Cookie 的域**的整串。
 *
 * 为什么不是「各域合并成一段」：同名 Cookie 在不同域上可能是两个值
 * （灵犀的 `wps_sid` 就可能一边是未登录的匿名会话、一边是登录态），
 * 拼在一起会让内核 `normalize_paste` 取到哪一个全看拼接顺序。
 * 这里改成「挑一个域，交它自己的整串」—— 顺序即优先级，登录页那一域排第一
 * （用户在那个域上刚刚完成登录，它的 `wps_sid` 一定是新的）。
 *
 * 读不到（或该域没有登录态）返回 null，调用方继续等下一拍。
 */
private fun firstCookieWithSession(urls: List<String>, names: List<String>): String? {
    if (names.isEmpty()) return null
    urls.forEach { url ->
        val raw = runCatching { CookieManager.getInstance().getCookie(url) }.getOrNull()
        if (raw != null && hasSessionCookie(raw, names)) return raw
    }
    return null
}

/** 登录会话参数：AddProviderSheet 点卡片 → start 成功后产生。 */
data class LoginSession(
    val providerId: String,
    val state: String,
    val authUrl: String,
)

/**
 * 受控 WebView 登录页（方案 v4.0 §3.6「进程内虚拟拦截」的 M3-3 首版）：
 *
 * - 加载 login/start 给的 authUrl，JS/DOM/Cookie 全开（OAuth 页与 HttpOnly Cookie 需要）；
 * - 回调分两路：
 *   · http(s) 回调打到本机 8787 端口的（loopback），**不拦**，WebView 直达内核
 *     （内核各家的 *-callback 路由自己会消化，wait 随之转 done）；
 *   · zcode:// 等自定义 scheme 拦下，POST /api/session/login/callback 转发内核；
 * - 完成检测统一靠 wait 轮询（1.8s 间隔，5 分钟超时），与服务端中介型（Qoder 设备
 *   授权 / ZCode CLI 轮询）天然兼容 —— 那两家的完成本就发生在服务端。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AuthWebViewSheet(
    session: LoginSession,
    onDismiss: () -> Unit,
    onResult: (success: Boolean, message: String) -> Unit,
) {
    val C = agateColors()
    val scope = rememberCoroutineScope()
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)

    // 登录页主机名：既让用户看得见「到底在打开谁」，也顺带成为**新老包的标记**
    // —— 老版本的这一行只会说「登录页已就绪」，从不带域名。
    val loginHost = remember(session.authUrl) {
        runCatching { java.net.URI(session.authUrl).host }.getOrNull()
            ?: session.authUrl
                .removePrefix("https://")
                .removePrefix("http://")
                .substringBefore('/')
    }
    var status by remember { mutableStateOf("正在打开 $loginHost…") }
    var finished by remember { mutableStateOf(false) }
    // 「重新加载」：AndroidView 的 factory 只跑一次，重载指令用计数器传给 WebViewBlock
    var reloadTick by remember { mutableIntStateOf(0) }

    val label = PresetRegistry.labelOf(session.providerId)
    // 这家是不是「靠读登录窗口的 Cookie 收尾」（见 ProviderPreset.webLoginCookies）
    val preset = PresetRegistry.byId(session.providerId)
    val cookieLogin = preset?.webLoginCookies == true
    // 该认哪些 Cookie 名算登录成功（各家不同，见 ProviderPreset.sessionCookieNames）
    val cookieNames = preset?.sessionCookieNames.orEmpty()
    // 读 Cookie 的域（登录页那一域优先，其余见 ProviderPreset.extraCookieHosts）
    val cookieUrls = remember(session.authUrl, preset) {
        buildList {
            add(session.authUrl)
            preset?.extraCookieHosts?.forEach { host ->
                if (host != session.authUrl) add(host)
            }
        }
    }
    // 交回去的端点：null = 通用回调；KukuAI 这类走专用收尾（{state, cookie}）
    val cookieCompletePath = preset?.cookieCompletePath
    // Cookie 只交回一次（登录成功后页面还会继续导航，每拍都交会重复 POST；
    // 内核那边虽然幂等，但没必要反复发）
    val cookieSubmitted = remember { mutableStateOf(false) }
    // 交回失败（读到的是匿名 Cookie）后的冷却时刻，避免每 1.8s 打一次上游探活
    var retryAfter by remember { mutableStateOf(0L) }
    // 上一次交回的 Cookie 串：**变了就立刻重试**（用户完成登录，值必然变），
    // 没变就按冷却走 —— 既不刷上游，也不会因为一次网络抖动永远卡住。
    var lastSubmitted by remember { mutableStateOf("") }

    // 完成检测：wait 轮询；结果只报一次（finished 防重复）。
    LaunchedEffect(session.state) {
        val deadline = System.currentTimeMillis() + 300_000L
        while (isActive && !finished && System.currentTimeMillis() < deadline) {
            delay(1_800L)
            // ── 壳侧读 Cookie（cookieLogin 的家）───────────────────────
            // 这类家没有授权码可拦：凭证就是登录后写进站点域的那套 Cookie。
            // CookieManager 读得到 HttpOnly 的会话 Cookie（`sessionid` 就是），
            // 因此这里读的是「完整 Cookie 串」，拼成 HTTP 头形态直接交回内核
            // （内核 `login::normalize_import` 认这个形态）。
            if (cookieLogin && !cookieSubmitted.value) {
                val cookie = firstCookieWithSession(cookieUrls, cookieNames)
                if (cookie != null) {
                    // Cookie 变了（用户刚登录完）立刻重试；没变则等冷却结束
                    val due = cookie != lastSubmitted || System.currentTimeMillis() >= retryAfter
                    if (due) {
                        lastSubmitted = cookie
                        cookieSubmitted.value = true
                        status = "已获取登录态，正在加入账号…"
                        // 两种收尾形态：通用回调（把 Cookie 当 callbackUrl 交回，内核各家
                        // 自己认）与专用端点（KukuAI：{state, cookie} → finish_kuku_login）
                        val accepted = if (cookieCompletePath != null) {
                            LoginClient.forwardCookie(session.state, cookie, cookieCompletePath)
                        } else {
                            LoginClient.forwardCallback(session.state, cookie)
                        }
                        if (!accepted) {
                            // ⚠️ 未登录时通行证也会下发一条**匿名** `wps_sid`，内核探活
                            // 回 401 是**正常中间态**（登录任务仍是 pending，见
                            // `api::session::login_callback` 的 Err 分支）。这里复位
                            // 标记继续等，别把「还没登录完」说成「登录失败」。
                            cookieSubmitted.value = false
                            retryAfter = System.currentTimeMillis() + 30_000L
                            status = "还没登录成功 —— 在登录页完成登录后会自动继续"
                        }
                    }
                }
            }
            when (val r = LoginClient.wait(session.state)) {
                LoginClient.WaitResult.Pending -> Unit
                is LoginClient.WaitResult.Done -> {
                    finished = true
                    onResult(true, "已添加 $label 账号")
                    return@LaunchedEffect
                }
                is LoginClient.WaitResult.Failed -> {
                    finished = true
                    onResult(false, "$label 登录未完成：${r.reason}")
                    return@LaunchedEffect
                }
            }
        }
        if (!finished) {
            finished = true
            onResult(false, "$label 登录超时（5 分钟）")
        }
    }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheetState,
        containerColor = C.cardBg,
    ) {
        Column(modifier = Modifier.fillMaxWidth()) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text(text = "登录 $label", color = C.textMain)
                    Spacer(Modifier.height(2.dp))
                    Text(text = status, color = C.textDim)
                }
                AgateBadge(text = "内嵌引擎", tone = BadgeTone.BLUE)
            }
            Spacer(Modifier.height(8.dp))
            WebViewBlock(
                authUrl = session.authUrl,
                host = loginHost,
                reloadTick = reloadTick,
                onPageHint = { status = it },
                onSchemeCallback = { url ->
                    // state 来自本次登录会话：内核靠它认任务（缺了必 400）
                    scope.launch { LoginClient.forwardCallback(session.state, url) }
                },
            )
            Spacer(Modifier.height(12.dp))
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp),
                horizontalArrangement = Arrangement.End,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                // 白屏/出错时不必重开弹窗（重开会浪费一次登录任务）
                TextButton(onClick = { reloadTick += 1 }) {
                    Text("重新加载", color = C.textDim)
                }
                Spacer(Modifier.width(8.dp))
                Button(onClick = {
                    scope.launch {
                        LoginClient.cancel(session.state)
                        onDismiss()
                    }
                }) { Text("取消登录") }
            }
            Spacer(Modifier.height(16.dp))
        }
    }
}

@SuppressLint("SetJavaScriptEnabled")
@Composable
private fun WebViewBlock(
    authUrl: String,
    host: String,
    reloadTick: Int,
    onPageHint: (String) -> Unit,
    onSchemeCallback: (String) -> Unit,
) {
    val C = agateColors()
    var web by remember { mutableStateOf<WebView?>(null) }
    // 「重新加载」：factory 只跑一次，重载指令靠计数器传进来
    LaunchedEffect(reloadTick) {
        if (reloadTick > 0) web?.reload()
    }
    // sheet 关闭即销毁 WebView，避免 native 资源滞留（登录频次低，泄漏也该关）。
    DisposableEffect(Unit) {
        onDispose {
            web?.stopLoading()
            web?.destroy()
            web = null
        }
    }
    AndroidView(
        modifier = Modifier
            .fillMaxWidth()
            .height(520.dp),
        factory = { context ->
            WebView(context).apply {
                web = this
                setBackgroundColor(C.cardBg.toArgb())
                settings.javaScriptEnabled = true
                settings.domStorageEnabled = true
                // 桌面布局的登录页按宽度缩放（移动布局本身有 viewport，不受影响）。
                // 少了这两行，桌面版登录页在手机上只能看到左上角一块。
                settings.useWideViewPort = true
                settings.loadWithOverviewMode = true
                settings.setSupportZoom(true)
                settings.builtInZoomControls = false
                settings.displayZoomControls = false
                // 去掉 WebView 指纹（UA 里的 `; wv)` 与 ` Version/4.0`）：不少站点见到
                // WebView 就改发「请用 App 打开」或客户端容器页 —— 灵犀自己就是这样
                // （`lingxi.kdocs.cn` 给 WPS App 的容器页，普通 WebView 里全白）。
                // **只删标记、不改引擎版本号**：版本号决定站点下发哪套语法，编造会坏页。
                settings.userAgentString?.let { ua ->
                    settings.userAgentString =
                        ua.replace("; wv)", ")").replace(" Version/4.0", "")
                }
                CookieManager.getInstance().setAcceptCookie(true)
                CookieManager.getInstance().setAcceptThirdPartyCookies(this, true)

                // ── 出错时如实说 ────────────────────────────────────
                // 这个弹窗过去只会说「已就绪」：页面加载完成但一片空白时，用户对着
                // 空白等满 5 分钟超时，日志里什么都没有。下面四处（主文档错误、HTTP
                // 错误、证书错误、加载后无可见文字）把它变成可诊断的状态。
                //
                // ⚠️ `mainFrameError` 是**粘性**的：主文档出错时 onReceivedError 先到、
                // onPageFinished 后到，后写的会把真正的错误覆盖成「已就绪」—— 这正是
                // 上一版把「打不开」说成「已就绪」的原因。
                var mainFrameError: String? = null
                var consoleError: String? = null
                var blankRetries = 0
                webChromeClient = object : WebChromeClient() {
                    override fun onConsoleMessage(msg: ConsoleMessage): Boolean {
                        if (msg.messageLevel() == ConsoleMessage.MessageLevel.ERROR) {
                            consoleError = msg.message()
                        }
                        return true
                    }
                }
                webViewClient = object : WebViewClient() {
                    override fun shouldOverrideUrlLoading(
                        view: WebView,
                        request: WebResourceRequest,
                    ): Boolean {
                        val scheme = request.url.scheme ?: return false
                        // 自定义 scheme（zcode:// 等）WebView 加载不了 → 转发内核。
                        // loopback 的 http(s) 回调不拦：WebView 直达内核 callback 路由。
                        if (scheme.equals("http", true) || scheme.equals("https", true)) {
                            return false
                        }
                        onSchemeCallback(request.url.toString())
                        return true
                    }

                    override fun onReceivedError(
                        view: WebView,
                        request: WebResourceRequest,
                        error: WebResourceError,
                    ) {
                        // 子资源（图片/字体）失败不打断登录，只有主文档才报
                        if (!request.isForMainFrame) return
                        val desc = error.description?.toString().orEmpty()
                        mainFrameError = "打不开 $host：$desc（错误码 ${error.errorCode}）"
                        onPageHint(mainFrameError.orEmpty())
                    }

                    override fun onReceivedHttpError(
                        view: WebView,
                        request: WebResourceRequest,
                        errorResponse: WebResourceResponse,
                    ) {
                        if (!request.isForMainFrame) return
                        val code = errorResponse.statusCode
                        // 3xx 由 WebView 自己跟，不算拒绝；只有 4xx/5xx 才是站点拒绝
                        if (code < 400) return
                        mainFrameError = "登录页被拒绝：HTTP $code（$host）"
                        onPageHint(mainFrameError.orEmpty())
                    }

                    override fun onReceivedSslError(
                        view: WebView,
                        handler: SslErrorHandler,
                        error: SslError,
                    ) {
                        // 证书错误就地说明并取消，**不 proceed**（放行等于自毁 TLS 校验）
                        mainFrameError = "证书校验失败：主错误码 ${error.primaryError}（$host）"
                        onPageHint(mainFrameError.orEmpty())
                        handler.cancel()
                    }

                    override fun onPageFinished(view: WebView, url: String) {
                        // 有错就不再改口（见上面 `mainFrameError` 的说明）
                        if (mainFrameError == null) {
                            onPageHint("$host 已打开，登录完成后自动关闭")
                        }
                        // 加载完成 ≠ 显示出来。6 秒后探一次：可见文字为 0 或文档高度
                        // 塌成 0（首次布局高度为 0、脚本抢跑都可能），先自动重载一次，
                        // 再不行就如实说明并指出还能走哪条路。
                        view.postDelayed({
                            view.evaluateJavascript(JS_PAGE_PROBE) { raw ->
                                val probe = parsePageProbe(raw) ?: return@evaluateJavascript
                                if (!probe.blank) return@evaluateJavascript
                                if (blankRetries < 1) {
                                    blankRetries += 1
                                    onPageHint("$host 打开了但没显示出内容，正在重试…")
                                    view.reload()
                                    return@evaluateJavascript
                                }
                                onPageHint(
                                    mainFrameError
                                        ?: "登录页没有显示出内容${
                                            consoleError?.take(80)?.let { "（$it）" }.orEmpty()
                                        } —— 可改用「粘贴」通道"
                                )
                            }
                        }, 6_000L)
                    }
                }

                // F4 会话隔离：每次登录用全新会话，清掉上一家/上一账号的
                // Cookie 与站点存储，防止串号。
                // ⚠️ removeAllCookies 是**异步**的，紧跟着 loadUrl 会让清理与页面
                // 第一个请求赛跑（登录页刚下发的 Cookie 可能被随后清掉，表现为
                // 「登录了但一直判不出登录态」）。改成清理完成后再加载。
                WebStorage.getInstance().deleteAllData()
                val started = AtomicBoolean(false)
                val loadOnce = {
                    if (started.compareAndSet(false, true)) loadUrl(authUrl)
                }
                CookieManager.getInstance().removeAllCookies {
                    CookieManager.getInstance().flush()
                    loadOnce()
                }
                // 兜底：个别 WebView 不回调（回调丢了就永远不加载）
                postDelayed({ loadOnce() }, 2_000L)
            }
        },
    )
}

/** 页面探针：`可见文字数|文档高度`；`-1|-1` = 读不到（不据此报警）。 */
private const val JS_PAGE_PROBE =
    "(function(){try{var b=document.body;if(!b)return '-1|-1';" +
        "var t=(b.innerText||'').replace(/\\s+/g,'');" +
        "return t.length+'|'+(b.scrollHeight||0);}catch(e){return '-1|-1'}})()"

/** 探针结果：`blank` = 一个可见字符都没有，或文档高度塌成 0（都等于白屏）。 */
private data class PageProbe(val chars: Int, val scrollHeight: Int) {
    val blank: Boolean get() = chars == 0 || scrollHeight < 24
}

/**
 * 解析 [JS_PAGE_PROBE] 的返回值。
 *
 * `evaluateJavascript` 回的是 **JSON 值**：字符串带引号、读不到时是字面量
 * `null`。任何一步对不上都回 `null`（= 不据此报警）—— 宁可不出提示，也不冤枉页面。
 */
private fun parsePageProbe(raw: String?): PageProbe? {
    val text = raw?.trim()?.trim('"') ?: return null
    if (text.isEmpty() || text == "null") return null
    val parts = text.split('|')
    if (parts.size < 2) return null
    val chars = parts[0].toIntOrNull() ?: return null
    val scroll = parts[1].toIntOrNull() ?: return null
    if (chars < 0 || scroll < 0) return null
    return PageProbe(chars, scroll)
}
