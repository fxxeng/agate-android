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
use crate::server::core::providers::adapter::adapter_for;
use crate::server::core::providers::{kind_from_id, ProviderKind};

use super::catalog;

/// 上游响应的**形态**：不是所有家都回标准 OpenAI `{data:[{b64_json|url}]}`。
///
/// ── 枚举里只放**已经接通**的形态（2026-11-06）─────────────────
/// 上游另有 `Gemini`（SSE 帧里的 inlineData）与 `Doubao`（对话流里取图 +
/// 换无水印原图）两种，本仓还没接，所以这里不出现这两个名字：留一个
/// 「声明了但 `collect` 里没有分支」的变体，只会在运行时变成一个没人能
/// 解释的 500。接一家就加一种，加到哪一种就实现到哪一种
/// （`core::upstream::images::collect` 的 match 会强制代码跟上）。
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
}

/// 一家 provider 为某次生图/编辑拼好的上游请求。
///
/// 与 `ChatRequestPlan` 的分工相同：这里只管「往哪发、带什么头、发什么体」，
/// 不发请求、不改响应。响应怎么读由 [`ImageResponseKind`] 决定，由转发层统一做。
#[derive(Debug)]
pub struct ImageRequestPlan {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
    pub response: ImageResponseKind,
}

impl ImageRequestPlan {
    pub fn openai(url: String, headers: Vec<(String, String)>, body: Value) -> Self {
        Self {
            url,
            headers,
            body,
            response: ImageResponseKind::Openai,
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
}
