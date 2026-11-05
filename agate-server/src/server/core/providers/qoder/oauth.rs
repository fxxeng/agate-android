//! 设备授权（PKCE，轮询型）。verifier 只留在后端，不能进入前端状态或日志。
//!
//! ── 三个站点都走这一条（2026-11-03 复核）────────────────────────
//! 千问办公的 `/device/selectAccounts` 一直是可用的：上游只要求 `nonce` 是
//! 标准 UUID（带横线）。本模块曾把横线去掉，于是每次都被判成非法请求
//! （`400 INVALID_DEVICE_FLOW`），当时误判成「入口已停用」并改走授权码 +
//! 本机回调 —— 而授权页只认它自己登记的
//! `https://gateway.qwenwork.*/oauth/callback`，本机回环一律
//! `redirect_uri does not match`，那条路本来就走不通。现在恢复设备授权，
//! 三个站点共用这一份实现。
//!
//! ── Qoder 两个站点同一套流程，只有主机名不同 ──────────────────
//! 授权页、轮询端点、参数形状两站一致；`client_id` / `redirect_uri` 历史上
//! 不带也能登录，保持不变（见 [`Site::device_client_id`]）。
//!
//! ── 千问办公没有作业令牌交换 ────────────────────────────────
//! 它的 `/api/v1/jobToken/exchange` 不存在，设备令牌本身就是对话令牌
//! （见 [`Site::device_only`]）。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::server::errors::GatewayError;
use crate::server::logging;

use super::credentials::{self, Credentials};
use super::{auth, endpoints, machine};

pub struct DeviceLogin {
    pub state: String,
    pub auth_url: String,
    poll_url: String,
    machine_id: String,
    site: endpoints::Site,
}

impl DeviceLogin {
    pub fn new(site: endpoints::Site) -> Result<Self, GatewayError> {
        let verifier = machine::random_secret()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        // nonce 必须是标准 UUID（带横线）：上游按 UUID 解析它，去横线会被判成
        // 非法参数并整条拒绝（`INVALID_DEVICE_FLOW`，见模块头）。
        let nonce = machine::random_uuid()?;
        let machine_id = machine::machine_id_for(site)?;
        let mut auth_url = url::Url::parse(&site.device_login_url())
            .map_err(|_| GatewayError::with_status(500, "授权地址配置无效"))?;
        auth_url
            .query_pairs_mut()
            .append_pair("challenge", &challenge)
            .append_pair("challenge_method", "S256")
            .append_pair("machine_id", &machine_id)
            .append_pair("nonce", &nonce);
        // 千问办公必须带这两个参数，缺任何一个授权页都进不去；Qoder 两个站点
        // 历史上不带也能登录（改它会动到已发布的登录链路），见 `Site` 的方法。
        if let Some(client_id) = site.device_client_id() {
            auth_url.query_pairs_mut().append_pair("client_id", client_id);
        }
        if let Some(redirect) = site.device_redirect() {
            auth_url.query_pairs_mut().append_pair("redirect_uri", redirect);
        }
        let mut poll_url = url::Url::parse(&format!("{}{}", site.open_api(), endpoints::DEVICE_POLL_PATH))
            .map_err(|_| GatewayError::with_status(500, "轮询地址配置无效"))?;
        poll_url.query_pairs_mut()
            .append_pair("nonce", &nonce)
            .append_pair("verifier", &verifier)
            .append_pair("challenge_method", "S256");
        Ok(Self {
            state: machine::random_secret()?,
            auth_url: auth_url.to_string(),
            poll_url: poll_url.to_string(),
            machine_id,
            site,
        })
    }

    pub async fn poll(&self) -> Result<Option<Credentials>, GatewayError> {
        let response = auth::request("GET", &self.poll_url, None, &auth::account_headers(self.site, None), None).await?;
        if response.status == 202 || response.status == 404 {
            return Ok(None);
        }
        let data = auth::payload(response, "设备授权")?;
        // 两家的键名不同：Qoder 回 `token`，千问办公回 `device_token`。
        let token = credentials::secret(&data, &["token", "device_token"])?;
        if token.is_empty() {
            return Err(GatewayError::with_status(502, "设备授权响应缺少 token"));
        }
        let refresh_token = credentials::secret(&data, &["refresh_token"])?;
        if refresh_token.contains('|') {
            return Err(GatewayError::with_status(502, "设备授权响应的 refresh_token 格式无效"));
        }
        let user_id = credentials::text(&data, &["user_id", "uid"]);
        let mut credentials = Credentials::from_payload(&json!({
            "mode": self.site.id(),
            "accessToken": token,
            "refreshToken": if refresh_token.is_empty() { String::new() }
                else { format!("{}|{}|{}", refresh_token, user_id, self.machine_id) },
            "userId": user_id,
            "machineId": self.machine_id,
            // 设备令牌的绝对到期时刻（千问办公的轮询响应带它）；缺省按 30 天算
            "expiresAt": credentials::timestamp(data.get("expires_at"))
                .unwrap_or_else(|| logging::now_ms() + 30 * 24 * 60 * 60 * 1000),
        }))?;
        // 站点以 provider 身份为准（见 `refresh::snapshot` 的同一条规则）
        credentials.set_site(self.site);
        match auth::fetch_profile(&credentials.access_token, self.site, None).await {
            Ok(profile) => auth::apply_profile(&mut credentials, &profile)?,
            Err(error) if credentials.user_id.is_empty() => return Err(error),
            Err(_) => {}
        }
        credentials.complete_identity()?;
        Ok(Some(credentials))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 授权地址必须带上站点要求的参数，且不能给自己加白名单之外的跳转。
    ///
    /// 这两个参数缺了的表现是「浏览器打开是一张 400 页」，从服务端日志里
    /// 完全看不出原因 —— 所以钉在这里。
    #[test]
    fn the_authorization_url_carries_what_the_site_requires() {
        for site in endpoints::Site::ALL {
            let login = DeviceLogin::new(site).expect("构造登录");
            let url = url::Url::parse(&login.auth_url).expect("授权地址可解析");
            let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
            let has = |key: &str| pairs.iter().any(|(name, _)| name == key);
            assert!(has("challenge") && has("challenge_method") && has("machine_id") && has("nonce"));
            assert_eq!(has("client_id"), site.device_client_id().is_some());
            assert_eq!(has("redirect_uri"), site.device_redirect().is_some());
            // nonce 必须是带横线的标准 UUID（见模块头）
            let nonce = pairs
                .iter()
                .find(|(name, _)| name == "nonce")
                .map(|(_, value)| value.clone())
                .expect("有 nonce");
            assert_eq!(nonce.len(), 36, "{} 的 nonce 不是标准 UUID", site.display_name());
            // 轮询地址落在本站点的账号主机上
            let poll = url::Url::parse(&login.poll_url).expect("轮询地址可解析");
            let expected = url::Url::parse(site.open_api())
                .expect("账号主机可解析")
                .host_str()
                .map(str::to_string);
            assert_eq!(poll.host_str().map(str::to_string), expected);
        }
    }
}
