//! DeepSeek 网页版凭证：**一枚 `userToken`**（无 OAuth 流程、无续期接口）。
//!
//! ── 上游凭什么是这个（不是 API Key、也不是 Cookie）──────────────
//! 三个容易走偏的点，逐条写下来：
//! ```text
//!   1. 不是 `sk-…` 的开放平台 Key。平台 Key 走 api.deepseek.com，
//!      那是另一套（计费）接口，本家要的是**网页版自己的身份**。
//!   2. 不是 Cookie。网页版认的是浏览器 localStorage 里
//!      `userToken` 对象的 `.value` 字段。把 Cookie 整串粘进来会稳定 401
//!      —— 这是本家最常见的「加了账号但一用就失败」的原因。
//!   3. 不过期续期：上游**没有** refresh 接口（官方文档的能力表里
//!      refresh / model_refresh / usage / checkin 全部 `undeclared`），
//!      因此 `supports_refresh = false`。登录态过期只能**重新登录**再取一份。
//! ```
//!
//! ── 缓存/联调的现实（如实声明）────────────────────────────────
//! 本家的算法（`pow.rs`）有官方的自检向量，可离线证明正确；但**整条链路**
//! （领题 → 求解 → 转发）没有真实 `userToken` 就无法联调。因此本模块的线上
//! 探活是 **best-effort 且默认「判不出来就放行」**：拿到 401/403 才判无效，
//! 网络失败 / 5xx 一律按「暂时判不出来」通过 —— 否则一次上游抖动就会让用户
//! 加不进账号（与 Command Code 的 `probe_key` 同一取舍）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::Value;

use crate::server::errors::GatewayError;

use super::endpoints;

/// 输入长度上限（userToken 是 JWT 形态，几千字符；超过这个长度一定粘错了）
const MAX_TOKEN_LENGTH: usize = 16384;

/// 形态检查：非空、长度合理、**不含空白**（JWT 里没有空白；有空白说明
/// 粘进来的是整行 `localStorage` 输出或多段文本）。
///
/// 刻意**不**校验「是不是三段式 JWT」：上游并未承诺 token 形态恒为 JWT，
/// 把「必须 a.b.c」写死会在上游换形态时误拒合法凭证。宁可放过、让上游 401。
pub fn shape_ok(token: &str) -> bool {
    let trimmed = token.trim();
    !trimmed.is_empty()
        && trimmed.chars().count() <= MAX_TOKEN_LENGTH
        && !trimmed.chars().any(char::is_whitespace)
}

/// 展示用的脱敏形态（只露尾 6 位；日志与报错文案都用它）
pub fn masked(token: &str) -> String {
    let chars: Vec<char> = token.trim().chars().collect();
    if chars.len() <= 8 {
        return "userToken…".to_string();
    }
    let tail: String = chars[chars.len() - 6..].iter().collect();
    format!("userToken…{tail}")
}

/// 一次转发用的凭证快照
#[derive(Clone, Debug)]
pub struct DeepSeekCredentials {
    /// 账号记录 id
    pub id: String,
    /// 网页版 userToken（明文）
    pub user_token: String,
    /// 展示名
    pub name: String,
}

/// 从一条取值链里挑第一个非空字符串（记录可能被手工编辑过，宽容读取）
fn pick(record: &Value, keys: &[&str]) -> String {
    for key in keys {
        let value = record
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if !value.is_empty() {
            return value.to_string();
        }
    }
    String::new()
}

/// 从账号记录解析凭证（`record` 为 None 时报「没有可用账号」）。
///
/// 字段兼容链：`accessToken`（本项目落盘名，与别家统一）/ `userToken` /
/// `token`。形态不合格时给**可执行**的报错（去哪儿重新取 token）。
pub fn from_record(record: Option<&Value>) -> Result<DeepSeekCredentials, GatewayError> {
    let Some(record) = record else {
        return Err(GatewayError::with_status(
            401,
            "没有可用的 DeepSeek 网页版账号：请在账号页粘贴 userToken 添加",
        ));
    };
    let user_token = pick(record, &["accessToken", "userToken", "token"]);
    if user_token.is_empty() {
        return Err(GatewayError::with_status(
            401,
            "DeepSeek 网页版账号缺少 userToken，请重新粘贴",
        ));
    }
    if !shape_ok(&user_token) {
        return Err(GatewayError::with_status(
            401,
            format!(
                "DeepSeek userToken 形态不对（当前 {}）：应粘贴 localStorage 里 \
                 userToken 的 value（不是 Cookie，也不含空白）",
                masked(&user_token)
            ),
        ));
    }
    Ok(DeepSeekCredentials {
        id: pick(record, &["id"]),
        user_token,
        name: pick(record, &["name"]),
    })
}

/// 线上探活的结果（三档，与 Command Code 同一口径）
#[derive(Clone, Debug)]
pub enum KeyCheck {
    /// 上游认这个 token（2xx）
    Valid,
    /// 上游明确拒绝（401/403）—— token 无效 / 登录态已过期
    Invalid(String),
    /// 这次判不出来（网络失败 / 其他状态码）—— **按可用放行**
    Unknown(String),
}

/// 探活超时（只顺手问一句，不该拖住添加流程）
const PROBE_TIMEOUT_MS: u64 = 10_000;

/// 探活：`POST /api/v0/chat/create_pow_challenge`。
///
/// ── 为什么用「领题」这个端点而不是别的 ────────────────────────
///   - 它**只读、零副作用**：既不建会话也不消耗生成额度（对比：拿
///     `chat/completion` 探活会真的发一次提问）；
///   - 它是**认证必过**的端点：不带 token 会稳定返回 `{"code":40002,
///     "msg":"Missing Token"}`，带了无效 token 则 401 —— 判据干净；
///   - 它同时也是**转发路径的第一步**，探活成功等价于「这条链路的第一步
///     在你这个 token 下是通的」，比拿一个无关端点探更有意义。
///
/// 注意**不做 PoW 求解**：领题本身不需要解题，只要题面回来就说明认证过了。
pub async fn probe_token(user_token: &str) -> KeyCheck {
    let mut headers = endpoints::base_headers();
    headers.extend(endpoints::auth_headers(user_token));
    let body = serde_json::json!({ "target_path": endpoints::COMPLETION_PATH });
    let result = crate::server::core::auth_http::send_raw(
        "POST",
        &endpoints::url(endpoints::POW_CHALLENGE_PATH),
        Some(&body),
        &headers,
        None,
        Some(PROBE_TIMEOUT_MS),
    )
    .await;
    match result {
        Ok(response) => {
            if response.status == 401 || response.status == 403 {
                KeyCheck::Invalid(format!(
                    "上游返回 HTTP {}（登录态可能已过期，请重新登录后取新的 userToken）",
                    response.status
                ))
            } else if response.ok {
                // 题面回来了 = 认证通过（不需要解题）
                KeyCheck::Valid
            } else {
                KeyCheck::Unknown(format!("上游返回 HTTP {}", response.status))
            }
        }
        Err(error) => KeyCheck::Unknown(format!("探活请求失败：{error}")),
    }
}

/// 归一化粘贴的凭证（用户从 DevTools 复制的形态很杂）。
///
/// 认得的形态：
/// ```text
///   裸 token
///   带成对引号（JSON 里复制过来的 "eyJ…"）
///   `userToken: eyJ…` / `"value":"eyJ…"`（整行 localStorage 输出）
///   整串 JSON：{"value":"eyJ…","__version":"0"}
/// ```
/// 先尝试按 JSON 解析取 `.value`（这是浏览器里 `userToken` 对象的真实形态），
/// 失败再退回「剥引号 + 剥常见前缀」。
pub fn normalize_token(raw: &str) -> String {
    let trimmed = raw.trim();
    // 形态 1：整串 JSON（含 value 字段）
    if trimmed.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            if let Some(token) = value.get("value").and_then(Value::as_str) {
                let token = token.trim();
                if !token.is_empty() {
                    return token.to_string();
                }
            }
        }
    }
    // 形态 2：剥引号与常见「键名:」前缀
    let stripped = trimmed.trim_matches(|ch| matches!(ch, '"' | '\'' | '`' | ',' | ';' | '='));
    let stripped = stripped.trim();
    for prefix in ["userToken", "user_token", "token", "value"] {
        if let Some(rest) = stripped.strip_prefix(prefix) {
            let rest = rest.trim_start_matches(|ch| matches!(ch, ':' | '=' | ' ' | '"' | '\''));
            if !rest.is_empty() {
                return rest.trim().to_string();
            }
        }
    }
    stripped.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_rejects_whitespace_and_empty() {
        assert!(shape_ok("eyJhbGciOi.payload.sig"));
        assert!(!shape_ok(""));
        assert!(!shape_ok("   "));
        assert!(!shape_ok("eyJ... eyJ...")); // 含空白 = 粘错了
    }

    #[test]
    fn masked_keeps_tail_only() {
        let masked_value = masked("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.tail");
        assert!(masked_value.starts_with("userToken…"));
        assert!(masked_value.ends_with("9.tail"));
        assert!(!masked_value.contains("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"));
    }

    #[test]
    fn normalize_accepts_localstorage_json() {
        // 浏览器里 copy(JSON.parse(localStorage.getItem('userToken')).value)
        // 得到的是裸 token；但用户常直接复制整串
        let raw = r#"{"value":"eyJhbGciOiJIUzI1NiJ9.abc.def","__version":"0"}"#;
        assert_eq!(normalize_token(raw), "eyJhbGciOiJIUzI1NiJ9.abc.def");
    }

    #[test]
    fn normalize_strips_quotes_and_key_prefix() {
        assert_eq!(normalize_token(r#""eyJ.abc.def""#), "eyJ.abc.def");
        assert_eq!(normalize_token("userToken: eyJ.abc.def"), "eyJ.abc.def");
        assert_eq!(normalize_token("  eyJ.abc.def  "), "eyJ.abc.def");
    }

    #[test]
    fn from_record_reads_project_field_name() {
        let record = serde_json::json!({
            "id": "deepseek-web-abc",
            "provider": "deepseek-web",
            "name": "DeepSeek 网页",
            "accessToken": "eyJ.payload.sig"
        });
        let creds = from_record(Some(&record)).expect("应能解析");
        assert_eq!(creds.user_token, "eyJ.payload.sig");
        assert_eq!(creds.id, "deepseek-web-abc");
    }

    #[test]
    fn from_record_errors_are_actionable() {
        let empty = serde_json::json!({ "id": "x" });
        let error = from_record(Some(&empty)).expect_err("没有 token 应报错");
        assert!(error.message.contains("userToken"));
    }
}