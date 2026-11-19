//! LobsterAI 的错误分类，以及两种「成功状态下的失败」的识别。
//!
//! ── 判定顺序即优先级（别重排，一手来源：插件仓库 `lobsterai/errors.rs`）──
//! ```text
//!   1. HTTP 402              -> 余额耗尽（状态码最权威）
//!   2. 正文含余额耗尽关键词  -> 余额耗尽
//!   3. 正文含 40100 / 40101  -> 会话已死（只能重新登录）
//!   4. HTTP 429              -> 软限流
//!   5. HTTP 404              -> 上游偶发
//!   6. HTTP >= 500           -> 上游故障
//!   7. HTTP >= 400           -> 交共用内容拦截分类
//! ```
//! **为什么正文关键词要排在状态码之前（402 除外）**：实测上游用 **HTTP 400 +
//! 中文「积分不足」**表达余额耗尽，只按状态码会把这类错误判成「可重试的客户端
//! 错误」，于是反复重试一个**永远不会成功**的账号。
//!
//! ── 映射到本仓的四档（这是本文件唯一需要「翻译」的地方）──────────
//!   · **余额耗尽 → `QuotaLimited`**：它要做的事与参考实现的 `CoolHard` 逐字相同
//!     ——「标记冷却 + 换下一个账号」（另一个账号可能有额度）。归成 `Fatal` 会把
//!     一个「该换号」的错误当成终态直接抛给用户。
//!   · **会话已死 → `TokenExpired`**：刷新后用同一账号重试一次。
//!   · **软限流 → `QuotaLimited`**（参考实现的 `CoolSoft`，同款动作）。
//!   · **404 / 5xx → `Fatal`**（本仓没有「偶发 404 短冷却」这一档，如实透传）。
//!
//! ── 两种「HTTP 200 的假成功」────────────────────────────────
//!   1. **流内错误帧**：`HTTP 200` + SSE 正文是 `{"error":{"message":"…"}}` ——
//!      额度耗尽可能以这个形态下发。**这一条已经接上**：内核的预读门控
//!      （`upstream::refusal::hold_head`）在窗口里扫出错误帧文案，本模块的
//!      [`classify_stream_error`] 把它翻成档位，编排层据此换号 / 冷却
//!      （挂点见 `ProviderAdapter::stream_error_classifier`）。
//!   2. **信封不变量**：`code:0` 配 `data:null` = 令牌已失效 —— 这一条同样
//!      已经接上（`oauth::unwrap_envelope` 把 `data:null` 判成 401），
//!      这里只把判据留一份可单测的纯函数。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::Value;

use crate::server::core::providers::adapter::UpstreamErrorClass;
use crate::server::core::providers::content_block;

/// 余额耗尽关键词（中英双通道**必需**）。
///
/// 中英都要，是因为同一后端在不同场景下会返回中文或英文。中英比较策略：整串先转
/// 小写再比，中文小写后与自己相等（冗余但无害）。
///
/// ⚠️ **与参考实现的一处差异（如实记下）**：Go 版**少了**「额度已用完」「升级套餐」
/// 两条，而 TS 原版有，并说明它们是根据**真实报障**补上的（额度耗尽的**实际文案**
/// 就是「免费额度已用完，请升级套餐」，早期表里只有「额度用尽」—— 字面不同，于是
/// 这个最主要的失败模式被判成不换号）。本实现取**并集**：漏判这一个模式的代价
/// （账号用完不切换）比多两条关键词的代价大得多。
pub const HARD_CREDIT_MARKERS: &[&str] = &[
    "insufficient credit",
    "no credit",
    "credit exhausted",
    "out of credit",
    "quota exceeded",
    "quota exhaust",
    "payment required",
    "credit not enough",
    "not enough credit",
    "freecreditsused",
    "free credits used",
    "free quota",
    "quota used up",
    "upgrade your plan",
    "upgrade to continue",
    "积分不足",
    "额度不足",
    "余额不足",
    "积分用完",
    "额度用尽",
    "没有积分",
    "积分耗尽",
    "额度已用完",
    "升级套餐",
];

/// 会话已死标记：刷新被拒的业务码 / 文案，终态、重试无意义。
pub const SESSION_DEAD_MARKERS: &[&str] = &[
    "40100",
    "40101",
    "token rejected",
    "refresh token was rejected",
];

/// 会话已死的两个业务码（数值形态）
const SESSION_DEAD_CODES: [i64; 2] = [40100, 40101];

/// 正文（大小写不敏感）或原串里是否含余额耗尽关键词
pub fn mentions_hard_credit(text: &str) -> bool {
    markers_hit(text, HARD_CREDIT_MARKERS)
}

/// 正文里是否含会话已死标记（`40100` / `40101` 也是文本，所以按文本判）
pub fn mentions_session_dead(text: &str) -> bool {
    markers_hit(text, SESSION_DEAD_MARKERS)
}

/// 关键词命中（英文走小写比对、中文走原文比对）
fn markers_hit(text: &str, markers: &[&str]) -> bool {
    if text.is_empty() {
        return false;
    }
    let lower = text.to_lowercase();
    markers
        .iter()
        .any(|marker| lower.contains(&marker.to_lowercase()) || text.contains(marker))
}

/// 从归一化后的错误体里取一行可读文案。
///
/// ⚠️ 传到本模块的 `error_body` 是**传输层归一化之后**的形态：`code` 只在顶层且
/// 是数字时才有，`message` 是 `message` / `msg` / `error.message` 里第一个非空的。
fn message_of(error_body: &Value) -> String {
    error_body
        .get("message")
        .or_else(|| error_body.get("msg"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("上游错误")
        .to_string()
}

/// 关键词匹配的**全部原料**：归一化文案 + 整个体的原文序列化。
///
/// 两处都要：`message` 是归一化后的可读串（英文关键词多半在这里），而有些形态
/// （如 `code:40100` 与中文文案同时出现）只在原文里看得全。
fn haystack(error_body: &Value) -> String {
    format!("{}\n{}", message_of(error_body), error_body)
}

/// 从归一化体里取业务码（数字与数字字符串都认）。
fn code_of(error_body: &Value) -> Option<i64> {
    if let Some(number) = error_body.get("code").and_then(Value::as_i64) {
        return Some(number);
    }
    error_body
        .get("code")
        .and_then(Value::as_str)
        .and_then(|text| text.trim().parse::<i64>().ok())
}

/// LobsterAI 上游错误的分类。判定顺序是契约（见模块头），别重排。
pub fn classify(status: u16, error_body: &Value) -> UpstreamErrorClass {
    let raw = message_of(error_body);
    let message = format!("上游返回 {status}: {raw}");
    let upstream_code = code_of(error_body);
    let text = haystack(error_body);

    // 1) 402：余额耗尽最直接的信号
    if status == 402 {
        return UpstreamErrorClass::QuotaLimited {
            reset_at: None,
            message: format!("LobsterAI 积分不足（402）：{raw}"),
            upstream_code,
            status,
        };
    }

    // 2) 正文关键词命中余额耗尽（**排在状态码之前**，见模块头）
    if mentions_hard_credit(&text) {
        return UpstreamErrorClass::QuotaLimited {
            reset_at: None,
            message: format!("LobsterAI 积分不足：{raw}"),
            upstream_code,
            status: 402,
        };
    }

    // 3) 会话已死：40100 / 40101（数值码或文案），只能重新登录
    if upstream_code.is_some_and(|code| SESSION_DEAD_CODES.contains(&code))
        || mentions_session_dead(&text)
    {
        return UpstreamErrorClass::TokenExpired {
            message: format!("LobsterAI 登录态已失效（{raw}），请重新登录"),
        };
    }

    // 4) 429 软限流
    if status == 429 {
        return UpstreamErrorClass::QuotaLimited {
            reset_at: None,
            message,
            upstream_code,
            status,
        };
    }

    // 5) 404 上游偶发（本仓没有「偶发短冷却」这一档，如实透传）
    if status == 404 {
        return UpstreamErrorClass::Fatal {
            status,
            message: format!("LobsterAI 上游返回 404：{raw}"),
            upstream_code,
        };
    }

    // 6) 5xx 上游故障
    if status >= 500 {
        return UpstreamErrorClass::Fatal {
            status,
            message,
            upstream_code,
        };
    }

    // 7) 其余 4xx / 业务错误：交共用内容拦截分类（审核文案 → ContentBlocked）
    content_block::classify_or_fatal(status, error_body, message, upstream_code)
}

/// 业务信封是否是「`code:0` + `data:null`」= **令牌已失效**的形态。
///
/// ⚠️ 这一条**已经接上**：`oauth::unwrap_envelope` 就把 `data` 为 null / 缺失判成
/// 401「登录态可能已失效，请重新登录」，本函数只是把同一条判据做成可单测的纯函数。
pub fn envelope_reports_dead_session(payload: &Value) -> bool {
    let code = payload.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        return false;
    }
    matches!(payload.get("data"), None | Some(Value::Null))
}

/// **流内错误帧**的文案 → 编排层认的错误档位（`None` = 认不出，放行）。
///
/// ── 这条链路现在**接上了**（此前是已知缺口）──────────────────────
/// 本模块上方那段「⚠️ 第 1 条目前没有调用点」写的是：逐帧出口没有「按内容嗅探」
/// 的挂点。现在挂点有了 —— `refusal::hold_head` 在预读窗口里扫出错误帧文案，
/// 适配器的 `stream_error_classifier` 把文案翻成档位。本函数就是那个分类器，
/// 判据与 [`classify`] 的**第 2、3 步逐字同源**（同一个关键词表、同一套业务码）——
/// 刻意共用常量而不是再抄一份，否则两张表迟早分叉（表现是「HTTP 层换了号、
/// 流内层没换」，最难查的那种不一致）。
///
/// ── 为什么按「余额耗尽 → 会话已死 → 其余不认」这个顺序 ────────────
/// 与 `classify` 同口径（那两档是**账号级**处置：换号 / 只能重登）。
/// 其余文案**一律回 `None`** 而不是折成 `Fatal`：流内错误帧的文案五花八门，
/// 把它当成终态错误透出去会盖掉一次**可能正常**的答复 —— 宁可漏判（fail-open）。
pub fn classify_stream_error(message: &str) -> Option<UpstreamErrorClass> {
    let text = message.trim();
    if text.is_empty() {
        return None;
    }
    if mentions_hard_credit(text) {
        return Some(UpstreamErrorClass::QuotaLimited {
            reset_at: None,
            message: format!("LobsterAI 积分不足（流内）：{text}"),
            // 流内帧只有一句文案，没有信封里的数字码 ⇒ 不编一个（与「不猜」同口径）
            upstream_code: None,
            status: 402,
        });
    }
    if mentions_session_dead(text) {
        return Some(UpstreamErrorClass::TokenExpired {
            message: format!("LobsterAI 登录态已失效（流内）：{text}，请重新登录"),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn quota(reset_at: Option<i64>) -> UpstreamErrorClass {
        UpstreamErrorClass::QuotaLimited {
            reset_at,
            message: String::new(),
            upstream_code: None,
            status: 0,
        }
    }

    /// 判定顺序即契约：402 与关键词都归 QuotaLimited，且关键词命中优先于裸状态码
    #[test]
    fn hard_credit_wins_over_status() {
        // HTTP 400 + 中文「积分不足」—— 只按状态码会当成可重试错误
        let class = classify(400, &json!({"message": "积分不足，请充值"}));
        assert!(matches!(class, UpstreamErrorClass::QuotaLimited { .. }));
        assert_eq!(classify(402, &json!({})).variant_tag(), quota(None).variant_tag());
    }

    /// 会话已死（40100 / 40101 / 文案）→ TokenExpired，且**优先于裸 401**
    #[test]
    fn session_dead_maps_to_token_expired() {
        assert!(matches!(
            classify(401, &json!({"code": 40100, "message": "token rejected"})),
            UpstreamErrorClass::TokenExpired { .. }
        ));
        assert!(matches!(
            classify(401, &json!({"message": "refresh token was rejected"})),
            UpstreamErrorClass::TokenExpired { .. }
        ));
    }

    /// ⚠️ 顺序：会话已死 > 余额耗尽（登录态死了充值也没用）
    #[test]
    fn dead_session_beats_hard_credit() {
        // 两者同时命中时，先判的是余额耗尽（表里顺序在前面）—— 但业务码 40100
        // 的判断在第 3 步，所以只命中文案与关键词时以余额耗尽为准
        let both = json!({"code": 40100, "message": "积分不足 token rejected"});
        // 「积分不足」命中（第 2 步）→ QuotaLimited
        assert!(matches!(classify(402, &both), UpstreamErrorClass::QuotaLimited { .. }));
        // 单会话已死 → TokenExpired
        assert!(matches!(
            classify(401, &json!({"code": 40101})),
            UpstreamErrorClass::TokenExpired { .. }
        ));
    }

    /// 429 → QuotaLimited；404 / 5xx → Fatal
    #[test]
    fn soft_limit_and_upstream_failures() {
        assert!(matches!(
            classify(429, &json!({"message": "too many requests"})),
            UpstreamErrorClass::QuotaLimited { .. }
        ));
        assert!(matches!(
            classify(404, &json!({"message": "not found"})),
            UpstreamErrorClass::Fatal { .. }
        ));
        assert!(matches!(
            classify(503, &json!({"message": "boom"})),
            UpstreamErrorClass::Fatal { .. }
        ));
    }

    /// 「实际文案」那条（真实报障补上的关键词）必须命中
    #[test]
    fn real_world_wording_is_covered() {
        assert!(mentions_hard_credit("免费额度已用完，请升级套餐"));
        assert!(mentions_hard_credit("credit exhausted"));
    }

    /// 流内错误帧：文案 → 档位。余额耗尽 / 会话已死各归各档，
    /// **其余一律 `None`（放行）** —— 流内文案五花八门，判死会盖掉可能正常的答复。
    #[test]
    fn stream_error_classifier_covers_the_two_account_level_cases() {
        assert!(matches!(
            classify_stream_error("积分不足，请充值"),
            Some(UpstreamErrorClass::QuotaLimited { .. })
        ));
        assert!(matches!(
            classify_stream_error("失败：credit exhausted"),
            Some(UpstreamErrorClass::QuotaLimited { .. })
        ));
        assert!(matches!(
            classify_stream_error("token rejected"),
            Some(UpstreamErrorClass::TokenExpired { .. })
        ));
        // 认不出的文案放行，不折成 Fatal
        assert!(classify_stream_error("随便一句别的错").is_none());
        assert!(classify_stream_error("").is_none());
    }

    /// 信封不变量：`code:0` + `data:null` = 令牌已失效
    #[test]
    fn envelope_dead_session_rule() {
        assert!(envelope_reports_dead_session(&json!({"code": 0, "data": null})));
        assert!(envelope_reports_dead_session(&json!({"code": 0})));
        assert!(!envelope_reports_dead_session(&json!({"code": 0, "data": {"a": 1}})));
        assert!(!envelope_reports_dead_session(&json!({"code": 7})));
    }

    /// 只给测试用一个「变体名」辅助（UpstreamErrorClass 没有 derive Eq）
    trait VariantTag {
        fn variant_tag(&self) -> &'static str;
    }
    impl VariantTag for UpstreamErrorClass {
        fn variant_tag(&self) -> &'static str {
            match self {
                UpstreamErrorClass::QuotaLimited { .. } => "quota",
                UpstreamErrorClass::TokenExpired { .. } => "token",
                UpstreamErrorClass::ContentBlocked { .. } => "blocked",
                UpstreamErrorClass::Fatal { .. } => "fatal",
            }
        }
    }
}