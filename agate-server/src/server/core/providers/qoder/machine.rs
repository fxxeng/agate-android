//! PKCE 随机串与设备标识。随机源失败时终止登录，不降级为可预测值。
//!
//! ── 设备标识按站点分开（这是账号关联级的差别）──────────────────
//! 上游把它当作「这台设备」，并且**按设备聚合账号**。Qoder 与千问办公是两个
//! 产品、两套风控，共用一个标识等于把它们关联到同一台设备上；千问办公自己
//! 也是「一个账号一份机器码」的口径（dumate2api 的 `genMachineId` 注释）。
//!
//! 因此这里按站点分开取：
//!   - Qoder：沿用它既有的 `qoder-machine-id`（两个地区共用 —— 那是历史行为，
//!     两家本来就是同一套风控，改它会动到已发布账号的设备指纹）；
//!   - 千问办公：自己的 `qwenwork-machine-id`，并且**优先沿用官方客户端
//!     (`~/.qoderworkcn/.auth/machine_id`) 里那份** —— 那是已被证明能用于
//!     签名的值（dumate2api 的实测结论），随机生成一个能不能过服务端校验
//!     还没被证实。

use std::path::PathBuf;
use std::sync::Mutex;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

use crate::server::errors::GatewayError;

use super::endpoints::Site;

/// 每个站点一份缓存（`0` = Qoder / `1` = 千问办公；见 [`slot`]）
static MACHINE_IDS: Mutex<[Option<String>; 2]> = Mutex::new([None, None]);

fn slot(site: Site) -> usize {
    if site.is_qwenwork() { 1 } else { 0 }
}

/// 本插件数据目录里的标识文件名。
fn file_name(site: Site) -> &'static str {
    if site.is_qwenwork() { "qwenwork-machine-id" } else { "qoder-machine-id" }
}

/// 官方客户端自己那份标识的候选位置（按优先级）。
///
/// 千问办公国内版的客户端目录是 `.qoderworkcn`（它的产品内部名就是
/// 「qoderwork」）。
fn client_candidates(site: Site) -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
    else {
        return Vec::new();
    };
    if site.is_qwenwork() {
        vec![
            home.join(".qoderworkcn").join(".auth/machine_id"),
            home.join(".qoderworkcn").join("machine_id"),
        ]
    } else {
        vec![
            home.join(".qoder-proxy/machine_id"),
            home.join(".qoder/.auth/machine_id"),
            home.join(".qoder/machine_id"),
        ]
    }
}

pub fn random_secret() -> Result<String, GatewayError> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|_| GatewayError::with_status(500, "无法生成安全的登录随机串"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub(super) fn random_uuid() -> Result<String, GatewayError> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|_| GatewayError::with_status(500, "无法生成机器标识"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]))
}

fn read_id(path: &std::path::Path) -> Option<String> {
    let value = std::fs::read_to_string(path).ok()?.trim().to_string();
    (!value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
        .then_some(value)
}

/// 本站点的设备标识（首次取到之后进程内缓存）。
pub fn machine_id_for(site: Site) -> Result<String, GatewayError> {
    let index = slot(site);
    let mut cached = MACHINE_IDS
        .lock()
        .map_err(|_| GatewayError::with_status(500, "机器标识锁不可用"))?;
    if let Some(value) = cached.get(index).and_then(Option::as_ref) {
        return Ok(value.clone());
    }
    let directory = crate::server::config::config_dir();
    let path = directory.join(file_name(site));
    if let Some(value) = read_id(&path) {
        cached[index] = Some(value.clone());
        return Ok(value);
    }
    let existing = client_candidates(site)
        .iter()
        .find_map(|candidate| read_id(candidate));
    let value = match existing {
        Some(value) => value,
        None => random_uuid()?,
    };
    std::fs::create_dir_all(&directory)
        .and_then(|_| std::fs::write(&path, &value))
        .map_err(|error| GatewayError::with_status(500, format!("保存机器标识失败: {error}")))?;
    cached[index] = Some(value.clone());
    Ok(value)
}

/// Qoder 的设备标识（沿用既有的文件名与候选位置）。
pub fn machine_id() -> Result<String, GatewayError> {
    machine_id_for(Site::QoderCn)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两个产品**不能共用一份机器码**：共用等于把两家账号关联到同一台设备上，
    /// 而这件事在界面上完全看不出来。
    #[test]
    fn the_two_products_never_share_a_slot_or_a_file() {
        assert_ne!(slot(Site::QoderCn), slot(Site::QwenworkCn));
        assert_ne!(file_name(Site::QoderCn), file_name(Site::QwenworkCn));
        // 两个 Qoder 地区共用（历史行为，改它会动到已发布账号的指纹）
        assert_eq!(slot(Site::QoderCn), slot(Site::QoderIntl));
        assert_eq!(file_name(Site::QoderCn), file_name(Site::QoderIntl));
    }

    /// 生成的 UUID 必须是标准形态（带横线、版本位 4）：上游按 UUID 解析
    /// `nonce`，去掉横线会被判非法（见 `endpoints` 模块头）。
    #[test]
    fn random_uuid_keeps_the_dashes_and_the_version_bits() {
        let uuid = random_uuid().expect("生成 UUID");
        assert_eq!(uuid.len(), 36);
        assert_eq!(uuid.chars().filter(|ch| *ch == '-').count(), 4);
        assert_eq!(uuid.as_bytes()[14], b'4', "版本位必须是 4");
        assert!(matches!(uuid.as_bytes()[19], b'8' | b'9' | b'a' | b'b'), "变体位必须是 10xx");
    }
}
