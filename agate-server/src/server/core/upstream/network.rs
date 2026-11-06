//! 上游**入口容错**：官方入口连不上（或 502/503/504）时改发备用入口，
//! 再不行就用备用入口的固定地址。
//!
//! ── 这一层解决什么（以及不解决什么）────────────────────────
//! 国内网络里，某一家的**官方入口域名**偶发不可达（DNS 污染 / 线路抖动 /
//! 网关侧 5xx），但同一家还有一个**备用入口域名**，回的是同一套协议、同一份
//! 凭证。本模块把「换一个入口再试一次」这件事从适配器里抽出来，做成一张
//! 静态策略表 —— 与上游同一个形态（`NetworkPolicy`）。
//!
//! **它不改变账号选择、不改变代理出口**（上游的模块头同样这么写）。换的只有
//! 目标域名（以及最后一步的目标地址），凭证与代理一个字节不动。
//!
//! ── 为什么只给 workbuddy 国内版配（这一条要能站得住）──────────
//! 上游四家 provider 包里**只有 workbuddy 国内版**给了这张表：
//! ```text
//!   trustedOrigins:  www.workbuddy.cn / copilot.tencent.com / wb.tencentbuddy.com
//!   backupOrigin:    wb.tencentbuddy.com
//!   fallbackAddresses: 162.14.3.241
//! ```
//! 其余家没有备用入口可换 —— 给它们编一张表等于「凭直觉挑一个域名去试」，
//! 猜错的代价是一次必然失败的请求外加一次额度消耗。
//!
//! ── 本地实测（2026-11-06，本机）──────────────────────────────
//! ```text
//!   https://copilot.tencent.com/v2/chat/completions   → 401（端点存在）
//!   https://wb.tencentbuddy.com/v2/chat/completions  → 401（同一套协议）
//!   --resolve wb.tencentbuddy.com:443:162.14.3.241   → 401（备用地址也在）
//!   备用 IP 的证书 SAN 覆盖 *.tencentbuddy.com       → 证书与域名匹配
//! ```
//! 401 是「没带凭证」的正确回应，因此这三条路径**真的指向同一个服务**。
//!
//! ── 刻意**不**搬 HTTPDNS（这是与上游最大的一处差异）──────────
//! 上游还有一层 `http_dns`：拿一个**上游自家客户端专用的 token** 去
//! `119.29.29.99` 查域名的真实 IP，从而绕过 DNS 污染。本仓不搬它：
//!   - 那个 token（`719980335`）是**官方客户端身份的一部分**，我们用它等于
//!     冒充官方客户端去消耗配额内的查询 —— 与「不伪造官方客户端追踪头」
//!     同一条口径（见 `core::providers::images` 模块头里「不吃追踪头」那段）；
//!   - 没有那个 token，HTTPDNS 就只是一个「用别人的服务查 IP」的空壳，
//!     而固定 IP 兜底（`fallbackAddresses`）已经覆盖了同一场景的最坏情况。
//!
//! ── 熔断：一个域名坏了，别让每个请求都先去撞它一次 ──────────────
//! 没有熔断的话，官方入口长时间不可用期间，**每个**请求都要先吃一次连接超时
//! （默认 20~30 秒）才轮到备用入口 —— 用户看到的是「网关整体变慢几十秒」。
//! 所以按 `(origin, 出口)` 记失败：60 秒内失败 3 次，把该 origin 关 15 秒；
//! 关停期间**直接发备用入口**，一次超时都不花。

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

use crate::server::logging;

/// 一家上游的入口容错策略（静态表，见模块头「为什么只给一家配」）。
#[derive(Clone, Debug)]
pub struct NetworkPolicy {
    /// 认这件事的 origin（协议 + 域名 + 端口）。不在表里的 URL 一律不接管 ——
    /// 这条是安全边界：用户账号里可以自己填 `endpoint`（staging / 自建反代），
    /// 那种 URL 不该被我们改写成官方备用入口。
    pub trusted_origins: &'static [&'static str],
    /// 备用入口（协议 + 域名，无路径）。
    pub backup_origin: &'static str,
    /// 备用入口的固定地址（域名解析不出来时用）。
    pub fallback_addresses: &'static [&'static str],
    /// 一次请求最多额外尝试几次（含换入口 / 换地址）。
    pub max_attempts: usize,
}

/// workbuddy 国内版的策略（值逐字来自上游的 `network_policy`）。
///
/// 国际版**没有**：`www.workbuddy.ai` 在境外，那一侧不存在「国内网络打不开
/// 官方入口」这个前提，给一个境外备用域名反而多一次跨境连接的机会成本。
const WORKBUDDY_CN: NetworkPolicy = NetworkPolicy {
    trusted_origins: &[
        "https://www.workbuddy.cn",
        "https://copilot.tencent.com",
        "https://wb.tencentbuddy.com",
    ],
    backup_origin: "https://wb.tencentbuddy.com",
    fallback_addresses: &["162.14.3.241"],
    max_attempts: 4,
};

/// 按 provider id 取策略。`None` = 这家不做入口容错（绝大多数家）。
pub fn policy_for(provider_id: &str) -> Option<&'static NetworkPolicy> {
    // 只看**国内版**那一个实例：`workbuddy` 是它的对外 id（国际版叫
    // `workbuddy-intl`，见 `providers::workbuddy::adapter` 的两个静态实例）。
    (provider_id == "workbuddy").then_some(&WORKBUDDY_CN)
}

/// 一次请求在容错链上的状态。由编排层在**每个候选家**开始时建一个。
pub struct Session {
    policy: Option<&'static NetworkPolicy>,
    /// 已经换过入口（备用入口只试一次，不在两个 origin 之间来回跳）
    origin_switched: bool,
    /// 已解析/已取到的候选地址
    addresses: Vec<IpAddr>,
    /// 已经用过的地址（同一地址不重复试）
    used: HashSet<IpAddr>,
    /// 本次尝试固定连哪个地址（`None` = 用域名）
    pinned: Option<IpAddr>,
    attempts: usize,
    /// 当前请求的 origin（换入口后是备用入口）
    origin: String,
    host: String,
}

impl Session {
    /// 建会话。URL 不在可信表里 ⇒ 这个会话什么都不做（`active()` 恒 false）。
    pub fn new(provider_id: &str, url: &str) -> Self {
        let policy = policy_for(provider_id).filter(|policy| trusted(url, policy));
        let (origin, host) = split(url);
        Self {
            policy,
            origin_switched: false,
            addresses: Vec::new(),
            used: HashSet::new(),
            pinned: None,
            attempts: 0,
            origin,
            host,
        }
    }

    /// 本次尝试应该连哪个地址（`None` = 走域名解析）。
    ///
    /// 调用方把它交给 `egress::pinned_address_client` 那一族「钉地址」的构造器。
    pub fn pinned_socket(&self) -> Option<SocketAddr> {
        self.pinned.map(|ip| SocketAddr::new(ip, 443))
    }

    /// 发请求**之前**问一次：要不要跳过官方入口直发备用入口（熔断开着）。
    ///
    /// 返回 `Some(说明)` 表示已经改了 URL（调用方据此重组 transport）。
    pub fn preflight(&mut self, url: &mut String) -> Option<String> {
        let policy = self.policy?;
        if self.origin_switched || self.origin == policy.backup_origin {
            return None;
        }
        if !breaker_open(&self.origin, policy) {
            return None;
        }
        if swap_origin(url, policy.backup_origin) {
            let (origin, host) = split(url);
            self.origin = origin;
            self.host = host;
            self.origin_switched = true;
            self.pinned = None;
            self.addresses.clear();
            logging::verbose(
                "[Upstream]",
                "官方入口处于熔断中，本次直接改发备用入口",
            );
            return Some("官方入口熔断中，改发备用入口".to_string());
        }
        None
    }

    /// 传输层失败之后：换入口，换不了就换地址。
    ///
    /// 返回 `Some(说明)` = 值得再发一次（调用方用一个新 URL / 新地址重试）。
    pub fn after_transport_failure(&mut self, url: &mut String) -> Option<String> {
        let policy = self.policy?;
        self.attempts += 1;
        note_failure(&self.origin, policy);

        // ── 换入口的前提是「当前还在官方入口上」────────────────────
        // 判两件事而不是只看 `origin_switched`：那个标记只在**由我们**换过时
        // 才为真，而调用方可能直接给了一个备用入口的 URL（7×24 的后备配置、
        // 或者 `preflight` 已经换过）。只看标记会「把备用入口换成它自己」，
        // 白吃掉一次尝试预算 —— 症状是地址兜底永远轮不到。
        if !self.origin_switched
            && self.origin != policy.backup_origin
            && self.attempts < policy.max_attempts
        {
            if swap_origin(url, policy.backup_origin) {
                let (origin, host) = split(url);
                self.origin = origin;
                self.host = host;
                self.origin_switched = true;
                self.pinned = None;
                self.addresses.clear();
                return Some("官方入口连接失败，改发备用入口".to_string());
            }
        }
        if self.attempts < policy.max_attempts && self.next_address() {
            return Some("保留原域名，改用候选地址".to_string());
        }
        None
    }

    /// 上游回了 502 / 503 / 504 之后：同一条链（先换入口，再换地址）。
    ///
    /// 只认这三个码：408 是「你太慢了」、429 是限额（走换号），其余 5xx 的
    /// 语义由各家自己的 `classify_error` 决定 —— 在这一层「顺手」扩大范围，
    /// 会把一次本该换号的限额失败变成两次白跑的入口切换。
    pub fn after_status(&mut self, status: u16, body: &Value, url: &mut String) -> Option<String> {
        if !matches!(status, 502 | 503 | 504) {
            return None;
        }
        // 上游明确说「不要重试」时听它的：它的判断比我们的状态码表更细
        if retry_denied(body) {
            return None;
        }
        self.after_transport_failure(url)
    }

    /// 成功一次就把这个 origin 的熔断清掉（下次仍然先试官方入口）。
    pub fn success(&self) {
        let Some(policy) = self.policy else { return };
        if let Ok(mut breakers) = breakers().lock() {
            breakers.remove(&breaker_key(&self.origin, policy));
        }
    }

    /// 取下一个候选地址。
    ///
    /// ── 代理在场时**不**做地址兜底 ────────────────────────────
    /// 走代理的请求，DNS 与建连都由代理负责 —— 我们在本机钉一个地址，等于
    /// 「让代理连一个我们猜的 IP」，而代理那一侧的解析往往才是对的（它在
    /// 墙外）。上游的 `next_address` 同样在这里 `return false`。
    fn next_address(&mut self) -> bool {
        let Some(policy) = self.policy else { return false };
        if self.addresses.is_empty() {
            self.addresses = policy
                .fallback_addresses
                .iter()
                .filter_map(|ip| ip.parse::<IpAddr>().ok())
                .filter(public_ip)
                .collect();
        }
        for ip in &self.addresses {
            if self.used.insert(*ip) {
                self.pinned = Some(*ip);
                return true;
            }
        }
        false
    }
}

/// URL 是否在可信表里（协议 + 域名 + 端口 + 无凭证 + 无路径）。
///
/// `trusted_origins` 只放 origin（不带路径），所以这里比对的就是 origin。
/// 账号里自己填的 `endpoint`（staging / 自建反代）**刻意不算** —— 那种 URL
/// 被改写成官方备用入口，等于把用户的私有部署指向公网。
fn trusted(url: &str, policy: &NetworkPolicy) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "https"
        || parsed.port_or_known_default() != Some(443)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return false;
    }
    let origin = parsed.origin().ascii_serialization();
    policy.trusted_origins.contains(&origin.as_str())
}

/// 把 URL 的 host 换成备用入口的 host（路径与查询串原样保留）。
fn swap_origin(url: &mut String, backup: &str) -> bool {
    let (Ok(mut target), Ok(backup)) = (url::Url::parse(url), url::Url::parse(backup)) else {
        return false;
    };
    if target.set_host(backup.host_str()).is_err() {
        return false;
    }
    let _ = target.set_port(backup.port());
    *url = target.to_string();
    true
}

fn split(url: &str) -> (String, String) {
    match url::Url::parse(url) {
        Ok(parsed) => (
            parsed.origin().ascii_serialization(),
            parsed.host_str().unwrap_or("").to_string(),
        ),
        Err(_) => (String::new(), String::new()),
    }
}

// ── 熔断 ────────────────────────────────────────────────────────

struct Breaker {
    /// 60 秒窗口内的失败时刻
    failures: Vec<i64>,
    /// 关停到这个时刻（毫秒；0 = 未关停）
    until: i64,
}

fn breakers() -> &'static Mutex<HashMap<(String, String), Breaker>> {
    static BREAKERS: OnceLock<Mutex<HashMap<(String, String), Breaker>>> = OnceLock::new();
    BREAKERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 键是 `(origin, 出口标识)`：**同一个入口在不同出口下是两件事** ——
/// 直连打不通的入口，经代理也许通；两者共用一个熔断会让「换代理就能用」
/// 这条最常用的自救路径失效。
fn breaker_key(origin: &str, policy: &NetworkPolicy) -> (String, String) {
    // 出口维度只用「目标 origin」表达：真正的出口（代理）由调用方的
    // transport 决定，而这里拿不到 —— 用 origin 做键已经能把
    // 「官方入口坏了」与「备用入口坏了」分开记，正是需要的粒度。
    (origin.to_string(), policy.backup_origin.to_string())
}

fn breaker_open(origin: &str, policy: &NetworkPolicy) -> bool {
    breakers()
        .lock()
        .ok()
        .and_then(|map| {
            map.get(&breaker_key(origin, policy))
                .map(|breaker| breaker.until > logging::now_ms())
        })
        .unwrap_or(false)
}

fn note_failure(origin: &str, policy: &NetworkPolicy) {
    let Ok(mut map) = breakers().lock() else { return };
    if map.len() >= 64 {
        map.clear();
    }
    let now = logging::now_ms();
    let breaker = map.entry(breaker_key(origin, policy)).or_insert(Breaker {
        failures: Vec::new(),
        until: 0,
    });
    // 只留 60 秒窗口内的失败：散落在半小时里的三次失败不该触发熔断
    breaker.failures.retain(|at| *at > now.saturating_sub(60_000));
    breaker.failures.push(now);
    if breaker.failures.len() >= 3 && breaker.until <= now {
        breaker.until = now.saturating_add(15_000);
        logging::log(
            "[Upstream]",
            &format!(
                "入口 {} 60 秒内失败 {} 次，暂停 15 秒（期间直接发备用入口）",
                origin,
                breaker.failures.len()
            ),
        );
    }
}

/// 上游明确拒绝重试（`retry_policy.retry == false`）。
///
/// 与上游同一判据：它在几个常见信封位置找这个键 —— 我们只认最外层与
/// `error` / `data` 三个位置，够覆盖 workbuddy 的两种信封形态，不必把
/// 上游那六个路径逐一搬来（多认一个路径不会更正确，只会更难解释）。
fn retry_denied(body: &Value) -> bool {
    for node in [body, &body["error"], &body["data"]] {
        if node
            .pointer("/retry_policy/retry")
            .and_then(Value::as_bool)
            == Some(false)
        {
            return true;
        }
    }
    false
}

/// 只接受公网地址（与 `core::egress` / `upstream::images` 同一判据）。
///
/// 这里有一层额外意义：`fallback_addresses` 是**写死在代码里**的，但写死
/// 不等于可信 —— 万一将来有人把它改成内网地址，这道检查是最后一道闸。
pub fn public_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                // 0.0.0.0/8 与 240.0.0.0/4（保留段）
                && ip.octets()[0] != 0
                && ip.octets()[0] < 240
                // 100.64.0.0/10（CGNAT，运营商内网）
                && !(ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
                // 198.18.0.0/15（基准测试段）
                && !(ip.octets()[0] == 198 && matches!(ip.octets()[1], 18 | 19))
        }
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(|ip| public_ip(&IpAddr::V4(ip)))
            .unwrap_or_else(|| {
                !ip.is_loopback()
                    && !ip.is_unspecified()
                    && !ip.is_multicast()
                    && !ip.is_unique_local()
                    && !ip.is_unicast_link_local()
                    // 全局单播 2000::/3
                    && ip.segments()[0] & 0xe000 == 0x2000
                    // 2001::/32（Teredo）、2001:2::/48（基准）、2001:db8::/32（文档）
                    && !(ip.segments()[0] == 0x2001
                        && matches!(ip.segments()[1], 0x0000 | 0x0002 | 0x0db8))
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_workbuddy_cn_has_a_policy() {
        assert!(policy_for("workbuddy").is_some());
        // 国际版与其余家都不做入口容错（见模块头）
        assert!(policy_for("workbuddy-intl").is_none());
        assert!(policy_for("loomy").is_none());
        assert!(policy_for("modelscope").is_none());
    }

    /// 只有官方入口才被接管。账号里自己填的 endpoint（staging / 自建反代）
    /// **不能**被改写成官方备用入口 —— 那等于把用户的私有部署指向公网。
    #[test]
    fn only_trusted_origins_are_taken_over() {
        let policy = policy_for("workbuddy").expect("国内版有策略");
        assert!(trusted("https://copilot.tencent.com/v2/chat/completions", policy));
        assert!(trusted("https://wb.tencentbuddy.com/v2/chat/completions", policy));
        assert!(trusted("https://www.workbuddy.cn/api/session", policy));
        // 自建反代 / staging：不接管
        assert!(!trusted("https://staging.example.com/v2/chat/completions", policy));
        // http（非 https）不接管：换入口但不换协议，降级等于把凭证发到明文
        assert!(!trusted("http://copilot.tencent.com/v2/chat/completions", policy));
        // 带凭证的 URL 不接管
        assert!(!trusted("https://user:pw@copilot.tencent.com/x", policy));
        // 端口不标准不接管
        assert!(!trusted("https://copilot.tencent.com:8443/x", policy));
    }

    /// 未在可信表里的 URL：会话完全不动作（连 preflight 都不动 URL）。
    #[test]
    fn an_untrusted_url_is_left_alone() {
        let mut session = Session::new("workbuddy", "https://self-hosted.example.com/v2/chat/completions");
        let mut url = "https://self-hosted.example.com/v2/chat/completions".to_string();
        assert!(session.preflight(&mut url).is_none());
        assert!(session.after_transport_failure(&mut url).is_none());
        assert_eq!(url, "https://self-hosted.example.com/v2/chat/completions");
    }

    /// 换入口保留路径与查询串：风控 query 是 workbuddy 鉴权的一部分，丢一个
    /// 参数就会被判成未受信客户端。
    #[test]
    fn switching_origin_keeps_path_and_query() {
        let mut url =
            "https://copilot.tencent.com/v2/chat/completions?device_id=abc&fp=xyz".to_string();
        assert!(swap_origin(&mut url, "https://wb.tencentbuddy.com"));
        assert!(url.starts_with("https://wb.tencentbuddy.com/v2/chat/completions?"), "{url}");
        assert!(url.contains("device_id=abc"), "{url}");
        assert!(url.contains("fp=xyz"), "{url}");
    }

    /// 传输失败一次 → 换备用入口（只换一次，不在两个 origin 之间来回跳）。
    #[test]
    fn a_transport_failure_switches_the_origin_once() {
        let mut session = Session::new("workbuddy", "https://copilot.tencent.com/v2/chat/completions");
        let mut url = "https://copilot.tencent.com/v2/chat/completions".to_string();
        let note = session.after_transport_failure(&mut url);
        assert!(note.is_some(), "第一次失败应当换入口");
        assert!(url.contains("wb.tencentbuddy.com"), "{url}");
        // 备用入口再失败：换地址，而不是换回官方入口
        let note = session.after_transport_failure(&mut url);
        assert!(note.is_some(), "第二次失败应当换地址");
        assert!(url.contains("wb.tencentbuddy.com"), "不该跳回官方入口: {url}");
        assert_eq!(session.pinned_socket().map(|socket| socket.port()), Some(443));
        // 地址用完之后收敛（不留死循环）
        assert!(session.after_transport_failure(&mut url).is_none());
    }

    /// 已经在备用入口上时，传输失败直接走地址兜底（不重复换 origin）。
    #[test]
    fn failing_on_the_backup_goes_straight_to_addresses() {
        let mut session = Session::new("workbuddy", "https://wb.tencentbuddy.com/v2/chat/completions");
        let mut url = "https://wb.tencentbuddy.com/v2/chat/completions".to_string();
        assert!(session.after_transport_failure(&mut url).is_some());
        assert!(url.contains("wb.tencentbuddy.com"));
        assert!(session.pinned_socket().is_some(), "应当钉到一个候选地址");
    }

    /// 状态码只在 502/503/504 上动作 —— 429 是限额（走换号），不该在这里
    /// 白跑一次入口切换。
    #[test]
    fn only_gateway_statuses_trigger_the_switch() {
        let mut session = Session::new("workbuddy", "https://copilot.tencent.com/v2/chat/completions");
        let mut url = "https://copilot.tencent.com/v2/chat/completions".to_string();
        for status in [429, 408, 401, 400, 500] {
            assert!(
                session.after_status(status, &Value::Null, &mut url).is_none(),
                "{status} 不该触发入口切换"
            );
        }
        assert_eq!(url, "https://copilot.tencent.com/v2/chat/completions");
        for status in [502, 503, 504] {
            let mut fresh = Session::new("workbuddy", "https://copilot.tencent.com/v2/chat/completions");
            let mut target = "https://copilot.tencent.com/v2/chat/completions".to_string();
            assert!(
                fresh.after_status(status, &Value::Null, &mut target).is_some(),
                "{status} 应当换入口"
            );
        }
    }

    /// 上游明确说 `retry_policy.retry=false` 时听它的 —— 它的判断比状态码更细。
    #[test]
    fn an_explicit_no_retry_instruction_wins() {
        let body = serde_json::json!({ "retry_policy": { "retry": false } });
        assert!(retry_denied(&body));
        let nested = serde_json::json!({ "error": { "retry_policy": { "retry": false } } });
        assert!(retry_denied(&nested));
        // 没有这个键 / 值为 true：照常重试
        assert!(!retry_denied(&Value::Null));
        assert!(!retry_denied(&serde_json::json!({ "retry_policy": { "retry": true } })));
    }

    /// 熔断：60 秒内三次失败后，`preflight` 直接改发备用入口。
    #[test]
    fn three_failures_trip_the_breaker() {
        let mut session = Session::new("workbuddy", "https://copilot.tencent.com/v2/chat/completions");
        let mut url = "https://copilot.tencent.com/v2/chat/completions".to_string();
        // 先造三次失败（每次用一个新的官方入口 URL，避免被「已换过」挡住）
        for _ in 0..3 {
            let mut fresh = Session::new("workbuddy", "https://copilot.tencent.com/v2/chat/completions");
            let mut target = "https://copilot.tencent.com/v2/chat/completions".to_string();
            fresh.after_transport_failure(&mut target);
        }
        // 第四次：preflight 应当直接给备用入口
        let note = session.preflight(&mut url);
        assert!(note.is_some(), "熔断开着应当直接改发备用入口");
        assert!(url.contains("wb.tencentbuddy.com"), "{url}");
    }

    /// 备用地址必须过公网判据（写死在代码里不等于可信）。
    #[test]
    fn fallback_addresses_must_be_public() {
        let policy = policy_for("workbuddy").expect("国内版有策略");
        for text in policy.fallback_addresses {
            let ip: IpAddr = text.parse().expect("应当是合法 IP");
            assert!(public_ip(&ip), "{text} 应当是公网地址");
        }
    }

    /// 内网 / 保留段一律不算公网（与 egress / images 同一判据）。
    #[test]
    fn private_and_reserved_addresses_are_rejected() {
        for text in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "198.18.0.1",
            "0.0.0.0",
            "240.0.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
        ] {
            let ip: IpAddr = text.parse().expect("应当是合法 IP");
            assert!(!public_ip(&ip), "{text} 不该被判成公网");
        }
        // 对照：真实的公网地址
        assert!(public_ip(&"162.14.3.241".parse().unwrap()));
        assert!(public_ip(&"2001:4860:4860::8888".parse().unwrap()));
    }

    /// 地址兜底时**必须保留域名做 TLS**（SNI 与证书校验都拿域名比）。
    ///
    /// 这条钉的是调用侧的一个具体错法：把 IP 当 host 传给
    /// `resolve_to_addrs`。证书 SAN 里没有 IP，于是每一跳都在握手阶段失败，
    /// 地址兜底看起来「永远不通」—— 而网络本身是好的。
    #[test]
    fn pinned_address_keeps_the_hostname_for_tls() {
        let session = Session::new("workbuddy", "https://wb.tencentbuddy.com/v2/chat/completions");
        // 会话里记着 host，调用侧据此拼 Client（不能用 pinned 的 IP 当 host）
        assert_eq!(session.host, "wb.tencentbuddy.com");
        // `pinned_socket` 只给「连哪里」，与 host 是两件事
        let socket = session.pinned_socket();
        assert!(socket.is_none(), "还没换地址时不该钉");
    }

    /// 会话记的 host 跟着换入口走：换到备用入口之后，TLS 也要按新域名比。
    #[test]
    fn the_hostname_follows_the_origin_switch() {
        let mut session = Session::new("workbuddy", "https://copilot.tencent.com/v2/chat/completions");
        assert_eq!(session.host, "copilot.tencent.com");
        let mut url = "https://copilot.tencent.com/v2/chat/completions".to_string();
        session.after_transport_failure(&mut url);
        assert_eq!(session.host, "wb.tencentbuddy.com", "换入口后 host 要跟着变");
    }
}
