package dev.agate.android.ui.images

import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.ImageOps
import dev.agate.android.data.KernelOps
import dev.agate.android.ui.components.AgateChip
import dev.agate.android.ui.components.AgateFormInput
import dev.agate.android.ui.components.AgatePageHeader
import dev.agate.android.ui.components.AgateRemoteImage
import dev.agate.android.ui.components.agateCard
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import dev.agate.core.GatewayPrefs
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * 图像创作页：提示词 → 生图 → 结果网格 → 保存。
 *
 * ── 为什么这一页要求「网关运行中」（与 AI 运维助手同一条理由）────
 * 生图响应是几十 MB 的图片清单，而管理面通用的 JNI 直调是**全量缓冲**的
 * （4 MiB 上限）。所以这一页走真实端口 + OkHttp，Core 态下端口不监听 ——
 * 处理方式是**明说并引导**，而不是让用户对着一个永远转圈的错误猜。
 *
 * ── 结果按「历史」累积而不是覆盖 ────────────────────────────
 * 生图是**消耗性**的：改一版提示词再生成一次是常态，用户会想把两版放在一起
 * 比。所以每次生成把新图**追加**到列表前面（最新的在最上面），而不是替换。
 * 页头显示累计张数，超出一屏由用户自己下滑。
 *
 * ── 为什么不给「重新生成同一批」按钮 ─────────────────────────
 * 一次生图 = 一次真实计费调用。把「再来一次」做成按钮很容易被误触，
 * 而用户真要重来，改一个字再点生成的成本一样低。
 */
@Composable
fun ImagesPage(
    onOpenDrawer: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    var gatewayRunning by remember { mutableStateOf(false) }
    var prompt by remember { mutableStateOf("") }
    var size by remember { mutableStateOf("auto") }
    var models by remember { mutableStateOf<List<ImageOps.ImageModel>>(emptyList()) }
    var modelId by remember { mutableStateOf<String?>(null) }
    var loadingModels by remember { mutableStateOf(true) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var notice by remember { mutableStateOf<String?>(null) }

    /** 已生成的图（最新的在最前）。 */
    val results = remember { mutableStateListOf<ImageOps.GeneratedImage>() }

    val port = remember { GatewayPrefs.port(ctx) }
    val apiKey = remember { GatewayPrefs.apiKey(ctx) }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    // 网关是否在跑：这一页的必要条件（见模块头）。与助手页同一判据。
    LaunchedEffect(Unit) {
        gatewayRunning = withContext(Dispatchers.IO) {
            runCatching { KernelOps.isUp(ctx) }.getOrDefault(false)
        }
    }

    // 模型清单：网关在跑才拉（免鉴权探针，但仍然要端口活着）
    LaunchedEffect(gatewayRunning) {
        if (!gatewayRunning) {
            loadingModels = false
            return@LaunchedEffect
        }
        loadingModels = true
        models = ImageOps.availableModels(port, apiKey)
        modelId = modelId ?: models.firstOrNull()?.id
        loadingModels = false
    }

    fun generate() {
        val chosen = modelId
        if (chosen.isNullOrBlank() || prompt.isBlank() || busy) return
        busy = true
        error = null
        notice = null
        scope.launch {
            when (val result = ImageOps.generate(port, apiKey, chosen, prompt.trim(), size)) {
                is ImageOps.Result.Ok -> {
                    // 新的放最前：用户刚生成的那批应当立刻可见
                    results.addAll(0, result.images)
                    notice = "已生成 ${result.images.size} 张"
                }

                is ImageOps.Result.Failed -> error = result.message
                is ImageOps.Result.Unreachable -> {
                    error = result.message
                    // 连不上 = 网关可能刚停：把状态刷成未运行，让引导卡片出来
                    gatewayRunning = false
                }
            }
            busy = false
        }
    }

    Column(modifier = modifier.fillMaxSize().background(C.bg).imePadding()) {
        AgatePageHeader(title = "图像创作", onOpenDrawer = onOpenDrawer) {
            if (results.isNotEmpty()) {
                Text(
                    "${results.size} 张",
                    color = C.textSub,
                    fontSize = 12.sp,
                    style = AgateType.Num,
                    modifier = Modifier.padding(horizontal = 8.dp, vertical = 5.dp),
                )
            }
        }

        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            Spacer(Modifier.height(10.dp))

            if (!gatewayRunning) {
                NoticeCard(
                    title = "网关未运行",
                    body = "生图走网关的图片接口，需要网关处于运行状态。请先到「网关控制」启动网关，再回到这一页。",
                )
                Spacer(Modifier.height(10.dp))
            } else if (!loadingModels && models.isEmpty()) {
                NoticeCard(
                    title = "没有可用的生图模型",
                    body = "当前没有已启用的账号声明了生图能力。请到「提供商」添加 loomy / ModelScope / " +
                        "WorkBuddy / 小浣熊 / Antigravity / 豆包工作 之一的账号，并在「模型」页刷新目录。",
                )
                Spacer(Modifier.height(10.dp))
            }

            error?.let {
                NoticeCard(title = "生成失败", body = it, danger = true)
                Spacer(Modifier.height(10.dp))
            }
            notice?.let {
                Text(it, fontSize = 11.sp, color = C.textDim)
                Spacer(Modifier.height(8.dp))
            }

            // ── 表单 ──
            Column(
                Modifier
                    .fillMaxWidth()
                    .agateCard()
                    .background(C.cardBg, RoundedCornerShape(20.dp))
                    .border(1.dp, C.border, RoundedCornerShape(20.dp))
                    .padding(14.dp),
            ) {
                SectionLabel("模型")
                Spacer(Modifier.height(6.dp))
                if (models.isEmpty()) {
                    Text(
                        if (loadingModels) "正在读取模型清单…" else "—",
                        fontSize = 12.sp,
                        color = C.textDim,
                    )
                } else {
                    // 模型可能有好几条（六家各一条合成条目），横向排开、
                    // 换行由 Row 自己处理不了，这里用纵向列表 —— 手机上最多六条。
                    models.forEach { model ->
                        ModelRow(
                            model = model,
                            selected = model.id == modelId,
                            onSelect = { modelId = model.id },
                        )
                        Spacer(Modifier.height(4.dp))
                    }
                }

                Spacer(Modifier.height(12.dp))
                SectionLabel("画面尺寸")
                Spacer(Modifier.height(6.dp))
                // 尺寸按 chip 换行排：六个档位在手机上两行放得下
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    ImageOps.SIZE_PRESETS.take(3).forEach { (value, label) ->
                        AgateChip(
                            text = label,
                            selected = size == value,
                            onClick = { size = value },
                            modifier = Modifier.weight(1f),
                        )
                    }
                }
                Spacer(Modifier.height(6.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    ImageOps.SIZE_PRESETS.drop(3).forEach { (value, label) ->
                        AgateChip(
                            text = label,
                            selected = size == value,
                            onClick = { size = value },
                            modifier = Modifier.weight(1f),
                        )
                    }
                }

                Spacer(Modifier.height(12.dp))
                SectionLabel("提示词")
                Spacer(Modifier.height(6.dp))
                AgateFormInput(
                    value = prompt,
                    onValueChange = { prompt = it },
                    hint = if (busy) "正在生成…" else "描述你想要的画面",
                    singleLine = false,
                    minLines = 3,
                )

                Spacer(Modifier.height(12.dp))
                val canSend = gatewayRunning && !busy && prompt.isNotBlank() && !modelId.isNullOrBlank()
                Text(
                    if (busy) "生成中…" else "开始生成",
                    fontSize = 13.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = if (canSend) C.accent else C.textDim,
                    modifier = Modifier
                        .fillMaxWidth()
                        .border(
                            1.dp,
                            if (canSend) C.accent.copy(alpha = 0.4f) else C.border,
                            RoundedCornerShape(9.dp),
                        )
                        .clickable(enabled = canSend) { generate() }
                        .padding(horizontal = 14.dp, vertical = 10.dp),
                )
                if (modelId != null && models.firstOrNull { it.id == modelId }?.editing == true) {
                    Spacer(Modifier.height(6.dp))
                    // 编辑入口是下一批的事：这里**明说**而不是留一个点了没反应的按钮。
                    Text(
                        "参考图编辑请用 POST /v1/images/edits（客户端入口在下一版）",
                        fontSize = 10.sp,
                        color = C.textDim,
                    )
                }
            }

            // ── 结果 ──
            if (results.isNotEmpty()) {
                Spacer(Modifier.height(14.dp))
                SectionLabel("生成结果")
                Spacer(Modifier.height(8.dp))
                // 两列网格：手写而不是引 LazyVerticalGrid —— 外层已经是
                // verticalScroll，嵌套懒加载容器在 Compose 里是明确的错用。
                results.chunked(2).forEach { row ->
                    Row(
                        Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        row.forEach { image ->
                            ResultCard(
                                image = image,
                                modifier = Modifier.weight(1f),
                                onDownload = { ImageOps.downloadBytes(image.url) },

                            )
                        }
                        // 奇数张时补一个等宽空位，避免最后一张被拉满
                        if (row.size == 1) Spacer(Modifier.weight(1f))
                    }
                    Spacer(Modifier.height(8.dp))
                }
            }

            Spacer(Modifier.height(16.dp))
        }
    }
}

@Composable
private fun ResultCard(
    image: ImageOps.GeneratedImage,
    modifier: Modifier = Modifier,
    onDownload: suspend () -> ByteArray?,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    /** 已下载待落盘的字节（点「保存」后先取字节，再起文件选择器）。 */
    var pending by remember { mutableStateOf<ByteArray?>(null) }

    // SAF 的 `createDocument` 走系统文件选择器：**不申请任何权限**、全 API
    // 级别可用（minSdk 26 上没有 `WRITE_EXTERNAL_STORAGE` 就只能走这条路），
    // 与仓库里「导出配置」的做法同款。
    val launcher = rememberLauncherForActivityResult(
        ActivityResultContracts.CreateDocument("image/png"),
    ) { uri ->
        val bytes = pending
        pending = null
        if (uri == null || bytes == null) return@rememberLauncherForActivityResult
        val ok = runCatching {
            ctx.contentResolver.openOutputStream(uri)?.use { it.write(bytes) } != null
        }.getOrDefault(false)
        Toast.makeText(ctx, if (ok) "已保存" else "保存失败", Toast.LENGTH_SHORT).show()
    }

    Column(modifier) {
        AgateRemoteImage(
            url = image.url,
            modifier = Modifier.fillMaxWidth().aspectRatio(1f),
        )
        Spacer(Modifier.height(5.dp))
        Text(
            "保存",
            fontSize = 10.sp,
            color = C.accent,
            modifier = Modifier
                .clickable {
                    scope.launch {
                        // 先取字节再起选择器：反过来的话用户在选文件期间图片可能
                        // 已经过期（内核的链接是 1 小时 TTL），落盘的是空文件。
                        val bytes = onDownload()
                        if (bytes == null) {
                            Toast.makeText(ctx, "保存失败：图片已过期或无法下载", Toast.LENGTH_SHORT).show()
                            return@launch
                        }
                        pending = bytes
                        launcher.launch("agate-${System.currentTimeMillis()}.png")
                    }
                }
                .padding(vertical = 3.dp),
        )
    }
}

@Composable
private fun SectionLabel(text: String) {
    val C = agateColors()
    Text(
        text,
        fontSize = 10.sp,
        fontWeight = FontWeight.SemiBold,
        color = C.textDim,
        letterSpacing = 0.6.sp,
    )
}

@Composable
private fun ModelRow(
    model: ImageOps.ImageModel,
    selected: Boolean,
    onSelect: () -> Unit,
) {
    val C = agateColors()
    Row(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(10.dp))
            .background(if (selected) C.accent.copy(alpha = 0.12f) else C.insetBg)
            .border(
                1.dp,
                if (selected) C.accent.copy(alpha = 0.4f) else C.border,
                RoundedCornerShape(10.dp),
            )
            .clickable(onClick = onSelect)
            .padding(horizontal = 10.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(
                model.label,
                fontSize = 12.sp,
                fontWeight = FontWeight.Medium,
                color = if (selected) C.accent else C.textMain,
            )
            if (model.provider.isNotBlank()) {
                Text(model.provider, fontSize = 9.5.sp, color = C.textDim)
            }
        }
        if (selected) {
            Text("✓", fontSize = 12.sp, color = C.accent)
        }
    }
}

@Composable
private fun NoticeCard(title: String, body: String, danger: Boolean = false) {
    val C = agateColors()
    Column(
        Modifier
            .fillMaxWidth()
            .background(
                if (danger) C.redBg else C.amberBg,
                RoundedCornerShape(12.dp),
            )
            .border(
                1.dp,
                if (danger) C.red.copy(alpha = 0.35f) else C.amber.copy(alpha = 0.35f),
                RoundedCornerShape(12.dp),
            )
            .padding(12.dp),
    ) {
        Text(
            title,
            fontSize = 12.sp,
            fontWeight = FontWeight.SemiBold,
            color = if (danger) C.red else C.amber,
        )
        Spacer(Modifier.height(2.dp))
        Text(body, fontSize = 11.sp, color = C.textSub, lineHeight = 16.sp)
    }
}