//! `wps_sid` 凭证：粘贴三形态归一 + 设备 ID 稳定派生。
//!
//! 灵犀**没有 OAuth、没有 refresh 接口**（见方案 L1）。所以这里只做「把用户
//! 粘进来的东西变成一条可落库的记录」，不做任何续期 / 有效期推算：
//! 失效的唯一判据是上游 401（`refresh.rs` 的探活），不是时间。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic。

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::endpoints;

/// 归一化后的凭证。`cookie` 是整段原始 Cookie（存活更久），没有就为 None。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Credential {
    pub wps_sid: String,
    pub cookie: Option<String>,
    pub uid: Option<String>,
}

fn sid_value(text: &str) -> Option<&str> {
    let value = text.trim();
    if !value.starts_with("V02S") || value.len() < 8 {
        return None;
    }
    Some(value)
}

/// 从一段 Cookie 里挑出某个键（分号分隔，忽略空白与大小写）。
fn cookie_value<'a>(raw: &'a str, key: &str) -> Option<&'a str> {
    raw.split(';').find_map(|part| {
        let mut it = part.trim().splitn(2, '=');
        let name = it.next().unwrap_or("").trim();
        let value = it.next().unwrap_or("").trim();
        (name.eq_ignore_ascii_case(key) && !value.is_empty()).then_some(value)
    })
}

/// 接受三种形态（与 `lingxi2api-panel` README 一致）：
///   1. 裸 sid：`V02Sxxxxxxxx…`
///   2. 单键：`wps_sid=V02S…`
///   3. 整段 Cookie：`wps_sid=V02S…; wpsua=…; uid=…`
///
/// 空行与 `#` 开头的行由调用方跳过；这里收到的是**单行**。
pub fn normalize_paste(line: &str) -> Result<Credential, String> {
    let raw = line.trim();
    if raw.is_empty() {
        return Err("凭证为空".to_string());
    }
    let full_cookie = raw.contains(';') || raw.contains("wps_sid=");
    let sid = if full_cookie {
        cookie_value(raw, "wps_sid")
            .and_then(sid_value)
            .ok_or_else(|| "整段 Cookie 里没有有效的 wps_sid（V02S 前缀）".to_string())?
    } else {
        sid_value(raw).ok_or_else(|| "不是有效的 wps_sid（需要 V02S 前缀）".to_string())?
    };
    Ok(Credential {
        wps_sid: sid.to_string(),
        cookie: full_cookie.then(|| raw.to_string()),
        uid: full_cookie
            .then(|| cookie_value(raw, "uid").map(str::to_string))
            .flatten(),
    })
}

/// 上游按 device 维度管登录态：**同一账号永远同一个设备、不同账号不同设备**。
/// 参考实现用 `sha256("lingxi2api:" + uid)`；本仓按同一口径派生，避免随机值
/// 被上游当成新设备、也避免多个账号共用一个设备互相顶会话。
pub fn device_id(uid: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"lingxi2api:");
    hasher.update(uid.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest.iter() {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 账号记录的公开形态（不落 Cookie 明文到响应里，只留尾巴用于辨认）。
pub fn public_view(credential: &Credential) -> Value {
    let tail: String = credential.wps_sid.chars().rev().take(6).collect();
    json!({
        "provider": endpoints::ID,
        "sidTail": tail.chars().rev().collect::<String>(),
        "uid": credential.uid,
        "hasFullCookie": credential.cookie.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_sid_is_accepted() {
        let c = normalize_paste("V02Sabcdefghijklmnop").unwrap_or_default();
        assert_eq!(c.wps_sid, "V02Sabcdefghijklmnop");
        assert_eq!(c.cookie, None);
        assert_eq!(c.uid, None);
    }

    #[test]
    fn single_key_form_is_accepted() {
        let c = normalize_paste("wps_sid=V02Sabcdefghijklmnop").unwrap_or_default();
        assert_eq!(c.wps_sid, "V02Sabcdefghijklmnop");
        assert!(c.cookie.is_some());
    }

    #[test]
    fn full_cookie_keeps_the_whole_string_and_uid() {
        let c = normalize_paste("csrf=x; uid=1304453904; wps_sid=V02Sabcdefghij; wpsua=yyy")
            .unwrap_or_default();
        assert_eq!(c.wps_sid, "V02Sabcdefghij");
        assert_eq!(c.uid.as_deref(), Some("1304453904"));
        assert!(c.cookie.as_deref().unwrap_or("").contains("wpsua=yyy"));
    }

    #[test]
    fn malformed_input_is_rejected_not_silently_stored() {
        assert!(normalize_paste("").is_err());
        assert!(normalize_paste("abc=def").is_err());
        assert!(normalize_paste("wps_sid=short").is_err());
        assert!(normalize_paste("wps_sid=NOTV02; uid=1").is_err());
    }

    #[test]
    fn device_id_is_stable_per_account_and_distinct_across_accounts() {
        let a = device_id("1304453904");
        assert_eq!(a, device_id("1304453904"));
        assert_ne!(a, device_id("1304453905"));
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn public_view_never_contains_the_full_sid() {
        let c = Credential {
            wps_sid: "V02Sabcdefghijklmnop".to_string(),
            cookie: None,
            uid: None,
        };
        let view = public_view(&c).to_string();
        assert!(!view.contains("V02Sabcdefghijklmnop"));
        assert!(view.contains("sidTail"));
    }
}
