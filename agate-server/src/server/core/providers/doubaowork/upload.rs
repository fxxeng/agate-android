//! 文件 / 图片上传：`prepare_upload` 申请凭证 → imageX top API 走完四步。
//!
//! ── 上游长什么样（[二进制] 逐字；可以当作协议的完整规格）──────────
//! 上传**不是**一个接口，而是四个连续步骤 + 一次对象直传：
//! ```text
//!   1. POST /alice/resource/prepare_upload            （带风控 query + Cookie）
//!        body {tenant_id: "5", scene_id: "5", resource_type: 1|2}
//!        → data {service_id, upload_host, upload_path_prefix, upload_auth_token}
//!        upload_auth_token = {access_key, secret_key, session_token,
//!                             current_time, expired_time}       ← STS 临时凭证
//!   2. GET  /top/v1?Action=ApplyImageUpload&Version=2018-08-01&ServiceId=…
//!        （**SigV4 签名**；FileSize / FileExtension / s=随机8字节hex）
//!        → Result.UploadAddress {StoreInfos[{StoreUri,Auth,UploadID}],
//!                                UploadHosts[], SessionKey, UploadHeader}
//!   3. POST https://{UploadHosts[0]}/upload/v1/{StoreUri}    （对象直传）
//!        header: Authorization={Auth}、Content-CRC32、Content-Type + UploadHeader
//!   4. POST /top/v1?Action=CommitImageUpload&Version=2018-08-01&ServiceId=…
//!        body {"SessionKey": …}（同样 SigV4）
//!        → Result.Results[0].Uri                            ← 这就是资源 uri
//! ```
//!
//! ── SigV4 是对字节的，不是对「差不多」的（本文件最容易错的一处）─────
//! [二进制] 里签名的构件逐字都在：
//! ```text
//!   AWS4-HMAC-SHA256  AWS4  <access_key>/<YYYYMMDD>/cn-north-1/imagex/aws4_request
//!   SignedHeaders=host;x-amz-date;x-amz-security-token
//!   host:  x-amz-date:  x-amz-security-token:
//!   X-Amz-Date / X-Amz-Content-Sha256 / x-amz-security-token
//!   签名域：<date>/cn-north-1/imagex/aws4_request
//! ```
//! 三条**必须按原样**的细节（错一个字节签名就对不上，而错误表现是
//! `403 SignatureDoesNotMatch`，看不出是哪儿错了）：
//!   1. **`SignedHeaders` 只有三个头**（`host` / `x-amz-date` /
//!      `x-amz-security-token`）—— 参考实现把 `content-type` 等也签进去了，
//!      本实现按二进制那套（三个），因为那是官方插件的行为；
//!   2. **`x-amz-content-sha256` 参与 canonical headers 但不在 SignedHeaders 里**：
//!      [二进制] 的 `SignedHeaders=` 后面就是那三个，没有它。本实现遵循二进制；
//!      （若实测发现要签四个头，改动点只有 [`sign_v4`] 的 `signed_header_names`。）
//!   3. **payload hash 是对 body 的 sha256**（GET 时是对空串），
//!      与 [二进制] 里的 `X-Amz-Content-Sha256` 一致。
//!
//! ── 图片 / 文件走的是同一条链（`resource_type` 不同）────────────
//! `1` = 文件、`2` = 图片（[二进制] `ResourceTypeFile=1` / `ResourceTypeImage=2`）。
//! 两者的后续步骤完全一样，因此本文件只有 [`upload_bytes`] 一个入口。
//!
//! ── 大小上限（**拒绝在本地**，不把超限内容传上去）────────────────
//! [二进制] 里 ` 20 MiB` 出现两次（紧邻 `PNG` / `JPEG` / `WebP` 串）——
//! 那是图片的上限。本实现按同一口径设 [`MAX_UPLOAD_BYTES`]（20 MiB），
//! 超限当场 400 并给出可执行文案（而不是让上游回一个语焉不详的拒绝）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::core::auth_http::send_raw;
use crate::server::errors::GatewayError;

use super::credentials::Credentials;
use super::endpoints;

/// 资源类型（[二进制] `resource_center_model.ResourceType` 的取值）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResourceType {
    /// 文件（`resource_type = 1`）
    File,
    /// 图片（`resource_type = 2`）
    Image,
}

impl ResourceType {
    /// 上游 `resource_type` 的数值形态
    pub fn code(self) -> i64 {
        match self {
            ResourceType::File => 1,
            ResourceType::Image => 2,
        }
    }

    /// `EntryType` 一类展示用的名字（日志里用）
    pub fn label(self) -> &'static str {
        match self {
            ResourceType::File => "文件",
            ResourceType::Image => "图片",
        }
    }
}

/// `prepare_upload` 的 tenant（[二进制] `tenant_id` = `5`）
const UPLOAD_TENANT_ID: &str = "5";
/// `prepare_upload` 的 scene（[二进制] `scene_id` = `5`，即 `BOT_CHAT`）
const UPLOAD_SCENE_ID: &str = "5";

/// imageX top API 的版本（[二进制] `Version=2018-08-01`）
const IMAGEX_VERSION: &str = "2018-08-01";
/// imageX 区域（[二进制] `cn-north-1`）
const IMAGEX_REGION: &str = "cn-north-1";
/// imageX 服务名（[二进制] `imagex`，签名域的一部分）
const IMAGEX_SERVICE: &str = "imagex";

/// 上传大小上限（[二进制] ` 20 MiB`，图片与文件同一口径）
pub const MAX_UPLOAD_BYTES: usize = 20 << 20;

/// 单步请求超时（毫秒）：每一步都是控制面，给短
const STEP_TIMEOUT_MS: u64 = 30_000;

/// 对象直传的超时（毫秒）：要传字节，给得宽（与 [`endpoints::UPLOAD_TIMEOUT_MS`] 同量级）
const TRANSFER_TIMEOUT_MS: u64 = 120_000;

/// 上传前的**本地校验**（零网络、零 IO）。
///
/// ── 为什么抽成一个**同步**函数（而不是留在 [`upload_bytes`] 里）────
/// 两条理由：
///   1. 它是纯判断，测试不必起一个 tokio 运行时就能覆盖（起运行时的测试
///      在 CI 上是额外的脆弱面）；
///   2. 「多大算过大」这条口径将来要被**调用方**用到（前端预检、
///      `plan::collect_images` 的提前过滤），放成公开的独立函数才能共用 ——
///      抄一份到别处的下场是两处阈值慢慢分叉。
pub fn validate_upload(resource_type: ResourceType, data: &[u8]) -> Result<(), GatewayError> {
    if data.is_empty() {
        return Err(GatewayError::with_status(400, "上传内容为空"));
    }
    if data.len() > MAX_UPLOAD_BYTES {
        return Err(GatewayError::with_status(
            400,
            format!(
                "{}过大（{} 字节，上限 {} MiB）：请压缩后重试",
                resource_type.label(),
                data.len(),
                MAX_UPLOAD_BYTES >> 20
            ),
        ));
    }
    Ok(())
}

/// 把一段字节上传到豆包资源中心，返回可直接写进 `content_block` 的资源 uri。
///
/// 四步链路见模块头；任何一步失败都返回可读的 `GatewayError`（带是哪一步）。
pub async fn upload_bytes(
    credentials: &Credentials,
    resource_type: ResourceType,
    file_name: &str,
    mime: &str,
    data: &[u8],
) -> Result<String, GatewayError> {
    // 本地先拦（超限 / 空内容**不下网**，见 [`validate_upload`]）
    validate_upload(resource_type, data)?;
    let name = if file_name.trim().is_empty() {
        match resource_type {
            ResourceType::Image => "image.png".to_string(),
            ResourceType::File => "file.bin".to_string(),
        }
    } else {
        file_name.trim().to_string()
    };

    // ── 第一步：申请上传凭证 ──
    // 解构而不是逐字段 clone：这个结构的三个字段下面全都要用
    let Prepared {
        service_id,
        upload_host,
        token,
    } = prepare_upload(credentials, resource_type).await?;

    // ── 第二步：ApplyImageUpload ──
    let applied = apply_image_upload(&token, &service_id, &name, data.len()).await?;
    let session_key = applied.session_key.clone();
    let store = applied.store.clone();

    // ── 第三步：对象直传 ──
    // 主机优先用 `ApplyImageUpload` 返回的 `UploadHosts[0]`（那是**这次上传
    // 实际可用**的节点），空时才回落到 `prepare_upload` 给的 `upload_host`
    // （配置层面的默认域名）。[Go参考] 在 `UploadHosts` 为空时直接报错，
    // 本实现多一层回落 —— 依据是 [二进制] 里 `upload_host` 与
    // `upload_auth_token` **同处** `prepare_upload` 的响应结构，
    // 说明它本来就是给上传用的域名，不是只作展示。
    let host = if store.upload_host.trim().is_empty() {
        upload_host
    } else {
        store.upload_host.clone()
    };
    put_object(&host, &store, mime, data).await?;

    // ── 第四步：CommitImageUpload ──
    commit_image_upload(&token, &service_id, &session_key).await
}

/// 一次 `prepare_upload` 的结果
struct Prepared {
    service_id: String,
    upload_host: String,
    token: UploadAuthToken,
}

/// STS 临时凭证（[二进制] `upload_auth_token` 的五个字段）
#[derive(Clone, Debug, Default)]
struct UploadAuthToken {
    access_key: String,
    secret_key: String,
    session_token: String,
}

/// 申请上传凭证（链路第一步）
async fn prepare_upload(
    credentials: &Credentials,
    resource_type: ResourceType,
) -> Result<Prepared, GatewayError> {
    let query = endpoints::encode_query(&endpoints::security_params(&credentials.fingerprint));
    let url = format!("{}?{}", endpoints::prepare_upload_url(), query);
    let body = json!({
        "tenant_id": UPLOAD_TENANT_ID,
        "scene_id": UPLOAD_SCENE_ID,
        "resource_type": resource_type.code(),
    });
    let mut headers = endpoints::base_headers();
    headers.extend(endpoints::json_headers());
    headers.extend(endpoints::auth_headers(
        &credentials.cookie_header(),
        credentials.csrf_token().as_deref(),
    ));
    let response = send_raw("POST", &url, Some(&body), &headers, None, Some(STEP_TIMEOUT_MS))
        .await
        .map_err(|error| {
            GatewayError::with_status(502, format!("申请上传凭证失败：{error}"))
        })?;
    if response.status == 401 || response.status == 403 {
        return Err(GatewayError::with_status(
            401,
            format!(
                "豆包工作登录态已失效（HTTP {}）：请在账号页重新登录",
                response.status
            ),
        ));
    }
    let payload = response.payload.unwrap_or(Value::Null);
    if !response.ok {
        return Err(GatewayError::with_status(
            502,
            format!("申请上传凭证被拒绝：{}", readable_error(&payload)),
        ));
    }
    // 业务码（0 = 成功）
    if let Some(code) = payload.get("code").and_then(Value::as_i64) {
        if code != 0 {
            return Err(GatewayError::with_status(
                502,
                format!(
                    "申请上传凭证被拒绝（code={code}）：{}",
                    readable_error(&payload)
                ),
            ));
        }
    }
    let data = payload.get("data").unwrap_or(&payload);
    let service_id = text_of(data, "service_id");
    let upload_host = text_of(data, "upload_host");
    let token_node = data.get("upload_auth_token").unwrap_or(&Value::Null);
    let token = UploadAuthToken {
        access_key: text_of(token_node, "access_key"),
        secret_key: text_of(token_node, "secret_key"),
        session_token: text_of(token_node, "session_token"),
    };
    if service_id.is_empty() || token.access_key.is_empty() || token.secret_key.is_empty() {
        return Err(GatewayError::with_status(
            502,
            "申请上传凭证返回不完整（缺 service_id / access_key / secret_key）",
        ));
    }
    Ok(Prepared {
        service_id,
        upload_host,
        token,
    })
}

/// 一次 `ApplyImageUpload` 的结果
struct Applied {
    session_key: String,
    store: StoreInfo,
}

/// 一个上传地址（`UploadAddress.StoreInfos[0]` + `UploadHosts[0]`）
#[derive(Clone, Debug, Default)]
struct StoreInfo {
    store_uri: String,
    auth: String,
    upload_host: String,
    header: Vec<(String, String)>,
}

/// 申请上传地址（链路第二步，SigV4 签名）
async fn apply_image_upload(
    token: &UploadAuthToken,
    service_id: &str,
    file_name: &str,
    size: usize,
) -> Result<Applied, GatewayError> {
    let mut params: Vec<(String, String)> = vec![
        ("Action".to_string(), "ApplyImageUpload".to_string()),
        ("Version".to_string(), IMAGEX_VERSION.to_string()),
        ("ServiceId".to_string(), service_id.to_string()),
        ("NeedFallback".to_string(), "true".to_string()),
        ("s".to_string(), endpoints::random_hex(8)),
        ("FileSize".to_string(), size.to_string()),
    ];
    // `FileExtension` 带**前导点**（[二进制] 的 `FileExtension` 与 [Go参考]
    // `getFileSuffix` 的返回值形态：`.pdf`）
    if let Some(extension) = file_extension(file_name) {
        params.push(("FileExtension".to_string(), format!(".{extension}")));
    }
    perform_top_request("GET", token, &params, None).await
}

/// 提交上传（链路第四步，SigV4 签名）
async fn commit_image_upload(
    token: &UploadAuthToken,
    service_id: &str,
    session_key: &str,
) -> Result<String, GatewayError> {
    let params: Vec<(String, String)> = vec![
        ("Action".to_string(), "CommitImageUpload".to_string()),
        ("Version".to_string(), IMAGEX_VERSION.to_string()),
        ("ServiceId".to_string(), service_id.to_string()),
    ];
    let body = json!({ "SessionKey": session_key });
    let raw = perform_top_request_body("POST", token, &params, Some(&body)).await?;
    let uri = raw
        .pointer("/Result/Results/0/Uri")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let uri = match uri {
        Some(uri) => uri.to_string(),
        None => {
            return Err(GatewayError::with_status(
                502,
                format!(
                    "CommitImageUpload 未返回 uri：{}",
                    readable_error(&raw)
                ),
            ))
        }
    };
    Ok(uri)
}

/// 调一次 imageX top API（GET 无 body 形态，返回解析出的 `Applied`）
async fn perform_top_request(
    method: &str,
    token: &UploadAuthToken,
    params: &[(String, String)],
    body: Option<&Value>,
) -> Result<Applied, GatewayError> {
    let raw = perform_top_request_body(method, token, params, body).await?;
    // ── 错误信封（`ResponseMetadata.Error`）──
    let error = raw.pointer("/ResponseMetadata/Error");
    if let Some(error) = error.filter(|value| !value.is_null()) {
        let code = text_of(error, "Code");
        let message = text_of(error, "Message");
        return Err(GatewayError::with_status(
            502,
            format!("imageX 接口被拒绝（{code}）：{message}"),
        ));
    }
    let store_infos = raw
        .pointer("/Result/UploadAddress/StoreInfos")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let hosts = raw
        .pointer("/Result/UploadAddress/UploadHosts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let first = store_infos.first().cloned().unwrap_or(Value::Null);
    let store_uri = text_of(&first, "StoreUri");
    if store_uri.is_empty() || hosts.is_empty() {
        return Err(GatewayError::with_status(
            502,
            "ApplyImageUpload 未返回上传地址（StoreUri / UploadHosts 缺失）",
        ));
    }
    let header = raw
        .pointer("/Result/UploadAddress/UploadHeader")
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|text| (key.clone(), text.to_string()))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // `SessionKey` 在 `Result.UploadAddress` 下（[二进制] 那串键名里它紧随
    // `UploadHosts` 之后）—— 别在顶层找，找不到会让 Commit 那一步拿到空 session
    let address = raw.pointer("/Result/UploadAddress").unwrap_or(&Value::Null);
    Ok(Applied {
        session_key: text_of(address, "SessionKey"),
        store: StoreInfo {
            store_uri,
            auth: text_of(&first, "Auth"),
            upload_host: hosts
                .first()
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
            header,
        },
    })
}

/// 调一次 imageX top API，返回原始响应体（含 SigV4 签名）
async fn perform_top_request_body(
    method: &str,
    token: &UploadAuthToken,
    params: &[(String, String)],
    body: Option<&Value>,
) -> Result<Value, GatewayError> {
    if token.access_key.trim().is_empty() {
        return Err(GatewayError::with_status(502, "缺少上传凭证（access_key 为空）"));
    }
    let body_text = body.map(|value| value.to_string()).unwrap_or_default();
    let body_bytes = body_text.as_bytes();
    let host = host_of(&endpoints::imagex_top_url());
    let signed = sign_v4(
        method,
        endpoints::IMAGEX_TOP_PATH,
        params,
        &host,
        body_bytes,
        token,
    );
    let query = endpoints::encode_query(params);
    let url = format!("{}?{}", endpoints::imagex_top_url(), query);
    let mut headers: Vec<(String, String)> = vec![
        ("Accept".to_string(), "application/json".to_string()),
        ("Authorization".to_string(), signed.authorization),
    ];
    headers.extend(signed.headers);
    if body.is_some() {
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
    }
    let response = {
        // `send_raw` 只接受 `Option<&Value>`，因此这里把 body 原样传给它
        // （签名用的是同一份 `Value` 序列化结果，两者必须逐字节一致 ——
        // 这也是为什么 body 在签名与发送两处都从同一个 `Value` 取）
        send_raw(
            method,
            &url,
            body,
            &headers,
            None,
            Some(STEP_TIMEOUT_MS),
        )
        .await
        .map_err(|error| GatewayError::with_status(502, format!("imageX 请求失败：{error}")))?
    };
    let payload = response.payload.unwrap_or(Value::Null);
    if !response.ok {
        return Err(GatewayError::with_status(
            502,
            format!(
                "imageX 返回 HTTP {}：{}",
                response.status,
                readable_error(&payload)
            ),
        ));
    }
    Ok(payload)
}

/// 对象直传（链路第三步；**不是 SigV4**，用 `ApplyImageUpload` 返回的 `Auth`）
async fn put_object(
    host: &str,
    store: &StoreInfo,
    mime: &str,
    data: &[u8],
) -> Result<(), GatewayError> {
    if host.trim().is_empty() {
        return Err(GatewayError::with_status(502, "缺少上传主机（UploadHosts 为空）"));
    }
    let url = format!("https://{}/upload/v1/{}", host.trim(), store.store_uri);
    let crc32 = crc32_hex(data);
    let mut headers: Vec<(String, String)> = vec![
        ("Authorization".to_string(), store.auth.clone()),
        ("Content-CRC32".to_string(), crc32),
        (
            "Content-Type".to_string(),
            if mime.trim().is_empty() {
                "application/octet-stream".to_string()
            } else {
                mime.trim().to_string()
            },
        ),
    ];
    headers.extend(store.header.clone());
    // 直传是 **POST + 原始字节**：`send_raw` 只发 JSON，因此这里手写一个
    // 最小实现（用 `egress` 的 client 与之同一连接池 —— 不另起一套 HTTP 栈）。
    let client = crate::server::core::egress::client_for(None);
    let mut builder = client.post(&url);
    for (key, value) in &headers {
        builder = builder.header(key, value);
    }
    let response = builder
        .body(data.to_vec())
        .timeout(std::time::Duration::from_millis(TRANSFER_TIMEOUT_MS))
        .send()
        .await
        .map_err(|error| GatewayError::with_status(502, format!("上传对象失败：{error}")))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    if status != 200 {
        return Err(GatewayError::with_status(
            502,
            format!("上传对象失败（HTTP {status}）：{}", truncate(&text, 200)),
        ));
    }
    // 对象存储以 JSON 返回，成功码是 2000（[Go参考] 同款判据）
    if let Ok(payload) = serde_json::from_str::<Value>(&text) {
        if let Some(code) = payload.get("code").and_then(Value::as_i64) {
            if code != 0 && code != 2000 {
                return Err(GatewayError::with_status(
                    502,
                    format!(
                        "上传对象被拒绝（code={code}）：{}",
                        readable_error(&payload)
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// 一次 SigV4 签名的产物
struct SignedRequest {
    /// `Authorization` 头的值
    authorization: String,
    /// 需要随请求发送的头（`X-Amz-Date` / `x-amz-security-token` /
    /// `X-Amz-Content-Sha256`）
    headers: Vec<(String, String)>,
}

/// AWS SigV4（ByteDance top API 变体）——见模块头对三处细节的说明。
fn sign_v4(
    method: &str,
    path: &str,
    params: &[(String, String)],
    host: &str,
    body: &[u8],
    token: &UploadAuthToken,
) -> SignedRequest {
    let (amz_date, date_stamp) = amz_timestamps();
    let payload_hash = sha256_hex(if body.is_empty() { b"" } else { body });

    // ── canonical headers：**只有三个**（host / x-amz-date / x-amz-security-token）──
    let mut signed_header_names: Vec<(&str, String)> = vec![
        ("host", host.to_string()),
        ("x-amz-date", amz_date.clone()),
    ];
    let mut headers: Vec<(String, String)> = vec![
        ("X-Amz-Date".to_string(), amz_date.clone()),
        ("host".to_string(), host.to_string()),
    ];
    if !token.session_token.trim().is_empty() {
        signed_header_names.push(("x-amz-security-token", token.session_token.clone()));
        headers.push((
            "x-amz-security-token".to_string(),
            token.session_token.clone(),
        ));
    }
    // `X-Amz-Content-Sha256` **发送但不参与 SignedHeaders**（见模块头第 2 条）
    headers.push(("X-Amz-Content-Sha256".to_string(), payload_hash.clone()));

    // 排序（SigV4 要求 canonical headers 按名排序）
    signed_header_names.sort_by(|left, right| left.0.cmp(right.0));
    let canonical_headers = signed_header_names
        .iter()
        .map(|(name, value)| {
            // 值内部的连续空白折成一个空格（SigV4 的规范化要求）
            let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
            format!("{name}:{value}\n")
        })
        .collect::<String>();
    let signed_headers = signed_header_names
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(";");

    // ── canonical query string：**排序 + RFC3986**（与业务 query 的拼法不同）──
    let mut sorted_params: Vec<&(String, String)> = params.iter().collect();
    sorted_params.sort_by(|left, right| left.0.cmp(&right.0));
    let canonical_query = sorted_params
        .iter()
        .map(|(key, value)| format!("{}={}", rfc3986(key), rfc3986(value)))
        .collect::<Vec<_>>()
        .join("&");

    let canonical_request = format!(
        "{method}\n{path}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );
    let scope = format!("{date_stamp}/{IMAGEX_REGION}/{IMAGEX_SERVICE}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    // ── 派生签名密钥：AWS4+secret → date → region → service → aws4_request ──
    let key = {
        let secret = format!("AWS4{}", token.secret_key);
        let date_key = hmac_sha256(secret.as_bytes(), &date_stamp);
        let region_key = hmac_sha256(&date_key, IMAGEX_REGION);
        let service_key = hmac_sha256(&region_key, IMAGEX_SERVICE);
        hmac_sha256(&service_key, "aws4_request")
    };
    let signature = hex_of(&hmac_sha256_raw(&key, string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        token.access_key
    );
    SignedRequest {
        authorization,
        headers,
    }
}

/// `(X-Amz-Date, dateStamp)` 两个时间戳（UTC，`YYYYMMDDTHHMMSSZ` / `YYYYMMDD`）。
///
/// 用 `chrono`（本项目既有依赖）而不是手写公历换算：签名的时间戳一旦算错，
/// 上游回的是 `403 RequestTimeTooSkewed` —— 一个不会指向「你日历算错了」的错误。
fn amz_timestamps() -> (String, String) {
    let now = chrono::Utc::now();
    (
        now.format("%Y%m%dT%H%M%SZ").to_string(),
        now.format("%Y%m%d").to_string(),
    )
}

/// 取 URL 的 host（`https://www.doubao.com` → `www.doubao.com`）
fn host_of(url: &str) -> String {
    url.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("")
        .to_string()
}

/// 文件名的扩展名（不含点；没有或过长时返回 `None`）
fn file_extension(name: &str) -> Option<String> {
    let (_, extension) = name.rsplit_once('.')?;
    let trimmed = extension.trim();
    if trimmed.is_empty() || trimmed.len() > 8 {
        return None;
    }
    Some(trimmed.to_ascii_lowercase())
}

/// RFC3986 转义（SigV4 的 canonical query 要求未保留字符一律百分号转义）
fn rfc3986(value: &str) -> String {
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

/// sha256 的十六进制形态（小写）
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex_of(&hasher.finalize())
}

/// HMAC-SHA256，返回字节
fn hmac_sha256_raw(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    // `Hmac::new_from_slice` 对任意长度 key 都不失败（HMAC 本来就没有 key 长度限制），
    // 但签名返回 `Result` ⇒ 这里用 `if let` 明确处理「理论上不可能」的那一支：
    // 失败时给空串（签名必然对不上，上游会拒）而不是 panic（release 是 panic=abort）。
    let mut mac = match <Hmac<sha2::Sha256> as Mac>::new_from_slice(key) {
        Ok(mac) => mac,
        Err(_) => return Vec::new(),
    };
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// HMAC-SHA256，返回十六进制形态（内部先取原始字节再编码 —— 与上面的区别只在这一步）
fn hmac_sha256(key: &[u8], data: &str) -> Vec<u8> {
    hmac_sha256_raw(key, data.as_bytes())
}

/// 字节 → 小写十六进制
fn hex_of(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// TOS 的 `Content-CRC32`（**8 位小写十六进制**，[二进制] `Content-CRC32` 串）
fn crc32_hex(data: &[u8]) -> String {
    format!("{:08x}", crc32(data))
}

/// CRC32（IEEE 多项式，手写表驱动 —— 不引新依赖）
fn crc32(data: &[u8]) -> u32 {
    // 表在首次调用时算一次（`OnceLock`：无锁读、无初始化顺序问题）
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (index, slot) in table.iter_mut().enumerate() {
            let mut value = index as u32;
            for _ in 0..8 {
                value = if value & 1 == 1 {
                    0xEDB8_8320 ^ (value >> 1)
                } else {
                    value >> 1
                };
            }
            *slot = value;
        }
        table
    });
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        let index = ((crc ^ u32::from(*byte)) & 0xFF) as usize;
        crc = table[index] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// 从对象里读一个字符串键
fn text_of(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("")
        .to_string()
}

/// 从响应里读一条可读的错误文案
fn readable_error(payload: &Value) -> String {
    for key in ["message", "msg", "Message", "error_msg", "error"] {
        if let Some(text) = payload.get(key).and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    if let Some(code) = payload.get("code").and_then(Value::as_i64) {
        return format!("code={code}");
    }
    truncate(&payload.to_string(), 200)
}

/// 按字符截断（尾部补省略号）
fn truncate(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let head: String = chars[..max].iter().collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 资源类型编码（[二进制] `ResourceTypeFile=1` / `ResourceTypeImage=2`）
    #[test]
    fn resource_type_codes_match_binary() {
        assert_eq!(ResourceType::File.code(), 1);
        assert_eq!(ResourceType::Image.code(), 2);
        assert_eq!(ResourceType::File.label(), "文件");
        assert_eq!(ResourceType::Image.label(), "图片");
    }

    /// CRC32 已知向量（`123456789` → `CBF43926`，IEEE 标准向量）
    #[test]
    fn crc32_matches_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32_hex(b"123456789"), "cbf43926");
        // 8 位小写十六进制（前导零要补上）
        assert_eq!(crc32_hex(b""), "00000000");
        assert_eq!(crc32_hex(b"a").len(), 8);
    }

    /// sha256 已知向量（空串与 `abc`；签名对不上时上游只回一个语焉不详的 403）
    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// HMAC-SHA256 已知向量（RFC 4231 test case 2）
    #[test]
    fn hmac_matches_rfc4231() {
        let mac = hmac_sha256(b"Jefe", "what do ya want for nothing?");
        assert_eq!(
            hex_of(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    /// RFC3986 转义（canonical query 的规范）
    #[test]
    fn rfc3986_escapes_reserved_characters() {
        assert_eq!(rfc3986("a&b=c"), "a%26b%3Dc");
        assert_eq!(rfc3986("ok-_.~"), "ok-_.~");
        assert_eq!(rfc3986("中文"), "%E4%B8%AD%E6%96%87");
        // 空格是 `%20`（不是 `+` —— 那是 form 编码，SigV4 要 RFC3986）
        assert_eq!(rfc3986("a b"), "a%20b");
    }

    /// ⚠️ `SignedHeaders` **只有三个头**（host / x-amz-date / x-amz-security-token）
    /// —— 多签一个头签名就对不上，而错误表现是 `403 SignatureDoesNotMatch`
    #[test]
    fn signed_headers_are_exactly_three() {
        let token = UploadAuthToken {
            access_key: "AKIDEXAMPLE".to_string(),
            secret_key: "SECRET".to_string(),
            session_token: "SESSION".to_string(),
        };
        let params = vec![
            ("Action".to_string(), "ApplyImageUpload".to_string()),
            ("Version".to_string(), IMAGEX_VERSION.to_string()),
        ];
        let signed = sign_v4(
            "GET",
            endpoints::IMAGEX_TOP_PATH,
            &params,
            "www.doubao.com",
            b"",
            &token,
        );
        assert!(signed
            .authorization
            .contains("SignedHeaders=host;x-amz-date;x-amz-security-token"));
        // 签名域三件套（日期 / 区域 / 服务）都要在
        assert!(signed.authorization.contains("/cn-north-1/imagex/aws4_request"));
        assert!(signed.authorization.contains("Credential=AKIDEXAMPLE/"));
        assert!(signed.authorization.starts_with("AWS4-HMAC-SHA256 "));
        // X-Amz-Content-Sha256 **发送**但不在 SignedHeaders 里
        let names: Vec<&str> = signed.headers.iter().map(|(key, _)| key.as_str()).collect();
        assert!(names.contains(&"X-Amz-Content-Sha256"));
        assert!(names.contains(&"X-Amz-Date"));
        assert!(names.contains(&"x-amz-security-token"));
        assert!(!signed
            .authorization
            .contains("x-amz-content-sha256"));
    }

    /// 没有 session_token 时那条头不参与签名（也不会被发出去）
    #[test]
    fn session_token_is_optional() {
        let token = UploadAuthToken {
            access_key: "AK".to_string(),
            secret_key: "SK".to_string(),
            session_token: String::new(),
        };
        let signed = sign_v4(
            "GET",
            endpoints::IMAGEX_TOP_PATH,
            &[],
            "www.doubao.com",
            b"",
            &token,
        );
        assert!(signed.authorization.contains("SignedHeaders=host;x-amz-date,"));
        assert!(!signed
            .headers
            .iter()
            .any(|(key, _)| key.as_str() == "x-amz-security-token"));
    }

    /// 签名是**确定性的**：同一个输入两次签名必须逐字相同
    /// （否则「有时成功有时 403」这种最难查的形态就会出现）
    #[test]
    fn signature_is_deterministic_for_same_input() {
        let token = UploadAuthToken {
            access_key: "AK".to_string(),
            secret_key: "SK".to_string(),
            session_token: "ST".to_string(),
        };
        let params = vec![("Action".to_string(), "CommitImageUpload".to_string())];
        let body = br#"{"SessionKey":"x"}"#;
        let first = sign_v4("POST", endpoints::IMAGEX_TOP_PATH, &params, "h", body, &token);
        let second = sign_v4("POST", endpoints::IMAGEX_TOP_PATH, &params, "h", body, &token);
        assert_eq!(first.authorization, second.authorization);
        // 换 body 必须换签名（不然就是没签 body）
        let other = sign_v4(
            "POST",
            endpoints::IMAGEX_TOP_PATH,
            &params,
            "h",
            br#"{"SessionKey":"y"}"#,
            &token,
        );
        assert_ne!(first.authorization, other.authorization);
    }

    /// host 提取（SigV4 的 canonical headers 里 host 必须精确）
    #[test]
    fn host_extraction() {
        assert_eq!(host_of("https://www.doubao.com"), "www.doubao.com");
        assert_eq!(host_of("https://bucket.example.com/path/x"), "bucket.example.com");
        assert_eq!(host_of("http://127.0.0.1:8080/a"), "127.0.0.1:8080");
    }

    /// 扩展名：带前导点拼在 `FileExtension` 上；无扩展名 / 超长不给
    #[test]
    fn file_extension_rules() {
        assert_eq!(file_extension("a.pdf"), Some("pdf".to_string()));
        assert_eq!(file_extension("A.PDF"), Some("pdf".to_string()));
        assert_eq!(file_extension("noext"), None);
        assert_eq!(file_extension("a."), None);
        assert_eq!(file_extension("a.verylongext"), None);
    }

    /// 大小上限与空内容在**本地**拦（不把超限内容传上去换一个语焉不详的拒绝）
    ///
    /// 走同步的 [`validate_upload`] —— 不需要 tokio 运行时，也不碰网络。
    #[test]
    fn oversize_and_empty_are_rejected_locally() {
        let oversized = vec![0u8; MAX_UPLOAD_BYTES + 1];
        let over = validate_upload(ResourceType::Image, &oversized);
        assert_eq!(over.err().map(|error| error.status_code), Some(400));
        let empty = validate_upload(ResourceType::File, &[]);
        assert_eq!(empty.err().map(|error| error.status_code), Some(400));
        // 边界：正好等于上限要通过（`>` 而不是 `>=`）
        let exact = vec![0u8; MAX_UPLOAD_BYTES];
        assert!(validate_upload(ResourceType::File, &exact).is_ok());
        assert!(validate_upload(ResourceType::Image, b"x").is_ok());
        // 上限是 20 MiB（[二进制] ` 20 MiB`）
        assert_eq!(MAX_UPLOAD_BYTES, 20 << 20);
    }

    /// 可读错误文案：优先 message 一族，其次业务码，最后截断原文
    #[test]
    fn readable_error_prefers_message() {
        assert_eq!(readable_error(&json!({"message": "boom"})), "boom");
        assert_eq!(readable_error(&json!({"Message": "boom2"})), "boom2");
        assert_eq!(readable_error(&json!({"code": 500})), "code=500");
        let long = readable_error(&json!({"unexpected": "x".repeat(500)}));
        assert!(long.chars().count() <= 201);
    }
}
