//! ModelScope 两种出站请求的薄封装：网页站（cookie 鉴权）与推理域（Bearer）。
//!
//! ── 为什么收在一处 ──────────────────────────────────────────
//! 四个消费者（`models` / `balance` / `checkin` / 添加账号时的探活）共用同一套
//! 超时、错误翻译与「信封 → 人话」口径；各写一份会让「哪个码算令牌失效」
//! 这类判据分叉。
//!
//! ── 上游的**两种信封**（实测，同一家的不同接口）──────────────────
//! 魔搭自己没统一大小写，取值一律两种都认：
//! ```text
//!   /api/v1/users/login/info        {"Code":200,"Success":true,"Data":{…},"Message":"success"}
//!   /api/v1/inference/rate-limit    {"Code":200,"Success":true,"Data":{…}}
//!   /openapi/v1/magicubes/balance   {"success":true,"data":{…}}          ← 全小写
//! ```
//! 只按一种写死的后果：某条链路把成功读成失败（或反之），且没有任何日志提示。
//!
//! ── 令牌失效的判据 ──────────────────────────────────────────
//! 网页站对无效令牌给 **HTTP 401 + `{"Code":10019901001,"Message":"access forbidden"}`**；
//! 也有接口用 HTTP 200 + `Success:false` 表达失败。两条路都要认：401/403 →
//! 调用方按令牌失效处置，业务位为假 → 报错并把上游文案带出来。
//!
//! ── 出网为什么是直连（`proxy = None`）────────────────────────
//! 与本仓 Loomy / MonkeyCode 的余额与签到链路同口径：这些是**账号级辅助请求**，
//! 转发链路的账号代理不该被静默套到它们头上（用户看到的「直连」要真的是直连）。
//! 上游插件用的是网关级代理；本仓没有那个概念，因此如实直连。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::Value;

use crate::server::core::auth_http::send_raw;

use super::region::Region;

/// 辅助请求的超时：魔搭的接口都在国内，20 秒足够，且不让一次挂住的余额查询
/// 把界面的转圈转到天荒地老（`egress` 默认读超时 600 秒是给 SSE 用的）
const TIMEOUT_MS: u64 = 20_000;

/// 响应信封的**成功位**（`Success` / `success` / `Code==200` 三级判据）
pub fn succeeded(payload: &Value) -> bool {
    if let Some(flag) = payload.get("Success").and_then(Value::as_bool) {
        return flag;
    }
    if let Some(flag) = payload.get("success").and_then(Value::as_bool) {
        return flag;
    }
    match payload.get("Code").and_then(Value::as_i64) {
        Some(code) => code == 200,
        // HTTP 已经 2xx 了，没有反证就照实报成功
        None => true,
    }
}

/// 信封里的**业务数据**（`Data` / `data` 两层都认）
pub fn data_of(payload: &Value) -> Value {
    payload
        .get("Data")
        .or_else(|| payload.get("data"))
        .cloned()
        .unwrap_or(Value::Null)
}

/// 信封里的**上游文案**（`Message` / `message` / `msg` 三种都认）
pub fn message_of(payload: &Value) -> String {
    payload
        .get("Message")
        .or_else(|| payload.get("message"))
        .or_else(|| payload.get("msg"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

/// 信封里的业务码（`Code` / `code`；都没有时 0）
pub fn code_of(payload: &Value) -> i64 {
    payload
        .get("Code")
        .or_else(|| payload.get("code"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

/// 向**网页站**发一次带会话 cookie 的 GET。
///
/// 鉴权是 `Cookie: m_session_id=<令牌>`（实测口径）。返回 `(status, payload)`；
/// **业务位不在本函数里判** —— 各调用方自己知道「这次调用的成功长什么样」
/// （签到只看 HTTP，余额要看业务位）。
pub async fn site_get(
    region: Region,
    path: &str,
    token: &str,
    what: &str,
) -> Result<(u16, Value), String> {
    if token.trim().is_empty() {
        return Err("ModelScope 账号缺少令牌，无法查询".to_string());
    }
    let url = format!("{}{path}", region.site_base_url());
    let headers = vec![("Cookie".to_string(), format!("m_session_id={token}"))];
    let response = send_raw("GET", &url, None, &headers, None, Some(TIMEOUT_MS))
        .await
        .map_err(|error| transport_error(what, &error))?;
    Ok((response.status, response.payload.unwrap_or(Value::Null)))
}

/// 向**推理域**发一次带 Bearer 的 GET（目前只有模型目录用）。
///
/// 与 [`site_get`] 是两个域：网页站挂了会连签名一起失败，推理域挂了只影响目录 ——
/// 分开才不会让排障时误判成同一处故障。
pub async fn inference_get(
    region: Region,
    path: &str,
    token: &str,
    what: &str,
) -> Result<(u16, Value), String> {
    if token.trim().is_empty() {
        return Err("ModelScope 账号缺少令牌，无法查询".to_string());
    }
    let url = format!("{}{path}", region.inference_base_url());
    let headers = vec![("Authorization".to_string(), format!("Bearer {token}"))];
    let response = send_raw("GET", &url, None, &headers, None, Some(TIMEOUT_MS))
        .await
        .map_err(|error| format!("{what}失败：{error}"))?;
    Ok((response.status, response.payload.unwrap_or(Value::Null)))
}

/// 传输层错误 → 人话（超时单列，因为它是唯一「重试可能就好」的一类）
fn transport_error(what: &str, error: &reqwest::Error) -> String {
    if error.is_timeout() {
        format!("{what}超时，请稍后重试")
    } else {
        format!("{what}失败：{error}")
    }
}

/// 令牌失效的统一文案（网页站与推理域共用一个判据：401 / 403）。
///
/// 打错站点时上游给的就是 401 —— 那正是用户把两地账号加混时最需要看到的一句，
/// 所以文案里点明「不属于该站点」。
pub fn expired_message(region: Region, status: u16, site: bool) -> String {
    if site {
        format!(
            "ModelScope 令牌已失效或不属于该站点（{}），请重新粘贴（HTTP {status}）",
            region.site_base_url()
        )
    } else {
        format!("ModelScope 令牌已失效，请重新粘贴（HTTP {status}）")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 信封两种大小写都要认 —— 只认一种会把成功读成失败
    #[test]
    fn envelope_accepts_both_casings() {
        assert!(succeeded(&json!({"Success": true})));
        assert!(succeeded(&json!({"success": true})));
        assert!(succeeded(&json!({"Code": 200})));
        assert!(!succeeded(&json!({"Success": false})));
        assert!(!succeeded(&json!({"Code": 500})));
        assert_eq!(code_of(&json!({"Code": 401})), 401);
        assert_eq!(code_of(&json!({"code": 402})), 402);
    }

    /// 业务数据两种大小写都要认
    #[test]
    fn data_and_message_read_both_casings() {
        assert_eq!(data_of(&json!({"Data": {"a": 1}})), json!({"a": 1}));
        assert_eq!(data_of(&json!({"data": {"a": 1}})), json!({"a": 1}));
        assert_eq!(message_of(&json!({"Message": " ok "})), "ok");
        assert_eq!(message_of(&json!({"msg": "bad"})), "bad");
    }
}
