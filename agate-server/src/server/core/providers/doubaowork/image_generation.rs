//! 豆包工作生图：**在对话里派一次任务**，再换无水印原图。
//!
//! ── 上游没有独立的生图接口 ──────────────────────────────────
//! 出图就是「让 agent 干活」：请求发到 `POST /chat/completion`，agent 自己调用
//! 图像生成技能，结果作为 `gen_image_block`（`block_type = 10010`）挂在同一条
//! SSE 流里。所以这边的 [`ImageRequestPlan`] 本身就是一次**对话请求**，响应
//! 形态标成 `ImageResponseKind::Doubao` 由读取层认领（`core::upstream::images`）
//! —— 从流里取图、换原图这两步都在那儿，理由与 `ModelscopeAsync` 一致：
//! `ImageRequestPlan` 只表达"一次请求"，而换原图是**第二次**请求。
//!
//! ── 用哪条模式跑这次任务 ────────────────────────────────────
//! 生图能力挂在**通用任务模式**上，与目录里的对话模式不是一回事。上游用
//! `option.agent_mode != 0` 区分「派任务」与「普通对话」，所以这里不解析客户端
//! 传来的模型名（客户端给的是 [`MODEL_ID`] 那条合成条目），而是从目录里挑一条
//! `agent_mode` 非零的（判据见 `models::agent_choice`）。
//!
//! ── 参数口径 ────────────────────────────────────────────────
//! `size` 翻成**画面比例提示写进 prompt**：豆包从自然语言里读比例（实测把
//! "9:16" 写进 prompt 就会出竖版）。`quality` / `style` / `background` /
//! `moderation` 上游没有对应物，**明确拒绝**而不是静默丢掉 —— 静默会让用户
//! 以为风格生效了，这类「看起来成功了但没生效」最难查（与其余四家同一口径）。
//!
//! ── 能力位 ──────────────────────────────────────────────────
//! `supports_image_editing` 如实为 **false**：本家不吃参考图（那条路要经过
//! `prepare_upload` 把图换成上游资源 uri，是另一条链路）。声明成 true 会让编辑
//! 入口接受请求、然后在构造时失败 —— 比明确不支持更差。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use axum::http::HeaderMap;
use serde_json::{json, Value};

use crate::server::core::providers::images::{ImageOperation, ImageRequestPlan, ImageResponseKind};
use crate::server::errors::GatewayError;

/// 下游认的模型 id（目录里那条合成条目的 id）。
///
/// 不借用上游真身（`seedream_*` 之类）而另起一个名字，是因为**我们不是直连那个
/// 图像模型**：这条通道发出去的是对话请求，图由 agent 的任务产出。用上游真身
/// 当 id 会让人以为「请求 seedream 就等价于请求上游 seedream」。
pub const MODEL_ID: &str = "doubao-image";

/// 目录里那条合成条目。
///
/// `supportsImages` 恒 false：本家**不吃参考图**（见模块头的能力位说明）。
/// 标成能吃会让客户端在生图请求里附参考图，然后被这里的校验拒掉。
pub fn model_row() -> Value {
    json!({
        "id": MODEL_ID,
        "name": "豆包生图",
        "kind": "image",
        "output_modalities": ["image"],
        "supports_image_generation": true,
        "supports_image_editing": false,
        "supportsToolCall": false,
        "supportsImages": false,
        "supportsReasoning": false,
        "isDefault": false,
    })
}

/// `size` → 画面比例提示（写进 prompt）。
///
/// 八档常见比例，**差得太远就不猜**（返回 `None` ⇒ 不提比例，让上游自己定）：
/// 上游是从自然语言里读的，说错了它会照着错的画 —— 那比「不说」糟得多。
fn ratio(size: &str) -> Option<&'static str> {
    if size.is_empty() || size.eq_ignore_ascii_case("auto") {
        return None;
    }
    let (width, height) = size
        .trim()
        .split_once(['x', 'X'])?;
    let width: f64 = width.trim().parse().ok()?;
    let height: f64 = height.trim().parse().ok()?;
    if !(width.is_finite() && height.is_finite()) || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let target = width / height;
    let table: [(f64, &'static str); 8] = [
        (1.0, "1:1"),
        (16.0 / 9.0, "16:9"),
        (9.0 / 16.0, "9:16"),
        (4.0 / 3.0, "4:3"),
        (3.0 / 4.0, "3:4"),
        (3.0 / 2.0, "3:2"),
        (2.0 / 3.0, "2:3"),
        (21.0 / 9.0, "21:9"),
    ];
    let (matched, name) = table
        .into_iter()
        .min_by(|left, right| (left.0 - target).abs().total_cmp(&(right.0 - target).abs()))?;
    // 8%：`1024x1792`（客户端最常见值）对 9:16 有千分之几的偏差，卡死会把它判成
    // 「不认识的比例」—— 而它其实是我们要的那一档。
    ((matched - target).abs() / target <= 0.08).then_some(name)
}

/// 给 agent 的那句话。
///
/// 「不要只给文字描述」这句是必要的：agent 偶尔会先回一段画法说明就停下，
/// 实测带上它之后出图稳定得多。比例提示与内容拼在同一句里。
fn instruction(prompt: &str, ratio: Option<&str>) -> String {
    match ratio {
        Some(ratio) => format!(
            "请直接用图片生成能力画出来，不要只给文字描述。画面比例 {ratio}，画面内容：{prompt}"
        ),
        None => format!("请直接用图片生成能力画出来，不要只给文字描述。画面内容：{prompt}"),
    }
}

/// 构造上游生图请求。
pub fn build(
    account: &Value,
    body: &Value,
    _client_headers: &HeaderMap,
    operation: ImageOperation,
) -> Result<ImageRequestPlan, GatewayError> {
    if operation == ImageOperation::Edit {
        // 能力位已声明 false，候选链本来就不会把编辑派过来；真到了说明目录与
        // 能力位不同步，说清原因而不是拼一个注定失败的请求。
        return Err(GatewayError::bad_request(
            "豆包工作生图不支持参考图编辑，请改用支持图片编辑的模型",
        )
        .with_code(ImageOperation::Edit.unsupported_code()));
    }
    // 参考图在这条通道上不可能生效（要经 `prepare_upload` 换成资源 uri），
    // 收下再丢掉会让用户以为参考图起作用了。
    if ["images", "image", "mask", "input_fidelity"]
        .iter()
        .any(|key| body.get(*key).is_some_and(|value| !value.is_null()))
    {
        return Err(GatewayError::bad_request(
            "豆包工作生图不支持参考图，请改用支持图片编辑的模型",
        ));
    }
    if let Some(field) = ["quality", "style", "background", "moderation"]
        .into_iter()
        .find(|key| body.get(*key).is_some_and(|value| !value.is_null()))
    {
        return Err(GatewayError::bad_request(format!(
            "豆包工作生图不支持 {field}，请把要求写进 prompt"
        )));
    }
    if body.get("n").and_then(Value::as_u64).unwrap_or(1) != 1 {
        return Err(GatewayError::bad_request(
            "豆包工作一次任务只出一张图，请把 n 设为 1（需要多张请再生成一次）",
        ));
    }
    let prompt = body
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .ok_or_else(|| GatewayError::bad_request("图片请求缺少 prompt"))?;
    let hint = body.get("size").and_then(Value::as_str).and_then(ratio);
    let text = instruction(prompt, hint);

    let credentials = super::chat::credentials_for(account)?;
    // 任务模式：目录里挑一条 `agent_mode != 0` 的。挑不到就明确说清 ——
    // 拼一个普通对话请求发上去只会浪费一次额度，还让用户以为是 prompt 的问题。
    let chosen = super::models::agent_choice().ok_or_else(|| {
        GatewayError::with_status(
            400,
            "该豆包工作账号的目录里没有可跑任务的模式（生图需要它）：请刷新模型目录；\
             若仍不出现，请在该账号的客户端里确认「图像生成」可用",
        )
        .with_code("image_generation_not_supported")
    })?;
    let agent = super::plan::AgentChoice {
        agent_mode: chosen.get("agent_mode").and_then(Value::as_u64).unwrap_or(0),
        mode_id: chosen
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string),
        model_key: chosen
            .get("model_item_key")
            .and_then(Value::as_str)
            .or_else(|| chosen.get("id").and_then(Value::as_str))
            .unwrap_or("")
            .to_string(),
        extra: chosen
            .get("model_extra_params")
            .cloned()
            .unwrap_or_else(|| json!({})),
        deep_think: chosen
            .get("deep_think")
            .and_then(|value| match value {
                Value::Number(_) => value.as_i64(),
                Value::String(text) => text.trim().parse::<i64>().ok(),
                _ => None,
            })
            .unwrap_or(super::plan::THINK_OFF),
    };
    // 复用对话构造：任务信封与普通对话的差别**只有** `option` / `ext` 里那几件
    // agent 字段（见 `plan::AgentChoice::apply`），其余（风控 query、头、块结构）
    // 一条都不该重写 —— 重写一遍就多一处会和客户端行为漂开的实现。
    let built = super::plan::build_agent(
        &credentials,
        &json!({ "messages": [{ "role": "user", "content": text }] }),
        &super::plan::ReadyAttachments::default(),
        Some(&agent),
    )?;
    Ok(ImageRequestPlan::with_response(
        built.url,
        built.headers,
        built.body,
        ImageResponseKind::Doubao,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_only_matches_the_common_ones() {
        assert_eq!(ratio("1024x1024"), Some("1:1"));
        assert_eq!(ratio("1792x1024"), Some("16:9"));
        assert_eq!(ratio("1024x1792"), Some("9:16"));
        assert_eq!(ratio("2048x1536"), Some("4:3"));
        // auto / 空：不提比例（让上游自己定）
        assert_eq!(ratio("auto"), None);
        assert_eq!(ratio(""), None);
        // 2:1 离最近的档（16:9 差 11%、21:9 差 17%）都超过 8%：不猜，
        // 让上游自己定 —— 猜错的话它会照着错的比例画。
        assert_eq!(ratio("1000x500"), None);
        // 对照：5:3 与 16:9 只差 6.7%，落在容差内（客户端常见值就这一量级）
        assert_eq!(ratio("1000x600"), Some("16:9"));
        assert_eq!(ratio("abc"), None);
        assert_eq!(ratio("0x100"), None);
    }

    /// 指令里必须同时有「直接画」与内容：少了前一句，agent 会先回一段画法说明。
    #[test]
    fn instruction_asks_for_the_image_not_a_description() {
        let with_ratio = instruction("一只猫", Some("9:16"));
        assert!(with_ratio.contains("不要只给文字描述"), "{with_ratio}");
        assert!(with_ratio.contains("9:16"), "{with_ratio}");
        assert!(with_ratio.contains("一只猫"), "{with_ratio}");
        let plain = instruction("一只猫", None);
        assert!(!plain.contains("画面比例"), "{plain}");
    }

    /// 目录条目：进图像通道、不能编辑、不吃参考图、不参与默认模型回落。
    #[test]
    fn model_row_declares_the_channel_honestly() {
        let row = model_row();
        assert_eq!(row["kind"], json!("image"));
        assert_eq!(row["supports_image_generation"], json!(true));
        // 未接参考图 ⇒ 如实 false（见模块头）
        assert_eq!(row["supports_image_editing"], json!(false));
        assert_eq!(row["supportsImages"], json!(false));
        assert_eq!(row["isDefault"], json!(false));
    }

    /// 编辑入口要明确拒绝，而不是拼一个没有参考图的请求发上去。
    #[test]
    fn editing_is_refused_before_any_request_is_built() {
        let account = json!({ "id": "a", "auth": { "accessToken": "c=1" } });
        let error = build(
            &account,
            &json!({ "model": MODEL_ID, "prompt": "改一下" }),
            &HeaderMap::new(),
            ImageOperation::Edit,
        )
        .expect_err("本家不做参考图编辑");
        assert_eq!(
            error.code.as_deref(),
            Some(ImageOperation::Edit.unsupported_code())
        );
    }

    /// 参考图与 OpenAI 专属字段都在构造阶段拒绝：静默丢掉会让用户以为生效了。
    #[test]
    fn unsupported_fields_are_refused_with_a_next_step() {
        let account = json!({ "id": "a", "auth": { "accessToken": "c=1" } });
        let with_images = json!({
            "model": MODEL_ID,
            "prompt": "画一只猫",
            "images": [{ "image_url": "data:image/png;base64,AA" }],
        });
        let error = build(&account, &with_images, &HeaderMap::new(), ImageOperation::Generate)
            .expect_err("不吃参考图");
        assert!(error.message.contains("参考图"), "{}", error.message);

        let with_style = json!({ "model": MODEL_ID, "prompt": "画一只猫", "style": "vivid" });
        let error = build(&account, &with_style, &HeaderMap::new(), ImageOperation::Generate)
            .expect_err("没有 style 这个概念");
        assert!(error.message.contains("prompt"), "{}", error.message);

        let two = json!({ "model": MODEL_ID, "prompt": "画一只猫", "n": 2 });
        let error = build(&account, &two, &HeaderMap::new(), ImageOperation::Generate)
            .expect_err("一次任务只出一张");
        assert!(error.message.contains("n"), "{}", error.message);
    }
}