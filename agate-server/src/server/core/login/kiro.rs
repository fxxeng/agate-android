//! Kiro 的登录编排：起本机回调服务 → 让用户在系统浏览器里登录 → 落账号。
//!
//! ── 这条链与另外十家都不一样 ────────────────────────────────
//! 它是唯一一家**必须**把用户送到系统浏览器、同时又要自己接回调的：
//!   * 内嵌 WebView 不行 —— Google 与 GitHub 对 WebView 内的 OAuth 直接回
//!     `disallowed_useragent`（Accio 那次已经踩过同一条）；
//!   * 回调也不能落到网关端口上 —— 门户把 `redirect_uri` 与官方 IDE 注册过的
//!     那组端口比对，白名单外根本不跳回来。
//! 所以授权地址是**本进程现造**的，端口是现抢的，回调由本进程自己接
//! （见 `providers::kiro::login`）。
//!
//! ── 为什么授权地址可以同步返回 ──────────────────────────────
//! Trae / ZCode 那几家要等一次上游（guidance / init）才有地址，所以 `/start`
//! 里得用 `wait_for_auth_url` 等它落地。这里地址全由本机算出来（端口 + state +
//! PKCE），`begin()` 一返回就能把地址给出去 —— 客户端拿到就开始开浏览器，
//! 少一次等待与一处超时口径。
//!
//! ── 端口什么时候还 ──────────────────────────────────────────
//! 四条收尾路径（完成 / 失败 / 超时 / 取消）都要显式 `close()`：会话被 `Arc`
//! 共享，只靠 `Drop` 会拖到超时那一步，而白名单端口只有 10 条 —— 用户"取消后
//! 立刻重试"很容易把它们占满（这与 AutoClaw 那次「取消后端口被自己占着」是
//! 同一类）。因此会话在发起时就登记进 `LoginService::kiro_login`，`cancel(state)`
//! 会按 state 摘掉自己那一条并关端口。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::server::core::providers::kiro::credentials::KiroCredentials;
use crate::server::core::providers::kiro::login as kiro_login;
use crate::server::core::providers::{kind_id, ProviderKind};
use crate::server::core::endpoints::{resolve_edition, DEFAULT_EDITION};
use crate::server::logging;

use super::{finish_task_error, random_hex, LoginService, LoginTaskHandle, LOGIN_TIMEOUT_MS};

impl LoginService {
    /// 发起一次 Kiro 浏览器登录。
    ///
    /// 同步部分只做「抢端口 + 建任务 + 登记」：地址是本地算的，所以返回时
    /// `authUrl` 已经就绪，客户端可以直接开系统浏览器。
    pub async fn start_kiro_login(&self) -> Result<LoginTaskHandle, String> {
        // 登录发生在「还没有账号」的时候，没有账号级代理可解析（与另外几家
        // 登录链同一处置：走直连）
        let session = Arc::new(kiro_login::Session::begin(None).await?);
        let state = random_hex()?;
        let info = resolve_edition(Some(DEFAULT_EDITION));
        let handle = self.new_handle_for_provider(info, kind_id(ProviderKind::Kiro));
        handle.update(|task| {
            task.state = Some(state.clone());
            task.auth_url = Some(session.auth_url.clone());
        });
        self.tasks.register(&state, handle.clone());
        // 登记进待办表：取消路径要靠它找到这一轮并立刻还端口（白名单只有
        // 10 条，"取消后立刻重试"是最容易连点的动作）
        self.put_kiro_login(&state, session.clone());
        logging::log(
            "[Login]",
            &format!(
                "发起 Kiro 浏览器登录（回调端口 {}，等待系统浏览器…）",
                session.port
            ),
        );

        let service = self.clone();
        let wait = handle.clone();
        crate::spawn_task(async move {
            // 超时由这里兜：门户最长等多久不由我们定，但界面上的"登录中"是
            // 按 `LOGIN_TIMEOUT_MS` 走的 —— 两头必须同一个口径
            let outcome = tokio::time::timeout(
                Duration::from_millis(LOGIN_TIMEOUT_MS),
                session.complete(),
            )
            .await;
            // 无论成没成，先摘表再 close：端口当场还回去（摘表在前是为了让
            // 取消路径不会拿到一个已经收掉的会话）
            service.take_kiro_login(&state);
            session.close();
            match outcome {
                Ok(Ok(credentials)) => {
                    if let Err(error) = service.finish_kiro_login(&wait, credentials).await {
                        logging::log("[Login]", &format!("❌ Kiro 登录收尾失败: {error}"));
                    }
                }
                // 门户 / AWS 那边明说的失败（用户取消、授权码无效）原样透出；
                // 但用户已经在面板上点了取消时不再补一条错误 —— 否则两条路径
                // 会给出两个互相矛盾的结论（与 `login::trae` 同一条）
                Ok(Err(message)) => {
                    if !wait.snapshot().canceled {
                        finish_task_error(&wait, &message);
                    }
                }
                // 到点没等到回调：如实说超时，不说成功
                Err(_) => {
                    if !wait.snapshot().canceled {
                        finish_task_error(&wait, "Kiro 登录超时（5 分钟），请重新发起");
                    }
                }
            }
        });
        Ok(handle)
    }

    /// 登录收尾：把凭证落成账号并标记任务完成。
    ///
    /// 持锁后再判一次 `done`/`canceled`：等待期间用户可能已经取消，那时不该
    /// 再往账号库里塞一条记录（与 AStudio / CatPaw 同款裁决）。
    async fn finish_kiro_login(
        &self,
        handle: &LoginTaskHandle,
        credentials: KiroCredentials,
    ) -> Result<(), String> {
        let payload = kiro_login::payload_of(&credentials);
        let name = kiro_login::display_name(&credentials);
        // `source` 记的是「怎么加的」：浏览器登录与粘贴在账号列表里要能分开
        let source = if credentials.method.is_empty() {
            "web"
        } else {
            credentials.method.as_str()
        };
        let mut task = handle.lock();
        if task.done || task.canceled {
            return Ok(());
        }
        // 落账号走**既有**的添加路径：校验、id 生成、优先级分配、公开形态
        // 全在 `add_kiro_account` 里，不另写一份 —— 否则「登录加的账号」与
        // 「粘贴加的账号」会在某条规则上慢慢分叉
        match self.store.add_kiro_account(&payload, Some(&name), source) {
            Ok(account) => {
                task.session = Some(json!({
                    "accountUid": account.get("id"),
                    "nickname": account.get("name"),
                    "provider": kind_id(ProviderKind::Kiro),
                }));
                task.done = true;
                task.finished_at = Some(logging::now_ms());
                logging::log(
                    "[Login]",
                    &format!("✅ Kiro 登录完成，账号已加入列表（{name}）"),
                );
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 超时口径：这条链用的是全项目共用的 5 分钟，不是上游的 10 分钟
    ///
    /// 单独给一家延长会让界面上的"登录中"计时与轮询节奏出现两种口径，
    /// 而 `/wait` 是同一个协议（与 `login::trae` 同一条理由）。
    #[test]
    fn the_login_timeout_is_the_shared_one() {
        assert_eq!(LOGIN_TIMEOUT_MS, 5 * 60 * 1000);
    }

    /// 落库的 `source` 要如实反映登录方式（`social` / `idc`），不是笼统一句
    #[test]
    fn the_source_follows_the_login_method() {
        let social = KiroCredentials {
            method: "social".to_string(),
            access: "at".to_string(),
            profile: "arn:aws:codewhisperer:us-east-1:1:profile/X".to_string(),
            account_id: "a@b.com".to_string(),
            ..Default::default()
        };
        let payload = kiro_login::payload_of(&social);
        assert_eq!(payload["method"], json!("social"));
        assert_eq!(
            kiro_login::display_name(&social),
            "a@b.com",
            "没有登录方式时不加括号"
        );
    }
}
