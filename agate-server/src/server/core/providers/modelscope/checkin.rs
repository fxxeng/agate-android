//! ModelScope 每日签到：**读一次登录信息就完成**。
//!
//! ── 上游是什么 ──────────────────────────────────────────────
//! 魔搭**没有独立的签到接口**（官方网页那个「每日签到」按钮背后就是这条）：
//! ```text
//! GET {网页站}/api/v1/users/login/info          Cookie: m_session_id=<ms-令牌>
//!   → {"Code":200,"Success":true,"Data":{"Name":"…","HavanaId":"…"}, …}
//! ```
//! **调用这个端点本身就触发当日的魔粒签到**，返回体给的是账号身份，
//! 而不是「签到成功」四个字。
//!
//! ── 幂等 ────────────────────────────────────────────────────
//! 一天内调多次是安全的（上游没有「今天已签过」的拒绝返回）。因此「今天已经
//! 触发过」**不算失败** —— 如实报成功并说明是重复调用。
//!
//! ── 两个地区打各自的站 ──────────────────────────────────────
//! `region` 决定站点。打错了会拿到 `401 access forbidden`，这里把它如实翻成
//! 「令牌不属于该站点」而不是笼统的失败。
//!
//! ── 返回值形状 ──────────────────────────────────────────────
//! 与 `billing::checkin` 的其余家一致：`{success, msg}`。**不给 `already` 那一档**：
//! 上游根本不区分「第一次」与「第 N 次」，硬造一个「已签到」会让台账记下一条
//! 我们并没有证据的判断（语义诚实优先）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::account_store::AccountStore;

use super::credentials;
use super::{client, region::Region};

/// 登录信息路径（相对网页站）。**它就是签到**，见模块头。
///
/// `pub(super)`：`probe` 的探活走同一条链路 —— 两处各写一份路径，
/// 上游改路径时必然漏一处。
pub(super) const LOGIN_INFO_PATH: &str = "/api/v1/users/login/info";

/// 按账号 id 签到（`billing::checkin` 的分派入口：那一层手里只有公开形态的
/// 账号对象，没有令牌 —— 令牌必须由这里按 id 从账号库取）。
pub async fn claim_for_account(
    store: &AccountStore,
    region: Region,
    account_id: &str,
) -> Result<Value, String> {
    let record = store
        .modelscope_account_record(region, account_id)
        .ok_or_else(|| {
            format!(
                "找不到 ModelScope {}账号 {account_id}（可能已被删除）",
                region.label()
            )
        })?;
    let token = credentials::token_of(&record)?;
    claim(region, &token).await
}

/// 每日签到。
pub async fn claim(region: Region, token: &str) -> Result<Value, String> {
    let (status, payload) = client::site_get(region, LOGIN_INFO_PATH, token, "每日签到").await?;
    if status == 401 || status == 403 {
        // 令牌失效 / 打错站点：如实报（上层按 TokenExpired 处置）
        return Err(client::expired_message(region, status, true));
    }
    if !(200..300).contains(&status) {
        return Ok(json!({
            "success": false,
            "msg": format!("每日签到返回 HTTP {status}"),
        }));
    }
    if !client::succeeded(&payload) {
        let message = client::message_of(&payload);
        let msg = if message.trim().is_empty() {
            format!("每日签到失败（上游 Code={}）", client::code_of(&payload))
        } else {
            format!("每日签到失败：{message}")
        };
        return Ok(json!({ "success": false, "msg": msg }));
    }

    // 成功时上游给的是账号身份（不是「签到成功」）—— 把显示名播报出来，
    // 用户一眼能确认「这条记录签的是哪个账号」（多账号场景下这是有用的信息）
    let data = client::data_of(&payload);
    let display = data
        .get("NickName")
        .or_else(|| data.get("Name"))
        .or_else(|| data.get("nickname"))
        .or_else(|| data.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("")
        .to_string();
    let msg = if display.is_empty() {
        format!("魔搭{}每日签到已触发", region.label())
    } else {
        format!("魔搭{}每日签到已触发（{display}）", region.label())
    };
    Ok(json!({ "success": true, "msg": msg }))
}
