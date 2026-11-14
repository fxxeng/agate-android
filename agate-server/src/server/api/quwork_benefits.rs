//! QuWork 权益的管理端点：月度赠送（只读）+ 两种兑换（订单号 / 邀请码）。
//!
//! ```text
//!   GET  /api/accounts/{id}/quwork/benefits   月度赠送列表（只读）
//!   POST /api/accounts/{id}/quwork/redeem     { orderId } 或 { inviteCode }
//! ```
//!
//! ── 三条纪律 ────────────────────────────────────────────────
//!   1. **查询与写分开**：`GET` 只读，动账号权益一律 `POST`；
//!   2. **不虚报结果**：兑换只认上游 `success`；成功文案是「上游确认兑换成功」，
//!      不写「已到账 N 积分」——积分是另一路回读的当前值（`pointsAfter`）；
//!   3. **读不出就说读不出**：月度赠送读失败仍然 200，带 `ok:false` 与 `error`，
//!      界面写「暂不可用」，不拿空表冒充「没有赠送」。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic。

use axum::body::Bytes;
use axum::response::Response;
use serde_json::json;

use crate::server::core::providers::quwork::{benefits, credentials};
use crate::server::errors::management_error;
use crate::server::http::{ok_json, parse_body};
use crate::server::ServerState;

/// 这个账号是不是 QuWork（不是就 404，避免拿别家账号 id 去调 QuWork 协议）
fn is_quwork(state: &ServerState, account_id: &str) -> bool {
    state
        .store()
        .quwork_account_record(account_id)
        .is_some()
}

/// `GET /api/accounts/{id}/quwork/benefits`
pub async fn monthly(state: &ServerState, account_id: &str) -> Response {
    if !is_quwork(state, account_id) {
        return management_error(404, "没有这个 QuWork 账号");
    }
    match benefits::monthly(state.store(), account_id).await {
        Ok(value) => ok_json(json!({
            "ok": true,
            "available": value.get("available").cloned().unwrap_or(json!(true)),
            "count": value.get("count").cloned().unwrap_or(json!(0)),
            "items": value.get("items").cloned().unwrap_or(json!([])),
        })),
        // 只读查询：失败也是「这次读不到」，界面如实写暂不可用
        Err(error) => ok_json(json!({
            "ok": false,
            "available": false,
            "count": 0,
            "items": [],
            "upstreamStatus": error.status_code,
            "error": error.message,
        })),
    }
}

/// `POST /api/accounts/{id}/quwork/redeem`
///
/// 请求体两选一：`{ "orderId": "16 位数字" }` 或 `{ "inviteCode": "..." }`
/// （也认驼峰/下划线两种写法；两个都填 → 400，不猜）。
pub async fn redeem(state: &ServerState, account_id: &str, body: &Bytes) -> Response {
    if !is_quwork(state, account_id) {
        return management_error(404, "没有这个 QuWork 账号");
    }
    let payload = match parse_body(body) {
        Ok(value) => value,
        Err(_) => return management_error(400, "上传内容不是有效 JSON"),
    };
    let input = match benefits::parse_redeem_input(&payload) {
        Ok(input) => input,
        Err(error) => return management_error(error.status_code, error.message),
    };
    // 只有登录凭证才能兑换：一次网络都不发，直接说清缺什么
    match crate::server::core::providers::quwork::refresh::snapshot(state.store(), account_id).await {
        Ok(secret) => {
            if credentials::login_token_of(&secret).is_empty() {
                return management_error(
                    400,
                    "该 QuWork 账号只有模型凭证（llmToken），兑换需要登录凭证（accessToken）：\
                     请重新导入桌面客户端的登录库，或粘贴含 accessToken 的凭证 JSON",
                );
            }
        }
        Err(error) => return management_error(error.status_code, error.message),
    }
    match benefits::redeem(state.store(), account_id, &input).await {
        Ok(value) => ok_json(value),
        Err(error) => management_error(error.status_code, error.message),
    }
}
