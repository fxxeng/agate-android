package dev.agate.android.data

import dev.agate.android.R

/**
 * 「预置 API」目录 —— 上游 3.0.8 的 `ui/preset-providers.js` 逐条移植。
 *
 * ── 这一层是什么（先纠正一个命名）────────────────────────────
 * 上游把「添加」的第一步分成四类：**Agent（内置家）/ 预置 API / 自定义 / 导入**
 * （`add-provider-pick.tsx` 的 `AccountType`；导入段 `IMPORT_SEGMENT_ENABLED = false`
 * 当前收起）。所谓「预置 API」不是「中转」的别名，而是**一张模板卡片清单**：
 * 点某张卡 = 把这一家的名称 / 协议 / Base URL 预填进「新建自定义提供商」表单，
 * 建出来的仍是一条普通的 `custom-` 记录，与手动新建的没有任何区别。
 *
 * 因此本目录**只提供预填数据**，不参与路由、不产生 provider id：
 *   · 落库后的身份是 `custom-xxxx`（内核 `custom_providers::ID_PREFIX`）；
 *   · 同一张模板卡可以反复点 —— 没建过 = 新建，建过（同名家已存在）= 接着加账号。
 *     「已建过」的判据是**名称相等**（上游 `matchesPresetName` 就是这么判的），
 *     所以用户改了名字之后那家就与模板脱钩，回到「自定义」段里。
 *
 * ── quirks / balance / account 三个派生事实 ──────────────────
 * 这些字段表达的是「这一家是谁」，不是用户的选择，所以写在目录里：
 *   · [urlSuffix] 追加到出站 URL 末尾（GLM / MiniMax 的 Claude 兼容端点要 `?beta=true`）
 *   · [headers] 合并到默认头（Anthropic-Beta / OpenRouter 标识头 / Cline 客户端伪装）
 *   · [anthropicToolType] 走 anthropic 时给工具补 `type:"custom"`（MiniMax 两家）
 *   · [clientEmulation] 客户端形态伪装（只有 OpenCode Zen 免费档要）
 *   · [balanceScheme] 余额查询链路（取值逐字对齐内核 `BALANCE_SCHEMES`）
 *   · [noAuth] 「该上游无需鉴权」预勾选（本地 Ollama / OpenCode Zen 免费档）
 *
 * 建库时这些随 `POST /api/custom-providers` 一起提交、落进提供商记录，转发层
 * 读的是记录而不是本目录 —— 目录改了不会影响已建的记录（这是刻意的：用户的
 * 存量配置不该被一次改动悄悄改写）。
 */
object PresetApiRegistry {

    /** 一条预置模板。字段含义见对象头。 */
    data class Preset(
        /** 稳定标记（卡片 key，不是 provider id）。 */
        val key: String,
        val name: String,
        val iconRes: Int,
        val protocol: CustomProviderOps.Protocol,
        val baseUrl: String,
        /** Base URL 输入框的备注（该家另一端协议的地址 / 站点差异）。 */
        val hint: String,
        /** Base URL 占位提示（自建面板没有固定地址时给一句「填你自己的站点」）。 */
        val placeholder: String = "",
        val balanceScheme: String = "",
        /** 「该上游无需鉴权」预勾选。 */
        val noAuth: Boolean = false,
        val urlSuffix: String = "",
        val headers: Map<String, String> = emptyMap(),
        val anthropicToolType: String = "",
        val clientEmulation: String = "",
    ) {
        /** 是否按「无需用户填 Base URL」渲染（自建面板）。 */
        val selfHosted: Boolean get() = baseUrl.isBlank()
    }

    /**
     * Anthropic 兼容端点普遍要求的一组 beta 头（上游 `ANTHROPIC_BETA_HEADERS`）。
     * 缺了它部分 beta 特性不可用 —— 值逐字照抄，别按自己的理解改。
     */
    private val ANTHROPIC_BETA = mapOf(
        "Anthropic-Beta" to "claude-code-20250219,interleaved-thinking-2025-05-14",
    )

    /** Cline Pass 的客户端伪装头（抄自本仓 Cline 适配器的 DEFAULT_HEADERS）。 */
    private val CLINE_HEADERS = mapOf(
        "User-Agent" to "Cline/3.0.62",
        "HTTP-Referer" to "https://cline.bot",
        "X-Title" to "Cline",
        "X-IS-MULTIROOT" to "false",
        "X-CLIENT-TYPE" to "cline-sdk",
        "X-CLIENT-VERSION" to "3.0.62",
        "X-PLATFORM" to "terminal",
        "X-PLATFORM-VERSION" to "3.0.62",
        "X-CORE-VERSION" to "0.0.70",
    )

    private val OPENAI = CustomProviderOps.Protocol.OPENAI
    private val ANTHROPIC = CustomProviderOps.Protocol.ANTHROPIC

    /**
     * 24 张模板，顺序 = 上游数组顺序（用户对网格位置有肌肉记忆，别重排）。
     * 末尾三家（sub2api / newapi / sensenova）在上游是「后到居后」追加的，
     * 这里保持同一位置。
     */
    val ALL: List<Preset> = listOf(
        Preset(
            key = "openai", name = "OpenAI", iconRes = R.drawable.preset_openai,
            protocol = OPENAI, baseUrl = "https://api.openai.com/v1",
            hint = "官方 OpenAI；协议可选 Responses 或 Chat Completions",
        ),
        Preset(
            key = "anthropic", name = "Anthropic", iconRes = R.drawable.preset_anthropic,
            protocol = ANTHROPIC, baseUrl = "https://api.anthropic.com",
            hint = "官方 Claude；已预置 Anthropic-Beta 头（开启 interleaved thinking）",
        ),
        Preset(
            key = "openrouter", name = "OpenRouter", iconRes = R.drawable.preset_openrouter,
            protocol = OPENAI, baseUrl = "https://openrouter.ai/api/v1",
            hint = "一个 Key 用遍多家模型，模型名用「厂商/模型」全称（如 deepseek/deepseek-chat）",
        ),
        Preset(
            key = "huggingface", name = "HuggingFace", iconRes = R.drawable.preset_huggingface,
            protocol = OPENAI, baseUrl = "https://router.huggingface.co/v1",
            hint = "走 Inference Providers 路由，Key 在 hf.co/settings/tokens 创建",
        ),
        Preset(
            key = "nvidia", name = "NVIDIA", iconRes = R.drawable.preset_nvidia,
            protocol = OPENAI, baseUrl = "https://integrate.api.nvidia.com/v1",
            hint = "NVIDIA NIM 托管端点，Key 在 integrate.api.nvidia.com 申请",
        ),
        Preset(
            key = "deepseek", name = "DeepSeek", iconRes = R.drawable.preset_deepseek,
            protocol = OPENAI, baseUrl = "https://api.deepseek.com",
            hint = "Anthropic 兼容端点在 https://api.deepseek.com/anthropic（协议换 anthropic 时填它）",
            balanceScheme = "deepseek",
        ),
        Preset(
            key = "glm", name = "GLM Coding", iconRes = R.drawable.preset_glm,
            protocol = ANTHROPIC, baseUrl = "https://api.z.ai/api/anthropic",
            hint = "国际版 z.ai 的 Coding 套餐；OpenAI 兼容地址为 https://api.z.ai/api/coding/paas/v4",
            balanceScheme = "zhipu_zai",
            urlSuffix = "?beta=true", headers = ANTHROPIC_BETA,
        ),
        Preset(
            key = "glm-cn", name = "GLM 中国", iconRes = R.drawable.preset_glm_cn,
            protocol = OPENAI, baseUrl = "https://open.bigmodel.cn/api/coding/paas/v4",
            hint = "智谱 bigmodel.cn 的 Coding 套餐，Key 在 open.bigmodel.cn/usercenter/apikeys",
            balanceScheme = "zhipu",
        ),
        Preset(
            key = "minimax", name = "Minimax Coding", iconRes = R.drawable.preset_minimax,
            protocol = ANTHROPIC, baseUrl = "https://api.minimax.io/anthropic",
            hint = "国际站（minimax.io）；OpenAI 兼容地址为 https://api.minimax.io/v1",
            urlSuffix = "?beta=true", headers = ANTHROPIC_BETA, anthropicToolType = "custom",
        ),
        Preset(
            key = "minimax-cn", name = "Minimax 中国", iconRes = R.drawable.preset_minimax_cn,
            protocol = ANTHROPIC, baseUrl = "https://api.minimaxi.com/anthropic",
            hint = "国内站（minimaxi.com）；OpenAI 兼容地址为 https://api.minimaxi.com/v1",
            urlSuffix = "?beta=true", headers = ANTHROPIC_BETA, anthropicToolType = "custom",
        ),
        Preset(
            key = "siliconflow", name = "SiliconFlow", iconRes = R.drawable.preset_siliconflow,
            protocol = OPENAI, baseUrl = "https://api.siliconflow.com/v1",
            hint = "国内站为 https://api.siliconflow.cn/v1",
        ),
        Preset(
            key = "volcengine-ark", name = "火山方舟", iconRes = R.drawable.preset_volcengine_ark,
            protocol = OPENAI, baseUrl = "https://ark.cn-beijing.volces.com/api/coding/v3",
            hint = "火山方舟 Coding 套餐端点；普通推理端点是 /api/v3",
        ),
        Preset(
            key = "alicode", name = "Alibaba", iconRes = R.drawable.preset_alicode,
            protocol = OPENAI, baseUrl = "https://coding.dashscope.aliyuncs.com/v1",
            hint = "阿里云 Coding 套餐（国内站 dashscope）",
        ),
        Preset(
            key = "alicode-intl", name = "Alibaba Coding", iconRes = R.drawable.preset_alicode_intl,
            protocol = OPENAI, baseUrl = "https://coding-intl.dashscope.aliyuncs.com/v1",
            hint = "阿里云 Coding 套餐国际站（新加坡）",
        ),
        Preset(
            key = "alims-intl", name = "Alibaba Studio", iconRes = R.drawable.preset_alims_intl,
            protocol = OPENAI, baseUrl = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            hint = "阿里云百炼国际站 OpenAI 兼容模式",
        ),
        Preset(
            key = "alitp-intl", name = "Alibaba Token Plan", iconRes = R.drawable.preset_alitp_intl,
            protocol = OPENAI,
            baseUrl = "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
            hint = "阿里云 Token 套餐（新加坡）",
        ),
        Preset(
            key = "xiaomi-tokenplan", name = "小米 MiMo (Token Plan)", iconRes = R.drawable.preset_xiaomi_tokenplan,
            protocol = OPENAI, baseUrl = "https://token-plan-sgp.xiaomimimo.com/v1",
            hint = "小米 MiMo Token Plan 套餐（新加坡站）",
        ),
        Preset(
            key = "opencode-go", name = "OpenCode Go", iconRes = R.drawable.preset_opencode_go,
            protocol = OPENAI, baseUrl = "https://opencode.ai/zen/go/v1",
            hint = "同地址换 anthropic 协议即走它的 Claude 兼容端点；转发必须填 OpenCode Go 的 API Key",
            balanceScheme = "opencode",
        ),
        Preset(
            key = "opencode-zen", name = "OpenCode Zen", iconRes = R.drawable.preset_opencode_zen,
            protocol = OPENAI, baseUrl = "https://opencode.ai/inference/v1",
            hint = "免费模型不用填 Key（默认勾上「该上游无需鉴权」）；付费模型则填 Zen 的 API Key",
            noAuth = true, clientEmulation = "opencode",
        ),
        Preset(
            key = "clinepass", name = "Cline Pass", iconRes = R.drawable.preset_cline,
            protocol = OPENAI, baseUrl = "https://api.cline.bot/api/v1",
            hint = "Cline Pass 的 API Key 在 app.cline.bot 获取（订阅制）；已预置 Cline 客户端伪装头",
            headers = CLINE_HEADERS,
        ),
        Preset(
            key = "ollama-local", name = "Ollama（本地）", iconRes = R.drawable.preset_ollama_local,
            protocol = OPENAI, baseUrl = "http://localhost:11434/v1",
            hint = "本机 Ollama 的 OpenAI 兼容端点，无需 API Key（已预勾「无需鉴权」）",
            noAuth = true,
        ),
        Preset(
            key = "sub2api", name = "Sub2API", iconRes = R.drawable.preset_sub2api,
            protocol = OPENAI, baseUrl = "",
            placeholder = "https://你的 Sub2API 站点地址（OpenAI 兼容填到 /v1）",
            hint = "自建聚合面板：填你自己的站点地址；余额查询用该账号的 API Key，不需要额外配置",
            balanceScheme = "sub2api",
        ),
        Preset(
            key = "newapi", name = "NewAPI", iconRes = R.drawable.preset_newapi,
            protocol = OPENAI, baseUrl = "",
            placeholder = "https://你的 NewAPI 站点地址（OpenAI 兼容填到 /v1）",
            hint = "自建聚合面板：填你自己的站点地址；余额查询要另填 NewAPI 的个人访问令牌",
            balanceScheme = "new_api",
        ),
        Preset(
            key = "sensenova", name = "商汤日日新", iconRes = R.drawable.preset_sensenova,
            protocol = OPENAI, baseUrl = "https://api.sensenova.cn/compatible-mode/v1",
            hint = "商汤日日新的 OpenAI 兼容端点；余额查询走控制台账号密码",
            balanceScheme = "sensenova",
        ),
    )

    fun byKey(key: String): Preset? = ALL.firstOrNull { it.key == key }

    /**
     * 某条自定义记录是否「由某张模板建出来的」—— 判据是**名称相等**。
     * 与上游 `matchesPresetName` 同一口径；改了名的家会回到「自定义」段。
     */
    fun matchesPresetName(name: String?): Boolean {
        val trimmed = name?.trim().orEmpty()
        if (trimmed.isEmpty()) return false
        return ALL.any { it.name == trimmed }
    }
}
