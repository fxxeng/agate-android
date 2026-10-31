//! ModelScope 凭证：**只有一条来源** —— 账号记录里的 `ms-` 令牌。
//!
//! ── 为什么只要一个字段 ──────────────────────────────────────
//! 同一个 `ms-...` 令牌既作 `Authorization: Bearer` 调推理，又作
//! `Cookie: m_session_id=<key>` 调网页站（依据见 `region.rs` 的模块头）。
//!
//! ── 没有续期 ────────────────────────────────────────────────
//! `ms-` 令牌是**长期有效**的 API key，上游没有任何 refresh / renew 接口。
//! 所以 `supports_refresh()` 是 false，失效只能回魔搭控制台重新复制一个。
//!
//! ── 取值链为什么这么宽 ──────────────────────────────────────
//! `accessToken` 是本仓的落盘名；粘贴形态另外兼容 `apiKey` / `token` /
//! `session` —— 用户从别处复制的 JSON 里字段名并不统一，而这几串字面量
//! 不可能与「令牌本体」混淆（都以 `ms-` 开头）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::Value;

/// 令牌前缀（`ms-` 开头）。用于添加时的形态提示与脱敏，**不是**强校验：
/// 上游换前缀时宁可让 `probe` 去报 401，也不要在本地把用户挡在门外。
pub const TOKEN_PREFIX: &str = "ms-";

/// 从一条记录 / payload 里按取值链挑第一个非空字符串。
///
/// 记录可能被手工编辑过（少一个键、值是 null、值带空白），所以逐个容错读取。
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

/// 取值链（账号记录与上传 payload **共用这一条**）。
///
/// 两处分叉的话会出现「能探活成功却落不进账号」或反之 —— 那类静默问题最难查。
const KEYS: &[&str] = &["accessToken", "apiKey", "token", "session"];

/// 取令牌（**唯一入口**）。空 → 一句用户可行动的错误。
pub fn token_of(record: &Value) -> Result<String, String> {
    let token = pick(record, KEYS);
    if token.is_empty() {
        return Err("ModelScope 账号缺少令牌，请重新粘贴".to_string());
    }
    Ok(token)
}

/// 取令牌（添加路径专用：先探活再落账号，与 [`token_of`] 同一条取值链）
pub fn token_from_payload(payload: &Value) -> String {
    pick(payload, KEYS)
}

/// 令牌尾 4 字符（公开形态与默认备注名用）。
///
/// 按 `char` 取而不按字节切：粘贴框不做格式校验，一次带汉字的误粘贴就能让
/// 按字节回切落在多字节字符中间（`String` 会在那里 panic）。
pub fn token_tail(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    let start = chars.len().saturating_sub(4);
    chars[start..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 四条取值链在记录与 payload 上必须一致（分叉会造出「探活过、落不进账号」）
    #[test]
    fn token_chain_is_shared_by_record_and_payload() {
        for key in ["accessToken", "apiKey", "token", "session"] {
            let value = json!({ key: "ms-abc" });
            assert_eq!(token_of(&value).ok().as_deref(), Some("ms-abc"));
            assert_eq!(token_from_payload(&value), "ms-abc");
        }
    }

    /// 尾号按字符取：多字节输入不许 panic
    #[test]
    fn tail_survives_multibyte_input() {
        assert_eq!(token_tail("ms-中文结尾"), "文结尾");
        assert_eq!(token_tail("ab"), "ab");
    }
}
