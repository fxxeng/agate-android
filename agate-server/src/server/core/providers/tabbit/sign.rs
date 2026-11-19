//! Tabbit 请求签名与客户端指纹（纯函数，可离线验证）。
//!
//! ── 签名（实测，按线上行为为准）──────────────────────────────
//! ```text
//!   x-timestamp  Date.now() 毫秒
//!   x-signature  随机 UUID v4
//!   x-nonce      hex(HMAC-SHA256(signKey, "{ts}.{uuid}.{sha256hex(body)}"))
//! ```
//! ⚠️ **命名是反的**：`x-signature` 是那个随机 UUID，`x-nonce` 才是 HMAC。
//! 按名字直觉写会把两个头填反，上游只会回一句「签名错误」，看不出是哪一处。
//!
//! 签名 key 先取 `GET /chat/sign-key`（服务端会轮换），请求返回 **499** 时
//! 刷新一次 key 重试；拉不到时用**本站点**的兜底 key（见 `Realm`）。
//!
//! ── 指纹头（上游按它判客户端形态）────────────────────────────
//! - `x-req-ctx`：base64 的客户端版本号，形如 `1.17.16(10117016)`；
//!   格式不对会被判 493「版本过低」；
//! - `unique-uuid`：第 5 位是「是否默认浏览器」标记位，另 8 个位置
//!   `[2,7,11,14,18,21,25,28]` 编码当前秒的 hex（防重放）；命中标记位才解锁
//!   Premium 模型（见 `Realm` 与模块头）；
//! - `x-chrome-id-consistency-request`：固定的 Chrome 集成三元组，含由 uid
//!   派生的稳定设备 id（同一账号每次一致，避免上游把它当新设备）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use base64::Engine as _;
use hmac::{Hmac, Mac};
use md5::{Digest as _, Md5};
use sha2::Sha256;

use super::CLIENT_ID;

type HmacSha256 = Hmac<Sha256>;

/// 字节 → 小写十六进制
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// 随机字节。
///
/// `getrandom` 失败（极端环境）时退化成时间派生：这些字节只用作「随机标识」，
/// **不参与验签**（HMAC 的密钥是 sign key，不是这里），所以退化不影响正确性。
fn random_bytes(len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    if getrandom::getrandom(&mut bytes).is_ok() {
        return bytes;
    }
    let seed = format!("tabbit-random:{}", crate::server::logging::now_ms());
    let digest = sha2::Sha256::digest(seed.as_bytes());
    for (index, slot) in bytes.iter_mut().enumerate() {
        *slot = digest[index % digest.len()];
    }
    bytes
}

/// v4 UUID 字符串（带横线 —— 上游只认这个形态）
pub fn random_uuid() -> String {
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&random_bytes(16));
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{}-{}-{}-{}-{}",
        hex(&bytes[0..4]),
        hex(&bytes[4..6]),
        hex(&bytes[6..8]),
        hex(&bytes[8..10]),
        hex(&bytes[10..16])
    )
}

/// `sha256(input)` 的小写十六进制
pub fn sha256_hex(input: impl AsRef<[u8]>) -> String {
    hex(&sha2::Sha256::digest(input.as_ref()))
}

/// 空串的 md5 —— 上游 `entity.key` 的固定值（前端对一个空 tab 取的 md5）。
///
/// 它是 `d41d8cd98f00b204e9800998ecf8427e`，写死也行；但用库里算一次更诚实：
/// 这个值的来源就是「空内容的 md5」，不是某个随机常量。
pub fn md5_empty_hex() -> String {
    hex(&Md5::digest([]))
}

/// base64（标准表、带 padding）
pub fn base64_text(input: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(input.as_bytes())
}

/// `unique-uuid`：第 5 位是「默认浏览器」标记，另 8 个位置编码当前秒的 hex。
///
/// 上游用它判两件事：(a) 防重放（时间编码在位里）；(b) 是不是「默认浏览器」
/// —— 命中才解锁 Premium 模型。所以 `default_browser` 不是装饰，它决定这个
/// 账号能不能看到 Claude / GPT / Gemini 那几档。
pub fn unique_uuid(default_browser: bool, now_ms: i64) -> String {
    let seconds = now_ms.div_euclid(1000).max(0) as u64;
    let timestamp = format!("{seconds:08x}");
    let timestamp = &timestamp[timestamp.len().saturating_sub(8)..];
    let positions = [2usize, 7, 11, 14, 18, 21, 25, 28];
    let random = random_bytes(32);
    let digits = b"0123456789abcdef";
    // 第 5 位「非默认浏览器」的值域：排除掉 '1'，避免误命中标记位
    let non_one = b"023456789abcdef";
    let mut out = [0u8; 32];
    for (index, slot) in out.iter_mut().enumerate() {
        if let Some(position) = positions.iter().position(|candidate| *candidate == index) {
            *slot = timestamp.as_bytes()[position];
            continue;
        }
        if index == 5 {
            *slot = if default_browser {
                b'1'
            } else {
                non_one[(random[index] as usize) % non_one.len()]
            };
            continue;
        }
        *slot = digits[(random[index] as usize) % digits.len()];
    }
    let text = String::from_utf8_lossy(&out).to_string();
    format!(
        "{}-{}-{}-{}-{}",
        &text[0..8],
        &text[8..12],
        &text[12..16],
        &text[16..20],
        &text[20..32]
    )
}

/// 由 uid 派生**稳定**的设备 UUID（同一账号每次一致）。
///
/// 每次请求都换一个设备 id 会让上游把同一账号当成一串新设备 —— 那是风控
/// 关注的行为，不是「更安全」。
pub fn device_id(uid: &str) -> String {
    let mut digest = sha2::Sha256::digest(format!("tabbit-device:{uid}").as_bytes()).to_vec();
    digest.truncate(16);
    if digest.len() < 16 {
        digest.resize(16, 0);
    }
    digest[6] = (digest[6] & 0x0f) | 0x40;
    digest[8] = (digest[8] & 0x3f) | 0x80;
    format!(
        "{}-{}-{}-{}-{}",
        hex(&digest[0..4]),
        hex(&digest[4..6]),
        hex(&digest[6..8]),
        hex(&digest[8..10]),
        hex(&digest[10..16])
    )
}

/// 签名头三件套（顺序无关，上游按名字取）。
///
/// ⚠️ `x-signature` 里放的是随机 UUID，`x-nonce` 里放的才是 HMAC —— 见模块头。
pub fn signature_headers(sign_key: &str, body: &str, now_ms: i64) -> Vec<(String, String)> {
    let timestamp = now_ms.to_string();
    let uuid = random_uuid();
    let digest = sha256_hex(body.as_bytes());
    let message = format!("{timestamp}.{uuid}.{digest}");
    // `new_from_slice` 对 HMAC 永不失败（任意长度密钥都合法），仍按契约处理失败
    let nonce = match HmacSha256::new_from_slice(sign_key.as_bytes()) {
        Ok(mut mac) => {
            mac.update(message.as_bytes());
            hex(&mac.finalize().into_bytes())
        }
        Err(_) => String::new(),
    };
    vec![
        ("x-timestamp".to_string(), timestamp),
        ("x-signature".to_string(), uuid),
        ("x-nonce".to_string(), nonce),
    ]
}

/// `x-chrome-id-consistency-request` 的固定三元组
pub fn chrome_consistency(uid: &str) -> String {
    format!(
        "version=1,client_id={CLIENT_ID},device_id={},sync_account_id={uid},\
         signin_mode=all_accounts,signout_mode=show_confirmation",
        device_id(uid)
    )
}

/// 用户可见的短令牌尾巴（账号行显示「token 尾 8 位」）
pub fn token_tail(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() <= 8 {
        return String::new();
    }
    chars[chars.len() - 8..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 空串 md5 就是那个众所周知的常数 —— 上游 `entity.key` 的取值依据
    #[test]
    fn empty_md5_is_the_known_constant() {
        assert_eq!(md5_empty_hex(), "d41d8cd98f00b204e9800998ecf8427e");
    }

    /// 签名头：`x-signature` 必须是 UUID v4（带横线的 36 字符），
    /// `x-nonce` 必须是 64 位 hex —— 两者填反是最容易犯又最难查的错。
    #[test]
    fn signature_header_names_are_counterintuitive() {
        let headers = signature_headers("key", "{}", 1_700_000_000_000);
        let map: std::collections::BTreeMap<_, _> = headers.into_iter().collect();
        let signature = map.get("x-signature").map(String::as_str).unwrap_or("");
        let nonce = map.get("x-nonce").map(String::as_str).unwrap_or("");
        assert_eq!(signature.len(), 36, "x-signature 应是 UUID：{signature}");
        assert_eq!(signature.matches('-').count(), 4);
        assert_eq!(nonce.len(), 64, "x-nonce 应是 sha256 长度的 hex：{nonce}");
        assert!(nonce.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    /// HMAC 的输入是 `"{ts}.{uuid}.{sha256hex(body)}"` —— 换一个输入必须换结果，
    /// 且同一输入配同一 uuid 必然可复现（说明没有掺入隐藏的随机量）。
    #[test]
    fn nonce_is_a_pure_function_of_timestamp_uuid_and_body() {
        let digest = sha256_hex(b"abc");
        let message = format!("1700000000000.fixed-uuid.{digest}");
        let mut first = HmacSha256::new_from_slice(b"key").expect("hmac 密钥任意长度都合法");
        first.update(message.as_bytes());
        let expected = hex(&first.finalize().into_bytes());

        let mut second = HmacSha256::new_from_slice(b"key").expect("hmac 密钥任意长度都合法");
        second.update(message.as_bytes());
        assert_eq!(hex(&second.finalize().into_bytes()), expected);

        let mut other = HmacSha256::new_from_slice(b"other-key").expect("hmac 密钥任意长度都合法");
        other.update(message.as_bytes());
        assert_ne!(hex(&other.finalize().into_bytes()), expected);
    }

    /// `unique-uuid`：第 5 位是默认浏览器标记，8 个时间位必须与时秒一致
    #[test]
    fn unique_uuid_encodes_the_flag_and_the_clock() {
        let now_ms = 1_700_000_000_000i64;
        let seconds = (now_ms / 1000) as u64;
        let stamp = format!("{seconds:08x}");
        let stamp = &stamp[stamp.len() - 8..];
        let positions = [2usize, 7, 11, 14, 18, 21, 25, 28];

        let pro = unique_uuid(true, now_ms);
        let compact: String = pro.chars().filter(|ch| *ch != '-').collect();
        assert_eq!(compact.len(), 32);
        assert_eq!(compact.as_bytes()[5], b'1', "开启时第 5 位必须是 '1'");
        for (slot, position) in positions.iter().enumerate() {
            assert_eq!(
                compact.as_bytes()[*position] as char,
                stamp.as_bytes()[slot] as char,
                "第 {position} 位应编码时间"
            );
        }

        let plain = unique_uuid(false, now_ms);
        let compact_plain: String = plain.chars().filter(|ch| *ch != '-').collect();
        assert_ne!(compact_plain.as_bytes()[5], b'1', "关闭时第 5 位不能是 '1'");
    }

    /// 设备 id 由 uid 派生：同 uid 稳定、不同 uid 不同、且是 v4 形态
    #[test]
    fn device_id_is_stable_per_account() {
        assert_eq!(device_id("u1"), device_id("u1"));
        assert_ne!(device_id("u1"), device_id("u2"));
        let id = device_id("u1");
        assert_eq!(id.len(), 36);
        // 版本位与变体位
        let compact: String = id.chars().filter(|ch| *ch != '-').collect();
        assert_eq!(compact.as_bytes()[12], b'4');
        assert!(matches!(compact.as_bytes()[16], b'8' | b'9' | b'a' | b'b'));
    }

    /// base64 是标准表（带 `=` 填充）—— 上游按这个解 `x-req-ctx`
    #[test]
    fn version_header_is_plain_base64() {
        assert_eq!(base64_text("1.17.16(10117016)"), "MS4xNy4xNigxMDExNzAxNik=");
    }

    /// 一致性三元组里必须出现 uid 与固定 client_id
    #[test]
    fn chrome_consistency_carries_the_uid_and_client_id() {
        let value = chrome_consistency("u-42");
        assert!(value.contains(&format!("client_id={CLIENT_ID}")));
        assert!(value.contains("sync_account_id=u-42"));
        assert!(value.contains(&format!("device_id={}", device_id("u-42"))));
    }

    /// 尾号取最后 8 个字符；短令牌不回显（避免把整串当尾号显示出去）
    #[test]
    fn token_tail_shows_the_last_eight_characters_only() {
        assert_eq!(token_tail("abcdefghij"), "cdefghij");
        assert_eq!(token_tail("short"), "");
    }
}
