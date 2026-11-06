//! ModelScope API-Inference 的 Images 接口：**异步任务**形态。
//!
//! ── 为什么这家要单独一套（连响应都要轮询）──────────────────────
//! 它不像别的家那样「POST 一次拿一包图」，而是：
//! ```text
//! POST /v1/images/generations   X-ModelScope-Async-Mode: true
//!   → {"task_id":"…"}                          ← 首包只有任务号
//! GET  /v1/tasks/{id}           X-ModelScope-Task-Type: image_generation
//!   → {"task_status":"PENDING|RUNNING|SUCCEED","output_images":[…]}
//! ```
//! 「读响应」那一步因此在宿主侧（`core::upstream::image_tasks`），本文件只负责
//! **第一步**：把请求拼成它认的形态、声明异步、并告诉转发层「响应该按
//! `ModelscopeAsync` 读」。
//!
//! ── 型号能力是**静态表**，不是问上游 ────────────────────────────
//! 照官方文档与模型卡登记，只收「确认支持 Images 接口」的那几个：
//! 生图（`Qwen-Image` 系 / `Z-Image` 系）与编辑（`Qwen-Image-Edit` 系）是
//! **两组不同模型**，同一条 id 不会两样都能做。不把整个模型市场当清单 —— 那份
//! 清单里绝大多数型号根本没有 Images 接口，列出来的结果是「点了就 400」。
//!
//! ── 参数收窄（与别家不同的几处）────────────────────────────────
//!   - `n` 只能是 1：异步接口一次一个任务，多张要发多次（网关不替它排）。
//!   - `size` 只接受 `64..=2048` 的 `WIDTHxHEIGHT`（模型卡给的范围）。
//!   - `style` / `mask` / `input_fidelity` 一律拒绝：这个接口没有这些概念，
//!     静默丢掉会让用户以为设置生效了。
//!   - `quality` / `background` / `moderation` 只认它文档里那几个值。

use serde_json::{json, Value};

use crate::server::core::providers::images::{ImageRequestPlan, ImageResponseKind};
use crate::server::errors::GatewayError;

use super::region::Region;

/// 这个型号能不能做这类操作（`None` = 这家没有这个型号的 Images 能力）。
///
/// 返回 `(生图, 编辑)`。
pub fn capabilities(id: &str) -> Option<(bool, bool)> {
    let generation = [
        "Qwen/Qwen-Image",
        "Qwen/Qwen-Image-2512",
        "Tongyi-MAI/Z-Image-Turbo",
        "Tongyi-MAI/Z-Image",
    ];
    let editing = [
        "Qwen/Qwen-Image-Edit",
        "Qwen/Qwen-Image-Edit-2509",
        "Qwen/Qwen-Image-Edit-2511",
    ];
    if generation.iter().any(|model| model.eq_ignore_ascii_case(id)) {
        Some((true, false))
    } else if editing.iter().any(|model| model.eq_ignore_ascii_case(id)) {
        Some((false, true))
    } else {
        None
    }
}

/// 这个型号能不能做**这一类**操作。
///
/// 与 [`capabilities`] 的分工：那个回答「这条型号是哪一类」，这个回答「拿它做
/// 这件事行不行」。分成两个是因为调用点问的不是同一件事 —— 目录制作用前者，
/// 请求构造用后者（后者还要能被当成一道防御：入口放行的型号与操作必须自洽）。
pub fn supports(id: &str, editing: bool) -> bool {
    capabilities(id).is_some_and(|(generation, editable)| {
        if editing { editable } else { generation }
    })
}

/// 把一条目录条目标成图片模型（清单来自 `GET /models`，那条响应只有 id）。
///
/// 在 `models::map_entry` 里调：目录是**所有**出口（`/v1/models`、管理页、
/// 生图入口的默认模型挑选）的唯一源头，标在这里就不存在「某处认、某处不认」。
///
/// 图片模型同时要**关掉** `supportsToolCall`：那是对话侧的能力位，Images
/// 接口没有工具调用这回事；留着 true 会让客户端把它当可对话模型排进列表。
pub fn decorate(id: &str, row: &mut Value) {
    let Some((generation, editing)) = capabilities(id) else {
        return;
    };
    row["kind"] = json!("image");
    row["output_modalities"] = json!(["image"]);
    row["supports_image_generation"] = json!(generation);
    row["supports_image_editing"] = json!(editing);
    row["supportsToolCall"] = json!(false);
    // 编辑要读参考图 → `supportsImages` 只在编辑模型上为真（生图模型不看图）。
    // 它在这里的含义是「**输入**是否含图片」，与 `output_modalities` 不同维。
    row["supportsImages"] = json!(editing);
    row["supportsReasoning"] = json!(false);
    // 生图模型不能当默认对话模型（`isDefault` 是默认回落链与账号页筛选的键）
    row["isDefault"] = json!(false);
}

/// 构造 `POST {推理域}/images/generations`（异步模式）。
pub fn build(
    account: &Value,
    body: &Value,
    region: Region,
    editing: bool,
) -> Result<ImageRequestPlan, GatewayError> {
    let wire_model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let cap = capabilities(&wire_model).ok_or_else(|| {
        GatewayError::bad_request(format!(
            "该 ModelScope 型号（{wire_model}）尚未确认支持 Images 接口"
        ))
    })?;
    if (editing && !cap.1) || (!editing && !cap.0) {
        return Err(GatewayError::bad_request(
            "该 ModelScope 型号不支持这类图片操作，请选择对应的生图或编辑模型",
        ));
    }
    if body.get("n").and_then(Value::as_u64).unwrap_or(1) != 1 {
        return Err(GatewayError::bad_request(
            "ModelScope 异步图片接口单次仅支持 n=1",
        ));
    }
    for (field, allowed) in [
        ("quality", &["auto", "standard"][..]),
        ("background", &["auto", "opaque"][..]),
        ("moderation", &["auto"][..]),
    ] {
        if let Some(value) = body.get(field).filter(|value| !value.is_null()) {
            if !value.as_str().is_some_and(|value| allowed.contains(&value)) {
                return Err(GatewayError::bad_request(format!(
                    "ModelScope 不支持 {field}={value}"
                )));
            }
        }
    }
    for field in ["style", "mask", "input_fidelity"] {
        if body.get(field).is_some_and(|value| !value.is_null()) {
            return Err(GatewayError::bad_request(format!(
                "ModelScope 图片接口暂不支持 {field}"
            )));
        }
    }
    let mut out = json!({ "model": wire_model, "prompt": body.get("prompt") });
    if let Some(size) = body
        .get("size")
        .and_then(Value::as_str)
        .filter(|value| *value != "auto")
    {
        let (width, height) = size
            .split_once('x')
            .and_then(|(width, height)| {
                Some((width.parse::<u32>().ok()?, height.parse::<u32>().ok()?))
            })
            .filter(|(width, height)| (64..=2048).contains(width) && (64..=2048).contains(height))
            .ok_or_else(|| {
                GatewayError::bad_request("ModelScope size 需要 64 至 2048 范围内的 WIDTHxHEIGHT")
            })?;
        out["size"] = json!(format!("{width}x{height}"));
    }
    if editing {
        let images = body
            .get("images")
            .and_then(Value::as_array)
            .filter(|items| !items.is_empty() && items.len() <= 3)
            .ok_or_else(|| GatewayError::bad_request("ModelScope 参考图编辑需要 1 至 3 张图片"))?;
        out["image_url"] = Value::Array(
            images
                .iter()
                .map(|image| image.get("image_url").cloned().unwrap_or(Value::Null))
                .collect(),
        );
    }
    let token = super::credentials::token_of(account).map_err(|error| {
        GatewayError::with_status(401, format!("读取 ModelScope 账号失败：{error}"))
    })?;
    Ok(ImageRequestPlan {
        url: format!("{}/images/generations", region.inference_base_url()),
        headers: vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Authorization".to_string(), format!("Bearer {token}")),
            // 声明异步：不带这个头，上游会按同步接口处理（一次请求等几十秒），
            // 而本仓的「读响应」那一步是按任务号轮询的（见模块头）。
            ("X-ModelScope-Async-Mode".to_string(), "true".to_string()),
        ],
        body: out,
        response: ImageResponseKind::ModelscopeAsync,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_and_editing_are_disjoint() {
        // 同一型号不会两样都能做：这正是「按操作分开问能力」的理由。
        assert_eq!(capabilities("Qwen/Qwen-Image"), Some((true, false)));
        assert_eq!(capabilities("qwen/qwen-image-edit"), Some((false, true)));
        assert_eq!(capabilities("Qwen/Qwen3.8-Flash-Next"), None);
    }
}
