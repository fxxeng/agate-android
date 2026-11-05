//! AStudio 的登录编排：起本机登录页 → 等页面把会话送回来 → 落账号。
//!
//! ── 这条链在十家登录里的位置 ────────────────────────────────
//! ```text
//!   workbuddy  上游推 authUrl → 轮询 auth/token
//!   小浣熊      本地拼授权地址 → 等壳侧窗口捕获的自定义协议回调
//!   Qoder      同步问上游要设备码 → 轮询设备令牌
//!   CatPaw     同步问上游要登录入口 → 等**上游 POST 到本机**
//!   Cline      同步问上游要设备码 → 轮询
//!   AutoClaw   前端过一次风控验证码 → 网关拿授权地址 → 等浏览器 302 到本机
//!   Accio      本地拼授权地址（PKCE）→ 等浏览器 302 到本机
//!   Loomy      手机号 + 验证码（两次同步请求，无窗口）
//!   Tabbit     手机号 + 验证码（同上）
//!   AStudio    **我们自己起一个本机登录页**（极验滑块只能在地页面里做）→
//!              页面完成后把会话通过 channel 交回进程内
//! ```
//! 与前九家都不同的地方：**没有回调 URL**。页面和网关在同一个进程里，页面
//! 处理登录的那几个 API 就是网关自己 —— 所以结果不必经过任何 HTTP 跳转，
//! 一条 `tokio::sync::mpsc` 就够（见 `providers::astudio::login_page`）。
//!
//! ── 为什么不走通用网页登录链（`start_web_login`）────────────
//! 那条链的契约是「`build_login_url` 给外站地址 + 等 `submit_login_callback`
//! 带 code 回来」。AStudio 两样都不成立：地址是我们自己起的回环页，回来的
//! 也不是 code 而是**已经换好的完整会话**（含 modelBearerToken）。硬套那条链
//! 只能在 `exchange_login_code` 里假装 code 是「票据编号」——多一层转译，
//! 而故障时出错的地方离真正的原因更远。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::json;

use crate::server::core::providers::astudio::{auth, login_page};
use crate::server::core::providers::{kind_id, ProviderKind};
use crate::server::core::endpoints::{resolve_edition, DEFAULT_EDITION};
use crate::server::logging;

use super::{finish_task_error, random_hex, LoginService, LoginTaskHandle};

impl LoginService {
    /// 发起一次 AStudio 本机登录（手机号验证码 / 账号密码 + 极验滑块）。
    ///
    /// 返回的任务句柄与其它家同形：`/wait` 按 `state` 轮询它，客户端据此知道
    /// 「登录完成 / 失败 / 还在等」。
    pub async fn start_astudio_login(&self) -> Result<LoginTaskHandle, String> {
        // 登录发生在「还没有账号」的时候，因此没有账号级代理可解析；
        // 与另外几家的登录链同一处置（都走直连）。
        let (auth_url, mut results, abort) = login_page::start(None).await?;
        let state = random_hex()?;
        let info = resolve_edition(Some(DEFAULT_EDITION));
        let handle = self.new_handle_for_provider(info, kind_id(ProviderKind::Astudio));
        handle.update(|task| {
            task.state = Some(state.clone());
            task.auth_url = Some(auth_url.clone());
        });
        self.tasks.register(&state, handle.clone());
        logging::log("[Login]", "发起 AStudio 本机登录页（手机号 / 密码 + 极验滑块）");

        let service = self.clone();
        let wait = handle.clone();
        crate::spawn_task(async move {
            let outcome = results.recv().await;
            // 无论成没成，端口立刻关：一次登录一个端口，不该留到超时那一步
            abort.abort();
            match outcome {
                Some(Ok(session)) => {
                    // 落账号失败的原因要落到任务上，客户端那句提示才有内容
                    if let Err(error) = service.finish_astudio_login(&wait, session).await {
                        logging::log("[Login]", &format!("❌ AStudio 登录收尾失败: {error}"));
                    }
                }
                // 页面自己报的错（验证码不对 / 密码不对 / 账号被封禁）原样透出
                Some(Err(message)) => finish_task_error(&wait, &message),
                // 端口被超时任务关掉 / 页面被放弃：如实说超时，不说成功
                None => finish_task_error(&wait, "AStudio 登录超时（5 分钟），请重新发起"),
            }
        });
        Ok(handle)
    }

    /// 登录收尾：把会话落成账号并标记任务完成。
    ///
    /// 持锁后再判一次 `done`/`canceled`：等待期间用户可能已经取消，
    /// 那时不该再往账号库里塞一条记录（与 CatPaw 的 `finish_*_with` 同款裁决）。
    async fn finish_astudio_login(
        &self,
        handle: &LoginTaskHandle,
        session: auth::Session,
    ) -> Result<(), String> {
        let payload = session_payload(&session);
        let mut task = handle.lock();
        if task.done || task.canceled {
            return Ok(());
        }
        // 落账号走**既有**的添加路径：校验、id 生成、优先级分配、公开形态
        // 全在 `add_astudio_account` 里，不另写一份 —— 否则「登录加的账号」与
        // 「粘贴加的账号」会在某条规则上慢慢分叉。
        match self
            .store
            .add_astudio_account(&payload, Some(&session.display_name()), &session.method)
        {
            Ok(account) => {
                task.session = Some(json!({
                    "accountUid": account.get("id"),
                    "nickname": account.get("name"),
                    "provider": kind_id(ProviderKind::Astudio),
                    // 只有模型凭据的账号不会有网页会话；这条登录链一定两套都有，
                    // 但仍按账号的真实状态回，不假设
                    "hasSession": account.get("hasSession"),
                }));
                task.done = true;
                task.finished_at = Some(logging::now_ms());
                logging::log("[Login]", "✅ AStudio 登录完成，账号已加入列表");
                Ok(())
            }
            Err(error) => {
                task.error = Some(error.message.clone());
                task.done = true;
                task.finished_at = Some(logging::now_ms());
                Err(error.message)
            }
        }
    }
}

/// 会话 → `add_astudio_account` 认得的字段对象。
///
/// 形状与「粘贴 `astron-session.json`」那条路**同一套**（`key` + `token` +
/// `accountId` + `ssoSessionId` + `uid`），因为 `Credentials::from_fields`
/// 是唯一判据。这里刻意不拼嵌套 `session{}`：那是落盘形态，由 store 负责。
fn session_payload(session: &auth::Session) -> serde_json::Value {
    let mut payload = json!({
        "key": session.model_bearer_token,
        "uid": session.uid,
        "token": session.token,
        "accountId": session.account_id,
    });
    if !session.sso_session_id.is_empty() {
        payload["ssoSessionId"] = json!(session.sso_session_id);
    }
    if !session.mobile.is_empty() {
        payload["phoneTail"] = json!(mask_mobile(&session.mobile));
    }
    payload
}

/// 手机号脱敏（`138****8000`）—— 备注名与日志都只露这个形态。
///
/// 先剥国家码再看位数：`+86 138-0013-8000` 过滤数字后有 13 位，不剥的话会被
/// 判成「不是 11 位」而原样返回 —— 那就把完整号码写进备注名里了，正好是这个
/// 函数要避免的事。
fn mask_mobile(mobile: &str) -> String {
    let digits: String = mobile.chars().filter(char::is_ascii_digit).collect();
    let digits = digits
        .strip_prefix("86")
        .filter(|rest| rest.len() == 11)
        .unwrap_or(&digits);
    let chars: Vec<char> = digits.chars().collect();
    if chars.len() != 11 {
        return mobile.to_string();
    }
    format!(
        "{}****{}",
        chars[..3].iter().collect::<String>(),
        chars[7..].iter().collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 会话载荷的字段名必须与粘贴路径同一套（`Credentials::from_fields` 认的）
    #[test]
    fn the_payload_uses_the_same_field_names_as_pasting() {
        let session = auth::Session {
            account_id: "acc".to_string(),
            uid: "u1".to_string(),
            token: "tok".to_string(),
            sso_session_id: "sso".to_string(),
            mobile: "13800138000".to_string(),
            model_bearer_token: "key".to_string(),
            method: "sms".to_string(),
            ..Default::default()
        };
        let payload = session_payload(&session);
        assert_eq!(payload["key"], json!("key"));
        assert_eq!(payload["token"], json!("tok"));
        assert_eq!(payload["accountId"], json!("acc"));
        assert_eq!(payload["ssoSessionId"], json!("sso"));
        assert_eq!(payload["uid"], json!("u1"));
        // 载荷必须能被唯一的判据接受
        let credentials = crate::server::core::providers::astudio::credentials::Credentials::from_fields(&payload)
            .expect("登录得到的载荷必须能被 Credentials 接受");
        assert!(credentials.has_session());
        assert_eq!(credentials.account_id, "acc");
    }

    /// 没有 ssoSessionId 时不写这个键（空串会让「取第一个非空」白跑）
    #[test]
    fn an_empty_sso_session_is_not_written() {
        let session = auth::Session {
            account_id: "acc".to_string(),
            token: "tok".to_string(),
            model_bearer_token: "key".to_string(),
            ..Default::default()
        };
        let payload = session_payload(&session);
        assert!(payload.get("ssoSessionId").is_none());
    }

    /// 手机号脱敏只露前三后四
    #[test]
    fn the_mobile_is_masked() {
        assert_eq!(mask_mobile("13800138000"), "138****8000");
        // 不是 11 位就原样返回（不猜、不截断）
        assert_eq!(mask_mobile("123"), "123");
        assert_eq!(mask_mobile("+86 138-0013-8000"), "138****8000");
    }
}
