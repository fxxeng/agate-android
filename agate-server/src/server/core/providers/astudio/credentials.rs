//! AStudio 凭据：两套令牌的读写、Cookie 头、公开形态与身份。
//!
//! ── 落盘字段 ────────────────────────────────────────────────
//! ```text
//!   key             modelBearerToken（调模型；本家**没有**续期）
//!   uid             账号 uid（展示与去重）
//!   source          paste / login（谁加进来的）
//!   session         { token, accountId, ssoSessionId? }（网页会话）
//! ```
//! `session` 是**嵌套对象**而不是三个平铺键：这两套凭据各管一段（模型 vs
//! 余额/签到），嵌套让「有没有网页会话」成为一个一眼可判的结构，而不是三个
//! 必须一起检查的字段。读取时平铺写法也认（见 `from_fields`）。
//!
//! ── 为什么半套会话不落盘 ────────────────────────────────────
//! 账号面接口要 `token` **且** `accountId`（`cookie_header` 两个都拼）。只落
//! 一半的会话在功能上等价于没有，却会让「有没有会话」的判据变成「三个字段
//! 里至少两个非空」这种说不清的东西。所以 `write_into` 只在两者都有时写
//! `session`，其余情况靠公开视图的 `noSession` 如实告诉用户「余额与签到用
//! 不了」，并说明补哪些字段。
//!
//! ── Cookie 头为什么这么怪 ──────────────────────────────────
//! `ssoSessionId` 要发**两遍**（驼峰 + 全小写）：桌面端 `buildAstronCookieHeader`
//! 就是这么写的，上游按哪个名字读不确定，两个都发是唯一稳妥的做法。顺序也照
//! 抄（`ssoSessionId` → `account_id` → `token`）—— Cookie 不该有顺序语义，但
//! 抄错一个字母就是一个 401。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{Map, Value};

use crate::server::core::account_store::MAX_TOKEN_LENGTH;
use crate::server::errors::GatewayError;

/// 令牌类字段的别名表（`from_fields` 与 `normalize_paste` 共用一份）
const KEY_KEYS: [&str; 4] = ["key", "modelBearerToken", "model_bearer_token", "apiKey"];

fn text(fields: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(value) = fields.get(*key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

/// 去掉粘贴时常带的外壳（`Bearer ` / 整行 `Authorization: Bearer …`）。
fn bare_token(value: &str) -> String {
    let mut current = value.trim();
    if let Some(rest) = current.strip_prefix("Authorization:") {
        current = rest.trim();
    }
    for prefix in ["Bearer ", "bearer ", "BEARER "] {
        if let Some(rest) = current.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    }
    current.to_string()
}

/// 一份 AStudio 凭据。
#[derive(Clone, Debug, Default)]
pub struct Credentials {
    /// modelBearerToken（调模型）
    pub key: String,
    /// 账号 uid（展示与去重）
    pub uid: String,
    /// 网页会话 token
    pub token: String,
    /// 账号面 accountId（与 `token` 成对）
    pub account_id: String,
    /// 网页会话的补充材料（桌面端有就带）
    pub sso_session_id: String,
    /// `paste` / `login`
    pub source: String,
}

impl Credentials {
    /// 从账号记录（或任意字段对象）读凭据。
    ///
    /// 认三种写法，**同一份**判据：
    ///   1. 本项目落盘形态：`key` + 嵌套 `session{token, accountId, ssoSessionId}`；
    ///   2. 平铺形态：`token` / `accountId` / `ssoSessionId` 直接在顶层
    ///      （登录页与粘贴入口给的就是这个）；
    ///   3. 桌面端的 `astron-session.json` 原文：`modelBearerToken` +
    ///      `token` + `accountId` + `uid` + `ssoSessionId`。
    pub fn from_fields(fields: &Value) -> Result<Self, GatewayError> {
        let session = fields.get("session").cloned().unwrap_or(Value::Null);
        // 嵌套优先、平铺兜底：落盘形态是嵌套的，粘贴进来的是平铺的。
        // 反过来（平铺优先）会让「记录里同时有两者」时读到旧值 —— 旧记录
        // 升级后没人会删顶层残留，那就会一直读错的。
        let pick_session = |keys: &[&str]| -> String {
            let nested = text(&session, keys);
            if nested.is_empty() {
                text(fields, keys)
            } else {
                nested
            }
        };
        let credentials = Self {
            key: bare_token(&text(fields, &KEY_KEYS)),
            uid: text(fields, &["uid"]),
            // 登录弹窗把网页会话叫 `sessionToken`，落盘叫 `token`：两个都认
            // （否则「弹窗加账号」这条路会静默丢掉整个网页会话）
            token: pick_session(&["token", "sessionToken"]),
            account_id: pick_session(&["accountId", "account_id"]),
            sso_session_id: pick_session(&["ssoSessionId", "sso_sessionid"]),
            source: text(fields, &["source"]),
        };
        if credentials.key.is_empty() {
            return Err(GatewayError::with_status(
                400,
                "AStudio 账号没有模型凭据：请粘贴 modelBearerToken",
            ));
        }
        Ok(credentials)
    }

    /// 写回记录（**只写自己认识的键**，调用方负责保留未知字段）
    ///
    /// 空值不落：`uid` / `source` 缺省就是「没有」，写空串会让读取一族的
    /// 「取第一个非空」白跑一遍，也让记录看起来更脏（与 kiro 的 `write_into`
    /// 同一口径）。
    pub fn write_into(&self, record: &mut Map<String, Value>) {
        record.insert("key".to_string(), Value::String(self.key.clone()));
        if !self.uid.is_empty() {
            record.insert("uid".to_string(), Value::String(self.uid.clone()));
        }
        if !self.source.is_empty() {
            record.insert("source".to_string(), Value::String(self.source.clone()));
        }
        // 半套会话不落（见模块头）
        if !self.token.is_empty() && !self.account_id.is_empty() {
            let mut session = Map::new();
            session.insert("token".to_string(), Value::String(self.token.clone()));
            session.insert(
                "accountId".to_string(),
                Value::String(self.account_id.clone()),
            );
            if !self.sso_session_id.is_empty() {
                session.insert(
                    "ssoSessionId".to_string(),
                    Value::String(self.sso_session_id.clone()),
                );
            }
            record.insert("session".to_string(), Value::Object(session));
        }
    }

    /// 有网页会话（余额与签到要它）
    pub fn has_session(&self) -> bool {
        !self.token.is_empty() && !self.account_id.is_empty()
    }

    /// 账号面 Cookie 头（顺序与重复项都照桌面端，见模块头）
    pub fn cookie_header(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.sso_session_id.is_empty() {
            parts.push(format!("ssoSessionId={}", self.sso_session_id));
            parts.push(format!("sso_sessionid={}", self.sso_session_id));
        }
        if !self.account_id.is_empty() {
            parts.push(format!("account_id={}", self.account_id));
        }
        if !self.token.is_empty() {
            parts.push(format!("token={}", self.token));
        }
        parts.join("; ")
    }

    /// 日志用的脱敏形态（只露尾 4 位）
    pub fn masked(&self) -> String {
        let chars: Vec<char> = self.key.chars().collect();
        if chars.is_empty() {
            return "（空）".to_string();
        }
        let start = chars.len().saturating_sub(4);
        format!("…{}", chars[start..].iter().collect::<String>())
    }
}

/// 去重身份：`uid` 优先，其次 `accountId`，最后是 key 的哈希。
///
/// ── 为什么 uid 优先 ────────────────────────────────────────
/// `modelBearerToken` 会变（重新登录就换一枚），拿它当身份会让同一个人每重登
/// 一次就多一条账号记录。uid / accountId 是人身上的标识，换令牌不换人。
///
/// ⚠️ 这个函数**同时**被 `account_transfer::identity` 用（导入判重）。两处用
/// 同一个函数是刻意的：各写一份必然分叉，症状是「同一个账号导两次多出一条」。
pub fn identity_of(credentials: &Credentials) -> String {
    if !credentials.uid.is_empty() {
        return format!("uid:{}", credentials.uid);
    }
    if !credentials.account_id.is_empty() {
        return format!("account:{}", credentials.account_id);
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(credentials.key.as_bytes());
    let hex: String = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("key:{hex}")
}

/// 从任意字段对象算身份（读不出凭据也照样能给出 `key:` 之外的身份）
pub fn identity(fields: &Value) -> String {
    // `unwrap_or_default()` 不是 `unwrap()`：没有 panic 路径（本文件零 panic）
    identity_of(&Credentials::from_fields(fields).unwrap_or_default())
}

/// 把任一别名上的令牌搬到落盘名 `key`（已经搬过就什么都不做）。
///
/// 单独成函数是因为它要跑两次：一次在解 JSON **之前**（保住用户亲手填的值），
/// 一次在解 JSON **之后**（整段 session 文件里只有一个 `modelBearerToken`）。
/// 两次都靠「`key` 已经非空就跳过」保证先来后到 —— 用户填的永远优先。
fn promote_key(fields: &mut Map<String, Value>) {
    if fields
        .get("key")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
    {
        return;
    }
    for alias in KEY_KEYS {
        let candidate = fields
            .get(alias)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if let Some(candidate) = candidate {
            fields.insert("key".to_string(), Value::String(bare_token(&candidate)));
            return;
        }
    }
}

/// 把「用户随手粘的一大段」归一成可落库的字段对象（纯函数，无网络）。
///
/// ── 认四种粘法（按**内容**判，不按字段名）────────────────────
///   1. 整段 `astron-session.json`（`modelBearerToken` + `token` + `accountId`）；
///   2. 一枚裸 `modelBearerToken`；
///   3. 带 `Bearer ` 外壳的整行请求头；
///   4. 动态登录方式给的 `{ "inputs": { … } }` 包装（上游弹窗就是这么发的）。
/// 与 Tabbit / Kiro 同一条纪律：粘贴场景里字段名最不可靠，按内容认。
///
/// `banned` 为真时**明确拒绝**：上游确实会标记被封禁的账号，把一条注定失败的
/// 记录收进来只会让用户以为是我们的问题。
pub fn normalize_paste(payload: &Value) -> Result<Value, GatewayError> {
    let Some(object) = payload.as_object() else {
        return Err(GatewayError::with_status(400, "上传内容不是 JSON 对象"));
    };
    // ① 上传请求专用的键剥掉（`add_*_account` 会保留未知字段，留着只会让库里
    //    多一批与凭证无关的东西）
    let mut fields = Map::new();
    for (key, value) in object {
        let transport = matches!(key.as_str(), "provider" | "name" | "importDesktop")
            || key.starts_with("authFile");
        if !transport {
            fields.insert(key.clone(), value.clone());
        }
    }
    // ② 动态登录方式的 `{ inputs: {…} }` 包装
    if let Some(Value::Object(inputs)) = fields.get("inputs").cloned() {
        for (key, value) in inputs {
            fields.entry(key).or_insert(value);
        }
        fields.remove("inputs");
    }
    // ③ 显式给出的令牌搬到落盘名 `key`（解 JSON 之前做，后面用 `or_insert`
    //    就不会把用户亲手填的值顶掉）
    promote_key(&mut fields);
    // ④ 整段 JSON 粘在某个字符串字段里 → 解出来补进去
    let mut consumed: Vec<String> = Vec::new();
    for (key, value) in fields.clone() {
        let Some(raw) = value.as_str() else { continue };
        let trimmed = raw.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        if let Ok(Value::Object(inner)) = serde_json::from_str::<Value>(trimmed) {
            for (inner_key, inner_value) in inner {
                fields.entry(inner_key).or_insert(inner_value);
            }
            consumed.push(key);
        }
    }
    for key in consumed {
        fields.remove(&key);
    }
    // ④' 解出来的字段里可能才有令牌（整段 `astron-session.json` 就是这种）：
    //     再搬一次。**必须**在 ⑤ 之前 —— 否则 ⑤ 的通用键兜底会把网页会话的
    //     `token` 当成模型凭据收下，症状是「账号加上了、一发请求就 401」。
    promote_key(&mut fields);
    // ⑤ 还是没令牌 → 只剩「一枚裸令牌粘在通用字段里」这一种可能
    //    ⚠️ 这里的候选键**不含** `token`：本家的 `token` 是网页会话令牌，
    //    不是模型凭据（两套凭据各管一段，见模块头）
    if !fields
        .get("key")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
    {
        for key in ["text", "value", "content", "paste"] {
            let candidate = fields
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            if let Some(candidate) = candidate {
                fields.insert("key".to_string(), Value::String(bare_token(&candidate)));
                fields.remove(key);
                break;
            }
        }
    }
    // ⑥ 封禁账号明确拒绝（见函数头）
    if fields.get("banned").and_then(Value::as_bool) == Some(true) {
        return Err(GatewayError::with_status(
            400,
            "这个 AStudio 账号已被上游封禁，不能添加",
        ));
    }
    fields.remove("banned");
    // ⑦ 长度闸：一份「粘错东西」的凭据动辄几万字符，一路带到上游只是浪费
    if let Some(key) = fields.get("key").and_then(Value::as_str) {
        if key.chars().count() > MAX_TOKEN_LENGTH {
            return Err(GatewayError::with_status(
                400,
                "这份凭据异常地长，请确认只粘贴了 modelBearerToken",
            ));
        }
    }
    Ok(Value::Object(fields))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 三种写法（落盘嵌套 / 平铺 / 桌面端原文）读到同一组值
    #[test]
    fn the_session_may_be_nested_flat_or_a_desktop_file() {
        let nested = Credentials::from_fields(&json!({
            "key": "mb-1",
            "uid": "u1",
            "session": { "token": "t1", "accountId": "a1", "ssoSessionId": "s1" },
        }))
        .expect("嵌套形态应能读出来");
        let flat = Credentials::from_fields(&json!({
            "key": "mb-1",
            "uid": "u1",
            "token": "t1",
            "accountId": "a1",
            "ssoSessionId": "s1",
        }))
        .expect("平铺形态应能读出来");
        let desktop = Credentials::from_fields(&json!({
            "modelBearerToken": "mb-1",
            "uid": "u1",
            "token": "t1",
            "accountId": "a1",
            "ssoSessionId": "s1",
        }))
        .expect("桌面端原文应能读出来");
        for item in [&nested, &flat, &desktop] {
            assert_eq!(item.key, "mb-1");
            assert_eq!(item.uid, "u1");
            assert_eq!(item.token, "t1");
            assert_eq!(item.account_id, "a1");
            assert_eq!(item.sso_session_id, "s1");
            assert!(item.has_session());
        }
    }

    /// 嵌套优先：记录里同时有两者时读嵌套（旧记录升级后的顶层残留不该生效）
    #[test]
    fn the_nested_session_wins_over_leftovers() {
        let credentials = Credentials::from_fields(&json!({
            "key": "mb-1",
            "token": "old-flat",
            "accountId": "old-flat",
            "session": { "token": "new", "accountId": "new" },
        }))
        .expect("应能读出来");
        assert_eq!(credentials.token, "new");
        assert_eq!(credentials.account_id, "new");
    }

    /// Cookie 头的顺序与重复项照桌面端；半套会话不产生半套头
    #[test]
    fn the_cookie_header_matches_the_desktop_client() {
        let full = Credentials::from_fields(&json!({
            "key": "mb-1",
            "token": "t1",
            "accountId": "a1",
            "ssoSessionId": "s1",
        }))
        .expect("应能读出来");
        assert_eq!(
            full.cookie_header(),
            "ssoSessionId=s1; sso_sessionid=s1; account_id=a1; token=t1"
        );

        let minimal = Credentials::from_fields(&json!({
            "key": "mb-1",
            "token": "t1",
            "accountId": "a1",
        }))
        .expect("应能读出来");
        assert_eq!(minimal.cookie_header(), "account_id=a1; token=t1");
    }

    /// 只有 token 没有 accountId：不算有会话，且**不会**落下半套
    #[test]
    fn half_a_session_is_not_a_session() {
        let credentials = Credentials::from_fields(&json!({
            "key": "mb-1",
            "token": "t1",
        }))
        .expect("应能读出来");
        assert!(!credentials.has_session(), "缺 accountId 不算有会话");

        let mut record = Map::new();
        credentials.write_into(&mut record);
        assert!(
            !record.contains_key("session"),
            "半套会话不该落盘（余额接口两个字段都要）"
        );
        // 空值不落：`uid` / `source` 缺省就是「没有」
        assert!(!record.contains_key("uid"), "空 uid 不该落盘");
        assert!(!record.contains_key("source"), "空 source 不该落盘");
        assert_eq!(record.get("key").and_then(Value::as_str), Some("mb-1"));
    }

    /// 身份三级：uid → accountId → key 哈希
    #[test]
    fn identity_prefers_the_person_over_the_token() {
        let with_uid = Credentials::from_fields(&json!({
            "key": "mb-1", "uid": "u1", "accountId": "a1",
        }))
        .expect("应能读出来");
        assert_eq!(identity_of(&with_uid), "uid:u1");

        // 换一枚令牌（重新登录）仍是同一个人
        let rotated = Credentials::from_fields(&json!({
            "key": "mb-2", "uid": "u1", "accountId": "a1",
        }))
        .expect("应能读出来");
        assert_eq!(identity_of(&rotated), identity_of(&with_uid));

        let with_account = Credentials::from_fields(&json!({
            "key": "mb-1", "accountId": "a1",
        }))
        .expect("应能读出来");
        assert_eq!(identity_of(&with_account), "account:a1");

        let bare = Credentials::from_fields(&json!({ "key": "mb-1" })).expect("应能读出来");
        let bare_again = Credentials::from_fields(&json!({ "key": "mb-1" })).expect("应能读出来");
        assert_eq!(identity_of(&bare), identity_of(&bare_again));
        assert!(identity_of(&bare).starts_with("key:"));
        // 同一个人换了令牌、又没有 uid → 身份确实变了（这是有意的降级：
        // 没有稳定标识时宁可多一条记录，也不要把两个不同账号并成一条）
        let other = Credentials::from_fields(&json!({ "key": "mb-2" })).expect("应能读出来");
        assert_ne!(identity_of(&bare), identity_of(&other));
    }

    /// 整段 `astron-session.json` 粘进任意字段都能认出来
    #[test]
    fn a_pasted_session_file_becomes_fields() {
        let file = json!({
            "modelBearerToken": "mb-1",
            "token": "t1",
            "accountId": "a1",
            "uid": "u1",
            "ssoSessionId": "s1",
        })
        .to_string();
        let normalized = normalize_paste(&json!({
            "provider": "astudio",
            "name": "我的 AStudio",
            "credentials": file,
        }))
        .expect("应能归一");
        assert_eq!(
            normalized.get("key").and_then(Value::as_str),
            Some("mb-1")
        );
        assert_eq!(
            normalized.get("accountId").and_then(Value::as_str),
            Some("a1")
        );
        // 上传专用键与解析用的原文都该被吃掉
        assert!(normalized.get("provider").is_none());
        assert!(normalized.get("name").is_none());
        assert!(normalized.get("credentials").is_none());
        let parsed = Credentials::from_fields(&normalized).expect("应能读出来");
        assert!(parsed.has_session());
        assert_eq!(parsed.uid, "u1");
    }

    /// 动态登录方式的 `{ inputs: {…} }` 包装要拆开
    #[test]
    fn the_dynamic_login_wrapper_is_unwrapped() {
        let normalized = normalize_paste(&json!({
            "provider": "astudio",
            "inputs": {
                "modelBearerToken": "mb-1",
                "sessionToken": "t1",
                "accountId": "a1",
            },
        }))
        .expect("应能归一");
        assert!(normalized.get("inputs").is_none());
        assert_eq!(
            normalized.get("key").and_then(Value::as_str),
            Some("mb-1")
        );
        // 登录弹窗把网页会话叫 `sessionToken`，落盘叫 `token`
        let parsed = Credentials::from_fields(&normalized).expect("应能读出来");
        assert_eq!(parsed.token, "t1");
        assert!(parsed.has_session());
    }

    /// 带 `Bearer ` 外壳的整行请求头也能收（用户从开发者工具复制的是整行）
    #[test]
    fn the_authorization_shell_is_stripped() {
        let normalized = normalize_paste(&json!({
            "provider": "astudio",
            "text": "Authorization: Bearer mb-abcdefgh",
        }))
        .expect("应能归一");
        assert_eq!(
            normalized.get("key").and_then(Value::as_str),
            Some("mb-abcdefgh")
        );
    }

    /// 封禁账号明确拒绝；超长凭据也拒绝（都是「收进来只会更糟」）
    #[test]
    fn banned_and_oversized_credentials_are_refused() {
        let banned = normalize_paste(&json!({
            "provider": "astudio",
            "modelBearerToken": "mb-1",
            "banned": true,
        }));
        let Err(error) = banned else {
            panic!("封禁账号必须拒绝");
        };
        assert_eq!(error.status_code, 400);
        assert!(error.message.contains("封禁"));

        let huge = normalize_paste(&json!({
            "provider": "astudio",
            "modelBearerToken": "x".repeat(MAX_TOKEN_LENGTH + 1),
        }));
        let Err(error) = huge else {
            panic!("超长凭据必须拒绝");
        };
        assert_eq!(error.status_code, 400);
    }

    /// 脱敏只露尾 4 位，空值不 panic
    #[test]
    fn masking_never_prints_the_whole_token() {
        let credentials = Credentials::from_fields(&json!({ "key": "mb-abcdefgh1234" }))
            .expect("应能读出来");
        assert_eq!(credentials.masked(), "…1234");
        assert_eq!(Credentials::default().masked(), "（空）");
        // 多字节短令牌也不 panic
        let short = Credentials::from_fields(&json!({ "key": "令牌" })).expect("应能读出来");
        assert_eq!(short.masked(), "…令牌");
    }
}
