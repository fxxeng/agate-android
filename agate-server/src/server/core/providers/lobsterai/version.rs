//! LobsterAI 客户端版本解析：`X-LobsterAI-Client-Version` 要的是**日期式版本**
//! （形如 `2026.9.4`），不是我们自己的版本号。
//!
//! ── 从哪来 ──────────────────────────────────────────────────
//! 上游的更新接口（`CLIENT_VERSION_API`，第三方主机）报当前桌面端版本。
//! ⚠️ 它的响应**不是业务信封**：版本号在 `data.value.version`（**两层**，
//! `value` 是个对象）。
//!
//! ── 为什么要缓存 ────────────────────────────────────────────
//! 版本在每个请求上都要用（换码 / 续期 / 目录 / 签到），而它一天也变不了几次；
//! 参考实现缓存 12 小时。取不到就退回已知可用的常量，**绝不发空版本头** ——
//! 空版本头会被服务端按「不认识的客户端」对待。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use crate::server::core::auth_http::send_raw;
use crate::server::logging;

use super::endpoints::{
    CLIENT_VERSION_API, FALLBACK_CLIENT_VERSION, REQUEST_TIMEOUT_MS, USER_AGENT,
    VERSION_CACHE_TTL_MS,
};


struct VersionCache {
    value: String,
    fetched_at: i64,
}

fn cache() -> &'static std::sync::Mutex<Option<VersionCache>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Option<VersionCache>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

/// 从更新接口响应里读版本号。
///
/// ⚠️ **路径是 `data.value.version`，不是 `data.value`** —— `value` 是个**对象**，
/// 版本号在它的 `version` 成员里。这一处曾写错成把 `data.value` 当标量读，
/// 表现是版本头永远退化成兜底常量（测试夹具也照错的假设编，于是测试全绿而代码全错）。
/// 单独抽出来是为了能脱网单测：这一段是上游契约，改错就是整家发错版本头。
pub fn version_from_payload(payload: &serde_json::Value) -> Option<String> {
    let value = payload.get("data")?.get("value")?;
    let version = value.get("version")?;
    let text = match version {
        serde_json::Value::String(text) => text.trim().to_string(),
        other => other.to_string(),
    };
    (!text.is_empty()).then_some(text)
}

/// 同步版的兜底取值：给「已经拿着版本、只是要修空串」的场景用
pub fn fallback_version() -> &'static str {
    FALLBACK_CLIENT_VERSION
}

/// 解析当前客户端版本（缓存 12 小时；失败回落兜底版本）。
///
/// ── 为什么缓存而不每次现查 ────────────────────────────────
/// 版本在每个请求上都要用（换码 / 续期 / 目录 / 签到），而它一天也变不了几次 ——
/// 每请求打一次更新接口既没必要也招风控。失败也写缓存（值是空），下一次调用直接
/// 回落兜底版本，不会因为上游更新接口抖动而在每个请求上重试。
pub async fn resolve_client_version() -> String {
    {
        let guard = match cache().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(entry) = guard.as_ref() {
            if logging::now_ms() - entry.fetched_at < VERSION_CACHE_TTL_MS
                && !entry.value.is_empty()
            {
                return entry.value.clone();
            }
        }
    }
    let response = send_raw(
        "GET",
        CLIENT_VERSION_API,
        None,
        &[
            ("accept".to_string(), "application/json".to_string()),
            ("user-agent".to_string(), USER_AGENT.to_string()),
        ],
        None,
        Some(REQUEST_TIMEOUT_MS),
    )
    .await;
    let resolved = match response {
        Ok(response) if response.ok => response
            .payload
            .as_ref()
            .and_then(version_from_payload)
            .unwrap_or_default(),
        Ok(response) => {
            logging::verbose(
                "[LobsterAI]",
                &format!("版本接口返回 HTTP {}，改用兜底版本", response.status),
            );
            String::new()
        }
        Err(error) => {
            logging::verbose(
                "[LobsterAI]",
                &format!("版本接口不可达（{error}），改用兜底版本"),
            );
            String::new()
        }
    };
    let mut guard = match cache().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = Some(VersionCache {
        value: resolved.clone(),
        fetched_at: logging::now_ms(),
    });
    if resolved.is_empty() {
        FALLBACK_CLIENT_VERSION.to_string()
    } else {
        resolved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// ⚠️ 版本号在 `data.value.version`（**两层**），不是 `data.value` ——
    /// 这条夹具按上游参考实现的路径编，形状对不对由它守。
    #[test]
    fn version_reads_nested_value_version() {
        assert_eq!(
            version_from_payload(&json!({"data": {"value": {"version": " 2026.9.4 "}}})),
            Some("2026.9.4".to_string())
        );
        assert_eq!(
            version_from_payload(&json!({"data": {"value": {"version": 20260904}}})),
            Some("20260904".to_string())
        );
        // `data.value` 当标量读不出来（这正是曾经写错的那一处）
        assert_eq!(version_from_payload(&json!({"data": {"value": "2026.9.4"}})), None);
        assert_eq!(version_from_payload(&json!({"data": {}})), None);
    }

    /// 兜底版本不为空（空版本头会被按「不认识的客户端」对待）
    #[test]
    fn fallback_version_is_never_empty() {
        assert!(!fallback_version().trim().is_empty());
    }
}
