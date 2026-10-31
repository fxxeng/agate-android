//! LobsterAI 的网页登录收尾（**loopback 回调路由专用**）。
//!
//! ── 这条链属于哪一型 ────────────────────────────────────────
//! 与 Accio 同族：**浏览器把用户带回本机 HTTP 端口**
//! （`GET /auth/callback?code=…&state=…`）。授权地址由本实现本地拼（见
//! `providers::lobsterai::oauth::begin_login`），走的是**通用**网页登录发起入口
//! （`LoginService::start_web_login`，与 Accio / 小浣熊同一条路）。回调路径是
//! **裸 `/auth/callback`**（不是 `-lobsterai` 后缀）：那是参考实现已验证的形状，
//! 门户是否对路径有白名单没有实测支撑，所以照抄。
//!
//! ── 与 `start_web_login` 的分工 ─────────────────────────────
//! 发起侧只做「生成 state、登记任务、返回授权地址」，拿到回调里的 code 之前
//! 没有任何网络动作。本文件是收尾侧：校验 state 归属 → 交适配器换码落账号 →
//! 写任务状态。
//!
//! ── 校验口径（逐条都有理由）────────────────────────────────
//! 回调落在本机 HTTP 端口上，任何本机进程都能伪造一次 GET，因此：
//!   1. `state` 必须对应一个**进行中且属于 LobsterAI** 的登录任务；
//!   2. 本家那一轮 pending 由适配器取走即失效（`exchange_login_code` 里
//!      `take_pending`）——「授权码只能兑一次」的闸门在那里；
//!   3. 任务已完成时直接返回成功：用户刷新页面 / 浏览器预取会重复回调，
//!      第二次必然拿到「授权码已失效」，把一次成功变成一次失败。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::providers::{kind_id, ProviderKind};
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::{finish_task_error, LoginService};

impl LoginService {
    /// 浏览器回调：校验归属 → 用授权码换凭证 → 落账号 → 标记任务完成。
    pub async fn finish_lobsterai_login(
        &self,
        code: &str,
        state: &str,
    ) -> Result<String, GatewayError> {
        let code = code.trim();
        let state = state.trim();
        if code.is_empty() {
            return Err(GatewayError::with_status(400, "回调没有携带授权码"));
        }
        if state.is_empty() {
            return Err(GatewayError::with_status(
                400,
                "回调没有携带 state，无法确认这次登录归属",
            ));
        }
        let Some(handle) = self.tasks.get(state) else {
            return Err(GatewayError::with_status(
                404,
                "这次登录已取消或已过期，请重新发起",
            ));
        };
        let provider = kind_id(ProviderKind::LobsterAi);
        if handle.snapshot().provider != provider {
            return Err(GatewayError::with_status(400, "这次登录不属于 LobsterAI"));
        }
        if handle.snapshot().done {
            // 幂等：重复回调不是错误（见校验口径 3）
            return Ok(String::new());
        }
        let adapter = crate::server::core::providers::adapter::adapter_for(
            ProviderKind::LobsterAi,
        );
        match adapter.exchange_login_code(&self.store, code, state).await {
            Ok(account_id) => {
                handle.update(|task| {
                    task.done = true;
                    task.session = Some(json!({
                        "accountUid": account_id,
                        "nickname": Value::Null,
                        "provider": provider,
                    }));
                    task.finished_at = Some(logging::now_ms());
                });
                logging::log("[Login]", "✅ LobsterAI 网页登录完成，账号已加入列表");
                Ok(account_id)
            }
            Err(error) => {
                finish_task_error(&handle, &error.message);
                logging::log(
                    "[Login]",
                    &format!("❌ LobsterAI 网页登录换取凭证失败: {}", error.message),
                );
                Err(error)
            }
        }
    }
}