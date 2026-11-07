//! 豆包工作账号：**内嵌窗口取 Cookie 与粘贴 Cookie 共用同一个落盘入口**。
//!
//! ── 账号字段（与 KukuAI / Command Code 的对照）───────────────────
//! ```text
//!   { id: "doubaowork-<身份>", provider: "doubaowork", name,
//!     cookies: [ {name, value, domain, path, expires} … ],   ← 真正的凭据
//!     fingerprint: { deviceId, webId, fp, msToken? },
//!     accessToken: "<拼好的 Cookie 头>",                       ← 见下（刻意冗余）
//!     tokenTail, sessionTail,
//!     source, priority, enabled, addedAt, updatedAt }
//! ```
//!
//! ── `accessToken` 为什么必须**冗余存一份拼好的 Cookie 头**（本家关键一处）──
//! `StoredAccount::has_credentials()` 的判据是
//! `has_token() || is_desktop() || has_api_key() || has_jwt() || no_auth()
//! || has_officeace_key()` —— 全部围绕 **`accessToken` / `apiKey` / `jwt` /
//! `desktop` / `noAuth` / OfficeAce 那一对** 这「六个键」，**没有「cookies」这一条**。
//! 不写 `accessToken` 的两个后果都是静默的：
//!   1. `get_session_by_id` 直接返回 `None`（它开头就 `has_credentials()` 判一次）
//!      ⇒ 本家账号**永远排不进可转发队列**，症状是「账号加得好好的、点它没反应」；
//!   2. `public_account` 的 `hasCredentials` 为 false ⇒ 界面把账号显示成
//!      「需重新登录」。
//! 因此本模块同时写两份（与 KukuAI 的做法一致 —— 那家也是
//! `accessToken` 装 `BDUSS=…; STOKEN=…` 整串）。两份**不能不一致**：
//! 它们都由同一份 Cookie 列表生成（`credentials::account_from_cookies` 的
//! `cookie_header()`）。
//!
//! ── 身份（去重）走 `providers::doubaowork::account_id_of` ─────────
//! **必须与登录路径同一个函数**：两处分叉会让「同一个账号重新登录」变成
//! 「新增一条记录」（与 AtomCode 那边的教训同款）。身份判据与它的已知局限
//! （上游若在续期时整串换 `sessionid`）写在 `providers::doubaowork::identity_of`
//! 的文档里 —— 那里是唯一的判据出处。
//!
//! ── 本模块**不写 `expiresAt`**（如实声明）───────────────────────
//! 本家 Cookie 的过期时刻读不到（粘贴形态没有 `expires`；导出形态的
//! `expirationDate` 只对部分 Cookie 有效）。写一个假的 `expiresAt` 会让
//! 凭证维护链路据此判断「临期」—— 而本家**没有刷新接口**，那次判断只能
//! 制造一次必然白跑的调用（`supports_refresh = false`，见
//! `providers/doubaowork/adapter.rs`）。因此不写这个键。
//!
//! 本文件全是「读-改-写」存储操作，**没有任何网络请求**（持锁不做网络）。
//! 绝不 unwrap/expect（release 是 panic=abort）。

use serde_json::{Map, Value};

use crate::server::core::providers::doubaowork::{self, credentials};
use crate::server::logging;

use super::priority::next_free_priority;
use super::sql;
use super::state::{mark_name_custom, StoredAccount};
use super::store::{AccountStore, AccountStoreError};
use super::store_util::token_tail_of;
use super::DOUBAOWORK_PROVIDER_ID;

/// 备注名长度上限（与别家同一口径）
const MAX_NAME_LENGTH: usize = 100;

/// Cookie 输入长度上限（超过这个长度一定粘错了东西）
const MAX_COOKIE_INPUT_LENGTH: usize = 65_536;

/// 从 payload 里取第一个非空字符串（候选键覆盖本项目落盘名与粘贴形态）
fn pick(payload: &Value, keys: &[&str]) -> String {
    for key in keys {
        let value = payload
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if !value.is_empty() {
            return value.to_string();
        }
    }
    String::new()
}

/// 从 secret 里读出 `(name, value)` Cookie 列表（两条形态都认）。
///
/// 与 `credentials::from_record` 同一条取值链，但**失败语义不同**：那个面向
/// 转发路径（拿不到就报可读的 401），本函数面向落账号路径（拿不到由调用方
/// 就地报 400）。
fn cookie_pairs_of(secret: &Value) -> Vec<(String, String)> {
    match secret.get("cookies").and_then(Value::as_array) {
        Some(rows) if !rows.is_empty() => credentials::cookie_pairs_from_rows(rows),
        _ => credentials::parse_cookie_string(&pick(
            secret,
            &["cookie", "cookieString", "cookie_string"],
        )),
    }
}

/// 有没有登录态（三个候选 Cookie 名任一非空 —— **并集判据**，见
/// `providers/doubaowork/credentials.rs` 的模块头）
fn has_session_cookie(pairs: &[(String, String)]) -> bool {
    pairs.iter().any(|(name, value)| {
        credentials::SESSION_COOKIE_NAMES.contains(&name.as_str()) && !value.trim().is_empty()
    })
}

/// 把 `(name, value)` 列表转成落盘的行数组（含域 / 路径 / 过期三个附属字段）。
///
/// `domain` / `path` 给最接近真实的默认值（`.doubao.com` + `/`，与
/// `credentials::normalize_paste` 同一口径）：粘贴形态拿不到这两个值，而写
/// `null` 会让下游读域的逻辑多一条分支。`expires` 写 `null` —— 那是**会话
/// Cookie 的真实形态**（浏览器关闭即失效），不是「忘了填」。
fn cookie_rows_of(pairs: &[(String, String)]) -> Value {
    Value::Array(
        pairs
            .iter()
            .map(|(name, value)| {
                let mut row = Map::new();
                row.insert("name".to_string(), Value::String(name.clone()));
                row.insert("value".to_string(), Value::String(value.clone()));
                row.insert(
                    "domain".to_string(),
                    Value::String(".doubao.com".to_string()),
                );
                row.insert("path".to_string(), Value::String("/".to_string()));
                row.insert("expires".to_string(), Value::Null);
                Value::Object(row)
            })
            .collect(),
    )
}

/// 公开形态的**核心逻辑**（自由函数，便于直接测 —— 它不依赖存储）。
///
/// ── 必须摘掉的四样（每一项漏掉都是一次凭据泄漏）───────────────
/// ```text
///   cookies      ← 真正的登录态（等价于账号密码）
///   cookie / cookieString  ← 整串形态（手工编辑过的记录会有）
///   accessToken  ← 拼好的同一串 Cookie（见模块头的冗余说明）
///   fingerprint  ← 不是密码，但它是**风控指纹**：下发等于告诉别人
///                  「用这组值冒充这个账号」，因此与凭据同级对待
/// ```
/// 界面需要的三样取而代之：`tokenTail`（尾号，用于区分账号）、
/// `cookieNames`（**只出名字不出值** —— 「这次带了哪些 Cookie」是排障最需要
/// 的信息，而名字本身不是秘密）、`hasSession`（有没有登录态）。
pub(crate) fn public_shape(record: &StoredAccount) -> Value {
    let mut value = record.to_value();
    // 先算「有没有登录态」与「有哪些 Cookie 名」，再删凭据键 ——
    // 顺序不能反（删了之后就算不出来了）
    let pairs = cookie_pairs_of(&value);
    let has_session = has_session_cookie(&pairs);
    let cookie_names: Vec<String> = pairs.iter().map(|(name, _)| name.clone()).collect();
    if let Some(object) = value.as_object_mut() {
        for key in [
            "cookies",
            "cookie",
            "cookieString",
            "cookie_string",
            "accessToken",
            "fingerprint",
        ] {
            object.remove(key);
        }
        object.insert("available".to_string(), Value::Bool(record.enabled()));
        // 有没有登录态（本家语义的字段名：界面按它显示「已登录 / 需重新登录」）
        object.insert("hasSession".to_string(), Value::Bool(has_session));
        // Cookie 名清单（**只有名字**）：排障时最需要的信息，且名字不是秘密
        object.insert(
            "cookieNames".to_string(),
            Value::Array(cookie_names.iter().map(|name| Value::String(name.clone())).collect()),
        );
        object.insert("cookieCount".to_string(), Value::from(cookie_names.len()));
        // 本家没有续期（`supports_refresh = false`）—— 恒为 false 而不是缺键：
        // 界面按布尔读（与 AtomCode 的 `canRefresh` 同一形状）
        object.insert("canRefresh".to_string(), Value::Bool(false));
    }
    value
}

impl AccountStore {
    /// 取一条豆包工作账号记录（公开形态的原始 JSON）。
    ///
    /// `account_id` 非空 → 按 id 直查（不属于本家返回 `None`）；
    /// 为空 → 本家**启用中**账号里优先级最靠前的一条（转发默认用队首账号，
    /// 与 `loomy_account_record` / `commandcode_account_record` 的语义一致）。
    pub fn doubaowork_account_record(&self, account_id: &str) -> Option<Value> {
        let guard = self.guard();
        if !account_id.is_empty() {
            let record = self.record_by_id(&guard, account_id)?;
            return (record.provider() == DOUBAOWORK_PROVIDER_ID).then(|| record.to_value());
        }
        let mut candidates: Vec<StoredAccount> = self
            .records_for_provider(&guard, DOUBAOWORK_PROVIDER_ID)
            .into_iter()
            .filter(StoredAccount::enabled)
            .collect();
        candidates.sort_by_key(StoredAccount::order_key);
        candidates.into_iter().next().map(|item| item.to_value())
    }

    /// 签到台账（`doubaoworkCheckin`）：读-改-写的写侧。
    ///
    /// ── 为什么存在账号记录里，而不是另一个文件 ────────────────
    /// 与 CodeArts 的福利台账同一取向：per-account 可变状态跟着账号一起备份、
    /// 一起导出、删账号时一起消失。另开状态文件会多出一套「文件在而账号没了」
    /// 的对账问题。只写自己这一个命名空间，且记录在锁内现读 —— 既洗不掉别人
    /// 的凭据，也不存在「快照过期」。
    ///
    /// ── 为什么必须在发领取请求**之前**落盘 ────────────────────
    /// 领取是「上游可能已经受理、回执却丢了」的动作（断网 / 超时 / 进程重启）。
    /// 没有台账时用户再点一次签到就会**再提交一次领取**；有台账时下一次只回查
    /// 结果。所以落盘失败必须让调用方**不发**那个写请求（见
    /// `providers::doubaowork::checkin` 的模块头）。
    pub fn put_doubaowork_checkin_ledger(
        &self,
        account_id: &str,
        ledger: &Value,
    ) -> Result<(), AccountStoreError> {
        let guard = self.guard();
        let Some(mut record) = self
            .record_by_id(&guard, account_id)
            .filter(|record| record.provider() == DOUBAOWORK_PROVIDER_ID)
        else {
            return Err(AccountStoreError::new(
                "豆包工作账号已不存在，签到台账无处落盘",
                404,
            ));
        };
        record
            .fields_mut()
            .insert("doubaoworkCheckin".to_string(), ledger.clone());
        record.set_updated_at(logging::now_ms());
        self.with_conn(&guard, |conn| sql::update_in_place(conn, &record))?;
        Ok(())
    }

    /// 台账的读侧（`doubaowork_account_record` 已经把整条记录交回来了，
    /// 这里只是给「没有台账」一个统一取法，省得每处都去翻字段名）。
    pub fn doubaowork_checkin_ledger(&self, account_id: &str) -> Option<Value> {
        self.doubaowork_account_record(account_id)
            .and_then(|record| record.get("doubaoworkCheckin").cloned())
    }

    /// 添加 / 更新一条豆包工作账号（**内嵌窗口登录与粘贴 Cookie 共用这一个入口**）。
    ///
    /// `secret` 是已经归一好的凭证：
    ///   - 登录路径：`{cookies: [{name,value,domain,path,expires}], fingerprint?: {...}}`
    ///     （壳侧从登录窗口导出，beta `doubaowork_login.rs` 的形态）；
    ///   - 粘贴路径：`credentials::normalize_paste` 的产物（同样是行数组 + 指纹）；
    ///   - 也接受 `{cookie: "a=1; b=2"}` 整串（手工编辑过的记录 / 直接粘贴）。
    ///
    /// ── 校验：**本地形态检查**（零网络，见 `credentials::LocalCheck`）────
    /// 缺登录态（`sessionid` / `sessionid_ss` / `sid_guard` 全空）当场 400 并给出
    /// 可执行文案；其余放行（Cookie 真失效时转发链会给 401）。
    /// ⚠️ **不做线上探活**：本家验 Cookie 有没有用的唯一办法是发一次真实推理
    /// （额度接口在 Cookie 无效时也回 200 + 空读数，见 `quota` 的模块头），
    /// 那会花掉用户的一次额度 —— 添加账号不该有这种副作用。
    ///
    /// 同一身份的记录就地更新（保留优先级与启用状态、沿用用户改过的备注名）；
    /// 撞到**别的 provider** 的 id 时报错，绝不覆写（与另外几家同一策略）。
    pub fn add_doubaowork_account(
        &self,
        secret: &Value,
        name: Option<&str>,
        source: &str,
    ) -> Result<Value, AccountStoreError> {
        let pairs = cookie_pairs_of(secret);
        if pairs.is_empty() {
            return Err(AccountStoreError::new(
                "缺少 Cookie：请用「浏览器登录」打开豆包工作，或粘贴完整 Cookie\
                 （DevTools → Network → 任一请求 → Request Headers → Cookie）",
                400,
            ));
        }
        // 输入长度上限（只有整串粘贴形态才有这个键；行数组已在归一那一步拦过）
        let raw = pick(secret, &["cookie", "cookieString", "cookie_string"]);
        if raw.chars().count() > MAX_COOKIE_INPUT_LENGTH {
            return Err(AccountStoreError::new("Cookie 过长，请确认粘贴的是 Cookie", 400));
        }
        if !has_session_cookie(&pairs) {
            return Err(AccountStoreError::new(
                format!(
                    "Cookie 里缺少登录态（需要 {} 之一）：请确认已登录 www.doubao.com 再复制",
                    credentials::SESSION_COOKIE_NAMES.join(" / ")
                ),
                400,
            ));
        }

        let id = doubaowork::account_id_of(&pairs);

        let guard = self.guard();
        let existing = self.record_by_id(&guard, &id);
        if let Some(existing) = existing.as_ref() {
            let existing_provider = existing.provider();
            if existing_provider != DOUBAOWORK_PROVIDER_ID {
                return Err(AccountStoreError::new(
                    format!(
                        "账号 id「{id}」已被{existing_provider}账号占用，无法添加同一个登录态的\
                         豆包工作账号（请先处理那个账号）"
                    ),
                    400,
                ));
            }
        }

        // 备注名兜底：显式传入 → 既有记录里的备注名 → 「豆包工作 <身份>」
        let explicit_name = name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.chars().take(MAX_NAME_LENGTH).collect::<String>());
        let record_name = explicit_name
            .or_else(|| {
                existing
                    .as_ref()
                    .map(StoredAccount::name)
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_else(|| doubaowork::display_name_of(None, &pairs));

        let priority = match existing.as_ref() {
            Some(record) => record.priority(),
            None => {
                // 优先级全局唯一（各家共用一条队列），取全库已用号段
                let used = self.with_conn(&guard, |conn| sql::priorities_all(conn))?;
                next_free_priority(&used)
            }
        };

        // ── 指纹：优先用入参给的；否则沿用既有记录的；否则现造 ──────────
        // 「现造」在 `credentials::account_from_cookies` 里发生（`Fingerprint::derive`
        // 的兜底），这里只负责把已有的那份传下去 —— 同一账号不能换指纹
        // （换指纹才是风控信号，见 `endpoints::Fingerprint` 的文档）。
        let fingerprint_value = secret
            .get("fingerprint")
            .filter(|value| value.is_object())
            .cloned()
            .or_else(|| {
                existing
                    .as_ref()
                    .and_then(|record| record.get("fingerprint"))
                    .filter(|value| value.is_object())
                    .cloned()
            });
        let resolved = credentials::account_from_cookies(
            &id,
            &record_name,
            &pairs,
            fingerprint_value.as_ref(),
        );
        let cookie_header = resolved.cookie_header();
        let fingerprint_json = {
            let mut map = Map::new();
            map.insert(
                "deviceId".to_string(),
                Value::String(resolved.fingerprint.device_id.clone()),
            );
            map.insert(
                "webId".to_string(),
                Value::String(resolved.fingerprint.web_id.clone()),
            );
            map.insert(
                "fp".to_string(),
                Value::String(resolved.fingerprint.fp.clone()),
            );
            // msToken 缺失就**不写**（写空串等于伪造「上游给了一个空值」）
            if !resolved.fingerprint.ms_token.is_empty() {
                map.insert(
                    "msToken".to_string(),
                    Value::String(resolved.fingerprint.ms_token.clone()),
                );
            }
            Value::Object(map)
        };

        let now = logging::now_ms();
        let mut record = Map::new();
        record.insert("id".to_string(), Value::String(id.clone()));
        record.insert(
            "provider".to_string(),
            Value::String(DOUBAOWORK_PROVIDER_ID.to_string()),
        );
        record.insert("name".to_string(), Value::String(record_name.clone()));
        // 用户自己打的备注名才算 custom（`name` 为空 = 派生名，可被后续替换）
        mark_name_custom(&mut record, name.is_some(), existing.as_ref());
        // ① 真正的凭据：行数组
        record.insert("cookies".to_string(), cookie_rows_of(&pairs));
        record.insert("fingerprint".to_string(), fingerprint_json);
        // ② 冗余一份拼好的 Cookie 头 —— 为了让 `has_credentials()` 为真
        // （见模块头那一整段；漏了它账号排不进可转发队列）
        record.insert(
            "accessToken".to_string(),
            Value::String(cookie_header.clone()),
        );
        record.insert(
            "tokenTail".to_string(),
            Value::String(token_tail_of(&cookie_header)),
        );
        record.insert(
            "sessionTail".to_string(),
            Value::String(doubaowork::identity_of(&pairs)),
        );
        record.insert("source".to_string(), Value::String(source.to_string()));
        record.insert("priority".to_string(), Value::from(priority));
        record.insert(
            "enabled".to_string(),
            Value::Bool(
                existing
                    .as_ref()
                    .map(StoredAccount::enabled)
                    .unwrap_or(true),
            ),
        );
        record.insert(
            "addedAt".to_string(),
            Value::from(
                existing
                    .as_ref()
                    .map(StoredAccount::added_at)
                    .filter(|value| *value != 0)
                    .unwrap_or(now),
            ),
        );
        record.insert("updatedAt".to_string(), Value::from(now));
        // 未知字段全量保留（用户手工加过的字段不能因为一次「更新账号」丢掉）
        let mut merged = record;
        if let Some(existing) = existing.as_ref() {
            for (key, value) in existing.fields() {
                merged.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        let saved = StoredAccount::from_map(merged);
        self.with_conn(&guard, |conn| sql::put(conn, &saved))?;
        logging::log(
            "[Accounts]",
            &format!(
                "✅ 豆包工作账号已保存: {record_name}（优先级 {priority}，{}，{}）",
                resolved.masked_cookies(),
                resolved.masked_identity()
            ),
        );
        Ok(self.public_account(&saved))
    }

    /// 豆包工作账号的**公开形态**（见 [`public_shape`] 的说明）。
    ///
    /// ⚠️ **本家必须有自己的公开形态函数**（不能只依赖
    /// `store_view::to_public_account` 的默认实现）：那个是按 workbuddy 字段挑的，
    /// 它**不会**带出 `cookies`（碰巧安全），但也不会带出 `sessionTail` /
    /// `hasSession`，界面因此无法显示「登录态尾号」；而一旦将来有人把它改成
    /// 「整体透出 record 再删几个键」，本家的 `cookies` 就会跟着出去。
    /// 显式列出「去掉什么、留下什么」是这里唯一可靠的形式。
    pub fn to_doubaowork_public_account(&self, record: &StoredAccount) -> Value {
        public_shape(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record_of(map: Value) -> StoredAccount {
        StoredAccount::from_map(map.as_object().cloned().unwrap_or_default())
    }

    /// ⚠️ 公开形态**绝不能**带出 Cookie / Cookie 头 / 指纹（每一项漏掉都是泄漏）
    #[test]
    fn public_shape_never_leaks_credentials() {
        let record = record_of(json!({
            "id": "doubaowork-89abcdef",
            "provider": DOUBAOWORK_PROVIDER_ID,
            "cookies": [
                {"name": "sessionid", "value": "SUPERSECRET", "domain": ".doubao.com", "path": "/"},
                {"name": "passport_csrf_token", "value": "CSRFSECRET", "domain": ".doubao.com", "path": "/"}
            ],
            "fingerprint": {"deviceId": "111", "webId": "222", "fp": "verify_x"},
            "accessToken": "sessionid=SUPERSECRET; passport_csrf_token=CSRFSECRET",
        }));
        let public = public_shape(&record);
        let text = public.to_string();
        assert!(!text.contains("SUPERSECRET"), "Cookie 值泄漏：{text}");
        assert!(!text.contains("CSRFSECRET"), "Cookie 值泄漏：{text}");
        assert!(!text.contains("verify_x"), "风控指纹泄漏：{text}");
        for key in ["cookies", "cookie", "accessToken", "fingerprint"] {
            assert!(public.get(key).is_none(), "{key} 不该出现在公开形态里");
        }
        // 界面需要的几样要在
        assert_eq!(public.get("hasSession"), Some(&Value::Bool(true)));
        assert_eq!(public.get("canRefresh"), Some(&Value::Bool(false)));
        assert_eq!(public.get("cookieCount").and_then(Value::as_i64), Some(2));
        let names = public
            .get("cookieNames")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(names.len(), 2);
        assert!(names.iter().any(|name| name.as_str() == Some("sessionid")));
    }

    /// 只粘了整串 Cookie 的记录（手工编辑过）也要能算出 `hasSession`
    #[test]
    fn public_shape_handles_raw_cookie_string() {
        let record = record_of(json!({
            "id": "doubaowork-x",
            "provider": DOUBAOWORK_PROVIDER_ID,
            "cookie": "sessionid=abcdef; other=1",
        }));
        let public = public_shape(&record);
        assert_eq!(public.get("hasSession"), Some(&Value::Bool(true)));
        assert!(public.get("cookie").is_none());
        assert_eq!(public.get("cookieCount").and_then(Value::as_i64), Some(2));
    }

    /// 没有登录态的记录：`hasSession` 为 false（界面据此显示「需重新登录」）
    #[test]
    fn public_shape_reports_missing_session() {
        let record = record_of(json!({
            "id": "doubaowork-y",
            "provider": DOUBAOWORK_PROVIDER_ID,
            "cookies": [{"name": "foo", "value": "bar", "domain": ".doubao.com", "path": "/"}],
        }));
        let public = public_shape(&record);
        assert_eq!(public.get("hasSession"), Some(&Value::Bool(false)));
    }

    /// Cookie 取值链：行数组优先，整串兜底（与 `credentials::from_record` 同一条）
    #[test]
    fn cookie_pairs_prefer_rows_then_fall_back_to_string() {
        let rows = cookie_pairs_of(&json!({
            "cookies": [{"name": "sessionid", "value": "fromRows"}]
        }));
        assert_eq!(rows, vec![("sessionid".to_string(), "fromRows".to_string())]);
        let flat = cookie_pairs_of(&json!({"cookie": "sessionid=fromString; a=b"}));
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[0].1, "fromString");
        // 两条都没有 → 空
        assert!(cookie_pairs_of(&json!({"unrelated": 1})).is_empty());
    }

    /// 落盘行数组：域 / 路径给默认值，`expires` 是 `null`（会话 Cookie 的真实形态）
    #[test]
    fn cookie_rows_carry_domain_path_and_null_expiry() {
        let rows = cookie_rows_of(&[("sessionid".to_string(), "v".to_string())]);
        let row = rows
            .as_array()
            .and_then(|items| items.first())
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(row.get("name").and_then(Value::as_str), Some("sessionid"));
        assert_eq!(row.get("domain").and_then(Value::as_str), Some(".doubao.com"));
        assert_eq!(row.get("path").and_then(Value::as_str), Some("/"));
        assert!(row.get("expires").is_some_and(Value::is_null));
    }

    /// 登录态判据是**并集**（三个候选名任一非空）
    #[test]
    fn session_detection_is_a_union() {
        for name in credentials::SESSION_COOKIE_NAMES {
            assert!(has_session_cookie(&[(name.to_string(), "v".to_string())]));
        }
        assert!(!has_session_cookie(&[("foo".to_string(), "v".to_string())]));
        // 空值的登录态 Cookie 不算
        assert!(!has_session_cookie(&[("sessionid".to_string(), "  ".to_string())]));
    }
}
