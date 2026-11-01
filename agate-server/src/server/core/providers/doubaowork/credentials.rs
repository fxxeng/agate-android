//! 豆包工作凭证：**一整套 Cookie + 一组设备指纹**（无 OAuth、无续期接口）。
//!
//! ── 上游凭什么是这个 ──────────────────────────────────────────
//! 本家没有 `/oauth/token` 那样的续期接口，也没有可换的一次性 code：桌面端
//! 登录态就是 Chromium 的 Cookie 库本身（`sessionid` / `sid_guard` 一族）。
//! 因此：
//! ```text
//!   supports_refresh = false   （无 refresh 接口 → 只能 Cookie 失效后重新登录）
//!   凭证形态        = Cookie map + {device_id, web_id, fp, msToken}
//! ```
//! [二进制] 里 `expirationDate` / `sessionid_ss` / `cookieHeader` / `Cookie JSON`
//! 几串紧邻出现，说明插件侧的凭证形态就是**导出的 Cookie 行数组 + 过期时刻**：
//! ```text
//!   { "cookies": [ {"name","value","domain","path","expires"} … ] }
//! ```
//! beta 桌面端的 `doubaowork_login.rs` 正是产出这个形态（`Network.getAllCookies`
//! + 过滤 `doubao.com` / `www.doubao.com` + `/` 路径），因此**两种入口共用同一
//! 形态**：[`normalize_paste`] 把「整串 Cookie」也归一成同一个行数组。
//!
//! ── 哪些 Cookie 是必需的（判据来自两处）────────────────────────
//!   - [二进制]：`sessionid` / `sessionid_ss` 两串逐字出现（后者是备用名）；
//!   - beta `doubaowork_login.rs`：登录成功判据是
//!     `sessionid` **或** `sessionid_ss` 非空。
//! [Go参考] 的 `accountFromCookies` 用的是 `sessionid` / `sid_guard` 判据
//! （那是网页版的口径）—— 两者不冲突：本实现取**并集**
//! （`sessionid | sessionid_ss | sid_guard` 任一非空即视为有登录态），
//! 因为「有了 Cookie 但被判成没登录」是本家最贵的一类误判
//! （用户明明粘对了，界面说缺凭证）。
//!
//! ── 与 `Authorization: Bearer` 那条路的关系（写清楚，别混）────────
//! [Go参考] 里有一处提到 `Authorization: Bearer <sessionid>`。本实现**不用它**：
//! 桌面端发的是完整 Cookie 头（依据见 [`super::endpoints::auth_headers`]），
//! 而 `sessionid` 只是那份 Cookie 里必需的一项。因此 `Bearer` 既不生成也不解析。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{Map, Value};

use crate::server::errors::GatewayError;

use super::endpoints::Fingerprint;

/// 输入长度上限（整串 Cookie 一般几 KB；超过这个长度一定粘错了东西）
const MAX_COOKIE_INPUT_LENGTH: usize = 65_536;

/// 单条 Cookie 值的长度上限（上游自己的 Cookie 不会到这么长；
/// 超长值多半是把 base64 图片之类粘进来了）
const MAX_COOKIE_VALUE_LENGTH: usize = 16_384;

/// 判定「这个域属于豆包」时认的域名（beta `doubaowork_login.rs` 的
/// `DOMAINS` 里的前两个；第三个 `larksuite.com` 是登录页里飞书那部分，
/// 与对话/额度链路无关，不收）
const DOUBAO_DOMAINS: [&str; 2] = ["doubao.com", "www.doubao.com"];

/// 有登录态的 Cookie 名（**并集判据**，见模块头）
pub const SESSION_COOKIE_NAMES: [&str; 3] = ["sessionid", "sessionid_ss", "sid_guard"];

/// CSRF 头的两个来源 Cookie 名（[Go参考] `CSRFToken()` 的取值链）
const CSRF_COOKIE_NAMES: [&str; 2] = ["passport_csrf_token", "passport_csrf_token_default"];

/// CSRF 的**备用响应头名**（[二进制] 里 `s_v_web_id` 与 `Cookie` 族相邻；
/// 本模块把 `s_v_web_id` 作为 `web_id` 的候选来源，见 [`account_from_cookies`]）
const WEB_ID_COOKIE_NAMES: [&str; 2] = ["s_v_web_id", "web_id"];

/// 一次转发 / 查询用的凭证快照。
#[derive(Clone, Debug)]
pub struct Credentials {
    /// 账号记录 id
    pub id: String,
    /// 展示名
    pub name: String,
    /// Cookie 名 → 值（**含全部同名项的最后胜出值**，见 [`account_from_cookies`]）
    pub cookies: Vec<(String, String)>,
    /// 设备指纹（`device_id` / `web_id` / `fp` / `msToken`）
    pub fingerprint: Fingerprint,
}

impl Credentials {
    /// 拼出 `Cookie` 请求头的值。
    ///
    /// ── 为什么不用 `; ` 之外的任何整理 ──────────────────────────
    /// 上游按 `name=value; name=value` 解析。这里保持**录入顺序**
    /// （[`Credentials::cookies`] 是 `Vec` 而不是 map）：桌面端自己写 Cookie 时
    /// 也是固定顺序，顺序变了不影响语义，但**排障时两次请求要能 diff** ——
    /// 与 [`endpoints::security_params`] 同一个理由。
    pub fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .filter(|(name, value)| !name.is_empty() && !value.is_empty())
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// `x-tt-passport-csrf-token` 的取值（两个候选名，前者优先）。
    ///
    /// `passport_csrf_token` 与 `passport_csrf_token_default` 是同一件事在
    /// 不同域上的两份（桌面端两个域都写）；本实现只取**先命中的那个**，
    /// 不比对一致性 —— 两者不一致时上游自己会拒，我们不该替它挑。
    pub fn csrf_token(&self) -> Option<String> {
        for name in CSRF_COOKIE_NAMES {
            if let Some((_, value)) = self.cookies.iter().find(|(key, _)| key.as_str() == name) {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
        None
    }

    /// 有没有登录态（任一 [`SESSION_COOKIE_NAMES`] 非空）
    pub fn has_session(&self) -> bool {
        self.cookies.iter().any(|(name, value)| {
            SESSION_COOKIE_NAMES.contains(&name.as_str()) && !value.trim().is_empty()
        })
    }

    /// 展示用的脱敏 Cookie（**只露 Cookie 名，值一律不出**）。
    ///
    /// 日志与报错文案都用它：Cookie 值等价于账号密码，一条泄漏的日志
    /// 就是一次凭据泄漏，而故障排查真正需要的是「这次到底带了哪些 Cookie」。
    pub fn masked_cookies(&self) -> String {
        let names: Vec<String> = self
            .cookies
            .iter()
            .filter(|(_, value)| !value.is_empty())
            .map(|(name, _)| name.clone())
            .collect();
        if names.is_empty() {
            return "（无）".to_string();
        }
        format!("{} 项 [{}]", names.len(), names.join(", "))
    }

    /// 脱敏的账号标识（登录态值的尾 4 位；用于日志里区分不同账号）
    pub fn masked_identity(&self) -> String {
        for name in SESSION_COOKIE_NAMES {
            if let Some((_, value)) = self.cookies.iter().find(|(key, _)| key.as_str() == name) {
                if !value.is_empty() {
                    return format!("{name} …{}", tail_of(value));
                }
            }
        }
        "（无登录态）".to_string()
    }
}

/// 取末 4 个字符（不足 4 个则整串）
pub fn tail_of(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let start = chars.len().saturating_sub(4);
    chars[start..].iter().collect()
}

/// 一条归一后的 Cookie（与 beta `doubaowork_login.rs` 的导出形态同键名）。
///
/// 这个结构体的字段名就是**落盘契约**：`cookies` 数组原样写进账号记录，
/// [`Credentials`] 与公开形态都从它读。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookieRow {
    /// Cookie 名
    pub name: String,
    /// Cookie 值
    pub value: String,
    /// 域（**归一成前导点形态**：`.doubao.com` / `.www.doubao.com`）
    pub domain: String,
    /// 路径
    pub path: String,
    /// 过期时刻（秒；`None` = 会话 Cookie，浏览器关闭即失效）
    pub expires: Option<i64>,
}

impl CookieRow {
    /// 转成落盘 JSON 对象（键名与 beta 的导出形态逐字一致）
    pub fn to_value(&self) -> Value {
        let mut object = Map::new();
        object.insert("name".to_string(), Value::String(self.name.clone()));
        object.insert("value".to_string(), Value::String(self.value.clone()));
        object.insert("domain".to_string(), Value::String(self.domain.clone()));
        object.insert("path".to_string(), Value::String(self.path.clone()));
        object.insert(
            "expires".to_string(),
            match self.expires {
                Some(seconds) => Value::from(seconds),
                None => Value::Null,
            },
        );
        Value::Object(object)
    }
}

/// 从一条 JSON 记录里读一个字符串（宽容读取：可能被手工编辑过）
fn pick(record: &Value, keys: &[&str]) -> String {
    for key in keys {
        let value = record
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

/// 解析整串 Cookie（`a=1; b=2` 或每行一条，两种 DevTools 复制形态）。
///
/// ── 归一规则（每一条都有它防的事）──────────────────────────────
///   1. `\r\n` / `\n` 一律当分隔符（DevTools 复制出来的是每行一条）；
///   2. 值里的 `=` **不切分**（base64 值以 `=` 结尾是常态）—— 只按**第一个**
///      `=` 切分，切完的右半段原样保留（含后续的 `=`）；
///   3. 值两侧空白去掉（复制时常带尾空格）；
///   4. 空名或空值**整条丢弃**（空值 Cookie 发上去没有意义，还会让上游认为
///      「这个字段存在但无效」，与「没有」是两件事）。
pub fn parse_cookie_string(input: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    // 先按行切，再按分号切：两种形态一次处理完
    for line in normalized.split('\n') {
        for part in line.split(';') {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Some(offset) = trimmed.find('=') else {
                continue;
            };
            let name = trimmed[..offset].trim();
            let value = trimmed[offset + 1..].trim();
            if name.is_empty() || value.is_empty() {
                continue;
            }
            if value.chars().count() > MAX_COOKIE_VALUE_LENGTH {
                continue;
            }
            // 同名项**后者胜**（DevTools 的复制顺序里更靠下的更具体）
            if let Some(existing) = out.iter_mut().find(|(key, _)| key.as_str() == name) {
                existing.1 = value.to_string();
                continue;
            }
            out.push((name.to_string(), value.to_string()));
        }
    }
    out
}

/// 从 `cookies` 行数组（beta 导出形态）读回 `(name, value)` 列表。
///
/// ── 域名过滤的取舍（与 beta 唯一的差异，写清楚）────────────────
/// beta 的 `cookie()` 只收 `doubao.com` / `www.doubao.com`。本实现**放宽**成
/// 「名字非空即收」：粘贴通道给的是用户自己从 DevTools 复制的一串 Cookie，
/// 里面本来就没有域信息（`ParseCookieString` 也解析不出域）。按域过滤只适用于
/// **登录窗口导出的行数组**（那里有 `domain`），对粘贴形态会把全部 Cookie 丢掉。
/// 因此域过滤只作用于 [`normalize_exported_cookies`]（有 `domain` 的那条路），
/// 而本函数（从已落盘的数组读回）不做二次过滤 —— 落盘的已经是过滤后的结果。
pub fn cookie_pairs_from_rows(rows: &[Value]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for row in rows {
        let name = row.get("name").and_then(Value::as_str).unwrap_or("").trim();
        let value = row.get("value").and_then(Value::as_str).unwrap_or("").trim();
        if name.is_empty() || value.is_empty() {
            continue;
        }
        if value.chars().count() > MAX_COOKIE_VALUE_LENGTH {
            continue;
        }
        if let Some(existing) = out.iter_mut().find(|(key, _)| key.as_str() == name) {
            existing.1 = value.to_string();
            continue;
        }
        out.push((name.to_string(), value.to_string()));
    }
    out
}

/// 归一**登录窗口导出**的 Cookie 行数组（beta `doubaowork_login.rs` 的产物）。
///
/// 域过滤规则与 beta 逐条一致：
///   - 域去前导点后必须在 `doubao.com` / `www.doubao.com` 之内（含子域）；
///   - 路径必须恰好是 `/`（更深的路径上的 Cookie 不参与身份）；
///   - 已过期的行丢掉（`expires <= now`）；
///   - `www.doubao.com` 的值**覆盖** `.doubao.com` 的同名项（更具体优先，
///     与 [Go参考] `accountFromCookies` 的判据逐字相同）。
pub fn normalize_exported_cookies(rows: &[Value], now_seconds: i64) -> Vec<CookieRow> {
    let mut out: Vec<CookieRow> = Vec::new();
    for row in rows {
        let name = row.get("name").and_then(Value::as_str).unwrap_or("").trim();
        let value = row.get("value").and_then(Value::as_str).unwrap_or("").trim();
        if name.is_empty() || value.is_empty() {
            continue;
        }
        if value.chars().count() > MAX_COOKIE_VALUE_LENGTH {
            continue;
        }
        let raw_domain = row.get("domain").and_then(Value::as_str).unwrap_or("").trim();
        let domain = raw_domain.trim_start_matches('.');
        if domain.is_empty() || !is_doubao_domain(domain) {
            continue;
        }
        let path = row
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or("/");
        if path != "/" {
            continue;
        }
        let expires = row
            .get("expires")
            .and_then(Value::as_i64)
            .or_else(|| row.get("expirationDate").and_then(Value::as_i64));
        if expires.is_some_and(|value| value <= now_seconds) {
            continue;
        }
        let normalized_domain = format!(".{domain}");
        // 同名项：`www.doubao.com` 覆盖 `.doubao.com`，其余后者胜
        let replace = out
            .iter()
            .position(|existing| existing.name.as_str() == name)
            .map(|index| (index, domain == "www.doubao.com"))
            .filter(|(_, more_specific)| *more_specific)
            .map(|(index, _)| index);
        let entry = CookieRow {
            name: name.to_string(),
            value: value.to_string(),
            domain: normalized_domain,
            path: "/".to_string(),
            expires,
        };
        match replace {
            Some(index) => out[index] = entry,
            None if out.iter().any(|existing| existing.name == entry.name) => {
                // 已有同名但这次不是更具体的域：保留先到的那个
                continue;
            }
            None => out.push(entry),
        }
    }
    out
}

/// 域是否属于豆包（精确匹配或子域）
fn is_doubao_domain(domain: &str) -> bool {
    DOUBAO_DOMAINS
        .iter()
        .any(|base| domain == *base || domain.ends_with(&format!(".{base}")))
}

/// 从 Cookie 列表构造凭证。
///
/// ── `device_id` / `web_id` 的来源（按优先级）────────────────────
///   1. 记录里显式给的 `deviceId` / `device_id`（登录窗口导出时由壳侧读
///      Local Storage 填进来）；
///   2. 记录里的 `fingerprint.{deviceId,webId,fp,msToken}`（归一后的落盘形态）；
///   3. Cookie 里的 `s_v_web_id`（[二进制] 里它紧邻 `Cookie` 族出现，
///      网页版把设备标识写在 Cookie 里）；
///   4. 现造（见 [`endpoints::Fingerprint::derive`]）。
pub fn account_from_cookies(
    id: &str,
    name: &str,
    pairs: &[(String, String)],
    stored_fingerprint: Option<&Value>,
) -> Credentials {
    let read_pair = |cookie_name: &str| -> Option<String> {
        pairs
            .iter()
            .find(|(key, _)| key.as_str() == cookie_name)
            .map(|(_, value)| value.clone())
    };
    let from_fingerprint =
        |key: &str| -> Option<String> { stored_fingerprint.and_then(|value| pick_opt(value, key)) };
    // s_v_web_id 只作为 web_id 的兜底（它是网页版形态的键）
    let web_id_from_cookie = WEB_ID_COOKIE_NAMES
        .iter()
        .find_map(|cookie_name| read_pair(cookie_name));
    let ms_token_from_cookie = read_pair("msToken");

    let fingerprint = Fingerprint::derive(
        from_fingerprint("deviceId")
            .or_else(|| from_fingerprint("device_id"))
            .as_deref(),
        from_fingerprint("webId")
            .or_else(|| from_fingerprint("web_id"))
            .or(web_id_from_cookie)
            .as_deref(),
        from_fingerprint("fp").as_deref(),
        from_fingerprint("msToken")
            .or(ms_token_from_cookie)
            .as_deref(),
    );
    Credentials {
        id: id.to_string(),
        name: name.to_string(),
        cookies: pairs.to_vec(),
        fingerprint,
    }
}

/// 读一个可空的字符串字段（用于指纹那一层，缺失返回 `None` 而不是空串）
fn pick_opt(record: &Value, key: &str) -> Option<String> {
    record
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// 从账号记录解析凭证（**唯一入口**；`record` 为 `None` 时报「没有可用账号」）。
///
/// 取值链（两条都要认，因为落盘形态与粘贴形态不同）：
///   - `cookies`：**行数组**（登录窗口导出 / 粘贴归一后的落盘形态）；
///   - `cookie`：**整串**（用户直接粘的一串，归一后也会转成行数组，
///     这里兜住「手工编辑过记录」的情形）。
pub fn from_record(record: Option<&Value>) -> Result<Credentials, GatewayError> {
    let Some(record) = record else {
        return Err(GatewayError::with_status(
            401,
            "没有可用的豆包工作账号：请在账号页用「浏览器登录」或粘贴完整 Cookie 添加",
        ));
    };
    let id = pick(record, &["id"]);
    let name = pick(record, &["name"]);

    let rows = record.get("cookies").and_then(Value::as_array);
    let pairs: Vec<(String, String)> = match rows {
        Some(rows) if !rows.is_empty() => cookie_pairs_from_rows(rows),
        _ => {
            // 退回整串形态（可能是手工编辑过的记录）
            let raw = pick(record, &["cookie", "cookieString", "cookie_string"]);
            parse_cookie_string(&raw)
        }
    };
    if pairs.is_empty() {
        return Err(GatewayError::with_status(
            401,
            "豆包工作账号没有 Cookie（请重新登录，或粘贴完整 Cookie；\
             只粘 sessionid 一项也不够 —— 上游要的是整串）",
        ));
    }
    let fingerprint = record.get("fingerprint");
    let credentials = account_from_cookies(&id, &name, &pairs, fingerprint);
    if !credentials.has_session() {
        return Err(GatewayError::with_status(
            401,
            format!(
                "豆包工作账号缺少登录态 Cookie（需要 {} 之一；当前 {}）：\
                 请重新登录或粘贴完整 Cookie",
                SESSION_COOKIE_NAMES.join(" / "),
                credentials.masked_cookies()
            ),
        ));
    }
    Ok(credentials)
}

/// 把整串 Cookie 归一成落盘 secret（粘贴通道的入口）。
///
/// 产物键名与本家落盘契约一致：`cookies`（行数组）+ `fingerprint`
/// （`deviceId` / `webId` / `fp` / `msToken` 四项，缺的**不写** —— 写空串
/// 等于把「上游没给」伪装成「给了一个空值」，见 [`super::credentials`] 的
/// 说明）。
///
/// 行数组里的 `domain` 统一写 `.doubao.com`、`path` 写 `/`：粘贴形态本来
/// 拿不到域信息，而这两个值正是 beta 导出形态里最常见的组合 —— 写死它们
/// 比写 `null` 更接近真实形态（下游只读 `name` / `value` 两项）。
pub fn normalize_paste(
    cookie_input: &str,
    device_id: Option<&str>,
    web_id: Option<&str>,
    fp: Option<&str>,
) -> Result<Value, GatewayError> {
    let trimmed = cookie_input.trim();
    if trimmed.is_empty() {
        return Err(GatewayError::with_status(
            400,
            "Cookie 为空：请在浏览器登录 www.doubao.com 后，从开发者工具复制完整 Cookie \
             （DevTools → Network → 任一请求 → Request Headers → Cookie）",
        ));
    }
    if trimmed.chars().count() > MAX_COOKIE_INPUT_LENGTH {
        return Err(GatewayError::with_status(400, "Cookie 过长，请确认粘贴的是 Cookie 而不是别的内容"));
    }
    let pairs = parse_cookie_string(trimmed);
    if pairs.is_empty() {
        return Err(GatewayError::with_status(
            400,
            "未能从输入里解析出任何 Cookie（应形如 `name=value; name2=value2`）",
        ));
    }
    let has_session = pairs
        .iter()
        .any(|(name, value)| SESSION_COOKIE_NAMES.contains(&name.as_str()) && !value.is_empty());
    if !has_session {
        return Err(GatewayError::with_status(
            400,
            format!(
                "Cookie 里缺少登录态（需要 {} 之一）：请确认已登录 www.doubao.com 再复制",
                SESSION_COOKIE_NAMES.join(" / ")
            ),
        ));
    }
    let now = crate::server::logging::now_ms() / 1000;
    let rows: Vec<Value> = pairs
        .iter()
        .map(|(name, value)| {
            CookieRow {
                name: name.clone(),
                value: value.clone(),
                domain: ".doubao.com".to_string(),
                path: "/".to_string(),
                // 粘贴形态拿不到过期时刻；会话 Cookie 用 null 表达（不编一个时刻）
                expires: None,
            }
            .to_value()
        })
        .collect();
    let mut secret = Map::new();
    secret.insert("cookies".to_string(), Value::Array(rows));
    let mut fingerprint = Map::new();
    for (key, value) in [("deviceId", device_id), ("webId", web_id), ("fp", fp)] {
        if let Some(text) = value.map(str::trim).filter(|text| !text.is_empty()) {
            fingerprint.insert(key.to_string(), Value::String(text.to_string()));
        }
    }
    if let Some(token) = pairs
        .iter()
        .find(|(name, _)| name.as_str() == "msToken")
        .map(|(_, value)| value.clone())
    {
        fingerprint.insert("msToken".to_string(), Value::String(token));
    }
    if !fingerprint.is_empty() {
        secret.insert("fingerprint".to_string(), Value::Object(fingerprint));
    }
    // 供账号层派生稳定 id：登录态值的尾 4 位（见 `super::super::account_store`）
    secret.insert(
        "sessionTail".to_string(),
        Value::String(
            SESSION_COOKIE_NAMES
                .iter()
                .find_map(|cookie_name| {
                    pairs
                        .iter()
                        .find(|(name, _)| name.as_str() == *cookie_name)
                        .map(|(_, value)| tail_of(value))
                })
                .unwrap_or_default(),
        ),
    );
    secret.insert(
        "accountHint".to_string(),
        Value::String(masked_identity_of(&pairs)),
    );
    // 时间戳只是提示（本家没有 expiresAt 可算，见模块头）
    secret.insert("importedAt".to_string(), Value::from(now));
    Ok(Value::Object(secret))
}

/// 登录态 Cookie 的脱敏标识（`sessionid …abcd` 形态）
pub fn masked_identity_of(pairs: &[(String, String)]) -> String {
    for name in SESSION_COOKIE_NAMES {
        if let Some((_, value)) = pairs.iter().find(|(key, _)| key.as_str() == name) {
            if !value.is_empty() {
                return format!("{name} …{}", tail_of(value));
            }
        }
    }
    "（无登录态）".to_string()
}

/// 从记录里读一组 Cookie 名（公开形态用：**值一律不出**）。
pub fn cookie_names_from_record(record: &Value) -> Vec<String> {
    let rows = record.get("cookies").and_then(Value::as_array);
    match rows {
        Some(rows) => rows
            .iter()
            .filter_map(|row| row.get("name").and_then(Value::as_str))
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect(),
        None => parse_cookie_string(&pick(record, &["cookie", "cookieString", "cookie_string"]))
            .into_iter()
            .map(|(name, _)| name)
            .collect(),
    }
}

/// 本地形态检查（零网络）：有没有可用的 Cookie 行。
///
/// 与 [`from_record`] 的区别：后者面向**转发路径**（拿不到就报可读的 401），
/// 本函数面向**添加 / 探活路径**（返回布尔，由调用方决定怎么提示）。
pub fn shape_ok(record: &Value) -> bool {
    from_record(Some(record)).is_ok()
}

/// 一次「本地形态检查」的结果（不需要网络即可给出）。
///
/// ── 为什么本家的探活**不真打上游**（与 Command Code 的 `probe_key` 不同）──
/// 本家的凭证是一整串 Cookie，而**验它有没有用的唯一办法是发一次真实推理**
/// （额度接口在 Cookie 无效时也返回 200 + 空读数，见 [`super::quota`] 的模块头）
/// —— 那会花掉用户的一次额度。因此添加时的校验一律是**本地形态检查**：
/// 缺登录态当场拒（用户能自己修），其余放行（真失效时转发链会给 401，
/// 那时文案是「Cookie 已失效，请重新登录」，与「加不进来」是两种可分辨的体验）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalCheck {
    /// 形态合格（有登录态 Cookie）
    Ok {
        /// 脱敏标识（`sessionid …abcd`）
        identity: String,
        /// 有几个 Cookie（不含值）
        cookie_count: usize,
        /// 有没有设备指纹（缺的话转发时会现造，不影响可用性）
        has_fingerprint: bool,
    },
    /// 形态不合格（原因可直接展示给用户）
    Bad {
        /// 给用户看的原因
        reason: String,
    },
}

/// 本地检查一条归一后的 cookie secret（**零网络**）。
pub fn local_check(secret: &Value) -> LocalCheck {
    // `from_record` 认 `cookies` 行数组与 `cookie` 整串两种形态。归一后的 secret
    // 已经带 `cookies`，这里只为兜住「调用方只给了 `cookieString`」的情形 ——
    // 值先取出来再插入，避免同一对象上的借用重叠。
    let mut record = secret.clone();
    let needs_alias = record.get("cookies").is_none() && record.get("cookie").is_none();
    if needs_alias {
        if let Some(text) = record.get("cookieString").cloned() {
            if let Some(object) = record.as_object_mut() {
                object.insert("cookie".to_string(), text);
            }
        }
    }
    match from_record(Some(&record)) {
        Ok(credentials) => LocalCheck::Ok {
            identity: credentials.masked_identity(),
            cookie_count: credentials.cookies.len(),
            has_fingerprint: !credentials.fingerprint.device_id.is_empty()
                && !credentials.fingerprint.web_id.is_empty(),
        },
        Err(error) => LocalCheck::Bad {
            reason: error.message,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 整串 Cookie 解析：换行 / 分号两种形态都认；值里的 `=` 不切分
    #[test]
    fn parse_cookie_string_handles_both_shapes() {
        let flat = parse_cookie_string("sessionid=abc; passport_csrf_token=tok");
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[0], ("sessionid".to_string(), "abc".to_string()));
        // 每行一条（DevTools 复制形态）
        let lines = parse_cookie_string("sessionid=abc\nsid_guard=xyz\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].0, "sid_guard");
        // 值里的 `=` 原样保留（base64 值以 = 结尾是常态）
        let padded = parse_cookie_string("sessionid=YWJj==");
        assert_eq!(padded[0].1, "YWJj==");
        // CRLF 也要认
        let crlf = parse_cookie_string("a=1\r\nb=2");
        assert_eq!(crlf.len(), 2);
        // 空名 / 空值丢弃
        assert_eq!(parse_cookie_string("=novalue; novalue=; ok=1").len(), 1);
    }

    /// 同名项**后者胜**（DevTools 顺序里更靠下的更具体）
    #[test]
    fn duplicate_cookie_names_last_one_wins() {
        let pairs = parse_cookie_string("sessionid=first; sessionid=second");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].1, "second");
    }

    /// `from_record`：缺登录态报 401 并给出可执行文案；有登录态则通过
    #[test]
    fn from_record_requires_session_cookie() {
        let ok = from_record(Some(&json!({
            "id": "doubaowork-1",
            "name": "我的豆包",
            "cookie": "sessionid=abcdef; other=1"
        })));
        let credentials = ok.ok().unwrap_or_else(|| Credentials {
            id: String::new(),
            name: String::new(),
            cookies: Vec::new(),
            fingerprint: Fingerprint::default(),
        });
        assert!(credentials.has_session());
        assert_eq!(credentials.masked_identity(), "sessionid …cdef");
        // 只有无关 Cookie → 401
        let bad = from_record(Some(&json!({"cookie": "foo=bar"})));
        assert_eq!(bad.err().map(|error| error.status_code), Some(401));
        // 一条都没有 → 401
        assert_eq!(
            from_record(Some(&json!({}))).err().map(|error| error.status_code),
            Some(401)
        );
        // 没有账号 → 401
        assert!(from_record(None).is_err());
    }

    /// `sessionid_ss` / `sid_guard` 都是合法登录态（**并集判据**，见模块头）
    #[test]
    fn session_cookie_names_are_a_union() {
        for name in SESSION_COOKIE_NAMES {
            let record = json!({ "cookie": format!("{name}=value") });
            assert!(
                from_record(Some(&record)).is_ok(),
                "{name} 应被认作登录态"
            );
        }
    }

    /// 登录窗口导出：域过滤 / 路径过滤 / 过期过滤 / 子域覆盖
    #[test]
    fn exported_cookies_are_filtered_by_domain_path_and_expiry() {
        let rows = vec![
            json!({"name": "sessionid", "value": "base", "domain": ".doubao.com",
                   "path": "/", "expires": null}),
            // www 域更具体 → 覆盖
            json!({"name": "sessionid", "value": "www", "domain": "www.doubao.com",
                   "path": "/", "expires": null}),
            // 别的域 → 丢
            json!({"name": "other", "value": "x", "domain": ".example.com",
                   "path": "/", "expires": null}),
            // 深路径 → 丢
            json!({"name": "deep", "value": "x", "domain": ".doubao.com",
                   "path": "/chat", "expires": null}),
            // 已过期 → 丢
            json!({"name": "expired", "value": "x", "domain": ".doubao.com",
                   "path": "/", "expires": 1000}),
        ];
        let normalized = normalize_exported_cookies(&rows, 2_000);
        let names: Vec<&str> = normalized.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, vec!["sessionid"]);
        // 前导点归一 + www 覆盖生效
        assert_eq!(normalized[0].value, "www");
        assert_eq!(normalized[0].domain, ".www.doubao.com");
    }

    /// 指纹来源优先级：显式 fingerprint 段 > Cookie 里的 s_v_web_id > 现造
    #[test]
    fn fingerprint_sources_follow_priority() {
        let pairs = parse_cookie_string(
            "sessionid=abc; s_v_web_id=webFromCookie; msToken=msFromCookie",
        );
        let credentials = account_from_cookies(
            "id",
            "n",
            &pairs,
            Some(&json!({"deviceId": "devFromRecord", "webId": "webFromRecord"})),
        );
        assert_eq!(credentials.fingerprint.device_id, "devFromRecord");
        assert_eq!(credentials.fingerprint.web_id, "webFromRecord");
        // msToken 没有显式给 → 用 Cookie 里的那份
        assert_eq!(credentials.fingerprint.ms_token, "msFromCookie");
        // 没有指纹段 → 用 s_v_web_id 兜底
        let fallback = account_from_cookies("id", "n", &pairs, None);
        assert_eq!(fallback.fingerprint.web_id, "webFromCookie");
        assert_eq!(fallback.fingerprint.device_id.len(), 15);
    }

    /// 粘贴归一：产出行数组 + 指纹；缺登录态 400；空输入 400
    #[test]
    fn normalize_paste_produces_rows_and_fingerprint() {
        let secret = normalize_paste(
            "sessionid=abcdef; msToken=mt; other=1",
            Some("  dev  "),
            None,
            Some("verify_x"),
        )
        .ok()
        .unwrap_or(Value::Null);
        let rows = secret
            .get("cookies")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].get("name").and_then(Value::as_str), Some("sessionid"));
        assert_eq!(rows[0].get("domain").and_then(Value::as_str), Some(".doubao.com"));
        assert_eq!(rows[0].get("path").and_then(Value::as_str), Some("/"));
        assert!(rows[0].get("expires").is_some_and(Value::is_null));
        let fingerprint = secret.get("fingerprint").cloned().unwrap_or(Value::Null);
        assert_eq!(
            fingerprint.get("deviceId").and_then(Value::as_str),
            Some("dev")
        );
        assert_eq!(
            fingerprint.get("msToken").and_then(Value::as_str),
            Some("mt")
        );
        // 空指纹字段不落（写空串等于伪造「上游给了一个空值」）
        assert!(fingerprint.get("webId").is_none());
        assert_eq!(
            secret.get("sessionTail").and_then(Value::as_str),
            Some("cdef")
        );
        // 缺登录态 / 空输入都是 400
        assert_eq!(
            normalize_paste("foo=bar", None, None, None)
                .err()
                .map(|error| error.status_code),
            Some(400)
        );
        assert_eq!(
            normalize_paste("   ", None, None, None)
                .err()
                .map(|error| error.status_code),
            Some(400)
        );
    }

    /// **脱敏**：任何对外字符串都不含 Cookie 值（只出名字与尾号）
    #[test]
    fn masking_never_leaks_cookie_values() {
        let pairs = parse_cookie_string("sessionid=SUPERSECRETVALUE; passport_csrf_token=CSRFVALUE");
        let credentials = account_from_cookies("id", "n", &pairs, None);
        let masked = credentials.masked_cookies();
        assert!(!masked.contains("SUPERSECRETVALUE"));
        assert!(!masked.contains("CSRFVALUE"));
        assert!(masked.contains("sessionid"));
        // 身份只露尾 4 位（`SUPERSECRETVALUE` 的末 4 位是 `ALUE`）
        let identity = credentials.masked_identity();
        assert!(!identity.contains("SUPERSECRET"));
        assert!(identity.ends_with("ALUE"));
        // csrf 取值链：passport_csrf_token 优先
        assert_eq!(credentials.csrf_token().as_deref(), Some("CSRFVALUE"));
    }

    /// 本地检查：合格给出身份与 Cookie 计数；不合格给出可读原因
    #[test]
    fn local_check_reports_identity_and_reasons() {
        // 合格：给出身份（尾 4 位）与 Cookie 计数
        let good = local_check(&json!({"cookie": "sessionid=abcdefgh"}));
        let (identity, cookie_count) = match &good {
            LocalCheck::Ok {
                identity,
                cookie_count,
                ..
            } => (identity.clone(), *cookie_count),
            // 不合格时走这里：把 Bad 的原因当断言消息带出来
            LocalCheck::Bad { reason } => (format!("不合格：{reason}"), usize::MAX),
        };
        assert!(identity.ends_with("efgh"), "身份应当是尾 4 位：{identity}");
        assert_eq!(cookie_count, 1);

        // 不合格：缺登录态，给出可读原因
        let bad = local_check(&json!({"cookie": "foo=bar"}));
        let reason = match &bad {
            LocalCheck::Bad { reason } => reason.clone(),
            LocalCheck::Ok { identity, .. } => format!("意外合格：{identity}"),
        };
        assert!(
            reason.starts_with("豆包工作账号缺少登录态 Cookie"),
            "原因应当点明缺登录态：{reason}"
        );
    }

    /// 从记录读 Cookie 名（公开形态）：只出名不出值
    #[test]
    fn cookie_names_are_read_without_values() {
        let record = json!({"cookies": [
            {"name": "sessionid", "value": "SECRET"},
            {"name": "", "value": "x"}
        ]});
        let names = cookie_names_from_record(&record);
        assert_eq!(names, vec!["sessionid".to_string()]);
        // 整串形态也要能读
        let flat = cookie_names_from_record(&json!({"cookie": "a=1; b=2"}));
        assert_eq!(flat, vec!["a".to_string(), "b".to_string()]);
    }
}
