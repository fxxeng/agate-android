//! KukuAI 的「业务会话令牌」换发：BDUSS + PTOKEN → genflowpro 作用域的 STOKEN。
//!
//! ── 为什么需要换发 ──────────────────────────────────────────
//! KukuAI 的业务接口（`userreport` / `model/list` / `sendmsg`…）认的是**按产品
//! 签发**的 STOKEN，而网页登录 Cookie 里那份是**通行证级**的 —— 无论带多少
//! Cookie 都过不了校验（`errno=-6`）。症状很有欺骗性：余额接口只看 BDUSS，所以
//! 「余额能查、模型拉不下」（跨天、跨客户端、跨 TLS 指纹、跨 Cookie 子集全部
//! 复现，已逐一排除其它变量）。
//!
//! 换发材料是 `PTOKEN`（`credentials::ptoken_of`），端点：
//! `POST https://passport.baidu.com/v3/login/api/auth`。
//!
//! ── 签名：纯算法，不加载 DLL ────────────────────────────────
//! 官方客户端用 `genflowengine.dll` 的 `genflow_engine_get_login_auth_param`
//! 构造参数体，但它本质只是「六个字段拼串 + 内置密钥 + 一次 MD5」：
//!
//!   sig  = md5("appid=1&bduss=<BDUSS>&ptoken=<PTOKEN>&return_type=1&\
//!              tpl=genflowpro&tpl_list=genflowpro|netdisk&sign_key=<KEY>")
//!   body = "…同一串（去掉 sign_key）…&sig=<sig>"
//!
//! 密钥与公式由 `IKUNHEIHIE/kuku2api`（0BSD）用真实 DLL 对两组合成输入验证过、
//! 再以真实抓包复算过；上游 3.4.1 的插件版即按此实现（见 `upstream/` 的
//! `Agent2Providers-v3.4.1/crates/providers/src/providers/kuku/engine.rs`）。
//!
//! ── Android 为什么必须走纯算法（本次改动的理由）────────────
//! 旧实现 `LoadLibraryW("genflowengine.dll")` 取签名参数：那是 Windows 客户端
//! 安装目录里的原生库，Android 既没有这个文件也没有 Win32 加载器，换发恒失败
//! ⇒ 网页登录 / 粘贴 Cookie 的账号一律 `errno=-6`，只有「从桌面客户端导出」的
//! 账号能用 —— 于是客户端把整家置灰成「Android 端暂不可用」。改成纯算法后
//! 这条链在**任何平台**都能换发，与桌面客户端彻底解耦。
//!
//! ── 缺签 / 错签不假装成功 ───────────────────────────────────
//! 上游回 `errno=110003`（另一种表现是 `-6`）：调用方按 `Err` 处理，回退到
//! 「用原 STOKEN 继续」（与网关「换发失败只记日志、不阻断」的取向一致）。
//!
//! ── 影响面 ──────────────────────────────────────────────────
//! 只影响「网页登录 / 手粘 Cookie」的账号（其 Cookie 里那份是通行证级
//! STOKEN）；桌面端导入的账号 Cookies 里带的就是业务级 STOKEN，本不需要换发。
//! `SIGN_KEY` 是编译进官方客户端的常量，上游若更换需同步更新（换错的唯一
//! 表现就是 110003，不会有别的歧义）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use md5::{Digest as _, Md5};
use serde_json::Value;

use crate::server::core::egress;
use crate::server::errors::GatewayError;

/// 换发端点（passport 的 v3 登录 API，客户端 `app.asar` 认证模块的 `lo` 常量）
const PASSPORT_AUTH_URL: &str = "https://passport.baidu.com/v3/login/api/auth";

/// 官方引擎内置的签名密钥，编译进 `genflowengine.dll`（kuku2api 用真实 DLL
/// 与真实抓包双重验证）。上游更换时这里要跟着更新。
const SIGN_KEY: &str = "34868b316de55815273b2616954b0286";

/// 换发这一步对「客户端引擎 UA」敏感 —— 浏览器 UA 会被判成非客户端。
/// 取值与抓包到的 `/v3/login/api/auth` 请求逐字一致（与业务接口那支 UA 不同）。
const ENGINE_UA: &str = "genflow;1.6.5;PC;PC-Windows;10.0.26200;GenFlowPro";

/// 签名覆盖的字段串（**顺序即签名顺序**，改一个字符服务端就回 110003）。
///
/// 单独抽一个函数而不是在两处各写一遍：`sig` 的材料与上行报文必须逐字一致，
/// 唯一保证办法是同一个来源。BDUSS 是 base64 风格、PTOKEN 是十六进制，都不含
/// 需要百分号编码的字符，`|` 也不需要转义 —— 与官方客户端一样直接拼串。
fn signed_fields(bduss: &str, ptoken: &str) -> String {
    format!(
        "appid=1&bduss={bduss}&ptoken={ptoken}&return_type=1&tpl=genflowpro&tpl_list=genflowpro|netdisk"
    )
}

/// 小写十六进制（`md-5` 的摘要不直接实现 `LowerHex`，自己转更稳）。
fn md5_hex(bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 构造换发用的签名参数体：公共字段 + `sign_key` 算签名 → 报文换成 `sig`。
fn build_auth_param(bduss: &str, ptoken: &str) -> String {
    let base = signed_fields(bduss, ptoken);
    let signature = md5_hex(format!("{base}&sign_key={SIGN_KEY}").as_bytes());
    format!("{base}&sig={signature}")
}

/// 用 BDUSS + PTOKEN 换发「genflowpro 作用域」的 STOKEN。
///
/// 成功返回新 STOKEN；失败（缺材料 / 被拒 / 网络异常）返回带可读原因的错误，
/// 由调用方决定回退方式（本项目的口径是「只记日志，用原凭证继续」）。
pub async fn exchange_genflowpro_stoken(bduss: &str, ptoken: &str) -> Result<String, GatewayError> {
    if bduss.trim().is_empty() || ptoken.trim().is_empty() {
        return Err(GatewayError::with_status(
            400,
            "换发 KukuAI 会话令牌需要 BDUSS 与 PTOKEN（登录 Cookie 里缺 PTOKEN）",
        ));
    }
    let body = build_auth_param(bduss, ptoken);
    // 为什么直发一次而不是走 `http::send_json`：那个函数的 body 是 JSON，
    // passport 只认 `application/x-www-form-urlencoded`（发错编码的表现是 400
    // `invalid_request`，与「凭证不对」难以区分）。
    let client = egress::client_for(None);
    let response = client
        .post(PASSPORT_AUTH_URL)
        .header("Content-Type", "application/x-www-form-urlencoded;charset=UTF-8")
        .header("Referer", format!("{}/genflowpro", super::BASE_URL))
        .header("Origin", super::BASE_URL)
        .header("User-Agent", ENGINE_UA)
        .body(body)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|error| {
            GatewayError::with_status(502, format!("换发 KukuAI 会话令牌失败：{error}"))
        })?;
    let status = response.status().as_u16();
    let value: Value = response.json().await.map_err(|error| {
        GatewayError::with_status(502, format!("换发 KukuAI 会话令牌响应异常：{error}"))
    })?;
    if !(200..300).contains(&status) {
        return Err(GatewayError::with_status(
            502,
            format!("换发 KukuAI 会话令牌失败（HTTP {status}）"),
        ));
    }
    let errno = value.get("errno").and_then(Value::as_i64).unwrap_or(-1);
    if errno != 0 {
        let message = value
            .get("errmsg")
            .or_else(|| value.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        return Err(GatewayError::with_status(
            502,
            format!("换发 KukuAI 会话令牌被拒（errno={errno}：{message}）"),
        ));
    }
    let stoken = value
        .pointer("/stoken_list/genflowpro")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if stoken.is_empty() {
        return Err(GatewayError::with_status(
            502,
            "换发 KukuAI 会话令牌成功但响应缺少 genflowpro 令牌",
        ));
    }
    Ok(stoken)
}

#[cfg(test)]
mod tests {
    //! 这两条钉的是「签名材料」而不是网络：字段顺序、分隔符、`sign_key` 与
    //! `sig` 的区别都是上游逐字复算的对象，任何一处改动都会让线上回 110003
    //! （而日志里只看到一句「被拒」，排查成本极高）。期望值由独立实现
    //! （python hashlib）算出，不是拿本文件自己的输出回填。

    use super::*;

    /// 哈希接线要对：用的是 `md-5` crate，不是自搓实现。
    #[test]
    fn md5_matches_known_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    /// 金标：签名字符串与上行报文逐字固定。
    #[test]
    fn auth_param_matches_golden_signature() {
        let base = concat!(
            "appid=1&bduss=BDUSSX&ptoken=PTOKENY&return_type=1",
            "&tpl=genflowpro&tpl_list=genflowpro|netdisk"
        );
        assert_eq!(signed_fields("BDUSSX", "PTOKENY"), base);
        assert_eq!(
            build_auth_param("BDUSSX", "PTOKENY"),
            concat!(
                "appid=1&bduss=BDUSSX&ptoken=PTOKENY&return_type=1",
                "&tpl=genflowpro&tpl_list=genflowpro|netdisk",
                "&sig=ba22336cd315a7cefa85cab235f86069"
            )
        );
        // 密钥只参与签名，不进上行报文。
        let body = build_auth_param("BDUSSX", "PTOKENY");
        assert!(!body.contains(SIGN_KEY), "上行报文不应包含签名密钥");
        assert!(!body.contains("sign_key="), "上行报文不应包含 sign_key 字段");
    }

    /// BDUSS / PTOKEN 原样拼入（不做百分号编码）—— 与官方客户端同口径。
    #[test]
    fn auth_param_keeps_material_verbatim() {
        let body = build_auth_param("a+b/c==", "deadBEEF");
        assert!(body.contains("bduss=a+b/c=="));
        assert!(body.contains("ptoken=deadBEEF"));
    }
}
