//! Kiro 的地址、地区判定与请求头口径（**全是纯函数**）。
//!
//! ── 三组地址，三套口径 ──────────────────────────────────────
//! ```text
//!   控制面（目录 / 余额）  https://management.{region}.kiro.dev
//!   推理                  https://runtime.{region}.kiro.dev/generateAssistantResponse
//!   AWS OIDC（idc 登录）  https://oidc.{region}.amazonaws.com
//!   社交登录刷新          https://prod.{region}.auth.desktop.kiro.dev/refreshToken
//!   换码（社交登录）      https://prod.us-east-1.auth.desktop.kiro.dev/oauth/token
//! ```
//! 最后一条**与地区无关**：门户固定打 us-east-1 的 auth 服务（上游实测），
//! 所以它是个常量而不是 `format!` —— 写成按地区拼会让 EU 账号的换码打到不存在的
//! 主机上。
//!
//! ── 地区从哪来 ──────────────────────────────────────────────
//! profile ARN（`arn:aws:codewhisperer:eu-central-1:…`）是权威：它是账号登录时
//! AWS 分配的，比我们记录的登录地区可靠。ARN 缺失 / 形态不对时才按登录地区判
//! （`eu-` 开头 → 法兰克福，其余 → 弗吉尼亚）。**不拆家**：两地共用账号与令牌。
//!
//! ── 为什么要把「地区 / Start URL」当不可信输入 ──────────────
//! 它们会被拼进主机名与请求体：一个带 `/` 或 `@` 的字符串能把请求导到别的
//! 主机去。`is_aws_region` / `is_https_url` 就是这两道闸（纯形态检查）。

use super::DEFAULT_REGION;

/// 控制面（目录 / 余额）
pub fn management(region: &str) -> String {
    format!("https://management.{region}.kiro.dev")
}

/// 推理地址
pub fn runtime(region: &str) -> String {
    format!("https://runtime.{region}.kiro.dev")
}

/// AWS OIDC（IAM Identity Center 登录换码）
pub fn oidc(region: &str) -> String {
    format!("https://oidc.{region}.amazonaws.com")
}

/// 社交登录（Google / GitHub / Builder ID）的刷新地址
pub fn refresh_url(region: &str) -> String {
    format!("https://prod.{region}.auth.desktop.kiro.dev/refreshToken")
}

/// 推理路径（`generateAssistantResponse`）
pub const CHAT_PATH: &str = "/generateAssistantResponse";

/// AWS JSON 1.0 的 `x-amz-target`：余额
pub const TARGET_USAGE_LIMITS: &str = "AmazonCodeWhispererService.GetUsageLimits";
/// AWS JSON 1.0 的 `x-amz-target`：可用模型（API key 通道）
pub const TARGET_KEY_MODELS: &str = "KiroControlPlaneBearerService.ListAvailableModels";
/// AWS JSON 1.0 的 `x-amz-target`：可用模型（OAuth 通道）
pub const TARGET_PROFILE_MODELS: &str = "AmazonCodeWhispererService.ListAvailableModels";

/// 从 profile ARN 里取地区；取不到时按登录地区判。
///
/// ARN 形态：`arn:aws:codewhisperer:<region>:<account>:profile/<id>`
pub fn region_of(profile: &str, signed_in: &str) -> String {
    let parts: Vec<&str> = profile.split(':').collect();
    if parts.len() > 4 && !parts[3].trim().is_empty() {
        let region = parts[3].trim();
        if is_aws_region(region) {
            return region.to_string();
        }
    }
    if signed_in.trim().starts_with("eu-") {
        "eu-central-1".to_string()
    } else {
        DEFAULT_REGION.to_string()
    }
}

/// 形态上是不是一个 AWS 地区名（小写字母 + 连字符 + 结尾数字）。
///
/// 只做形态检查：它要拼进 AWS 的主机名，允许任意内容等于允许把请求导到别处。
pub fn is_aws_region(value: &str) -> bool {
    // 长度按字节算即可：下面逐段只允许 ASCII 小写字母 / 数字，非 ASCII 内容
    // 一定被拒，不会因为「字符数」与「字节数」的差放过什么。
    // 地区不一定是三段（`ap-southeast-1` 就有四段），末尾必须是 1~2 位数字。
    if !(9..=32).contains(&value.len()) {
        return false;
    }
    let parts: Vec<&str> = value.split('-').collect();
    let Some((number, words)) = parts.split_last() else {
        return false;
    };
    words.len() >= 2
        && words[0].len() == 2
        && words
            .iter()
            .all(|word| !word.is_empty() && word.bytes().all(|ch| ch.is_ascii_lowercase()))
        && (1..=2).contains(&number.len())
        && number.bytes().all(|ch| ch.is_ascii_digit())
}

/// 是 https 且不带用户名（AWS Start URL 的形态检查）。
pub fn is_https_url(value: &str) -> bool {
    // 手写 strip_prefix 会把 `https://x@y/`、`https://x/#frag` 乃至带控制字符
    // 的地址放过去；交给 URL 解析器判 scheme/host/无凭据/无 fragment。
    if value.bytes().any(|ch| ch <= b' ' || ch == 0x7f) {
        return false;
    }
    url::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
    })
}

/// query 参数的百分号编码（只有 profile ARN 这类值需要）。
///
/// 不用 `url::form_urlencoded`：那套把空格编成 `+`，而 AWS 的 query 解析按
/// RFC 3986 解，`+` 会被当成字面量。这里只做一条规则 —— 非 unreserved
/// 一律 `%XX`，与上游插件 `profile_query` 同一做法。
pub fn query_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// 从 profile ARN 里取账号段（`arn:aws:codewhisperer:<region>:<account>:…`）。
///
/// 拿不到就回空串 —— 这个值只用于展示与「同账号去重」的兜底，
/// 没有它也能正常工作。
pub fn account_of(profile: &str) -> String {
    let parts: Vec<&str> = profile.split(':').collect();
    if parts.len() > 4 {
        let account = parts[4].trim();
        if !account.is_empty() && account.chars().all(|ch| ch.is_ascii_digit()) {
            return account.to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// profile 里的地区优先，登录地区只做兜底
    #[test]
    fn the_profile_region_wins_over_the_sign_in_region() {
        let profile = "arn:aws:codewhisperer:eu-central-1:123456789012:profile/AAA";
        assert_eq!(region_of(profile, "us-east-1"), "eu-central-1");
        // 没有 profile（API key / 登录后未取到）→ 按登录地区
        assert_eq!(region_of("", "eu-central-1"), "eu-central-1");
        assert_eq!(region_of("", "us-east-1"), "us-east-1");
        assert_eq!(region_of("", ""), DEFAULT_REGION);
        // ARN 形态不对（地区段是垃圾）时也不能拿它拼主机名
        assert_eq!(
            region_of("arn:aws:codewhisperer:../evil:1:profile/A", "eu-central-1"),
            "eu-central-1"
        );
    }

    /// 地区形态检查挡住「能改写主机名」的输入
    #[test]
    fn region_shape_rejects_hostile_values() {
        assert!(is_aws_region("us-east-1"));
        assert!(is_aws_region("eu-central-1"));
        assert!(is_aws_region("ap-southeast-2"));
        for bad in [
            "",
            "us-east-1/../evil",
            "US-EAST-1",
            "us-east",
            "us-east-1a",
            "a-b-1",
            "us--1",
            "us-east-",
            "us-east-1.evil.com",
            &"a".repeat(64),
        ] {
            assert!(!is_aws_region(bad), "{bad} 不该被当成地区");
        }
    }

    /// Start URL 必须是无用户名的 https（否则能把票据发给别处）
    #[test]
    fn start_urls_are_plain_https() {
        assert!(is_https_url("https://view.awsapps.com/start"));
        assert!(is_https_url("https://example.com/d-9067/start/"));
        for bad in ["http://x.com", "https://", "https:///start", "https://user@x.com/start", "view.awsapps.com"] {
            assert!(!is_https_url(bad), "{bad} 不该被当成 Start URL");
        }
    }

    /// 换码地址**不随地区变**（写成按地区拼会让 EU 账号打到不存在的主机）
    #[test]
    fn the_token_exchange_host_is_region_independent() {
        assert_eq!(
            super::super::AUTH_SERVICE,
            "https://prod.us-east-1.auth.desktop.kiro.dev"
        );
        assert_eq!(
            refresh_url("eu-central-1"),
            "https://prod.eu-central-1.auth.desktop.kiro.dev/refreshToken"
        );
        assert_ne!(refresh_url("eu-central-1"), refresh_url("us-east-1"));
    }

    /// 三组主机名各自成形态
    #[test]
    fn the_three_host_families_are_built_correctly() {
        assert_eq!(management("us-east-1"), "https://management.us-east-1.kiro.dev");
        assert_eq!(runtime("eu-central-1"), "https://runtime.eu-central-1.kiro.dev");
        assert_eq!(oidc("us-east-1"), "https://oidc.us-east-1.amazonaws.com");
        assert_eq!(CHAT_PATH, "/generateAssistantResponse");
    }

    /// profile 里的账号段只在全是数字时才认
    #[test]
    fn the_account_segment_is_numeric_or_nothing() {
        assert_eq!(
            account_of("arn:aws:codewhisperer:us-east-1:638616132270:profile/AAA"),
            "638616132270"
        );
        assert_eq!(account_of("arn:aws:codewhisperer:us-east-1:abc:profile/AAA"), "");
        assert_eq!(account_of(""), "");
        assert_eq!(account_of("arn:aws"), "");
    }

    /// ARN 拼进 query 时必须整段编码：`:` 与 `/` 不编码会让 URL 变成
    /// 路径分隔符（服务端把 profile 的后半截当成新路径段）
    #[test]
    fn the_profile_arn_is_percent_encoded_for_the_query() {
        assert_eq!(
            query_escape("arn:aws:codewhisperer:us-east-1:1:profile/AAA"),
            "arn%3Aaws%3Acodewhisperer%3Aus-east-1%3A1%3Aprofile%2FAAA"
        );
        // unreserved 原样保留（`-` `_` `.` `~` 与字母数字）
        assert_eq!(query_escape("us-east-1_A.b~c"), "us-east-1_A.b~c");
        // 空格是 `%20` 而不是 `+`（AWS 按 RFC 3986 解 query）
        assert_eq!(query_escape("a b"), "a%20b");
    }
}
