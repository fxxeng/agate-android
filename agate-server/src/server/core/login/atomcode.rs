//! AtomCode 的登录任务：**服务端中介的轮询**（与 ZCode / OfficeAce 同一形态）。
//!
//! ── 这条链属于哪一型 ────────────────────────────────────────
//! **轮询型**，不是回调型。授权回调由登录代理自己在 `acs.atomgit.com/callback`
//! 终结，**永远不回到**运行网关的这台机器（一手依据：上游 `oauth.rs` 的模块头，
//! 以及 `endpoints.rs` 里「本家没有 loopback 监听器」那一节）。所以：
//!
//! ```text
//!   ① GET  {broker}/auth/login?provider=atomgit   -> {login_url, state}
//!   ② 把 login_url 交给界面打开（不阻塞等待）
//!   ③ 每 3 秒 GET {broker}/auth/check?state=…     -> {valid:bool}
//!   ④ valid 后 GET {broker}/auth/token?state=…    -> 令牌包 -> 落账号
//! ```
//!
//! ── 与 ZCode / OfficeAce 的两处结构共性 ──────────────────────
//!   · 授权地址**要等一次网络往返**（先问代理要 `login_url`），而 `start_*` 是
//!     同步函数（API 层是同步 match）—— 所以先把任务登记进状态表（state 用本地
//!     关联串），再由 spawn 出去的任务回填 `auth_url`；`api::session::login_start`
//!     用 `wait_for_auth_url` 等它落地再响应（壳侧只读一次 start 响应，
//!     读到 `authUrl: null` 就直接报「后端未返回登录链接」）。
//!   · 取令牌靠**轮询代理**，不需要壳侧开监听窗口、不注册回调路由。
//!
//! ── 网络抖动当 pending，真失败才结束 ──────────────────────────
//! [`BrokerLogin::check`] 已经把 5xx / 非 2xx / 畸形体折成 `Ok(false)`：
//! 那是「这次没问成」而不是「问成了但被拒」。把它当致命错误会让一次抖动
//! 打断用户正在进行的授权（这正是上游 `oauth.rs` 特别强调的一条）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::time::Duration;

use serde_json::{json, Value};

use crate::server::core::providers::atomcode;
use crate::server::core::providers::atomcode::oauth::BrokerLogin;
use crate::server::core::providers::kind_id;
use crate::server::core::providers::ProviderKind;
use crate::server::logging;

use super::{finish_task_error, LoginService, LoginTaskHandle, LOGIN_TIMEOUT_MS};

/// 轮询间隔：3 秒。
///
/// 代理那边是浏览器里的一步人工操作，3 秒足够灵敏又不会打得太密
/// （与上游插件的 `poll_interval_ms = 3_000` 同值，也是它实测过的节奏）。
const POLL_INTERVAL: Duration = Duration::from_millis(3_000);

/// 轮询间隔（供测试断言与日志引用，避免在两处各写一个魔数）
pub(crate) fn poll_interval() -> Duration {
    POLL_INTERVAL
}

/// 登录任务完成后写进 `task.session` 的载荷（前端 `/wait` 读它）。
///
/// 三个键与其它网页登录分支**逐字一致**（`accountUid` / `nickname` / `provider`）：
/// 前端与壳侧只认这三个，多一个 provider 维度不该改动它们。
fn session_of(account: &Value) -> Value {
    json!({
        "accountUid": account.get("id"),
        "nickname": account.get("name"),
        "provider": kind_id(ProviderKind::Atomcode),
    })
}

impl LoginService {
    /// 发起 AtomCode 登录（等待浏览器授权）。
    pub fn start_atomcode_login(&self) -> Result<LoginTaskHandle, String> {
        let info = crate::server::core::endpoints::resolve_edition(None);
        let handle = self.new_handle_for_provider(info, kind_id(ProviderKind::Atomcode));
        // 本地关联串（真 state 要等 `BrokerLogin::start` 之后才有）。
        // 用 uuid 而不是时间戳：任务表按它做取消 / 查询的索引，两个并发登录
        // 拿到同一个串会让「取消其中一个」变成取消另一个。
        let local_state = format!("atomcode-{}", atomcode::ids::uuid_v4());
        handle.update(|task| {
            task.state = Some(local_state.clone());
        });
        self.tasks.register(&local_state, handle.clone());
        let service = self.clone();
        let task = handle.clone();
        crate::spawn_task(async move {
            service.run_atomcode_login(task).await;
        });
        logging::log("[Login]", "发起 AtomCode 网页登录（等待 AtomGit 授权…）");
        Ok(handle)
    }

    /// 后台那半个登录：要 state → 回填授权地址 → 轮询 → 取令牌 → 落账号。
    async fn run_atomcode_login(&self, handle: LoginTaskHandle) {
        // ① 要 login_url + state：失败就直接结束（这一步没有可等的东西）
        let login = match BrokerLogin::start(LOGIN_TIMEOUT_MS).await {
            Ok(login) => login,
            Err(error) => {
                finish_task_error(&handle, &error.message);
                return;
            }
        };
        if handle.snapshot().canceled {
            return;
        }
        // 回填授权地址：**同时**把真 state 写进任务并按它再登记一次
        // （取消 / `/wait` 都是按 state 找 handle 的，本地串到此完成使命）
        let auth_url = login.login_url().to_string();
        let real_state = login.state().to_string();
        handle.update(|task| {
            task.auth_url = Some(auth_url.clone());
            task.state = Some(real_state.clone());
        });
        self.tasks.register(&real_state, handle.clone());

        let deadline = tokio::time::Instant::now() + Duration::from_millis(LOGIN_TIMEOUT_MS);
        loop {
            if handle.snapshot().canceled {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                finish_task_error(&handle, "AtomCode 网页登录超时，请重新发起");
                return;
            }
            tokio::time::sleep(poll_interval()).await;
            if handle.snapshot().canceled {
                return;
            }
            match login.check().await {
                // 还没授权完 / 这次没问成 —— 继续等（见模块头：抖动不算失败）
                Ok(false) => continue,
                Ok(true) => match login.fetch_token().await {
                    Ok(bundle) => {
                        // 网络动作到此结束，接下来只碰本机存储与任务锁
                        let secret = atomcode::secret_from_bundle(bundle);
                        // 与取消共用任务锁：取消先发生就绝不落账号
                        let mut task = handle.lock();
                        if task.done || task.canceled {
                            return;
                        }
                        match self.store.add_atomcode_account(&secret, None, "web") {
                            Ok(account) => {
                                task.session = Some(session_of(&account));
                                logging::log(
                                    "[Login]",
                                    "✅ AtomCode 网页登录完成，账号已加入列表",
                                );
                            }
                            Err(error) => task.error = Some(error.message),
                        }
                        task.done = true;
                        task.finished_at = Some(logging::now_ms());
                        return;
                    }
                    Err(error) => {
                        logging::log(
                            "[Login]",
                            &format!("❌ AtomCode 登录换取凭证失败: {}", error.message),
                        );
                        finish_task_error(&handle, &error.message);
                        return;
                    }
                },
                Err(error) => {
                    finish_task_error(&handle, &error.message);
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 轮询节奏是 3 秒（与上游插件 `poll_interval_ms` 同值）：
    /// 改它等于改上游压力 —— 太密会被代理限流，太疏会让用户「授权完了还在等」
    #[test]
    fn poll_interval_is_three_seconds() {
        assert_eq!(poll_interval(), Duration::from_millis(3_000));
    }

    /// 任务载荷的三个键与其它网页登录分支一致（前端只认它们）
    #[test]
    fn session_payload_uses_shared_keys() {
        let session = session_of(&json!({"id": "atomcode-42", "name": "qq_1"}));
        assert_eq!(
            session.get("accountUid").and_then(Value::as_str),
            Some("atomcode-42")
        );
        assert_eq!(session.get("nickname").and_then(Value::as_str), Some("qq_1"));
        assert_eq!(
            session.get("provider").and_then(Value::as_str),
            Some("atomcode")
        );
    }
}
