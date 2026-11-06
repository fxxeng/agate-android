//! 生图 / 图片编辑这条可选扩展的**判定层**：模型条目是不是图片模型、某次操作
//! 能派给哪些家、以及请求往哪条路走（`/v1/images/generations` 还是 `edits`）。
//!
//! ── 为什么单独一个模块，不塞进 catalog ─────────────────────────
//! 目录（`catalog`）回答的是「这个名字有没有人认」，生图要额外回答两件目录不
//! 关心的事：
//!   1. **这个模型产出的东西是不是图**（`kind` / `output_modalities`）：对话
//!      模型和图片模型在目录里是同一种条目，转发前必须能把它们分开，否则
//!      「拿 chat 打图片模型」会得到一条上游 400，而真正的原因是入口选错了；
//!   2. **这次操作（生成 / 编辑）有几家能接**：同一家可能只会生成不会编辑
//!      （ModelScope 的 Qwen-Image 与 Qwen-Image-Edit 是两个不同模型），能力
//!      位必须按操作分开问，否则「能生成」会被误当成「能改图」。
//!
//! ── 与上游（插件架构）的形态差异 ───────────────────────────────
//! 上游把这两件事问插件（`images.capabilities` RPC）；本仓 provider 是静态
//! 编译的，直接问 [`ProviderAdapter`] 的两个能力位。**判据一致**：家声明了
//! 能力位 ∧ 模型条目自己没否认（`supports_image_generation != false`）。
//!
//! ── 自定义提供商为什么不在这条链上 ─────────────────────────────
//! 自定义家不走 `ProviderAdapter`（走 `custom::forward`），没有能力位可问。
//! 因此 `operation_providers` **不会**把自定义家选进候选链 —— 这是有意的：
//! 靠「名字像个图片模型」去猜一家能不能生图，猜错的代价是一次真金白银的
//! 上游调用。错误文案会点明这一层（见 [`unsupported_message`]），不让用户
//! 对着「模型存在但打不通」自己猜。

use serde_json::Value;

use crate::server::core::models::model_id;
use crate::server::core::upstream::multipart::MultipartBody;
use crate::server::core::providers::adapter::adapter_for;
use crate::server::core::providers::{kind_from_id, ProviderKind};

use super::catalog;

/// 上游响应的**形态**：不是所有家都回标准 OpenAI `{data:[{b64_json|url}]}`。
///
/// ── 枚举里只放**已经接通**的形态（2026-11-06 五家全接）──────────
/// 五家 provider 的四种外壳一一对应：OpenAI 系（loomy / raccoon）、异步任务
/// （modelscope）、信封（workbuddy）、流式帧（antigravity 的 Gemini、
/// doubaowork 的对话流）。再加形态就加变体 ——
/// `core::upstream::images::collect` 的 `match` 会强制代码跟上，不存在
/// 「声明了但没人读」的静默状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageResponseKind {
    /// 标准 OpenAI Images：`{ "data": [{ "b64_json" | "url", "revised_prompt" }] }`。
    Openai,
    /// ModelScope API-Inference 异步：首包只给 `task_id`，要轮询
    /// `/v1/tasks/{id}` 直到 `SUCCEED`（见 `core::upstream::image_tasks`）。
    ModelscopeAsync,
    /// WorkBuddy 信封：`{ "code", "msg", "request_id",
    /// "data": { "data": [...], "usage": { "credit" } } }`。
    ///
    /// 与 OpenAI 形态的差别只有「多一层壳」：`code != 0` 才是失败（HTTP 可能
    /// 是 200），图藏在 `data.data` 里。**不能**把它当 OpenAI 形态硬读 ——
    /// 顶层没有 `data` 数组，会被误判成「上游没给图」。
    Workbuddy,
    /// Gemini / Antigravity：**SSE 帧**里的 `candidates[].content.parts[].inlineData`。
    ///
    /// 与另外三种的根本差别：图片不是「一包 JSON 的结果」，而是**一段流里的
    /// 若干帧**。所以读法完全不同 —— 要逐帧解析、累积图、还要等 `finishReason`
    /// 才敢说「收齐了」：
    ///   - 只认 `finishReason == "STOP"` 为正常收尾。流断了却没有 STOP，说明
    ///     图可能只到一半，**绝不能**当成功（截断的 PNG 解码后是半张图，用户
    ///     看到的是「生成了一张奇怪的东西」，比报错更难判断）；
    ///   - `SAFETY` / `IMAGE_SAFETY` / `PROHIBITED_CONTENT` 是内容策略拦截，
    ///     单独给 400 + `content_policy_violation`（不罚账号，见 `collect`）；
    ///   - `thought` 为 true 的 part 要跳过：Gemini 3 的思考过程也可能带
    ///     `inlineData`（那是草稿图，不是结果）。
    Gemini,
    /// 豆包工作：**对话流里取图**，再换无水印原图。
    ///
    /// 上游没有独立的生图接口 —— 出图就是让 agent 干活，结果作为
    /// `gen_image_block`（`block_type = 10010`）挂在 `/chat/completion` 那条
    /// SSE 流里。所以读法有三种形态之外的两件额外的事：
    ///   - **两种块都要收**：`10010` 给带水印的预览，`10084`（画布渲染）给
    ///     **无水印原图**与对象键。只认前者拿到的是带角标那张，只认后者在画布块
    ///     缺失时就什么都没有 —— 两边都收、按对象键合并；
    ///   - **流里没给原图时补一次**：`/creativity/resource/get_without_watermark`
    ///     用对象键换原图。实测同一张图，流里那份 `image_raw` 与那个接口返回的
    ///     `cgen_wm0` **字节完全相同**，所以正常一次请求就够，补的那次只服务
    ///     「旧版本 / 画布块缺失」。换不到就退回预览：有图总比报错好。
    Doubao,
}

/// 一家 provider 为某次生图/编辑拼好的上游请求。
///
/// 与 `ChatRequestPlan` 的分工相同：这里只管「往哪发、带什么头、发什么体」，
/// 不发请求、不改响应。响应怎么读由 [`ImageResponseKind`] 决定，由转发层统一做。
///
/// ── `body` 与 `multipart` 的分工（读代码前先看这段）────────────
/// `body` **恒**是一份 JSON：它是这次请求的可读描述，日志、调试报文与
/// `pointer()` 查询读的都是它。
///
/// `multipart` 为 `Some` 时，真正发出去的**不是** `body` 序列化后的字节，
/// 而是这里已拼好的多段表单 —— 上游那几家（目前只有 raccoon 的参考图编辑）
/// 要的是 `multipart/form-data`，图片原样嵌在里面。此时 `body` 是同一件事的
/// 可读近似（`{"prompt": …, "image": [{"filename", "content_type", "bytes"}]}`），
/// 两者由同一个 [`crate::server::core::upstream::multipart::MultipartForm`] 生成，
/// 不会各说各话。
///
/// 为什么要分成两个字段而不是把 `body` 换成枚举：`body` 的 `Value` 形态在本
/// 模块之外被大量读（调试报文、日志），换成枚举会让那些读点全部改一遍，而
/// 它们关心的本来就是「这份 JSON 长什么样」。
#[derive(Debug)]
pub struct ImageRequestPlan {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
    pub response: ImageResponseKind,
    /// `Some` = 真正发出去的是这段字节，`body` 只是可读近似（见结构体说明）。
    pub multipart: Option<MultipartBody>,
}

impl ImageRequestPlan {
    pub fn openai(url: String, headers: Vec<(String, String)>, body: Value) -> Self {
        Self {
            url,
            headers,
            body,
            response: ImageResponseKind::Openai,
            multipart: None,
        }
    }

    /// 多段表单出站（参考图上传）。
    ///
    /// `Content-Type` 头**由这里写**：它的 boundary 必须与报文字节里的分隔符
    /// 逐字一致，两处各写一遍就迟早会不一致（症状是上游回「拿不到字段」，
    /// 而报文看起来完全正常）。调用方传进来的同名头会被替换掉。
    pub fn multipart(
        url: String,
        mut headers: Vec<(String, String)>,
        form: MultipartBody,
    ) -> Self {
        headers.retain(|(name, _)| !name.eq_ignore_ascii_case("content-type"));
        headers.push(("Content-Type".to_string(), form.content_type()));
        Self {
            url,
            headers,
            body: form.preview.clone(),
            response: ImageResponseKind::Openai,
            multipart: Some(form),
        }
    }

    /// 非 OpenAI 响应形态的构造：**响应形态必须由调用方明写**，没有默认值。
    ///
    /// 刻意不提供 `Default` 或「按 provider 猜」的便利方法：猜错的后果是
    /// 把 `{code,msg,data:{...}}` 当标准形态读，症状是「上游明明出图了，
    /// 网关说上游没给图」。
    pub fn with_response(
        url: String,
        headers: Vec<(String, String)>,
        body: Value,
        response: ImageResponseKind,
    ) -> Self {
        Self {
            url,
            headers,
            body,
            response,
            multipart: None,
        }
    }
}

/// 生图与图片编辑。两者的入口路径、候选链与错误码都不同，所以做成枚举而不是
/// 一个 `bool` —— 调用点几乎总是要同时问「路径」和「能力位」，`bool` 会让每个
/// 调用点各写一份三元表达式。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImageOperation {
    Generate,
    Edit,
}

impl ImageOperation {
    pub fn label(self) -> &'static str {
        match self {
            Self::Generate => "图片生成",
            Self::Edit => "图片编辑",
        }
    }

    pub fn path(self) -> &'static str {
        match self {
            Self::Generate => "/v1/images/generations",
            Self::Edit => "/v1/images/edits",
        }
    }

    /// 入口门禁用的错误码（客户端据此区分「模型没有这个能力」与「名字不存在」）。
    pub fn unsupported_code(self) -> &'static str {
        match self {
            Self::Generate => "image_generation_not_supported",
            Self::Edit => "image_editing_not_supported",
        }
    }

    /// 这家是否声明了这类能力。
    pub fn supported_by(self, kind: ProviderKind) -> bool {
        let adapter = adapter_for(kind);
        match self {
            Self::Generate => adapter.supports_image_generation(),
            Self::Edit => adapter.supports_image_editing(),
        }
    }
}

/// 这条模型条目产出的是图片吗。
///
/// 认两种声明（与上游同一判据）：`kind` / `type` 为 `image`，或
/// `output_modalities` 里含 `image`。两者都不看模型名 —— 名字里带 `image` 的
/// 对话模型（多模态输入）不少，按名字猜会把它们误判成生图模型。
pub fn model_is_image(item: &Value) -> bool {
    item.get("kind")
        .or_else(|| item.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("image"))
        || item
            .get("output_modalities")
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("image")))
}

/// 某一家目录里，这个（已按家解析过发送名的）模型是不是图片模型。
pub fn provider_model_is_image(provider: &str, requested: &str) -> bool {
    let wire = catalog::wire_target_for_provider(requested, provider, None);
    catalog::manifest_for_provider(provider)
        .iter()
        .find(|item| model_id(item).eq_ignore_ascii_case(&wire.model))
        .is_some_and(model_is_image)
}

/// 这类操作能派给哪些家。
///
/// 三层过滤，缺一不可：
///   1. **有开启的转发绑定**（`forwarding_providers`）：关掉的家不该因为「它也
///      有这个模型」就复活；
///   2. **这家声明了这类能力**（能力位）；
///   3. **这家目录里这条模型确实是图片模型、且没被逐条否认**。
///
/// 第 3 层为什么不能省：同一家可能同时有 `qwen-image`（图片）与 `qwen-plus`
/// （对话），映射把某个名字指到对话模型上时，能力位仍然为真，但请求必然失败。
pub fn operation_providers(model: &str, operation: ImageOperation) -> Vec<String> {
    catalog::forwarding_providers(model)
        .into_iter()
        .filter(|provider| {
            kind_from_id(provider).is_some_and(|kind| operation.supported_by(kind))
        })
        .filter(|provider| {
            let wire = catalog::wire_target_for_provider(model, provider, None);
            catalog::manifest_for_provider(provider)
                .iter()
                .find(|item| model_id(item).eq_ignore_ascii_case(&wire.model))
                .is_some_and(|item| match operation {
                    ImageOperation::Generate => {
                        model_is_image(item)
                            && item.get("supports_image_generation").and_then(Value::as_bool)
                                != Some(false)
                    }
                    // 编辑没写 `supports_image_editing` 时**不算支持**：能生成不等于
                    // 能改图（ModelScope 就是两个独立模型），宁可让用户看到「没有可用
                    // 通道」，也不要发一次注定失败、已经扣了额度的请求。
                    ImageOperation::Edit => {
                        model_is_image(item)
                            && item.get("supports_image_editing").and_then(Value::as_bool)
                                == Some(true)
                    }
                })
        })
        .collect()
}

/// 「这个模型没有可用的生图通道」时给用户看的话。
///
/// 分开写两种原因：模型确实存在但没有任何家声明能力（要更新/换家），与模型
/// 压根不在任何目录里（名字错了）。合成一句话会让用户去改错的地方。
///
/// ⚠️ **本函数只回答「能力」问题，不回答「权限」问题**。当候选家存在、只是
/// 被网关 Key 的 provider 白名单排除时，必须用 [`key_scope_message`] ——
/// 两件事混用的症状见下面那条注释。
pub fn unsupported_message(model: &str, operation: ImageOperation) -> String {
    let known = catalog::providers_for_model(model);
    if known.is_empty() {
        return format!(
            "模型 '{model}' 不在任何提供商的目录里：请从 GET /v1/models 选一个 {} 模型（kind=image）",
            operation.label()
        );
    }
    format!(
        "模型 '{model}' 没有可用的{}通道：目录里有这家（{}），但它没有声明这类能力，\
         或它的自定义提供商尚未支持生图。请换一个 {} 模型（kind=image）",
        operation.label(),
        known.join("、"),
        operation.label()
    )
}

/// 「模型有能力，但这把 Key 不允许任何能用它的家」时给用户看的话。
///
/// ── 为什么必须与 [`unsupported_message`] 分开（2026-11-06 审计发现）──
/// `operation_providers()` 只看能力位、**不看 Key**。所以「候选非空但全被 Key
/// 排除」这条路径以前会走到 `unsupported_message`，输出「目录里有这家……
/// 但它没有声明这类能力，请换一个模型」——后半句恰好自证前提为假（这家明明
/// 声明了），而用户按提示去换模型，换到任何一家都被同一把 Key 挡住，死循环。
///
/// 这一条要给出**能照做的下一步**：去改 Key 的允许列表，或者换一把 Key。
/// 同时把那几家点名，让人知道该放开谁。
pub fn key_scope_message(model: &str, operation: ImageOperation, providers: &[String]) -> String {
    let names = if providers.is_empty() {
        String::from("（无）")
    } else {
        providers.join("、")
    };
    format!(
        "模型 '{model}' 的{}通道在（{names}），但这把网关 Key 不允许其中任何一家：\
         请在 Key 设置里放开上述家，或改用另一把 Key",
        operation.label()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kind_and_modalities_both_count() {
        assert!(model_is_image(&json!({ "id": "a", "kind": "image" })));
        assert!(model_is_image(&json!({ "id": "a", "type": "IMAGE" })));
        assert!(model_is_image(&json!({ "id": "a", "output_modalities": ["text", "image"] })));
        // 对话模型即使名字里带 image 也不算
        assert!(!model_is_image(&json!({ "id": "glm-4.6v", "supportsImages": true })));
        assert!(!model_is_image(&json!({ "id": "qwen-image-understanding" })));
    }

    #[test]
    fn operation_codes_and_paths_are_distinct() {
        assert_ne!(ImageOperation::Generate.path(), ImageOperation::Edit.path());
        assert_ne!(
            ImageOperation::Generate.unsupported_code(),
            ImageOperation::Edit.unsupported_code()
        );
    }
    /// 「Key 不允许」与「模型没能力」必须说成两句不同的话。
    ///
    /// 混用的后果是死循环：`operation_providers` 只看能力位，所以「候选非空但
    /// 全被 Key 排除」以前会落到 `unsupported_message`，输出「目录里有这家……
    /// 但它没有声明这类能力，请换一个模型」。用户照着换模型，换到任何一家都被
    /// 同一把 Key 挡住 —— 而两句话的下一步动作完全不同（改 Key vs 换模型）。
    #[test]
    fn key_exclusion_message_points_at_the_key_not_the_model() {
        let providers = vec!["loomy".to_string(), "raccoon".to_string()];
        let message = key_scope_message("raccoon-image", ImageOperation::Generate, &providers);

        // 必须点名是哪几家（用户才知道该放开谁）
        assert!(message.contains("loomy"), "{message}");
        assert!(message.contains("raccoon"), "{message}");
        // 必须指向 Key，而不是「换模型」
        assert!(message.contains("Key"), "{message}");
        assert!(
            !message.contains("换一个图片生成模型"),
            "不能与 unsupported_message 共用那句误导性建议：{message}"
        );
        // 模型名与操作要出现，否则用户不知道是哪次请求被打回
        assert!(message.contains("raccoon-image"), "{message}");
        assert!(message.contains("图片生成"), "{message}");
    }

    /// 空候选（极端情况）也不能输出「在（）」这种空括号。
    #[test]
    fn key_exclusion_message_survives_empty_provider_list() {
        let message = key_scope_message("x-image", ImageOperation::Edit, &[]);
        assert!(!message.contains("（）"), "{message}");
        assert!(message.contains("图片编辑"), "{message}");
    }

    /// 两者判据不同：`unsupported_message` 说的是「能力」，`key_scope_message`
    /// 说的是「权限」。同一段输入下，两句话不应有任何一句相同的关键建议。
    #[test]
    fn the_two_failure_messages_are_not_interchangeable() {
        let capability = unsupported_message("some-model", ImageOperation::Generate);
        let scope = key_scope_message(
            "some-model",
            ImageOperation::Generate,
            &["loomy".to_string()],
        );
        assert_ne!(capability, scope);
        // 前者不出现 Key 字样，后者必须出现 —— 这是两类问题的分水岭
        assert!(!capability.contains("Key"), "{capability}");
        assert!(scope.contains("Key"), "{scope}");
    }
}
