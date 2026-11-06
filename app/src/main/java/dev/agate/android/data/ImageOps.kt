package dev.agate.android.data

import android.util.Base64
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONObject
import java.util.concurrent.TimeUnit

/**
 * 生图通道（`POST /v1/images/generations`）。
 *
 * ── 为什么走 127.0.0.1 端口，不走 [CoreTransport] ───────────────
 * 管理面通用的 JNI 直调是**全量缓冲**的（`jni_bridge::api_call` 用
 * `to_bytes`，上限 4 MiB、超时 120 秒）。生图的响应是三四十 MB 的
 * base64 / 一小时的图片清单，根本装不进去。所以这一层与 `AssistantOps`
 * 的对话链路同款：走真实端口 + OkHttp。
 *
 * 代价与助手页一样 —— Core 态（网关未运行）下端口不监听，调用方必须先判
 * `gatewayRunning`，并在没跑时给引导而不是让用户对着转圈猜。
 *
 * ── 结果为什么默认要 `url` 而不是 `b64_json` ───────────────────
 * 内核的 `normalize` 在 `response_format=url` 时把图片存进内存缓存
 * （`api::image_assets`，128 MiB / 1 小时 TTL），回一个
 * `/v1/images/files/<256 位令牌>` 链接。那个链接**免鉴权**（内核刻意挂 public
 * 组：客户端要把它塞进 `<img src>`，图片组件不带 Authorization 头）。
 * 于是：响应体从几十 MB 降到几百字节，客户端也不必自己解 base64。
 *
 * 超过 TTL 或进程重启后链接失效 → `AgateRemoteImage` 会显示加载失败，
 * 这时唯一的正确做法是让用户重新生成（不存在"链接还能救回来"的路径）。
 */
object ImageOps {

    private val JSON = "application/json; charset=utf-8".toMediaType()

    /** 单张图最大边（内核侧上限 8192，这里给客户端一个更保守的常用档位）。 */
    val SIZE_PRESETS: List<Pair<String, String>> = listOf(
        "auto" to "自动",
        "1024x1024" to "1:1 方图",
        "1792x1024" to "16:9 横图",
        "1024x1792" to "9:16 竖图",
        "1536x1024" to "3:2 横图",
        "1024x1536" to "2:3 竖图",
    )

    /** 一个可用的生图模型（只含 `kind=image` 且这家能力位为真的条目）。 */
    data class ImageModel(
        val id: String,
        val label: String,
        val provider: String,
        /** 这条模型能不能改图（决定编辑入口开不开）。 */
        val editing: Boolean,
    )

    data class GeneratedImage(val url: String, val revisedPrompt: String?)

    sealed class Result {
        data class Ok(val images: List<GeneratedImage>, val qualityHint: String?) : Result()

        /**
         * 业务失败。`code` 是内核给的可判别码 —— 目前会用到两个：
         * `image_generation_not_supported`（模型没通道）、
         * `content_policy_violation`（被策略拦了，换提示词才有用）。
         */
        data class Failed(val message: String, val code: String?, val status: Int) : Result()

        /** 端口没监听 / 连不上：与「上游拒绝」是两件事，文案也不同。 */
        data class Unreachable(val message: String) : Result()
    }

    /**
     * 可用的生图模型清单。
     *
     * 读 `/api/models/list-view` 的 `ids` 再回查 `/api/session`？不 —— 那条路
     * 只有名字、没有能力位。这里直接读 `/v1/models`（网关面，**免鉴权**探针）：
     * 它给全 `kind` / `output_modalities` / `supports_image_generation`，
     * 正是判「能不能生图」需要的三个键。
     */
    suspend fun availableModels(port: Int, apiKey: String): List<ImageModel> =
        withContext(Dispatchers.IO) {
            val request = Request.Builder()
                .url("http://127.0.0.1:$port/v1/models")
                .header("Accept", "application/json")
                .get()
                .build()
            val text = runCatching {
                client().newCall(request).execute().use { response ->
                    if (!response.isSuccessful) return@withContext emptyList<ImageModel>()
                    response.body?.string()
                }
            }.getOrNull() ?: return@withContext emptyList()

            val data = runCatching { JSONObject(text).optJSONArray("data") }.getOrNull()
                ?: return@withContext emptyList()
            val out = ArrayList<ImageModel>(data.length())
            for (index in 0 until data.length()) {
                val item = data.optJSONObject(index) ?: continue
                if (!isImage(item)) continue
                val id = item.optString("id").takeIf(String::isNotBlank) ?: continue
                val provider = item.optString("owned_by").ifBlank {
                    item.optString("provider")
                }
                out += ImageModel(
                    id = id,
                    label = item.optString("name").ifBlank { id },
                    provider = provider,
                    editing = item.optBoolean("supports_image_editing", false),
                )
            }
            out
        }

    /** `kind=image` 或 `output_modalities` 含 image（与内核同一判据）。 */
    private fun isImage(item: JSONObject): Boolean {
        val kind = item.optString("kind").ifBlank { item.optString("type") }
        if (kind.equals("image", ignoreCase = true)) return true
        val modalities = item.optJSONArray("output_modalities") ?: return false
        for (index in 0 until modalities.length()) {
            if (modalities.optString(index) == "image") return true
        }
        return false
    }

    /**
     * 生成图片。
     *
     * @param size `auto` 或 `WIDTHxHEIGHT`（见 [SIZE_PRESETS]）
     * @param apiKey 局域网模式下的网关 Key；本机模式可为空
     */
    suspend fun generate(
        port: Int,
        apiKey: String,
        model: String,
        prompt: String,
        size: String,
    ): Result = withContext(Dispatchers.IO) {
        val payload = JSONObject()
            .put("model", model)
            .put("prompt", prompt)
            .put("n", 1)
            // 显式要链接：见对象头「结果为什么默认要 url」。
            .put("response_format", "url")
        if (size.isNotBlank() && size != "auto") payload.put("size", size)

        val builder = Request.Builder()
            .url("http://127.0.0.1:$port/v1/images/generations")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .post(payload.toString().toRequestBody(JSON))
        if (apiKey.isNotBlank()) builder.header("Authorization", "Bearer $apiKey")

        val outcome = runCatching {
            client().newCall(builder.build()).execute().use { response ->
                response.code to (response.body?.string().orEmpty())
            }
        }.getOrElse { error ->
            // 连不上端口：网关没在跑，或刚停。与「上游拒绝」分开报（见 Result）。
            return@withContext Result.Unreachable(
                "连不上本机网关（${error.message ?: "连接失败"}）：请先在「网关控制」启动网关",
            )
        }
        parseResponse(outcome.first, outcome.second)
    }

    /**
     * 下载一张图并转成可直接写进相册的字节。
     *
     * `data:` 开头就地解 base64；`http(s)` 走网络。返回 `null` 表示这一步失败
     * （调用方给一句「保存失败」，而不是让整页崩）。
     */
    suspend fun downloadBytes(url: String): ByteArray? = withContext(Dispatchers.IO) {
        if (url.startsWith("data:", ignoreCase = true)) {
            val comma = url.indexOf(',')
            if (comma < 0) return@withContext null
            return@withContext runCatching {
                Base64.decode(url.substring(comma + 1), Base64.DEFAULT)
            }.getOrNull()
        }
        runCatching {
            client().newCall(Request.Builder().url(url).build()).execute().use { response ->
                if (!response.isSuccessful) return@withContext null
                response.body?.bytes()
            }
        }.getOrNull()
    }

    // ══════════════════════════════════════════════════════════
    //  参考图编辑（POST /v1/images/edits）
    // ══════════════════════════════════════════════════════════

    /**
     * 一张已准备好上传的参考图。
     *
     * 到这一步**一定**是 PNG / JPEG / WebP 的字节：内核的 `mime_of` 用
     * `image::guess_format` 判文件头，只认这三种，别的（相册里的 HEIC 最典型）
     * 会被 400 拒掉。所以转码发生在 [`prepareReference`] 里，不在这里。
     */
    class Reference(val bytes: ByteArray, val mime: String)

    /**
     * 参考图总大小上限（内核侧是 16 MiB / 单张 10 MiB）。
     *
     * 客户端留一点余量：表单还有 prompt / boundary 等开销，卡到 16 MiB 整
     * 会在边界上被内核 413。
     */
    private const val MAX_TOTAL_BYTES = 14L * 1024 * 1024
    private const val MAX_ONE_BYTES = 9L * 1024 * 1024
    private const val MAX_REFERENCES = 16

    /**
     * 把相册选来的图转成内核认的格式，并按需降采样。
     *
     * ── 为什么客户端要转码（而不是让内核去认 HEIC）───────────────
     * 内核只认 PNG / JPEG / WebP 是**刻意的**：参考图要被真解一次
     * （`inspect_bytes`，挡解压炸弹），而多支持一种格式就多一片解码器的
     * 攻击面。iOS 相机与部分安卓相机默认存 HEIC，用户从相册选的往往就是它
     * —— 所以在客户端转一次（`BitmapFactory` 在 API 28+ 能解 HEIC，28 以下
     * 解不了、会返回 null，那时的报错文案要说清「请换一张 JPG/PNG」）。
     *
     * ── 为什么要降采样 ────────────────────────────────────────
     * 一张 12 MP 的照片解出来是 48 MB（ARGB_8888），PNG 压完仍可能超 10 MiB
     * 的单张上限。这里按长边 2048 采样再编成 JPEG —— 对「改背景 / 换风格」
     * 这类编辑完全够用，而字节数掉一个量级。
     *
     * 失败返回 `null`（调用方给一句能照做的文案），不抛异常。
     */
    suspend fun prepareReference(open: (() -> ByteArray?)): Reference? =
        withContext(Dispatchers.IO) {
            val original = open() ?: return@withContext null
            // 已经是内核认的三种格式且不太大：原样发（不做无谓的重编码 ——
            // 重编码一次是有损的，用户上传 PNG 却拿到 JPEG 质量的图会很困惑）。
            if (original.size <= MAX_ONE_BYTES && sniff(original) != null) {
                return@withContext Reference(original, sniff(original)!!)
            }
            resampleToJpeg(original)
        }

    /**
     * 文件头判格式（与内核 `mime_of` 同一批：PNG / JPEG / WebP）。
     *
     * `internal` 而非 `private`：判错的方向有两个、代价都不小（见
     * `ImageReferenceSniffTest` 的模块头），所以要能被单测直接钉住 —— 让测试
     * 抄一份镜像逻辑等于什么都没测。
     */
    internal fun sniff(bytes: ByteArray): String? {
        if (bytes.size < 12) return null
        fun at(index: Int) = bytes[index].toInt() and 0xFF
        // PNG: 89 50 4E 47 0D 0A 1A 0A
        if (at(0) == 0x89 && at(1) == 0x50 && at(2) == 0x4E && at(3) == 0x47) return "image/png"
        // JPEG: FF D8 FF
        if (at(0) == 0xFF && at(1) == 0xD8 && at(2) == 0xFF) return "image/jpeg"
        // WebP: "RIFF"…"WEBP"
        if (at(0) == 0x52 && at(1) == 0x49 && at(2) == 0x46 && at(3) == 0x46 &&
            at(8) == 0x57 && at(9) == 0x45 && at(10) == 0x42 && at(11) == 0x50
        ) {
            return "image/webp"
        }
        return null
    }

    /** 解码 → 按长边 2048 采样 → 编成 JPEG（质量 88）。 */
    private fun resampleToJpeg(bytes: ByteArray): Reference? {
        val bounds = android.graphics.BitmapFactory.Options().apply { inJustDecodeBounds = true }
        android.graphics.BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
        if (bounds.outWidth <= 0 || bounds.outHeight <= 0) {
            // 解不出来：多半是 HEIC 且系统 API 太低。返回 null 让调用方说清楚。
            return null
        }
        var sample = 1
        while (bounds.outWidth / (sample * 2) >= 2048 || bounds.outHeight / (sample * 2) >= 2048) {
            sample *= 2
        }
        val options = android.graphics.BitmapFactory.Options().apply { inSampleSize = sample }
        val bitmap = try {
            android.graphics.BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options)
        } catch (_: OutOfMemoryError) {
            null
        } ?: return null
        val out = java.io.ByteArrayOutputStream()
        val ok = runCatching {
            bitmap.compress(android.graphics.Bitmap.CompressFormat.JPEG, 88, out)
        }.getOrDefault(false)
        bitmap.recycle()
        if (!ok) return null
        return Reference(out.toByteArray(), "image/jpeg")
    }

    /**
     * 参考图编辑（`POST /v1/images/edits`，`multipart/form-data`）。
     *
     * ── 为什么是 multipart 而不是 JSON ──────────────────────────
     * 内核两条路都收（JSON 的 `images[].image_url` 塞 data URL 也行），但
     * multipart 少一次 base64 膨胀（33%），而参考图是**二进制**、本来就大。
     * 字段名用 OpenAI SDK 的写法（参考图一律叫 `image`，可重复）。
     *
     * ── 能力位在这之前就要判 ──────────────────────────────────
     * 六家里只有五家支持编辑（豆包工作明确 false），而且 ModelScope 的
     * 「生成型号」与「编辑型号」是**两组不同 id**。调用方必须先用
     * [ImageModel.editing] 过滤，别让用户点了才发现「这个模型没有编辑通道」。
     */
    suspend fun edit(
        port: Int,
        apiKey: String,
        model: String,
        prompt: String,
        size: String,
        references: List<Reference>,
    ): Result = withContext(Dispatchers.IO) {
        if (references.isEmpty()) {
            return@withContext Result.Failed("参考图编辑需要至少一张图片", null, 400)
        }
        if (references.size > MAX_REFERENCES) {
            return@withContext Result.Failed(
                "参考图最多 $MAX_REFERENCES 张",
                null,
                400,
            )
        }
        val total = references.sumOf { it.bytes.size.toLong() }
        if (total > MAX_TOTAL_BYTES) {
            return@withContext Result.Failed(
                "参考图合计超过 14 MiB，请少选几张或换小一点的图",
                null,
                413,
            )
        }
        if (references.any { it.bytes.size > MAX_ONE_BYTES }) {
            return@withContext Result.Failed("单张参考图不得超过 9 MiB", null, 413)
        }

        // 手拼 multipart：仓库里没有能拼表单的客户端库，而字段只有三类
        // （文本 / 文件），拼错的风险远小于为一个生图页引一棵依赖树。
        // boundary 用时间戳 + 随机数，撞进图片字节的概率可以忽略；真撞上
        // 的后果是上游 200 但图是花的 —— 所以内核那边（出站）才要做逐段检查，
        // 客户端这边**只要保证 boundary 足够随机**即可。
        val boundary = "----agate${System.currentTimeMillis()}${kotlin.random.Random.nextInt(1_000_000)}"
        val body = okhttp3.MultipartBody.Builder().apply {
            // `setType` 必调：不设的话 reqwest/服务端拿不到 boundary
            setType("multipart/form-data".toMediaType())
            addFormDataPart("model", model)
            addFormDataPart("prompt", prompt)
            addFormDataPart("n", "1")
            addFormDataPart("response_format", "url")
            if (size.isNotBlank() && size != "auto") addFormDataPart("size", size)
            references.forEachIndexed { index, reference ->
                val extension = when (reference.mime) {
                    "image/png" -> "png"
                    "image/webp" -> "webp"
                    else -> "jpg"
                }
                addFormDataPart(
                    "image",
                    // 文件名由我们生成：客户端给的名字不可信，非 ASCII 还要另做
                    // RFC 7578 编码 —— 与内核出站 multipart 同一处置。
                    "reference-${index + 1}.$extension",
                    reference.bytes.toRequestBody(reference.mime.toMediaType()),
                )
            }
        }.build()

        val request = Request.Builder()
            .url("http://127.0.0.1:$port/v1/images/edits")
            .header("Accept", "application/json")
            .post(body)
            .apply { if (apiKey.isNotBlank()) header("Authorization", "Bearer $apiKey") }
            .build()

        val outcome = runCatching {
            client().newCall(request).execute().use { response ->
                response.code to (response.body?.string().orEmpty())
            }
        }.getOrElse { error ->
            return@withContext Result.Unreachable(
                "连不上本机网关（${error.message ?: "连接失败"}）：请先在「网关控制」启动网关",
            )
        }
        parseResponse(outcome.first, outcome.second)
    }

    /** 生成与编辑共用同一份响应解析（两个入口的响应形态完全一致）。 */
    private fun parseResponse(status: Int, text: String): Result {
        val body = runCatching { JSONObject(text) }.getOrNull()
        if (status !in 200..299) {
            val error = body?.optJSONObject("error")
            return Result.Failed(
                message = error?.optString("message").orEmpty().ifBlank {
                    "图片编辑失败（HTTP $status）"
                },
                code = error?.optString("code")?.ifBlank { null },
                status = status,
            )
        }
        val data = body?.optJSONArray("data") ?: return Result.Failed("网关没有返回图片数据", null, status)
        val images = ArrayList<GeneratedImage>(data.length())
        for (index in 0 until data.length()) {
            val item = data.optJSONObject(index) ?: continue
            val url = item.optString("url").ifBlank {
                item.optString("b64_json")
                    .takeIf(String::isNotBlank)
                    ?.let { "data:image/png;base64,$it" }
                    .orEmpty()
            }
            if (url.isBlank()) continue
            images += GeneratedImage(
                url = url,
                revisedPrompt = item.optString("revised_prompt").ifBlank { null },
            )
        }
        if (images.isEmpty()) return Result.Failed("网关返回了空图片列表", null, status)
        return Result.Ok(images, body.optString("size").ifBlank { null })
    }

    /**
     * 生图链路的 Client。
     *
     * 读超时要给够：**同步**生图（loomy / raccoon / workbuddy / antigravity）
     * 一次往返就可能几十秒，异步的（modelscope）还要轮询任务。内核侧那边
     * 自己的 body 预算是 300 秒，客户端这边比它略长，让内核先把错误说出来
     * —— 客户端先超时的话，用户看到的是「网络超时」，而真实原因是上游那边
     * 的内容策略拦截。
     */
    private fun client(): OkHttpClient = OkHttpClient.Builder()
        .connectTimeout(8, TimeUnit.SECONDS)
        .readTimeout(330, TimeUnit.SECONDS)
        .writeTimeout(30, TimeUnit.SECONDS)
        .retryOnConnectionFailure(false)
        .build()
}