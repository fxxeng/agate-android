//! Qoder 系（Qoder 中国版 / 国际版 / 千问办公国内版）的设备授权任务与
//! 通用 start / wait / cancel 状态表的衔接。

use std::time::Duration;

use serde_json::json;

use crate::server::core::providers::qoder::{endpoints::Site, oauth::DeviceLogin};
use crate::server::logging;

use super::{finish_task_error, LoginService, LoginTaskHandle, LOGIN_TIMEOUT_MS};

impl LoginService {
    /// 发起 Qoder 系设备授权（三站同构，站点由 **provider 身份**决定）。
    ///
    /// 拆家后界面上是三张卡片（`qoder` 中国版 / `qoder-intl` 国际版 /
    /// `qwenwork` 千问办公国内版），点哪张就发哪个 provider id —— 由调用方按
    /// kind 反查站点传进来（与 ZCode 同款：请求里的 `edition` 只是回显字段，
    /// 不当权威）。三站是同一套设备授权协议，只有主机与客户端身份常量不同。
    pub fn start_qoder_login(&self, site: Site) -> Result<LoginTaskHandle, String> {
        let flow = DeviceLogin::new(site).map_err(|error| error.message)?;
        // 任务表里的 `edition` 只影响日志与前端回显（登录窗口标题等），
        // 用 provider 自己的取值而不是 workbuddy 那套 id。
        let info = crate::server::core::endpoints::resolve_edition(Some(site.edition()));
        let handle = self.new_handle_for_provider(info, site.provider_id());
        handle.update(|task| {
            task.state = Some(flow.state.clone());
            task.auth_url = Some(flow.auth_url.clone());
        });
        self.tasks.register(&flow.state, handle.clone());
        let service = self.clone();
        let task = handle.clone();
        crate::spawn_task(async move {
            service.run_qoder_login(task, site, flow).await;
        });
        logging::log(
            "[Login]",
            &format!("发起 {} 网页登录（等待授权…）", site.display_name()),
        );
        Ok(handle)
    }

    async fn run_qoder_login(&self, handle: LoginTaskHandle, site: Site, flow: DeviceLogin) {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(LOGIN_TIMEOUT_MS);
        loop {
            if handle.snapshot().canceled {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                finish_task_error(&handle, &format!("{} 网页登录超时，请重新发起", site.product()));
                return;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            if handle.snapshot().canceled {
                return;
            }
            let outcome = tokio::time::timeout_at(deadline, flow.poll()).await;
            if handle.snapshot().canceled {
                return;
            }
            match outcome {
                Ok(Ok(None)) => continue,
                Ok(Err(error)) if error.code.as_deref() == Some("qoder_transport") => {
                    logging::verbose(
                        "[Login]",
                        &format!("{} 设备授权暂时不可达，继续等待", site.display_name()),
                    );
                }
                Ok(Err(error)) => {
                    finish_task_error(&handle, &error.message);
                    return;
                }
                Err(_) => {
                    finish_task_error(
                        &handle,
                        &format!("{} 网页登录超时，请重新发起", site.product()),
                    );
                    return;
                }
                Ok(Ok(Some(credentials))) => {
                    // 与取消共用任务锁：取消先发生就绝不落账号，落盘先发生则视为已完成。
                    let mut task = handle.lock();
                    if task.done || task.canceled {
                        return;
                    }
                    match self.store.add_qoder_account(site, &credentials, None, "web") {
                        Ok(account) => {
                            // edition 取凭证自己记着的站点（不代表用户这次的界面选择）：
                            // 落账号时 `add_qoder_account` 已按站点去重与生成 id，
                            // 这里只把它原样回给前端，避免显示成另一站。
                            task.session = Some(json!({
                                "accountUid": account.get("id"),
                                "nickname": account.get("name"),
                                "edition": credentials.site.edition(),
                                "provider": site.provider_id(),
                            }));
                            logging::log(
                                "[Login]",
                                &format!(
                                    "✅ {} 网页登录完成，账号已加入列表",
                                    credentials.site.display_name()
                                ),
                            );
                        }
                        Err(error) => task.error = Some(error.message),
                    }
                    task.done = true;
                    task.finished_at = Some(logging::now_ms());
                    return;
                }
            }
        }
    }
}
