//! 小浣熊桌面端身份头（对齐上游 3.4.1 的 `raccoon/headers.rs`）。
//!
//! ── 为什么单独一层 ──────────────────────────────────────────
//! 对话、余额、签到、新手任务各自拼一份头，以前只保证「token 在」就能发。
//! 上游把官方桌面端的身份头当风控基线：缺 `X-Client-*` / `X-Request-Id`
//! 时请求可能被静默降级，而日志里只看到「上游没回内容」。这些头不该由
//! 四处各写一遍，收在这里 + `balance::desktop_client_headers()` 一处取值。
//!
//! ── 刻意不搬的两件桌面专属东西 ──────────────────────────────
//!   · 上游 `append_external()` 读 `/var/lib/agent2api/raccoon-headers.json`
//!     覆盖版本号 —— Android 没有这个路径，也没有「运维改文件调协议」的场景；
//!   · 上游 `desktop_client_headers()` 按 `cfg!(target_os)` 报 windows/macos/
//!     linux —— 本项目跑在 Android 上，照抄会得到 `desktop-linux`。这个头
//!     表达的是**被伪装的官方客户端**，不是宿主 OS，所以沿用本项目既有的
//!     `desktop-windows`（见 `balance.rs` 的说明）。

use serde_json::Value;

use crate::server::errors::GatewayError;
use crate::server::logging;

/// 组织身份在账号记录里的键（与 `oauth.rs` 落库、`add_raccoon_account`
/// 的取值链同一套；兼容上游返回的驼峰写法）。
fn org_code(account: &Value) -> String {
    let pick = |value: &Value| -> Option<String> {
        value
            .get("office_identity")
            .or_else(|| value.get("officeIdentity"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    pick(account)
        .or_else(|| account.get("auth").and_then(pick))
        .unwrap_or_default()
}

/// 组织身份头：个人账号不发；值必须是可见 ASCII（脏值会让整条请求失败，
/// 而不是只丢这一个头，所以要在拼头之前挡掉）。
fn append_org(headers: &mut Vec<(String, String)>, account: &Value) -> Result<(), GatewayError> {
    let code = org_code(account);
    if code.is_empty() || code.eq_ignore_ascii_case("personal") {
        return Ok(());
    }
    if !code.is_ascii() || code.bytes().any(|byte| byte < 32 || byte == 127) {
        return Err(GatewayError::with_status(
            400,
            "小浣熊组织身份无效，请重新导入登录态",
        ));
    }
    headers.push(("X-Org-Code".to_string(), code));
    Ok(())
}

/// 每条对话请求一个唯一 id，形态与官方客户端一致：
/// `relay_js_<毫秒>_<8 位 hex>`。
///
/// 随机段复用项目里唯一的不可预测随机源（`upstream::request::new_request_id`，
/// UUID v4），只取它的 hex 字符 —— 不引第二个随机实现，也不在这里生成
/// 可能失败的随机字节（release 是 `panic=abort`，零 unwrap/expect/panic）。
fn request_id() -> String {
    let hex: String = crate::server::core::upstream::request::new_request_id()
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(8)
        .collect();
    format!("relay_js_{}_{hex}", logging::now_ms())
}

/// 给**对话请求**补齐全套身份头（调用于 `mod.rs::build_chat_request`）。
///
/// 顺序与上游一致：组织身份 → 桌面端三头 → 请求 id。
/// 桌面端三头直接复用 `balance::desktop_client_headers()`：签到、新手任务、
/// 余额那三处立刻同步拿到 `X-Client-Name` 与新版本号，不需要各改一遍。
pub(super) fn append_identity(
    headers: &mut Vec<(String, String)>,
    account: &Value,
) -> Result<(), GatewayError> {
    append_org(headers, account)?;
    headers.extend(super::balance::desktop_client_headers());
    headers.push(("X-Request-Id".to_string(), request_id()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn has(headers: &[(String, String)], name: &str) -> Option<String> {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    }

    /// 桌面三头齐全、版本与官方一致；与个人身份共存的头不能重复
    #[test]
    fn desktop_headers_are_complete_and_shared() {
        let mut headers = vec![("Authorization".to_string(), "Bearer x".to_string())];
        append_identity(&mut headers, &json!({})).expect("个人账号应当能补全");
        assert_eq!(has(&headers, "X-Client-Name").as_deref(), Some("raccoon-work"));
        assert_eq!(
            has(&headers, "X-Client-Platform").as_deref(),
            Some("desktop-windows")
        );
        assert_eq!(has(&headers, "X-Client-Version").as_deref(), Some("v1.1.5"));
        assert!(!headers.iter().any(|(key, _)| key == "X-Org-Code"));
        assert_eq!(headers.iter().filter(|(key, _)| key == "Authorization").count(), 1);
    }

    /// 组织账号要把 X-Org-Code 带上；驼峰与 auth 子对象两种存法都认
    #[test]
    fn org_code_is_read_from_either_shape() {
        let mut headers = Vec::new();
        append_identity(&mut headers, &json!({ "office_identity": "org-42" }))
            .expect("组织账号");
        assert_eq!(has(&headers, "X-Org-Code").as_deref(), Some("org-42"));

        let mut nested = Vec::new();
        append_identity(
            &mut nested,
            &json!({ "auth": { "officeIdentity": "org-77" } }),
        )
        .expect("auth 子对象里的驼峰键");
        assert_eq!(has(&headers, "X-Org-Code").as_deref(), Some("org-42"));
        assert_eq!(has(&nested, "X-Org-Code").as_deref(), Some("org-77"));
    }

    /// 个人身份不发明一个空组织头
    #[test]
    fn personal_identity_sends_no_org_header() {
        let mut headers = Vec::new();
        append_identity(&mut headers, &json!({ "office_identity": "personal" }))
            .expect("个人身份");
        assert!(has(&headers, "X-Org-Code").is_none());
    }

    /// 脏值在拼头之前挡掉：让请求整体失败比发一条非法头更安全
    #[test]
    fn unsafe_org_code_is_rejected() {
        let mut headers = Vec::new();
        let err = append_identity(&mut headers, &json!({ "office_identity": "组织\n42" }));
        assert!(err.is_err());
        assert!(!headers.iter().any(|(key, _)| key == "X-Org-Code"));
    }

    /// 形态与官方一致，且每条请求都不同
    #[test]
    fn request_id_has_the_official_shape_and_is_unique() {
        let first = request_id();
        let second = request_id();
        assert!(first.starts_with("relay_js_"), "{first}");
        let tail = first.rsplit('_').next().unwrap_or("");
        assert_eq!(tail.len(), 8, "{first}");
        assert!(tail.chars().all(|c| c.is_ascii_hexdigit()), "{first}");
        assert_ne!(first, second, "同一毫秒内也必须不同");
    }
}
