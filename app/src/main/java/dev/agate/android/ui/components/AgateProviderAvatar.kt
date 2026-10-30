package dev.agate.android.ui.components

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.ui.Alignment
import androidx.compose.ui.graphics.Color
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import dev.agate.android.R
import dev.agate.android.data.CustomProviderOps
import dev.agate.android.data.PresetApiRegistry

/**
 * 提供商官方 logo（取自上游 3.0.8 的 `assets/provider-icons`，16 家官方 Agent 全有）。
 * 自定义 / 中转上游没有官方图标 → 回落为两字母头像。
 */
object ProviderIcons {
    private val map: Map<String, Int> = mapOf(
        "workbuddy" to R.drawable.provider_workbuddy,
        "raccoon" to R.drawable.provider_raccoon,
        "catpaw" to R.drawable.provider_catpaw,
        "autoclaw" to R.drawable.provider_autoclaw,
        "qoder" to R.drawable.provider_qoder,
        "accio" to R.drawable.provider_accio,
        "zcode" to R.drawable.provider_zcode,
        "codearts" to R.drawable.provider_codearts,
        "trae" to R.drawable.provider_trae,
        "loomy" to R.drawable.provider_loomy,
        "kuku" to R.drawable.provider_kuku,
        "monkeycode" to R.drawable.provider_monkeycode,
        "commandcode" to R.drawable.provider_commandcode,
        "antigravity" to R.drawable.provider_antigravity,
        "officeace" to R.drawable.provider_officeace,
        "orcaterm" to R.drawable.provider_orcaterm,
        "cline" to R.drawable.provider_cline,
    )

    /**
     * 变体归一：同一产品线的国际版 / 国内版 / 双池共用同一图标
     * （`workbuddy-intl` → `workbuddy`、`accio-cn` → `accio`、`cline-free` → `cline`）。
     */
    private fun normalize(providerId: String): String = when {
        providerId.endsWith("-intl") -> providerId.removeSuffix("-intl")
        providerId.endsWith("-cn") -> providerId.removeSuffix("-cn")
        providerId.startsWith("cline") -> "cline"
        else -> providerId
    }

    fun resOf(providerId: String): Int? = map[normalize(providerId)] ?: map[providerId]

    /**
     * 自定义提供商的图标：按**名称**反查「预置 API」模板的图标。
     *
     * ── 为什么需要这一层 ─────────────────────────────────────────
     * 从「预置 API」卡片建出来的家，落库后身份是 `custom-xxxx`（内核
     * `custom_providers::ID_PREFIX`），上面那张 map 里当然没有它 —— 于是商汤
     * 日日新、OpenAI、DeepSeek 这些明明有官方 logo 的模板，建完就只剩一个
     * 两字母色块。模板的图标其实一直在手边（`PresetApiRegistry.ALL[].iconRes`），
     * 缺的只是「名称 → 图标」这一跳。
     *
     * ── 名称反查已降级为**回落**（2026-10-30）──────────────────
     * 这个函数曾是唯一反查路径，按「名称精确匹配」判身份 —— 理由是「改名
     * 即脱钩」的语义。但名称解析链有兜底层（账号备注名）且可能损坏，反查
     * 输入一旦不是模板名，图标就跟着掉（实际发生过两次）。现在身份优先走
     * 创建时固化的 `presetKey`（见 [resOfCustomIdentity]），名称匹配只服务
     * presetKey 之前的存量家。
     *
     * 匹配**忽略大小写**：用户手打「openai」也该认出来。
     */
    fun resOfCustomName(name: String): Int? {
        val key = name.trim().lowercase()
        if (key.isEmpty()) return null
        return presetIconsByName[key]
    }

    /**
     * 自定义提供商图标的**身份版**反查：优先用创建时固化的 `presetKey`
     * （slug，与改名无关），没有再回落名称精确匹配 —— 兼容 presetKey 之前
     * 建的存量家。
     *
     * ── 为什么不能再只靠名称（2026-10-30 修）──────────────────
     * 名称是可变的展示串，且它的解析链有兜底层（账号备注名）：名称链一旦
     * 出问题，反查输入就不再是模板名，图标跟着掉 —— 已被实际故障验证过
     * 两次。presetKey 随创建固化在提供商记录里（`custom_providers::create`
     * 的 `presetKey` 字段），与名称、账号、快照全链解耦。
     */
    fun resOfCustomIdentity(presetKey: String, name: String): Int? {
        if (presetKey.isNotBlank()) {
            PresetApiRegistry.findByKey(presetKey)?.let { return it.iconRes }
        }
        return resOfCustomName(name)
    }

    /**
     * 模板名 → 图标（归一化后的名字为键，见 [resOfCustomName]）。
     *
     * 预建一次而不是每次现扫 `PresetApiRegistry.ALL`：额度中心/提供商列表里
     * 每一行都会问一次，24 条的线性扫虽然不贵，但没必要。
     * 同名模板只取第一条（当前目录没有重名；真有重名时取先出现的那个，
     * 与「顺序即上游数组顺序」的约定一致）。
     */
    private val presetIconsByName: Map<String, Int> by lazy {
        val out = LinkedHashMap<String, Int>(PresetApiRegistry.ALL.size)
        PresetApiRegistry.ALL.forEach { preset ->
            out.putIfAbsent(preset.name.trim().lowercase(), preset.iconRes)
        }
        out
    }

    /**
     * 这张 logo 的**透明像素占比**（0~1）；未知返回 null（按满底处理）。
     *
     * 为什么要这张表：`drawable-nodpi/` 里的 PNG 是各家官方图，形态不统一 ——
     * 有的是满底色块（zcode / accio：透明仅 2~4%），有的是透明底 + 单色字形
     * （antigravity 80.7% 透明且近白像素 0% = 深色字形；trae 近白 63% = 白色字形）。
     * 后一类在某一模式的卡片底色上会**看不清**，需要衬一层中性底座（见
     * `AgateProviderAvatar` 里的用法）。
     *
     * 值是在构建时逐张量出来写死在这里的（`Image` 解码后取像素统计），不是拍的：
     * 运行时去解码 20 多张 256×256 只为算一个比例，不值得。新增图标时若不确
     * 定，先在本地量一次再决定是否加进来；漏了不会出错，只是少一层底座。
     */
    private val transparency: Map<String, Float> = mapOf(
        "antigravity" to 0.807f,
        "monkeycode" to 0.510f,
        "workbuddy" to 0.380f,
        "catpaw" to 0.366f,
        "autoclaw" to 0.291f,
        "trae" to 0.229f,
        "loomy" to 0.209f,
    )

    fun transparencyOf(providerId: String): Float? =
        transparency[normalize(providerId)] ?: transparency[providerId]
}

/**
 * 提供商头像：有官方 logo 画 logo，否则用两字母头像兜底。
 *
 * [customName] 只给**自定义提供商**（`custom-xxxx`）用：它们的官方图标不在
 * 内置 map 里，但「从预置 API 卡片建出来的」家名字仍然对得上模板，能按名字
 * 反查出同一张 logo（见 [ProviderIcons.resOfCustomName] 的说明）。
 * 传空串 = 不是自定义家，行为与之前逐字一致。
 */
@Composable
fun AgateProviderAvatar(
    providerId: String,
    label: String,
    size: Dp,
    corner: Dp = 13.dp,
    customName: String = "",
    /** 预置模板 slug（创建时固化）；空 = 无身份记录，按名反查 */
    customPresetKey: String = "",
) {
    // 顺序不能颠倒：内置 id 优先（`workbuddy` 这类永远走 map）；只有**自定义家**
    // （`custom-xxxx`）才拿名字去撞预置模板 —— 内置家没图标就是没图标，不该
    // 因为名字碰巧像某张模板而借用别人的 logo。
    val res = ProviderIcons.resOf(providerId)
        ?: if (CustomProviderOps.isCustomProviderId(providerId)) {
            ProviderIcons.resOfCustomIdentity(customPresetKey, customName.ifBlank { label })
        } else {
            null
        }
    if (res != null) {
        // ── 透底图标的柔光底座 ──────────────────────────────────
        // 有些官方 logo 是**透明底 + 单色字形**，字色与某一模式撞车：
        //   · antigravity：80.7% 透明、近白像素 0% → 深色字形，深色模式几乎看不见
        //   · trae：22.9% 透明、近白 63% → 白色字形，**浅色**模式下糊成一片
        // （比例是逐张量出来的，不是猜的；满底图标如 zcode/accio 只有 2~4% 透明，
        //  完全不受影响，所以只给「明显透底」的那些衬一层底座。）
        //
        // 底座用中性薄色而不是品牌色：品牌色会把白描边图标压成一块色斑，
        // 中性色对「深色字形」与「白色字形」两边都成立。深色模式下用略亮的
        // 白叠层，浅色模式下用略暗的黑叠层 —— 两种模式都给字形挣出对比度。
        val alpha = ProviderIcons.transparencyOf(providerId)
        if (alpha != null && alpha > 0.25f) {
            Box(
                modifier = Modifier
                    .size(size)
                    .background(plateColor(), RoundedCornerShape(corner))
                    .border(1.dp, plateBorderColor(), RoundedCornerShape(corner)),
                contentAlignment = Alignment.Center,
            ) {
                // 内缩 12%：底座要露出来才叫底座，图标顶到边就成了一张满底色块
                Image(
                    painter = painterResource(res),
                    contentDescription = label,
                    modifier = Modifier.size(size * 0.76f),
                )
            }
        } else {
            Image(
                painter = painterResource(res),
                contentDescription = label,
                modifier = Modifier.size(size).clip(RoundedCornerShape(corner)),
            )
        }
    } else {
        AgateAvatar(
            name = label,
            size = size,
            corner = corner,
            letters = initialsOfLabel(label),
        )
    }
}

/** 底座底色：浅色模式压暗、深色模式提亮，都是极薄的一层。 */
@Composable
private fun plateColor(): Color =
    if (isSystemInDarkTheme()) Color.White.copy(alpha = 0.10f) else Color.Black.copy(alpha = 0.05f)

/** 底座的 1dp 亮边：让底座在卡片底色上「立」起来，而不是像一块污渍。 */
@Composable
private fun plateBorderColor(): Color =
    if (isSystemInDarkTheme()) Color.White.copy(alpha = 0.14f) else Color.Black.copy(alpha = 0.06f)

/** 名称 → 两字母（供无 logo 的兜底使用）。 */
internal fun initialsOfLabel(label: String): String {
    val words = label.split(' ', '　', '-', '_').filter { it.isNotBlank() }
    return if (words.size >= 2) {
        words.take(2).map { it.first().uppercaseChar() }.joinToString("")
    } else {
        label.take(2).uppercase()
    }
}