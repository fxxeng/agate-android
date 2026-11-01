//! `/chat/completion` 的请求构造：**OpenAI 的 `messages` → 上游桌面端信封**。
//!
//! ── 这一层是「两个协议之间唯一的翻译点」，所以最厚 ────────────────
//! 客户端说的是 OpenAI Chat（`{model, messages:[{role,content}], stream}`），
//! 上游说的是豆包桌面端的**消息块信封**：
//!
//! ```text
//!   { client_meta: { local_conversation_id, conversation_id, bot_id,
//!                    last_section_id, last_message_index, … },
//!     messages: [ { local_message_id, content_block: [ …块… ],
//!                   message_status, … } ],
//!     option:   { need_create_conversation, conversation_init_option,
//!                 need_ack_conversation, answer_with_suggest,
//!                 sse_recv_event_options: {support_chunk_delta},
//!                 req_create_time_sec, append_sse_event_scene,
//!                 message_storage_type, … },
//!     ext:      { sub_conv_firstmet_type, commerce_credit_config_enable,
//!                 use_deep_think, fp, … } }
//! ```
//!
//! ── 键名从哪来（**全部**是二进制字符串表里的逐字键，逐条可核）──────
//! [二进制] 里这一长串键是**连排**出现的，因此键名与「它们属于同一个信封」
//! 这件事都有依据：
//! ```text
//!   local_conversation_id last_message_index local_message_id option
//!   need_create_conversation conversation_init_option need_ack_conversation
//!   answer_with_suggest sse_recv_event_options support_chunk_delta
//!   req_create_time_sec append_sse_event_scene message_storage_type ext
//!   sub_conv_firstmet_type commerce_credit_config_enable
//!   content_block block_type attachment_block block_id parse_state
//!   review_state upload_status src image uri image_ori width height
//!   url_formats file name size identifier is_finish
//! ```
//! 另一段（`thread_local_message_id shared_folder_path agent_task_param
//! agent_task_param_change sandbox_auth_type_change need_modify_conversation
//! {"project_context":{}} model_extra_params model_config aggregate_params
//! provider_id general_task_param is_finish access_key`）属于**Agent 模式**的
//! 扩展位；本实现不发这些键（它们只在「Agent 任务」那条链上有意义），但把它们
//! 记在这里，免得下次核对时又得翻一遍二进制。
//!
//! ── 三条结构性决定，每条都有理由 ─────────────────────────────
//!   1. **多轮历史被拍平成单段文本**（与 [Go参考] 的 `flattenMessages` 同一做法）。
//!      豆包原生按 `conversation_id` 维护上下文，但 OpenAI 客户端**每次都带全量
//!      历史**；把历史塞进同一轮里的语义等价（模型看到的是同一串），且不依赖
//!      服务端会话 —— 也就不会出现「网关重启后上下文丢失」这类问题。
//!      `client_meta.conversation_id` 因此恒发 `"0"`（= 新会话）。
//!   2. **system / developer 消息提到最前面单独成段**（不是按出现顺序内联）：
//!      上游的 system 语义是「整场对话的约束」，而 OpenAI 客户端可能在中途
//!      再插一条 system —— 内联会把它变成「用户说了这句话」。
//!   3. **只有一条 user 消息时不加角色前缀**（`user: …`）：加了会把一句普通的
//!      提问变成「像在引用别人的话」，实测会轻微改变模型的自述口吻。多条时才加
//!      前缀（那时必须靠前缀区分谁说的）。
//!
//! ── 思考档位怎么定（`need_deep_think` / `use_deep_think`）───────
//! 上游用整数档位表达（[二进制] `deep_think` 键；[Go参考] 的
//! `ThinkOff=0 / ThinkOn=1 / ThinkExpert=3`）。本实现按下面的优先级取：
//!   1. 请求体顶层的 [`BODY_THINK_LEVEL_KEY`]（**网关内部通道**，见下）；
//!   2. 请求体里的 `reasoning_effort` / `reasoning` / `thinking`（客户端显式指定）；
//!   3. 模型名的后缀 / 关键词（本家目录里的模型名自带档位语义，见 [`super::models`]）；
//!   4. 默认 [`THINK_OFF`]。
//! `option.need_deep_think` 收**整数**、`ext.use_deep_think` 收**字符串**
//! （[Go参考] `strconv.Itoa(req.ThinkLevel)`）—— 两处类型不同是上游的形态，
//! 不要「顺手统一」。
//!
//! ⚠️ [`resolve_think_level`] 读的那个**内部通道**是刻意留的，理由如下
//! （改它之前先读这段）：映射绑定的思考等级由 `ProviderAdapter::reasoning_patch`
//! 注入，而那个 trait 方法**只能写一个顶层字段**（`ReasoningPatch::Set` 的契约）。
//! 若它直接写 `option`（本家档位的真正所在），则 `ext.use_deep_think` 会留在
//! 「客户端没指定」时那个推断值上 —— 两处口径不一致，而上游两处口径必须一致。
//! 因此 `reasoning_patch` 写的是**顶层整数通道**，本模块再把它同时落到
//! `option`（整数）与 `ext`（字符串）两处 —— 一个值、两处形态、必然一致。
//! 这个通道是**网关内部的**：最终的出网信封（`client_meta` / `messages` /
//! `option` / `ext`）里**没有**这个顶层的键（见 [`build`] 的构造），因此它
//! 不会漏给上游。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use serde_json::{json, Value};

use crate::server::errors::GatewayError;

use super::credentials::Credentials;
use super::endpoints;

/// 深度思考档位：快速模式（[二进制] `deep_think` 键；[Go参考] `ThinkOff`）
pub const THINK_OFF: i64 = 0;
/// 思考模式（带思维链）
pub const THINK_ON: i64 = 1;
/// 专家模式（深度推理）
pub const THINK_EXPERT: i64 = 3;

/// 默认 Bot（[Go参考] `DefaultBotID`，逐字）。
///
/// 那是「桌面端扩展 Bot，支持全部能力」的那一个 —— 换一个 bot 会少掉一部分
/// 能力（联网 / 深度思考），因此它是默认值而不是「随便挑一个」。
pub const DEFAULT_BOT_ID: &str = "7338286299411103781";

/// **网关内部**的思考档位通道（顶层整数键；见模块头那一整段说明）。
///
/// 前缀与 `protocol::FIELD_*` 一族（`_wb_`）不同：那一族是「跨协议保真」的
/// 暂存字段，会被 `strip_internal_fields` **统一剥离**；而本键必须在
/// `build_chat_request` 里被读到（剥离发生在 `send_body` 之后、构造计划之前，
/// 见 `provider_loop`），因此它不能复用那个前缀。取 `_agent2api_` 前缀以标明
/// 「网关内部」这一性质，并在 [`build`] 里确保不落进上游信封。
pub const BODY_THINK_LEVEL_KEY: &str = "_agent2api_think_level";

/// 内容块类型（`content_block[].block_type`，[二进制] 键逐字 + [Go参考] 取值）
pub const BLOCK_TEXT: i64 = 10_000;
/// 思考链容器块
pub const BLOCK_THINKING: i64 = 10_040;
/// 附件块（图片 / 文件共用一个 block_type，靠 `attachment.type` 区分）
pub const BLOCK_ATTACHMENT: i64 = 10_052;

/// `attachment.type`：文件（[Go参考] `fileBlock` 的 `"type": 3`）
pub const ATTACHMENT_FILE: i64 = 3;
/// `attachment.type`：图片（[Go参考] `imageBlock` 的 `"type": 1`）
pub const ATTACHMENT_IMAGE: i64 = 1;

/// `message_storage_type` 的取值（[二进制] 键逐字）。
///
/// 取 `1`：桌面端把消息存到服务端（多处转发共用同一份会话存储）。
/// 这个值没有二义性依据可引（二进制只给键），因此**允许环境变量覆盖**
/// （见 [`message_storage_type`]）—— 它是本文件里唯一一个「取值存疑」的字段。
pub const MESSAGE_STORAGE_TYPE: i64 = 1;

/// `parse_state` / `review_state` / `upload_status` 的「已就绪」取值
/// （[Go参考] 对**已上传完成**的附件给的组合：`1/1/1`）
const READY_STATE: i64 = 1;

/// 一次构造好的请求（URL / 头 / 体）。
pub struct BuiltRequest {
    /// 上游完整 URL（**含风控 query**）
    pub url: String,
    /// 请求头（含 Cookie 与 csrf）
    pub headers: Vec<(String, String)>,
    /// 已按本家规则改写的请求体
    pub body: Value,
    /// 本次使用的思考档位（进请求日志的「上游等级」列）
    pub think_level: i64,
}

/// 读环境变量覆盖（空值视为未设置）
fn env_override(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// `message_storage_type` 的取值（`DOUBAOWORK_MESSAGE_STORAGE_TYPE` 可覆盖）
pub fn message_storage_type() -> i64 {
    env_override("DOUBAOWORK_MESSAGE_STORAGE_TYPE")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(MESSAGE_STORAGE_TYPE)
}

/// 从内容块数组里抽出纯文本（只认 text 类块）。
///
/// 与 `protocol::content_text` 同一口径：字符串直接返回；数组把
/// `text` / `input_text` / `output_text` 三类拼接。图片 / 文件块**不参与**
/// 拼接（它们是结构化内容，拍平成文本会丢语义）—— 由
/// [`collect_images`] / [`collect_files`] 各自处理。
fn text_of(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(text) = part.as_str() {
                    out.push_str(text);
                    continue;
                }
                let kind = part
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if matches!(kind.as_str(), "text" | "input_text" | "output_text") {
                    out.push_str(part.get("text").and_then(Value::as_str).unwrap_or(""));
                }
            }
            out
        }
        _ => String::new(),
    }
}

/// 一个待上传的图片附件（**只带字节**；上传后由 [`super::upload`] 给 uri）。
///
/// ── 为什么这里不直接带 uri ──────────────────────────────────
/// `plan::build` 是**同步**的（`build_chat_request` 是同步方法），不能在里面
/// 发网络请求去上传。因此构造阶段只把「有哪些图片、各自什么字节」收集出来，
/// 真正的上传由编排层在发请求前做（见 [`super::chat`] 的模块头与
/// [`super::upload`]）。这与 Command Code 的「指纹预请求落在
/// `ensure_access_token`」是同一个分工原则：**异步的事放异步钩子里**。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingImage {
    /// 文件名（用于推断 MIME 与展示）
    pub name: String,
    /// 原始字节
    pub data: Vec<u8>,
    /// MIME（`image/png` 这种；可能是从 data URL 里带出来的）
    pub mime: String,
}

/// 解析 `data:image/png;base64,xxxx` 形态的内联图片。
///
/// 返回 `(mime, bytes)`；不是 data URL 或 base64 坏掉时返回 `None`
/// （调用方按「这条分片不是内联图」处理，不报错：OpenAI 客户端里
/// `image_url` 既可能是外链也可能是内联，坏掉的内联图不该让整条请求失败）。
pub fn parse_data_url(value: &str) -> Option<(String, Vec<u8>)> {
    let rest = value.strip_prefix("data:")?;
    let comma = rest.find(',')?;
    let head = &rest[..comma];
    let payload = &rest[comma + 1..];
    if !head.contains("base64") {
        return None;
    }
    let mime = head
        .split(';')
        .next()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("image/png")
        .to_string();
    // base64 标准表的解码：用 base64 crate（本项目既有依赖）
    let engine = base64::engine::general_purpose::STANDARD;
    let bytes = base64::Engine::decode(&engine, payload.trim()).ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some((mime, bytes))
}

/// 从全部消息里收集待上传的图片。
///
/// 认三种分片写法（[Go参考] `extractContent` 的同一条链）：
/// ```text
///   {"type":"image_url","image_url":{"url":"https://…"}}   外链（本实现不收）
///   {"type":"image_url","image_url":"https://…"}           同上（字符串简化形态）
///   {"type":"input_image","image_url":"data:image/…"}      内联 base64
/// ```
/// ⚠️ **外链不收**（只收内联字节）：桌面端文档里说的是「图片输入」，而上游
/// **不会自己去抓外链**（[Go参考] 的注释明确「上游不会自行抓取外部资源，
/// 已实测」）。网关代下载外链意味着「网关替用户访问一个任意 URL」——那是 SSRF
/// 面，且没有任何证据表明上游需要这么绕。因此外链分片被**跳过**，并在详细日志
/// 里留一行；用户要发网络图，自己转成 base64 即可。
pub fn collect_images(messages: &[Value]) -> (Vec<PendingImage>, usize) {
    let mut out: Vec<PendingImage> = Vec::new();
    let mut skipped_links = 0usize;
    for message in messages {
        let Some(parts) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in parts {
            let kind = part
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            if !matches!(kind.as_str(), "image_url" | "input_image" | "image") {
                continue;
            }
            let url = match part.get("image_url") {
                Some(Value::String(text)) => Some(text.clone()),
                Some(Value::Object(object)) => {
                    object.get("url").and_then(Value::as_str).map(str::to_string)
                }
                _ => part
                    .get("url")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| part.get("data").and_then(Value::as_str).map(str::to_string)),
            };
            let Some(url) = url else { continue };
            match parse_data_url(&url) {
                Some((mime, data)) => {
                    let extension = mime.rsplit('/').next().unwrap_or("png").to_string();
                    out.push(PendingImage {
                        name: format!("image.{extension}"),
                        data,
                        mime,
                    });
                }
                None => {
                    // 外链（或坏掉的内联）：记数，由调用方打一行详细日志
                    skipped_links += 1;
                }
            }
        }
    }
    (out, skipped_links)
}

/// 从全部消息里收集待上传的文件（`file` / `input_file` 分片的内联 base64）。
///
/// 外链文件与图片同样被跳过（理由见 [`collect_images`]）。
pub fn collect_files(messages: &[Value]) -> Vec<PendingImage> {
    let mut out = Vec::new();
    for message in messages {
        let Some(parts) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in parts {
            let kind = part
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            if !matches!(kind.as_str(), "file" | "input_file") {
                continue;
            }
            // 两种写法：`{"type":"file","file":{…}}` 与 `{"type":"input_file",…}` 平铺
            let holder = part.get("file").filter(|value| value.is_object()).unwrap_or(part);
            let name = holder
                .get("filename")
                .or_else(|| holder.get("name"))
                .or_else(|| part.get("filename"))
                .or_else(|| part.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .unwrap_or("file.bin")
                .to_string();
            let raw = holder
                .get("file_data")
                .or_else(|| holder.get("data"))
                .or_else(|| part.get("file_data"))
                .or_else(|| part.get("data"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if raw.trim().is_empty() {
                continue;
            }
            // `file_data` 可能是 data URL，也可能是裸 base64（[Go参考] 两种都收）
            let decoded = parse_data_url(raw).or_else(|| {
                let engine = base64::engine::general_purpose::STANDARD;
                base64::Engine::decode(&engine, raw.trim())
                    .ok()
                    .filter(|bytes| !bytes.is_empty())
                    .map(|bytes| (mime_of_file_name(&name), bytes))
            });
            let Some((mime, data)) = decoded else { continue };
            out.push(PendingImage { name, data, mime });
        }
    }
    out
}

/// 由文件名推断 MIME（上传文件时用；[Go参考] `MIMEByFileName` 的同一张表）
pub fn mime_of_file_name(name: &str) -> String {
    let extension = name
        .rsplit_once('.')
        .map(|(_, tail)| tail.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "pdf" => "application/pdf",
        "txt" | "md" | "log" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "png" => "image/png",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// 把 OpenAI 的 `messages` 拍平成上游要的**一段提示文本**。
///
/// 返回 `(text, turn_count)`：`turn_count` 是「除了 system 之外说了几轮」，
/// 调用方用它决定要不要加角色前缀（见模块头第 3 条）。
pub fn flatten_messages(messages: &[Value]) -> (String, usize) {
    let mut systems: Vec<String> = Vec::new();
    let mut turns: Vec<String> = Vec::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let text = text_of(message.get("content").unwrap_or(&Value::Null));
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        match role.as_str() {
            "system" | "developer" => systems.push(text.to_string()),
            "assistant" => turns.push(format!("assistant: {text}")),
            "user" | "" => turns.push(format!("user: {text}")),
            // 工具结果等其它角色：保留角色名（丢掉会比「名字不好看」更糟 ——
            // 模型会以为自己没收到工具结果）
            other => turns.push(format!("{other}: {text}")),
        }
    }
    let mut out = String::new();
    if !systems.is_empty() {
        out.push_str(&systems.join("\n"));
        out.push_str("\n\n");
    }
    // 只有一轮 user 时去掉前缀（见模块头第 3 条）
    if turns.len() == 1 && turns[0].starts_with("user: ") {
        out.push_str(turns[0].trim_start_matches("user: "));
    } else {
        out.push_str(&turns.join("\n"));
    }
    (out, turns.len())
}

/// 一个**已归一**的等级字符串 → 本家的整数档位。
///
/// 取值链与判据都必须是**全项目那一套**：
///   - 输入来自 `model_rules::read_client_level`（客户端 body）或
///     `payload` 传进来的映射绑定值（两者都已被 `normalize_reasoning` 归一）；
///   - 映射只覆盖本家认识的三档，**表外值一律 `None`**
///     （把未知串猜成一个档位会让「我明明关了思考它却在思考」无从排查）。
///
/// ⚠️ **不要**在这里接受布尔 / 数字形态：`read_client_level` 明确把
/// 「`thinking: true`」这类**不当作档位**（见它的实现注释）—— 本家若另立一套
/// 更宽的判据，「绑定让位于客户端」这件事会在编排层与适配器两处得到不同答案
/// （编排层认为客户端没指定 → 注入绑定；适配器认为客户端指定了 → 不覆盖），
/// 最终表现是「绑定的档位偶尔不生效」。
pub fn think_level_of_level(level: &str) -> Option<i64> {
    let normalized = crate::server::core::model_rules::normalize_reasoning(level)?;
    // ⚠️ `normalize_reasoning` 只做「去空白 + 长度检查」，**不归一大小写**
    // （它的契约是「管能不能存」，不管语义）。本家要按档位名比对，因此自己
    // 折一次小写：界面下拉给的是小写，但**自定义输入框**里用户完全可能写
    // `HIGH`，而 `model_rules::reasoning_is_off` 本身就是忽略大小写的
    // —— 两处口径必须一致，否则 `OFF` 被当成关闭档、`HIGH` 却被当成表外值。
    match normalized.to_ascii_lowercase().as_str() {
        "off" | "none" | "minimal" | "low" => Some(THINK_OFF),
        "medium" | "high" => Some(THINK_ON),
        "max" | "xhigh" => Some(THINK_EXPERT),
        _ => None,
    }
}

/// 客户端显式指定的思考档位（走**全项目共用**的取值链
/// [`model_rules::read_client_level`]，再折算成本家的整数档位）。
///
/// 返回 `None` = 「客户端没有指定档次」（或指定了一个本家认不出的值）——
/// 两种情况都往下落到模型名推断那一层：认不出的值由 [`think_level_of_level`]
/// 判掉，而它作为「客户端是否指定」的答案仍然是「指定了」（因为它确实占了
/// 一个键），因此调用方在判断「绑定要不要让位」时应以
/// `read_client_level(body).is_some()` 为准（见 [`super::adapter`]）。
pub fn think_level_from_body(body: &Value) -> Option<i64> {
    let level = crate::server::core::model_rules::read_client_level(body)?;
    think_level_of_level(&level)
}

/// 从模型名推断思考档位（本家目录里的模型名自带档位语义）。
///
/// 依据：`deep_think` / `mode_id` / `mode_name` 三串在 [二进制] 里同处
/// （见 [`super::models`] 的模块头）—— 本家的「模型」以**模式**形态下发，
/// 模式名里带 `think` / `expert` 字样。这条推断只作为兜底（客户端显式指定时
/// 以客户端为准，见 [`think_level_from_body`]）。
pub fn think_level_from_model(model: &str) -> Option<i64> {
    let lowered = model.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return None;
    }
    if lowered.contains("expert") || lowered.contains("deep") || lowered.contains("max") {
        return Some(THINK_EXPERT);
    }
    if lowered.contains("think") || lowered.contains("reason") {
        return Some(THINK_ON);
    }
    if lowered.contains("fast") || lowered.contains("lite") || lowered.contains("turbo") {
        return Some(THINK_OFF);
    }
    None
}

/// 最终档位：**内部通道** > 客户端显式指定 > 模型名推断 > 默认关闭。
///
/// 内部通道（[`BODY_THINK_LEVEL_KEY`]）排在最前，因为它是「映射绑定」写进来的
/// ——绑定是用户的显式配置，优先级高于客户端的单次指定（与别家同一取舍；
/// 而「客户端已经指定」这件事由适配器在 [`super::adapter`] 的 `reasoning_patch`
/// 里先判掉，所以走到这里时通道里有值就意味着绑定该生效）。
pub fn resolve_think_level(body: &Value, model: &str) -> i64 {
    body.get(BODY_THINK_LEVEL_KEY)
        .and_then(|value| match value {
            Value::Number(_) => value.as_i64(),
            Value::String(text) => text.trim().parse::<i64>().ok(),
            _ => None,
        })
        .or_else(|| think_level_from_body(body))
        .or_else(|| think_level_from_model(model))
        .unwrap_or(THINK_OFF)
}

/// 构造一个文本内容块。
fn text_block(text: &str) -> Value {
    json!({
        "block_type": BLOCK_TEXT,
        "content": {
            "text_block": {
                "text": text,
                "icon_url": "",
                "icon_url_dark": "",
                "summary": "",
            },
            "pc_event_block": "",
        },
        "block_id": endpoints::uuid_v4(),
        "parent_id": "",
        "meta_info": [],
        "append_fields": [],
        "is_finish": true,
        "patch_type": 2,
    })
}

/// 构造一个图片附件块（`block_type=10052` + `attachment.type=1`）。
fn image_block(images: &[(String, String)]) -> Value {
    let attachments: Vec<Value> = images
        .iter()
        .map(|(uri, mime)| {
            // 扩展名由 MIME 反推（`image/jpeg` → `jpeg`）：上游的
            // `image.image_ori.format` 要的是**格式名**而不是 MIME。
            // 反推不出时给 `png` —— 与 [Go参考] 的默认值一致。
            let format = mime
                .split('/')
                .nth(1)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .unwrap_or("png")
                .to_string();
            json!({
                "type": ATTACHMENT_IMAGE,
                "identifier": endpoints::uuid_v4(),
                "image": {
                    "name": format!("image.{format}"),
                    "uri": uri,
                    "image_ori": {
                        "url": "",
                        "width": 0,
                        "height": 0,
                        "format": format,
                        "url_formats": {},
                    },
                },
                "parse_state": 0,
                "review_state": 0,
            })
        })
        .collect();
    json!({
        "block_type": BLOCK_ATTACHMENT,
        "content": {
            "attachment_block": { "attachments": attachments },
            "pc_event_block": "",
        },
        "block_id": endpoints::uuid_v4(),
        "parent_id": "",
        "meta_info": [],
        "append_fields": [],
        "is_finish": true,
        "patch_type": 2,
    })
}

/// 构造一个文件附件块（`block_type=10052` + `attachment.type=3` + 就绪三态）。
fn file_block(files: &[(String, String, u64)]) -> Value {
    let attachments: Vec<Value> = files
        .iter()
        .map(|(uri, name, size)| {
            json!({
                "type": ATTACHMENT_FILE,
                "identifier": endpoints::uuid_v4(),
                "file": {
                    "uri": uri,
                    "url": "",
                    "file_type": 0,
                    "name": name,
                    "size": size,
                },
                // 文件块三态给「就绪」（[Go参考] 的组合：1/1/1）
                "parse_state": READY_STATE,
                "review_state": READY_STATE,
                "upload_status": READY_STATE,
                "progress": 100,
                "src": "",
            })
        })
        .collect();
    json!({
        "block_type": BLOCK_ATTACHMENT,
        "content": {
            "attachment_block": { "attachments": attachments },
            "pc_event_block": "",
        },
        "block_id": endpoints::uuid_v4(),
        "parent_id": "",
        "meta_info": [],
        "append_fields": [],
    })
}

/// `plan::build` 的入参：上传完成后的附件（`uri` 已就绪）。
///
/// ── 为什么附件单独一个结构而不是直接塞进 body ────────────────
/// 附件要经过**异步上传**才能拿到 `uri`（见 [`PendingImage`] 的说明）。上传是
/// [`super::chat`] 的事，构造是同步的 —— 两者之间的接口就是这个结构体。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadyAttachments {
    /// 图片：`(uri, mime)`（`uri` 是上游资源中心的资源标识）
    pub images: Vec<(String, String)>,
    /// 文件：`(uri, name, size)`
    pub files: Vec<(String, String, u64)>,
}

/// 构造 `POST /chat/completion` 的完整请求。
///
/// `credentials` 给 Cookie 与风控指纹；`body` 是客户端原始请求体；
/// `attachments` 是**已上传完成**的附件（见 [`ReadyAttachments`]）。
pub fn build(
    credentials: &Credentials,
    body: &Value,
    attachments: &ReadyAttachments,
) -> Result<BuiltRequest, GatewayError> {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if messages.is_empty() {
        return Err(GatewayError::with_status(400, "请求缺少 messages"));
    }
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let (text, turn_count) = flatten_messages(&messages);
    if text.trim().is_empty() && attachments.images.is_empty() && attachments.files.is_empty() {
        return Err(GatewayError::with_status(
            400,
            "消息内容为空（既没有文本，也没有可用的图片 / 文件）",
        ));
    }
    // `turn_count` 只用于详细日志：多轮被拍平是**本实现的做法**（见模块头），
    // 而 [二进制] 的信封里**没有**这个键 —— 发一个上游不认识的自造字段是
    // 「把未知参数推给上游」，因此它只进日志、不进 body。
    if turn_count > 1 {
        crate::server::logging::verbose(
            "[DoubaoWork]",
            &format!("多轮历史已拍平成单段提示（{turn_count} 轮，含 system 分段）"),
        );
    }
    let think_level = resolve_think_level(body, &model);
    let bot_id = body
        .get("bot_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(DEFAULT_BOT_ID)
        .to_string();
    // 客户端显式给的会话 id（豆包原生多轮）；没有就发 "0"（新会话，见模块头）
    let conversation_id = body
        .get("conversation_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("0")
        .to_string();

    let mut blocks: Vec<Value> = Vec::with_capacity(3);
    if !attachments.files.is_empty() {
        blocks.push(file_block(&attachments.files));
    }
    if !attachments.images.is_empty() {
        blocks.push(image_block(&attachments.images));
    }
    blocks.push(text_block(&text));

    let now_ms = crate::server::logging::now_ms();
    let think_text = think_level.to_string();

    let client_meta = json!({
        // `local_` 前缀 + 随机十六进制（[二进制] `local_` 串；[Go参考] 同款）
        "local_conversation_id": format!("local_{}", endpoints::random_hex(8)),
        "conversation_id": conversation_id,
        "bot_id": bot_id,
        "last_section_id": "",
        "last_message_index": 0,
    });

    let option = json!({
        "need_create_conversation": true,
        "conversation_init_option": { "need_ack_conversation": true },
        "need_ack_conversation": true,
        // 必须为 true：置 false 时上游不返回后续建议，且（[Go参考] 实测）
        // 连带影响思维链的下发
        "answer_with_suggest": true,
        "sse_recv_event_options": { "support_chunk_delta": true },
        // 上游要**秒**（[二进制] `req_create_time_sec` 键名里的单位）
        "req_create_time_sec": now_ms / 1000,
        "append_sse_event_scene": "",
        "message_storage_type": message_storage_type(),
        "need_deep_think": think_level,
        "is_ai_playground": false,
    });

    let ext = json!({
        // 整数档位在这里是**字符串**（上游的形态，别统一，见模块头）
        "use_deep_think": think_text,
        "sub_conv_firstmet_type": "1",
        "commerce_credit_config_enable": "0",
        "fp": credentials.fingerprint.fp,
    });

    let payload = json!({
        "client_meta": client_meta,
        "messages": [{
            "local_message_id": endpoints::uuid_v4(),
            "content_block": blocks,
            "message_status": 0,
        }],
        "option": option,
        "ext": ext,
    });

    // ── URL：风控 query 必须带上（每个请求都现拼；`web_tab_id` 每次不同）──
    let query = endpoints::encode_query(&endpoints::security_params(&credentials.fingerprint));
    let url = format!("{}?{}", endpoints::chat_url(), query);

    // ── 头：基础头 + SSE 头 + 鉴权头 ──
    let mut headers = endpoints::base_headers();
    headers.extend(endpoints::sse_headers());
    headers.extend(endpoints::auth_headers(
        &credentials.cookie_header(),
        credentials.csrf_token().as_deref(),
    ));

    Ok(BuiltRequest {
        url,
        headers,
        body: payload,
        think_level,
    })
}

/// 从发送体里读出「随请求上行的思考档位」（请求日志「上游等级」列的采集口）。
///
/// `ProviderAdapter::outbound_reasoning` 的默认实现读的是通用键
/// （`reasoning_effort` 那套）。本家把档位翻译进了 `option.need_deep_think`
/// 与 `ext.use_deep_think`，**通用键在那份 body 里已经不存在** —— 因此适配器
/// 必须覆写那个钩子，用的就是这个函数。
///
/// 返回 `None` = 这条请求没有等级随行（关闭档，不预支一个「没发出去」的值）。
pub fn outbound_reasoning_of(body: &Value) -> Option<String> {
    // ⚠️ **两条载体都要读**，顺序不能反（这一处的调用时机很特别）：
    // `ProviderAdapter::outbound_reasoning` 是编排层在 `send_body` 之后、
    // `build_chat_request` **之前**调的 —— 那一刻 body 是客户端原始体，
    // 但**可能**已经带上 [`BODY_THINK_LEVEL_KEY`]（映射绑定由
    // `reasoning_patch` 注入在这一层）。
    //   · 先读内部通道：绑定注入的那次能读到真值；
    //   · 再读 `option.need_deep_think`：若调用方给的是**已构造**的信封
    //     （测试 / 将来把采集挪到构造之后），这一条也能读到。
    // 只读后者会漏掉真实转发路径上的注入（那种情况下日志永远显示「未注入」）。
    let level = body
        .get(BODY_THINK_LEVEL_KEY)
        .and_then(|value| match value {
            Value::Number(_) => value.as_i64(),
            Value::String(text) => text.trim().parse::<i64>().ok(),
            _ => None,
        })
        .or_else(|| {
            body.pointer("/option/need_deep_think")
                .and_then(Value::as_i64)
        })?;
    if level == THINK_OFF {
        return None;
    }
    Some(match level {
        THINK_ON => "on".to_string(),
        THINK_EXPERT => "expert".to_string(),
        other => other.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::core::providers::doubaowork::credentials::account_from_cookies;

    fn credentials_fixture() -> Credentials {
        let pairs = vec![
            ("sessionid".to_string(), "abcdef".to_string()),
            ("passport_csrf_token".to_string(), "csrf".to_string()),
        ];
        account_from_cookies("doubaowork-1", "测试账号", &pairs, None)
    }

    /// 拍平：system 提前成段；单轮 user 不加前缀；多轮加前缀
    #[test]
    fn flatten_hoists_system_and_keeps_single_turn_bare() {
        let single = vec![
            json!({"role": "system", "content": "你是助手"}),
            json!({"role": "user", "content": "你好"}),
        ];
        let (text, turns) = flatten_messages(&single);
        assert_eq!(turns, 1);
        assert_eq!(text, "你是助手\n\n你好");

        let multi = vec![
            json!({"role": "user", "content": "第一个问题"}),
            json!({"role": "assistant", "content": "第一个答案"}),
            json!({"role": "user", "content": "第二个问题"}),
        ];
        let (text, turns) = flatten_messages(&multi);
        assert_eq!(turns, 3);
        assert!(text.contains("user: 第一个问题"));
        assert!(text.contains("assistant: 第一个答案"));
        assert!(text.contains("user: 第二个问题"));
    }

    /// 内容分片的文本拼接：只认 text 类；图片分片不参与
    #[test]
    fn text_parts_are_concatenated_and_images_ignored() {
        let message = json!({"role": "user", "content": [
            {"type": "text", "text": "看这张图"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
            {"type": "input_text", "text": "（补充）"}
        ]});
        let (text, _) = flatten_messages(&[message]);
        assert_eq!(text, "看这张图（补充）");
    }

    /// 思考档位：客户端显式指定优先于模型名推断；未知名回落默认关闭
    #[test]
    fn think_level_priority() {
        assert_eq!(resolve_think_level(&json!({"reasoning_effort": "high"}), "x"), THINK_ON);
        assert_eq!(
            resolve_think_level(&json!({"reasoning_effort": "max"}), "x"),
            THINK_EXPERT
        );
        // 客户端说关闭时模型名不得翻案
        assert_eq!(
            resolve_think_level(&json!({"reasoning_effort": "off"}), "doubao-expert"),
            THINK_OFF
        );
        // 没有显式指定 → 模型名推断
        assert_eq!(resolve_think_level(&json!({}), "doubao-expert"), THINK_EXPERT);
        assert_eq!(resolve_think_level(&json!({}), "doubao-think"), THINK_ON);
        assert_eq!(resolve_think_level(&json!({}), "doubao-fast"), THINK_OFF);
        // 认不出的等级不猜（回落模型名 → 再回落默认）
        assert_eq!(resolve_think_level(&json!({"reasoning_effort": "???"}), "zzz"), THINK_OFF);
    }

    /// 布尔形态的 `thinking` **不构成档位**（与 `model_rules::read_client_level`
    /// 同一口径 —— 若本家自己放宽，会出现「编排层认为客户端没指定、本家认为
    /// 指定了」两处不一致，症状是「绑定的档位偶尔不生效」）
    #[test]
    fn boolean_thinking_is_not_a_level() {
        assert_eq!(think_level_from_body(&json!({"thinking": true})), None);
        assert_eq!(think_level_from_body(&json!({"thinking": false})), None);
        // 字符串形态才算
        assert_eq!(think_level_from_body(&json!({"thinking": "high"})), Some(THINK_ON));
    }

    /// ⚠️ 内部通道（映射绑定注入的档位）**优先于**客户端那一层
    #[test]
    fn bind_channel_beats_client_level() {
        // ⚠️ `json!` 的对象键位置写裸标识符会**取字面量当键名**（`"BODY_THINK_LEVEL_KEY"`），
        // 所以要用括号把常量求值：`(CONST)`
        let body = json!({
            (BODY_THINK_LEVEL_KEY): 3,
            "reasoning_effort": "low",
        });
        assert_eq!(resolve_think_level(&body, "doubao-fast"), THINK_EXPERT);
        // 通道是字符串形态也要认（手工编辑 / 落盘往返后可能变成字符串）
        let as_text = json!({ (BODY_THINK_LEVEL_KEY): "1" });
        assert_eq!(resolve_think_level(&as_text, "doubao-fast"), THINK_ON);
        // 通道里是垃圾 → 不动（落到客户端那一层）
        let junk = json!({ (BODY_THINK_LEVEL_KEY): "not-a-level", "reasoning_effort": "max" });
        assert_eq!(resolve_think_level(&junk, "doubao-fast"), THINK_EXPERT);
    }

    /// ⚠️ 大小写不敏感（自定义输入框里 `HIGH` 显然是这个意思，
    /// 与 `model_rules::reasoning_is_off` 的口径一致）
    #[test]
    fn level_mapping_is_case_insensitive() {
        assert_eq!(think_level_of_level("HIGH"), Some(THINK_ON));
        assert_eq!(think_level_of_level("MAX"), Some(THINK_EXPERT));
        // `off` 由上游（编排层）先拦，但本函数自己也应是忽略大小写的
        assert_eq!(think_level_of_level("OFF"), Some(THINK_OFF));
    }

    /// ⚠️ 「上游等级」采集口在**真实转发路径**上读的是内部通道
    /// （那一刻 body 还没被构造成信封 —— 只读 `option` 会永远显示「未注入」）
    #[test]
    fn outbound_reasoning_reads_the_bind_channel() {
        assert_eq!(
            outbound_reasoning_of(&json!({ (BODY_THINK_LEVEL_KEY): 3 })),
            Some("expert".to_string())
        );
        assert_eq!(
            outbound_reasoning_of(&json!({ (BODY_THINK_LEVEL_KEY): 1 })),
            Some("on".to_string())
        );
        // 关闭档 → None（不预支一个没发出去的值）
        assert_eq!(outbound_reasoning_of(&json!({ (BODY_THINK_LEVEL_KEY): 0 })), None);
        // 两条载体都没有 → None
        assert_eq!(outbound_reasoning_of(&json!({"messages": []})), None);
    }

    /// 等级折算：三档 + 表外值一律 None
    #[test]
    fn level_mapping_covers_three_tiers_only() {
        assert_eq!(think_level_of_level("off"), Some(THINK_OFF));
        assert_eq!(think_level_of_level("minimal"), Some(THINK_OFF));
        assert_eq!(think_level_of_level("low"), Some(THINK_OFF));
        assert_eq!(think_level_of_level("medium"), Some(THINK_ON));
        assert_eq!(think_level_of_level("high"), Some(THINK_ON));
        assert_eq!(think_level_of_level("xhigh"), Some(THINK_EXPERT));
        assert_eq!(think_level_of_level("max"), Some(THINK_EXPERT));
        // 表外的自定义等级/未知串 → None（不猜）
        assert_eq!(think_level_of_level("expert"), None);
        assert_eq!(think_level_of_level("whatever"), None);
        assert_eq!(think_level_of_level(""), None);
        // 归一也走共用的那一套（大小写与空白都能吃）
        assert_eq!(think_level_of_level("  HIGH  "), Some(THINK_ON));
    }

    /// 信封形状：三个顶层键 + 必需子键都在；`use_deep_think` 是**字符串**
    #[test]
    fn envelope_has_required_keys_and_string_think_level() {
        let built = build(
            &credentials_fixture(),
            &json!({"model": "doubao-think", "messages": [{"role": "user", "content": "hi"}]}),
            &ReadyAttachments::default(),
        );
        assert!(
            built.is_ok(),
            "构造应当成功：{:?}",
            built.as_ref().err().map(|error| error.message.clone())
        );
        let built = match built {
            Ok(built) => built,
            // 不可达（上面已断言 `is_ok`）
            Err(_) => return,
        };
        assert!(built.url.contains("/chat/completion?"));
        assert!(built.url.contains("device_id="));
        let body = &built.body;
        assert!(body.get("client_meta").is_some());
        assert!(body.get("messages").is_some());
        assert!(body.get("option").is_some());
        assert!(body.get("ext").is_some());
        // 会话 id 缺省是 "0"（新会话）
        assert_eq!(
            body.pointer("/client_meta/conversation_id").and_then(Value::as_str),
            Some("0")
        );
        // bot_id 缺省是本家默认 bot
        assert_eq!(
            body.pointer("/client_meta/bot_id").and_then(Value::as_str),
            Some(DEFAULT_BOT_ID)
        );
        // 档位：option 里是整数、ext 里是字符串（上游的形态，别统一）
        assert_eq!(
            body.pointer("/option/need_deep_think").and_then(Value::as_i64),
            Some(THINK_ON)
        );
        assert_eq!(
            body.pointer("/ext/use_deep_think").and_then(Value::as_str),
            Some("1")
        );
        // 消息里只有一个块（纯文本请求）
        let blocks = body
            .pointer("/messages/0/content_block")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].get("block_type").and_then(Value::as_i64),
            Some(BLOCK_TEXT)
        );
        // 请求头里必须有 Cookie 与 csrf
        let header_names: Vec<&str> = built.headers.iter().map(|(key, _)| key.as_str()).collect();
        assert!(header_names.contains(&"Cookie"));
        assert!(header_names.contains(&"x-tt-passport-csrf-token"));
        assert!(header_names.contains(&"Accept"));
    }

    /// 附件顺序：文件 → 图片 → 文本（三个块都在，顺序恒定）
    #[test]
    fn attachment_blocks_come_before_text() {
        let attachments = ReadyAttachments {
            images: vec![("tos-cn-img".to_string(), "image/png".to_string())],
            files: vec![("tos-cn-file".to_string(), "a.pdf".to_string(), 1234)],
        };
        let built = build(
            &credentials_fixture(),
            &json!({"messages": [{"role": "user", "content": "看图"}]}),
            &attachments,
        )
        .ok();
        let blocks = built
            .as_ref()
            .and_then(|built| built.body.pointer("/messages/0/content_block"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(blocks.len(), 3);
        assert_eq!(
            blocks[0].pointer("/content/attachment_block/attachments/0/type").and_then(Value::as_i64),
            Some(ATTACHMENT_FILE)
        );
        assert_eq!(
            blocks[0]
                .pointer("/content/attachment_block/attachments/0/upload_status")
                .and_then(Value::as_i64),
            Some(READY_STATE)
        );
        assert_eq!(
            blocks[1].pointer("/content/attachment_block/attachments/0/type").and_then(Value::as_i64),
            Some(ATTACHMENT_IMAGE)
        );
        assert_eq!(
            blocks[2].get("block_type").and_then(Value::as_i64),
            Some(BLOCK_TEXT)
        );
    }

    /// 空消息 400；只有附件没有文本也能发（图片是有效内容）
    #[test]
    fn empty_messages_rejected_but_attachment_only_is_fine() {
        assert_eq!(
            build(&credentials_fixture(), &json!({"messages": []}), &ReadyAttachments::default())
                .err()
                .map(|error| error.status_code),
            Some(400)
        );
        assert_eq!(
            build(
                &credentials_fixture(),
                &json!({"messages": [{"role": "user", "content": ""}]}),
                &ReadyAttachments::default()
            )
            .err()
            .map(|error| error.status_code),
            Some(400)
        );
        let attachment_only = build(
            &credentials_fixture(),
            &json!({"messages": [{"role": "user", "content": ""}]}),
            &ReadyAttachments {
                images: vec![("uri".to_string(), "image/png".to_string())],
                files: Vec::new(),
            },
        );
        assert!(attachment_only.is_ok());
    }

    /// data URL 解析：标准形态、非 base64 形态、坏 base64
    #[test]
    fn data_url_parsing() {
        let (mime, bytes) = parse_data_url("data:image/png;base64,SGk=").unwrap_or_default();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, b"Hi");
        // 没有 base64 标记 → 不认（本实现只收 base64 形态）
        assert!(parse_data_url("data:text/plain,hello").is_none());
        // 坏 base64 / 空 / 非 data URL
        assert!(parse_data_url("data:image/png;base64,!!!").is_none());
        assert!(parse_data_url("https://example.com/a.png").is_none());
    }

    /// 图片收集：内联收、外链**不收**（SSRF 面，见模块头）
    #[test]
    fn collect_images_takes_inline_only() {
        let messages = vec![
            json!({"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,SGk="}}
            ]}),
            json!({"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": "https://example.com/x.png"}}
            ]}),
        ];
        let (images, skipped) = collect_images(&messages);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].mime, "image/jpeg");
        assert_eq!(images[0].name, "image.jpeg");
        assert_eq!(skipped, 1, "外链应被跳过并计数");
    }

    /// 文件收集：`file` 包裹与 `input_file` 平铺两种写法都认；文件名推断 MIME
    #[test]
    fn collect_files_handles_both_shapes() {
        let messages = vec![json!({"role": "user", "content": [
            {"type": "file", "file": {"filename": "a.pdf", "file_data": "data:application/pdf;base64,SGk="}},
            {"type": "input_file", "filename": "b.txt", "file_data": "SGk="}
        ]})];
        let files = collect_files(&messages);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "a.pdf");
        assert_eq!(files[0].mime, "application/pdf");
        assert_eq!(files[1].name, "b.txt");
        assert_eq!(files[1].mime, "text/plain");
    }

    /// MIME 推断表（与参考实现同一张表）
    #[test]
    fn mime_table_covers_common_types() {
        assert_eq!(mime_of_file_name("a.PDF"), "application/pdf");
        assert_eq!(mime_of_file_name("a.docx"), "application/vnd.openxmlformats-officedocument.wordprocessingml.document");
        assert_eq!(mime_of_file_name("a.png"), "image/png");
        assert_eq!(mime_of_file_name("a.unknown"), "application/octet-stream");
        assert_eq!(mime_of_file_name("noext"), "application/octet-stream");
    }

    /// 「上游等级」采集口：关闭档返回 None（不预支一个没发出去的值）
    #[test]
    fn outbound_reasoning_skips_off_level() {
        assert_eq!(outbound_reasoning_of(&json!({"option": {"need_deep_think": 0}})), None);
        assert_eq!(
            outbound_reasoning_of(&json!({"option": {"need_deep_think": 1}})),
            Some("on".to_string())
        );
        assert_eq!(
            outbound_reasoning_of(&json!({"option": {"need_deep_think": 3}})),
            Some("expert".to_string())
        );
        assert_eq!(outbound_reasoning_of(&json!({})), None);
    }
}
