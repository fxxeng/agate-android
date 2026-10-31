//! ModelScope 模型目录：`GET {推理域}/models`（标准 OpenAI 形态）+ 进程缓存。
//!
//! ── 上游形态（真实账号实测）────────────────────────────────
//! ```text
//! GET https://api-inference.modelscope.cn/v1/models   Authorization: Bearer <ms-令牌>
//!   → {"object":"list","data":[{"id":"Qwen/Qwen3.8-Flash-Next","owned_by":"system",…}]}
//! ```
//! **只有 `id`**：没有 `name`、没有能力位、没有上下文长度。
//!
//! ── 清单是**账号相关**的（这是不做静态兜底表的理由）──────────────
//! 实测同一个令牌拉到 **36 条**，不是魔搭全站的几千个 —— 它给的是「这个账号在
//! API-Inference 上能调的模型」。编一份静态兜底表是**有害的**：用户会看到一批
//! 自己其实调不通的模型。拉不到就如实失败、保留旧清单。
//!
//! ── 清单里混着非对话模型（照实列出，不筛）──────────────────────
//! 36 条里含生图（`Qwen/Qwen-Image-Edit`）、基座（`PaddlePaddle/ERNIE-*-PT`）
//! 等并非 chat 用途的条目。上游给的就是它的清单，**不按 id 猜用途去过滤** ——
//! 猜错会把能用的模型藏起来。真调不通时 `classify` 会把上游的 400 如实透出。
//!
//! ── 两个地区两套缓存 ────────────────────────────────────────
//! 两个站的清单互不覆盖 —— 这正是当初把地区拆成两家 provider 要解决的问题。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use super::client;
use super::region::Region;
use crate::server::core::providers::adapter::ModelRefreshOutcome;

/// 模型列表路径（相对推理域）
const MODELS_PATH: &str = "/models";

/// 自动路径的缓存 TTL（与本仓其余家同档）
const TTL_MS: i64 = 10 * 60 * 1000;

/// 进程内缓存的一格
struct CatalogState {
    models: Vec<Value>,
    fetched_at: i64,
}

/// 两个地区各一个槽
static STATE_CN: OnceLock<Mutex<Option<CatalogState>>> = OnceLock::new();
static STATE_INTL: OnceLock<Mutex<Option<CatalogState>>> = OnceLock::new();

/// 取本地区的状态句柄（毒锁按「继续用」处理：这是可重建的展示数据）
fn state(region: Region) -> &'static Mutex<Option<CatalogState>> {
    match region {
        Region::Cn => STATE_CN.get_or_init(|| Mutex::new(None)),
        Region::Intl => STATE_INTL.get_or_init(|| Mutex::new(None)),
    }
}

/// 在锁内执行一段取值（毒锁不 panic，见 [`state`]）
fn with_state<R>(region: Region, f: impl FnOnce(&mut Option<CatalogState>) -> R) -> R {
    let mutex = state(region);
    let mut guard = mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut guard)
}

/// 把一个 OpenAI 目录条目映射成聚合层认的形态。
///
/// 上游只给 `id`，所以 `name` 就用 id 本身：魔搭的 id 是 `组织/模型名`，本身
/// 就是可读的展示名，拆掉组织段反而会让同名的不同组织条目在界面上无法区分。
///
/// 工具位按 OpenAI 兼容网关的既有能力给 true。**思考位不给** —— 上游没有能力位，
/// 按 id 猜会两个方向一起猜错；交给模型规则层由用户显式绑定更诚实
/// （与上游插件同一取舍）。
fn map_entry(row: &Value) -> Option<Value> {
    let id = row
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(json!({
        "id": id,
        "name": id,
        "supportsToolCall": true,
    }))
}

/// 解析 `/models` 响应（OpenAI `{data:[...]}` 或裸数组）
pub fn parse_models(payload: &Value) -> Vec<Value> {
    let rows = payload
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| payload.as_array())
        .cloned()
        .unwrap_or_default();
    rows.iter().filter_map(map_entry).collect()
}

/// 本地区当前的模型清单（内存缓存；没有时为空，由刷新路径填）
pub fn list(region: Region) -> Vec<Value> {
    with_state(region, |slot| {
        slot.as_ref().map(|state| state.models.clone()).unwrap_or_default()
    })
}

/// 清单是否来自远程（「有内容」就等于远程拉到过 —— 本家没有内置兜底表）
pub fn remote_refreshed(region: Region) -> bool {
    with_state(region, |slot| {
        slot.as_ref().map(|state| !state.models.is_empty()).unwrap_or(false)
    })
}

/// 上次拉取时刻（毫秒；没拉过给 0）
pub fn last_refreshed_at(region: Region) -> i64 {
    with_state(region, |slot| slot.as_ref().map(|state| state.fetched_at).unwrap_or(0))
}

/// 拉一次模型目录并落地（内存缓存）。
///
/// `force = false`（自动路径）时 TTL 内早退为 `unchanged()`；
/// `force = true`（用户手动点刷新）真打上游 —— 与 trait 契约里「谁发起决定
/// 要不要绕缓存」一致，不在实现里猜。
pub async fn refresh(region: Region, token: &str, force: bool) -> ModelRefreshOutcome {
    let now = crate::server::logging::now_ms();
    if !force {
        let fresh = with_state(region, |slot| {
            slot.as_ref()
                .map(|state| !state.models.is_empty() && now - state.fetched_at < TTL_MS)
                .unwrap_or(false)
        });
        if fresh {
            return ModelRefreshOutcome::unchanged();
        }
    }
    let (status, payload) =
        match client::inference_get(region, MODELS_PATH, token, "模型目录查询").await {
            Ok(result) => result,
            Err(error) => return ModelRefreshOutcome::failed(error),
        };
    if !(200..300).contains(&status) {
        return ModelRefreshOutcome::failed(format!(
            "ModelScope 模型目录返回 HTTP {status}（保留现有清单）"
        ));
    }
    let models = parse_models(&payload);
    if models.is_empty() {
        return ModelRefreshOutcome::failed(
            "ModelScope 模型目录为空或格式不认识（保留现有清单）",
        );
    }
    let count = models.len();
    with_state(region, |slot| {
        *slot = Some(CatalogState { models, fetched_at: now });
    });
    ModelRefreshOutcome::refreshed(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 上游只给 id：`name` 必须回落 id，且工具位给 true、思考位不给
    #[test]
    fn entry_maps_id_to_name_and_skips_reasoning() {
        let parsed = parse_models(&json!({
            "object": "list",
            "data": [
                {"id": "Qwen/Qwen3.8-Flash-Next"},
                {"id": "   "},
                {"object": "x"}
            ]
        }));
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].get("name").and_then(Value::as_str), Some("Qwen/Qwen3.8-Flash-Next"));
        assert_eq!(parsed[0].get("supportsToolCall").and_then(Value::as_bool), Some(true));
        assert!(parsed[0].get("supportsReasoning").is_none());
    }

    /// 裸数组形态也要认（上游换写法时不至于整份清单丢掉）
    #[test]
    fn parse_accepts_bare_array() {
        assert_eq!(parse_models(&json!([{"id": "a/b"}])).len(), 1);
        assert!(parse_models(&json!({"data": []})).is_empty());
    }
}
