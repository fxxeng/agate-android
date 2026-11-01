//! DeepSeek 网页版模型目录：**静态两条 + 不接目录缓存**。
//!
//! ── 上游为什么不给目录接口（本家与别家最不同的一处）────────────
//! 网页版的模型是**固定的两档**，不是一个可列举的清单：
//! ```text
//!   deepseek-web-flash   快速档（默认；不显示思考链）
//!   deepseek-web-pro     专家档（深度思考；题干里叫 expert）
//! ```
//! 上游 binary 里出现过 `deepseek-web-flashflash` / `deepseek-web-proexpert`
//! 这两个连写串，正是「模型名 + 档位名」相邻排布的证据。**没有**
//! `/v1/models` 之类的列举接口，官方文档的能力表里 `model_refresh` 也如实
//! 标着 `undeclared` —— 所以本家的清单是**静态的**：
//! `supports_model_refresh() = false`，不参与目录刷新调度。
//!
//! ── 为什么不接 `catalog_cache`（照 zcode 的先例，别顺手接上）──────
//! 目录持久化缓存（`providers::catalog_cache`）解决的是「**远程拉取过**的
//! 清单在重启后不丢」。本家**根本没有远程拉取这件事** —— 清单是编译期常量，
//! 重启后照样是这两条。给一个静态清单写缓存只会写出一条「刚刷新过」的
//! 时间戳，而实际上没有发过任何请求，那是**编造**（界面上的「更新日期」列
//! 会因此显示一个假时间）。
//!
//! 同款先例是 `zcode`（同为静态清单家）：它的模块头写着「不提供 refresh：
//! `refresh_models` 会如实回答『这家没有远程目录』，而不是假装刷了一次」。
//! 本家照此办理。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::sync::OnceLock;

use serde_json::{json, Value};

use super::endpoints;

/// 内置清单（静态，两条）：`(model id, 展示名, 描述)`
///
/// id 是转发时直接下发的名字（上游 `model_type` 由 `chat::model_type_of`
/// 从 id 折出来）。改一个字母就等于换模型，逐字保留。
const MODEL_ENTRIES: [(&str, &str, &str); 2] = [
    (
        endpoints::MODEL_FLASH,
        "DeepSeek Flash（网页）",
        "快速档；不显示思考链，响应最快",
    ),
    (
        endpoints::MODEL_PRO,
        "DeepSeek Pro（网页）",
        "专家档；带深度思考，适合复杂任务",
    ),
];

/// 本家清单（进程内缓存；`OnceLock` 避免每请求重建）
static MODELS: OnceLock<Vec<Value>> = OnceLock::new();

/// 模型清单（`ProviderAdapter::list_models` 的返回值）。
pub fn list() -> Vec<Value> {
    MODELS.get_or_init(builtin).clone()
}

/// 内置两条 → 目录条目形态
///
/// 字段用的是聚合目录认的键（与 zcode / commandcode 同一口径）。
/// 能力位的取值依据：
/// - `supportsReasoning`：pro 档是深度思考档，思考链走 `reasoning_content`
///   （`protocol::deepseek_stream` 的模块头有帧证据）→ true；
/// - `supportsImage`：网页版有 `file/upload_file` 这条路径的痕迹，但二进制里
///   **没找到**它（见 `chat.rs` 的说明），且图片不是本家宣传的能力 →
///   保守 false（报错方向只能选「少报」）；
/// - `supportsToolCall`：网页版**没有**原生 function calling，官方文档也没
///   声明 → false；
/// - `supportsUsage`：上游 SSE **不返回 token 计数**（如实声明，见 `chat.rs`）
///   → false。
fn builtin() -> Vec<Value> {
    MODEL_ENTRIES
        .iter()
        .map(|(id, label, description)| {
            json!({
                "id": id,
                "label": label,
                "description": description,
                "supportsImage": false,
                "supportsReasoning": true,
                "supportsToolCall": false,
                "supportsUsage": false,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_exactly_two_web_modes() {
        let models = list();
        assert_eq!(models.len(), 2);
        let ids: Vec<&str> = models
            .iter()
            .filter_map(|model| model.get("id").and_then(Value::as_str))
            .collect();
        assert!(ids.contains(&endpoints::MODEL_FLASH));
        assert!(ids.contains(&endpoints::MODEL_PRO));
    }

    #[test]
    fn ids_match_the_wire_names() {
        // id 必须与 endpoints 里的常量逐字相同（改一个字母 = 换模型）
        for model in list() {
            let id = model.get("id").and_then(Value::as_str).unwrap_or("");
            assert!(
                id == endpoints::MODEL_FLASH || id == endpoints::MODEL_PRO,
                "未知 id：{id}"
            );
        }
    }

    #[test]
    fn capability_flags_are_honest() {
        for model in list() {
            // 上游不给 token 计数 —— 不能标 true
            assert_eq!(
                model.get("supportsUsage").and_then(Value::as_bool),
                Some(false)
            );
            // 网页版没有原生 function calling
            assert_eq!(
                model.get("supportsToolCall").and_then(Value::as_bool),
                Some(false)
            );
            // 图片能力没有依据 —— 保守 false
            assert_eq!(
                model.get("supportsImage").and_then(Value::as_bool),
                Some(false)
            );
            // 思考是本家真有的能力
            assert_eq!(
                model.get("supportsReasoning").and_then(Value::as_bool),
                Some(true)
            );
        }
    }

    #[test]
    fn list_is_stable_across_calls() {
        // 静态清单，多次调用结果一致（且 OnceLock 不重复构造）
        let first = list();
        let second = list();
        assert_eq!(first, second);
    }

    #[test]
    fn every_entry_has_label_and_description() {
        for model in list() {
            assert!(model.get("label").and_then(Value::as_str).is_some_and(|text| !text.is_empty()));
            assert!(model
                .get("description")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.is_empty()));
        }
    }
}