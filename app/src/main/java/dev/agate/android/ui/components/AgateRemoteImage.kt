package dev.agate.android.ui.components

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.LruCache
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.OkHttpClient
import okhttp3.Request
import java.io.IOException
import java.util.concurrent.TimeUnit

/**
 * 远程图片组件：**手写**的下载 + 解码 + 内存缓存，刻意不引 Coil。
 *
 * ── 为什么不引图片库（这个决定要能站得住）──────────────
 * 全仓只有这一处要显示网络图片（生图结果）。为此引一个图片库（Coil 约 200 KB
 * + 一棵依赖树）换来的能力是「磁盘缓存 / 格式探测 / 变换」，我们一样都用不上：
 *   - **磁盘缓存**不需要：链接是内核自己发的 `/v1/images/files/<令牌>`，
 *     服务端已经带 `Cache-Control: private, no-store`，TTL 一小时；
 *   - **格式探测**不需要：内核在归一化时已经统一转码（PNG/JPEG/WebP）；
 *   - **变换**不需要：结果区按短边缩放显示。
 * 所以只需要「下载 → 采样解码 → 留在内存里」三件事，见下。
 *
 * ── 缓存为什么按**字节数**算而不是按张数 ─────────────────
 * 一张 4K PNG 解出来是 33 MB（4096×4096×2 字节按 ARGB_8888 算还要更多）。
 * 按「留 12 张」算，最坏情况是 400 MB —— 直接 OOM。所以按解码后字节计价，
 * 上限取运行时堆的 1/8（Android 官方建议的图片缓存量级）。
 *
 * ── 采样：解码前先读尺寸 ────────────────────────────────
 * 直接 `BitmapFactory.decodeStream` 一个 4K 图会先分配满尺寸的位图再交给
 * Compose 去缩 —— 两次内存峰值。这里用 `inJustDecodeBounds` 先量尺寸，
 * 按 2 的幂降采样到「不超过显示宽度的 2 倍」，只分配一次。
 */
object AgateImageLoader {

    /** 下载用：短超时、不跟随跨域跳转（链接由内核自己发，不需要跟）。 */
    private val client: OkHttpClient = OkHttpClient.Builder()
        .connectTimeout(8, TimeUnit.SECONDS)
        .readTimeout(20, TimeUnit.SECONDS)
        .build()

    /**
     * 解码后字节计的 LRU。上限取运行时堆的 1/8，并夹在 8~48 MB ——
     * 小堆设备（heap 64 MB）给 8 MB，大堆设备最多 48 MB（再多也用不上：
     * 结果区一屏最多三四张缩略图）。
     */
    private val cache: LruCache<String, Bitmap> = run {
        val maxHeapKb = Runtime.getRuntime().maxMemory() / 1024
        val targetKb = (maxHeapKb / 8).coerceIn(8L * 1024, 48L * 1024)
        object : LruCache<String, Bitmap>(targetKb.toInt()) {
            override fun sizeOf(key: String, value: Bitmap): Int = value.byteCount / 1024
        }
    }

    /** 命中的内存缓存（同步，供 `remember` 首帧直接用，避免闪一下占位）。 */
    fun cached(url: String): Bitmap? = cache.get(url)

    /**
     * 下载并解码；`widthPx` 是目标显示宽度（按它降采样）。
     *
     * 失败一律返回 `null`（调用方画失败态），**不抛异常** —— 一张图下载失败
     * 不该让整页崩掉，尤其是九宫格里还有八张是好的。
     */
    suspend fun load(url: String, widthPx: Int): Bitmap? = withContext(Dispatchers.IO) {
        cache.get(url)?.let { return@withContext it }
        val bytes = try {
            client.newCall(Request.Builder().url(url).build()).execute().use { response ->
                if (!response.isSuccessful) return@withContext null
                response.body?.bytes()
            }
        } catch (_: IOException) {
            null
        } catch (_: IllegalArgumentException) {
            // URL 不合法的形态（内核不会发，但客户端不该因此崩）
            null
        } ?: return@withContext null

        val bitmap = decodeSampled(bytes, widthPx) ?: return@withContext null
        cache.put(url, bitmap)
        bitmap
    }

    /**
     * 按目标宽度降采样解码。
     *
     * 采样率取 2 的幂（BitmapFactory 只认 2 的幂），目标定在「显示宽度的 2 倍」
     * —— 再大对肉眼没有区别，只是白占内存。
     */
    private fun decodeSampled(bytes: ByteArray, widthPx: Int): Bitmap? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
        if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null

        var sample = 1
        val target = widthPx.coerceAtLeast(1) * 2
        while (bounds.outWidth / (sample * 2) >= target && sample < 8) {
            sample *= 2
        }
        val options = BitmapFactory.Options().apply { inSampleSize = sample }
        return try {
            BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options)
        } catch (_: OutOfMemoryError) {
            // 极端情况下宁可显示失败态，也不要让进程被 OOM 杀掉
            null
        }
    }
}

/**
 * 显示一张远程图片：加载中转圈、失败给一行说明。
 *
 * `key` 决定「换一张图时要不要重新加载」—— 传 url 即可；同一 url 命中缓存时
 * 首帧就有画面（`cached()` 同步取），不会先闪一个占位再跳成图。
 */
@Composable
fun AgateRemoteImage(
    url: String,
    modifier: Modifier = Modifier,
    contentDescription: String? = null,
    cornerRadius: Int = 10,
) {
    val C = agateColors()
    var bitmap by remember(url) { mutableStateOf(AgateImageLoader.cached(url)) }
    var failed by remember(url) { mutableStateOf(false) }
    var loading by remember(url) { mutableStateOf(bitmap == null) }
    var cancelled by remember(url) { mutableStateOf(false) }

    LaunchedEffect(url) {
        // 缓存命中就不用发请求（`cached()` 已在 remember 里取过，这里兜「同 url
        // 换了组件实例」的情形）。
        if (bitmap != null) {
            loading = false
            return@LaunchedEffect
        }
        loading = true
        failed = false
        val loaded = AgateImageLoader.load(url, widthPx = 960)
        if (!cancelled) {
            bitmap = loaded
            failed = loaded == null
            loading = false
        }
    }

    DisposableEffect(url) {
        onDispose {
            // 组件销毁时把「结果别写回来了」标上：一次下载几十 KB 到几 MB，
            // 让它跑完比中途取消更简单，但结果不能写回已销毁的组件。
            cancelled = true
        }
    }

    Box(
        modifier = modifier
            .clip(RoundedCornerShape(cornerRadius.dp))
            .background(C.insetBg),
        contentAlignment = Alignment.Center,
    ) {
        val current = bitmap
        when {
            current != null -> Image(
                bitmap = current.asImageBitmap(),
                contentDescription = contentDescription,
                contentScale = ContentScale.Crop,
                modifier = Modifier.fillMaxSize(),
            )

            loading -> CircularProgressIndicator(
                modifier = Modifier.size(22.dp),
                strokeWidth = 2.dp,
                color = C.amber,
            )

            failed -> Text(
                "加载失败",
                fontSize = 10.sp,
                color = C.textDim,
            )
        }
    }
}
