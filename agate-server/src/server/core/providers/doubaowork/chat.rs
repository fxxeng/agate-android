//! 豆包工作的对话链路：`POST /chat/completion` 的 **SSE 事件解析**与请求构造。
//!
//! ╔══════════════════════════════════════════════════════════════════╗
//! ║ ⚠️ **需要接线方补：`UpstreamResponse::DoubaoworkSse` + 对应翻译器** ║
//! ╚══════════════════════════════════════════════════════════════════╝
//!
//! ── 结论：本家的 SSE **不是**标准 OpenAI 形态（依据逐条）─────────
//! 它是一套**自有事件流**，本文件下面那套 `parse_event` 就是它的解析器：
//!
//! ```text
//!   [二进制] 事件名（连排的同一段字符串表里逐字出现）：
//!     ERR  gateway-error  SSE_REPLY_END  STREAM_MSG_NOTIFY
//!     CHUNK_DELTA  FULL_MSG_NOTIFY  STREAM_CHUNK
//!   [二进制] 帧内字段：
//!     patch_op  patch_object  patch_value  content_block  patch_type
//!     user_type  block_id  block_type  icon_url  icon_url_dark
//!   [Go参考] internal/doubao/client.go 的 parseStream：
//!     SSE_ACK（会话建立，给 conversation_id）
//!     STREAM_ERROR / error_code + error_msg（业务错误）
//!     CHUNK_DELTA 的紧凑形态 {"text":"..."}
//!     patch_op[].patch_value.content 是一个 **JSON 字符串**，里面才有 text
//!     content_block[].block_type 决定这段增量是正文还是思维链
//! ```
//! 这几项**每一项都不是** OpenAI Chat SSE 的：那边是
//! `data: {"choices":[{"delta":{"content":…}}]}`；这边是
//! `patch_op` / `content_block` / `block_type` 三层嵌套，且增量可能藏在
//! **字符串化的 JSON** 里（`patch_value.content` 是 `"{\"text\":\"…\"}"`）。
//!
//! ── 因此需要新增一个变体（本文件只负责把这件事说清楚）──────────
//! 按本仓的既定解法（见 `providers/adapter.rs` 的 `UpstreamResponse` 文档与
//! `upstream::translate` 的先例：ZCode 的 Anthropic、Command Code 的 NDJSON、
//! Antigravity 的 Gemini 信封），本家应当：
//! ```text
//!   1. 在 `UpstreamResponse` 加一个变体：`DoubaoworkSse`
//!      （本文件下面已按这个名字写好常量与文档，接线方按它落即可）；
//!   2. 在 `upstream/translate.rs` 加一台状态机
//!      `DoubaoworkToChatStream`，它调用本文件的 [`parse_event`] 把上游事件
//!      折成 chat SSE 帧（正文 → `delta.content`、思维链 → `delta.reasoning_content`、
//!      工具/联网提示 → 文本、业务错误 → 流内错误帧、`SSE_REPLY_END` → 收尾）；
//!   3. 在 `provider_loop` 的翻译分派里加一条 arm。
//! ```
//! **本文件已经把这台状态机的全部输入解析好了**（[`parse_event`] 是纯函数、
//! 有单测），接线方要写的主要是「增量 → 帧」的搬运与收尾。
//!
//! ⚠️ 在变体落地之前，适配器 [`super::adapter`] 返回的是
//! `ChatRequestPlan::chat(...)` —— 那意味着**上游帧会被原样透传给客户端**，
//! 客户端解析不了。这条链路在接线完成前**不完整**，本文件如实声明而
//! **不是**假装它可用（也与任务约定的做法一致：适配器先按默认路径返回，
//! 变体与翻译器由接线方补）。
//!
//! ── 本轮为什么不做「有状态转发」绕开变体 ────────────────────────
//! `forward_conversation`（`is_stateful = true`）确实可以让适配器自己转发、
//! 自己翻译，绕开 `UpstreamResponse`。但那条路的代价是**放弃编排层已有的
//! 五样**：账号轮换、限额冷却、退避重试、usage 记账、取消处理（`adapter.rs`
//! 文档对这一点有完整论证）。本家**没有** CatPaw 那种多步会话协议
//! （它就是一次 HTTP 请求 + 一条 SSE），因此没有理由付那份代价。
//!
//! ── 另一个「需要接线方补」的点：附件的异步上传 ────────────────────
//! `build_chat_request` 是**同步**方法，而图片 / 文件必须先经
//! `POST /alice/resource/prepare_upload` 换成上游能认的资源 uri（见
//! [`super::upload`]）。本轮的处理：
//!   - [`prepare_attachments`] 这个**异步**函数已经写好，做真正的上传；
//!   - 同步的 [`build_chat_request`] 在没有上传结果时，把附件**以文本标记**
//!     表达（`[Attached image 1: a.png]`，形态取自 [二进制] 里的
//!     `[Attached image ` 串），并在详细日志里写明「附件未上传」——
//!     它**不会静默丢掉**附件；
//!   - 接线方要补的是「在 `build_chat_request` 之前 await 一次
//!     `prepare_attachments` 并把结果传进 [`plan::build`]」这一步
//!     （可参照 Command Code 把指纹预请求挂在 `ensure_access_token` 上的做法）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use axum::http::HeaderMap;
use serde_json::Value;

use super::credentials::Credentials;
use super::plan::{self, ReadyAttachments};
use super::upload;
use crate::server::core::auth_http::send_raw;
use crate::server::core::protocol::SseLineBuffer;
use crate::server::core::providers::adapter::ChatRequestPlan;
use crate::server::errors::GatewayError;

/// 上游事件名（[二进制] 逐字；**没有** `event:` 行时事件名可能为 `None`，
/// 那时按 `data:` 载荷里的 `text` / `patch_op` 自行判断）
pub const EVENT_GATEWAY_ERROR: &str = "gateway-error";
/// 回复结束（[二进制] `SSE_REPLY_END`）—— 本家的**收尾信号**
pub const EVENT_REPLY_END: &str = "SSE_REPLY_END";
/// 流式消息通知（承载 `content_block`）
pub const EVENT_STREAM_MSG_NOTIFY: &str = "STREAM_MSG_NOTIFY";
/// 紧凑增量（承载 `{"text": "…"}`）
pub const EVENT_CHUNK_DELTA: &str = "CHUNK_DELTA";
/// 完整消息通知（收尾时可能再发一次全文）
pub const EVENT_FULL_MSG_NOTIFY: &str = "FULL_MSG_NOTIFY";
/// 另一条流式分片事件名
pub const EVENT_STREAM_CHUNK: &str = "STREAM_CHUNK";
/// 会话建立（[Go参考] `SSE_ACK`，给出 `conversation_id`）
pub const EVENT_SSE_ACK: &str = "SSE_ACK";
/// 业务错误（[Go参考] `STREAM_ERROR` + `error_code` / `error_msg`）
pub const EVENT_STREAM_ERROR: &str = "STREAM_ERROR";

/// 内容块类型：正文（[Go参考] `BlockText`）
pub const BLOCK_TEXT: i64 = 10_000;
/// 内容块类型：思考链容器
pub const BLOCK_THINKING: i64 = 10_040;
/// 内容块类型：通用工具
pub const BLOCK_GENERIC_TOOL: i64 = 10_024;
/// 内容块类型：联网搜索结果
pub const BLOCK_SEARCH_RESULT: i64 = 10_025;
/// 内容块类型：加载中提示
pub const BLOCK_LOADING: i64 = 10_101;
/// 内容块类型：附件
pub const BLOCK_ATTACHMENT: i64 = 10_052;
/// 内容块类型：生成产物（文生图等，`creation_block`）
pub const BLOCK_CREATION: i64 = 2_074;

/// 一个解析出来的**增量**。
///
/// 它是翻译器的输入形态（[`super`] 模块头的第 2 条）：一台状态机把这一串
/// 增量按顺序折成 chat SSE 帧即可。**保持纯数据**：没有时间戳、没有 id ——
/// 那些是帧构造的事，与解析无关。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delta {
    /// 正文增量
    Text(String),
    /// 思维链增量（上游用 `block_type=10040` 的容器区分）
    Thinking(String),
    /// 工具 / 联网提示（`[tool] …` / `[search] …`，当文本透出）
    Tool(String),
    /// 会话建立（`SSE_ACK` 给的 conversation_id）
    Conversation(String),
    /// 上游业务错误（`error_code` 非 0，或 `gateway-error` 事件）
    Error {
        /// 上游业务码（认不出给 0）
        code: i64,
        /// 上游文案
        message: String,
    },
    /// 回复结束（`SSE_REPLY_END`）—— 翻译器据此收尾
    Done,
}

/// 解析一条 SSE 事件为增量序列（**纯函数**，翻译器的唯一输入口）。
///
/// ── 四种载体，一个函数全包（顺序即上游的推送顺序）───────────────
/// ```text
///   1. SSE_ACK             → conversation_id（会话建立）
///   2. gateway-error       → 错误（HTTP 仍是 200，错误在流内）
///   3. STREAM_ERROR / error_code → 业务错误
///   4. CHUNK_DELTA 的 {"text": "…"}  → 紧凑增量（最省事的一种载体）
///   5. patch_op[].patch_value.content_block[]  → 块数组
///   6. patch_op[].patch_value.content（**字符串化的 JSON**）→ {"text": "…"}
/// ```
/// 第 6 条是本家最容易漏的一处：`patch_value.content` 的类型是**字符串**，
/// 里面才是 JSON。漏掉它会让「有的回答一个字都出不来」（取决于上游这一轮
/// 用哪种载体推送）。
///
/// ── 为什么 `event` 是 `Option<&str>` ─────────────────────────
/// 上游有两类事件：带 `event:` 行的（`SSE_ACK` / `CHUNK_DELTA` …）与
/// 只在 `data:` 里带 JSON 的。后者没有事件名，只能从载荷形状判断 ——
/// 因此判据是「看 event 名**或**看载荷」，两者取并集（与 [Go参考]
/// `parseStream` 的写法一致）。
pub fn parse_event(event: Option<&str>, data: &str, in_thinking: &mut bool) -> Vec<Delta> {
    let mut out: Vec<Delta> = Vec::new();
    let trimmed = data.trim();
    // ── 收尾信号必须**先判**，且不依赖载荷非空（踩过）──────────────
    // 上游的 `SSE_REPLY_END` **通常不带载荷**（`data:` 空）。早先这里把
    // 「data 为空」当提前返回，于是收尾信号被整条吞掉 ⇒ 流看起来「在完成信号
    // 之前结束」⇒ 翻译器按**截断**报错，用户每次提问都拿到一条 502。
    // 这类错不会崩、不会 401，只是每次正常请求都失败 —— 因此判定顺序在这里
    // 是有意义的：信号（收尾 / 错误）先看，内容再看。
    if event == Some(EVENT_REPLY_END) || trimmed == "[DONE]" {
        out.push(Delta::Done);
        return out;
    }
    if trimmed.is_empty() {
        return out;
    }
    // ── 网关级错误：**先于** JSON 解析（它的载荷可能不是 JSON）──
    if event == Some(EVENT_GATEWAY_ERROR) {
        let payload: Value = serde_json::from_str(trimmed).unwrap_or(Value::Null);
        let message = readable_error(&payload).unwrap_or_else(|| trimmed.to_string());
        out.push(Delta::Error {
            code: payload
                .get("code")
                .and_then(Value::as_i64)
                .or_else(|| payload.get("error_code").and_then(Value::as_i64))
                .unwrap_or(0),
            message,
        });
        return out;
    }
    let Ok(payload) = serde_json::from_str::<Value>(trimmed) else {
        // 非 JSON 载荷直接跳过（[Go参考] 同款：不能让一条坏帧打断整条流）
        return out;
    };

    // ── SSE_ACK：会话建立 ──
    if event == Some(EVENT_SSE_ACK) {
        let conversation = payload
            .pointer("/ack_client_meta/conversation_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty() && *text != "0");
        if let Some(conversation) = conversation {
            // `Delta` 装的是 `String`（数据形态），这里的 `conversation` 是
            // `&str`（借自 `payload`）⇒ 必须 `to_string()` 拷出来
            out.push(Delta::Conversation(conversation.to_string()));
        }
        return out;
    }

    // ── 业务错误：`error_code` 非 0（不论事件名）──
    let error_code = payload
        .get("error_code")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if event == Some(EVENT_STREAM_ERROR) || error_code != 0 {
        let message = payload
            .get("error_msg")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or("上游返回业务错误")
            .to_string();
        out.push(Delta::Error {
            code: error_code,
            message,
        });
        return out;
    }

    // ── 紧凑增量：{"text": "…"}（CHUNK_DELTA 的形态）──
    if let Some(text) = payload.get("text").and_then(Value::as_str) {
        if !text.is_empty() {
            out.push(push_text(text, in_thinking));
            return out;
        }
    }

    // ── `patch_op` 一族：块数组 + 字符串化的 content ──
    if let Some(patches) = payload.get("patch_op").and_then(Value::as_array) {
        for patch in patches {
            let value = patch.get("patch_value").unwrap_or(&Value::Null);
            // 6. patch_value.content 是**字符串化的 JSON**（见函数文档第 6 条）
            if let Some(inner) = value.get("content").and_then(Value::as_str) {
                if let Ok(parsed) = serde_json::from_str::<Value>(inner) {
                    if let Some(text) = parsed.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            out.push(push_text(text, in_thinking));
                        }
                    }
                }
            }
            // 5. patch_value.content_block[] 的块数组
            if let Some(blocks) = value.get("content_block").and_then(Value::as_array) {
                for block in blocks {
                    out.extend(parse_block(block, in_thinking));
                }
            }
        }
    }

    // ── 顶层 content_block[]（与 patch_op 并列的另一种载体）──
    if let Some(blocks) = payload
        .pointer("/content/content_block")
        .and_then(Value::as_array)
    {
        for block in blocks {
            out.extend(parse_block(block, in_thinking));
        }
    }

    // ── 顶层块数组（兜底形态）──
    if let Some(blocks) = payload.get("content_block").and_then(Value::as_array) {
        for block in blocks {
            out.extend(parse_block(block, in_thinking));
        }
    }

    out
}

/// 按当前思考态把一个文本增量归到正文或思维链
fn push_text(text: &str, in_thinking: &mut bool) -> Delta {
    if *in_thinking {
        Delta::Thinking(text.to_string())
    } else {
        Delta::Text(text.to_string())
    }
}

/// 解析一个 `content_block`。
///
/// `in_thinking` 是**跨事件的状态**（调用方持有）：上游用「出现第一个
/// `block_type=10040` 的思考块」开启思维链、用「出现正文块」结束它 ——
/// 因此同一个块类型在不同时刻的意义不同，必须带状态解析。
/// （[Go参考] `parseStream` 用 `thinkingCount == 1` 表达同一件事：
/// 只有一个思考块算思考，后续的算正文。）
fn parse_block(block: &Value, in_thinking: &mut bool) -> Vec<Delta> {
    let mut out: Vec<Delta> = Vec::new();
    let block_type = block
        .get("block_type")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let content = block.get("content").unwrap_or(&Value::Null);
    match block_type {
        BLOCK_THINKING => {
            // 思考容器块本身不带文本；它的作用是**切换状态**
            *in_thinking = true;
        }
        BLOCK_TEXT => {
            let text = content
                .pointer("/text_block/text")
                .and_then(Value::as_str)
                .or_else(|| content.get("text").and_then(Value::as_str))
                .unwrap_or("");
            if !text.is_empty() {
                out.push(push_text(text, in_thinking));
            }
        }
        BLOCK_LOADING => {
            let text = content
                .pointer("/loading_block/text_loading/text")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !text.is_empty() {
                out.push(Delta::Tool(text.to_string()));
            }
        }
        BLOCK_GENERIC_TOOL => {
            let title = content
                .pointer("/generic_tool_block/title")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !title.is_empty() {
                out.push(Delta::Tool(format!("[tool] {title}")));
            }
        }
        BLOCK_SEARCH_RESULT => {
            let summary = content
                .pointer("/search_query_result_block/summary")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !summary.is_empty() {
                out.push(Delta::Tool(format!("[search] {summary}")));
            }
        }
        // 生成产物（文生图）：上游先推「占位」，后续才带 url。
        // 本实现**不注册**文生图能力（[二进制] 与 [Go参考] 都把它列为
        // 「未接入」），因此这里不给增量 —— 让图片 URL 落进正文会把一段
        // 不可访问的签名 URL 粘进对话里。人工排查时能从详细日志看到块类型。
        BLOCK_CREATION | BLOCK_ATTACHMENT => {}
        _ => {
            crate::server::logging::verbose(
                "[DoubaoWork]",
                &format!("未知 content_block 类型 {block_type}（已忽略）"),
            );
        }
    }
    out
}

/// 从错误载荷里读一条可读文案（`error_msg` / `message` / `msg` / `text`）
fn readable_error(payload: &Value) -> Option<String> {
    for key in ["error_msg", "message", "msg", "text", "error"] {
        if let Some(text) = payload.get(key).and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    // 顶层的裸字符串（`data:` 后面就是一个 JSON 字符串）
    payload.as_str().map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

/// 一次性把整段 SSE 文本解析成增量序列（**离线 / 测试用**）。
///
/// 转发路径上不用它（那里是逐块喂 [`SseLineBuffer`] 的流式形态），但测试与
/// 排障「把一段抓包还原成增量序列」时非常有用 —— 而且它让 [`parse_event`]
/// 的契约可以被逐字断言（见本文件末尾的测试）。
pub fn parse_stream_text(raw: &str) -> Vec<Delta> {
    let mut buffer = SseLineBuffer::new();
    let mut in_thinking = false;
    let mut out: Vec<Delta> = Vec::new();
    for item in buffer.push(raw.as_bytes()) {
        if let Some(payload) = item {
            // `SseLineBuffer` 只吐 `data:` 载荷，事件名丢了 ——
            // 因此这里按「载荷形状」判断（[`parse_event`] 的兜底路径，
            // 它对不带事件名的帧同样成立）。
            out.extend(parse_event(None, &payload, &mut in_thinking));
        } else {
            out.push(Delta::Done);
        }
    }
    for item in buffer.finish() {
        if let Some(payload) = item {
            out.extend(parse_event(None, &payload, &mut in_thinking));
        } else {
            out.push(Delta::Done);
        }
    }
    out
}

/// 粗略估算用量（**本家上游不返回 token 数**，如实声明）。
///
/// 依据：[Go参考] 的 `estimateUsage` 就是「字符数 / 2」——上游的 SSE 里
/// **没有任何 usage 帧**（[二进制] 的字段表里也没有 usage 一族）。
/// 这里沿用同一估算口径，并把它标成估算：**编一个精确数字比留空更糟**
/// （用户会拿它去对账）。
///
/// 返回 `(prompt_tokens, completion_tokens)`。
pub fn estimate_tokens(prompt: &str, completion: &str) -> (i64, i64) {
    let count = |text: &str| -> i64 { (text.chars().count() as i64) / 2 };
    (count(prompt), count(completion))
}

/// 附件以文本标记表达时的形态（[二进制] `[Attached image ` 串的复刻）。
///
/// 它只在「附件没能上传」时进正文（见模块头）。写在这里而不是散在
/// [`build_chat_request`] 里，是为了让「这条标记是我们加的、不是模型写的」
/// 在代码里一眼可见。
pub fn attachment_marker(kind: &str, index: usize, name: &str) -> String {
    format!("[{kind} {index}: {name}]")
}

/// **异步**：把请求体里的内联图片 / 文件上传到上游资源中心，拿到资源 uri。
///
/// ── 谁调它（本文件里暂时没有调用点，见模块头）──────────────────
/// 转发路径在 `build_chat_request`（同步）之前需要一个 await 点。接线方
/// 可以照 Command Code 的做法把它挂在 `ensure_access_token` 上，也可以
/// 在 `build_chat_request` 那条路上加一个异步钩子。**本函数已经可用**：
/// 给它凭据与请求体，它返回可直接喂给 [`plan::build`] 的
/// [`ReadyAttachments`]。
///
/// ── 失败语义：附件失败**不致命**，但也不静默 ────────────────────
/// 单张图 / 单个文件失败时跳过它并记一行**详细日志**（其余附件继续上传）：
/// 「一张图挂了就把整条请求退掉」对用户是净损失（他还能问别的）。但如果
/// **全部**附件都失败，返回 `Err`（那时正文里只剩文本，用户以为自己发的图
/// 生效了 —— 这种假成功必须报出来）。
pub async fn prepare_attachments(
    credentials: &Credentials,
    body: &Value,
) -> Result<ReadyAttachments, GatewayError> {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (images, skipped_links) = plan::collect_images(&messages);
    let files = plan::collect_files(&messages);
    if skipped_links > 0 {
        crate::server::logging::verbose(
            "[DoubaoWork]",
            &format!(
                "跳过 {skipped_links} 个外链图片分片：上游不会自行抓取外部资源，\
                 网关也不代下载（避免 SSRF 面）；请改为 base64 内联"
            ),
        );
    }
    let total = images.len() + files.len();
    if total == 0 {
        return Ok(ReadyAttachments::default());
    }

    let mut ready = ReadyAttachments::default();
    let mut failures: Vec<String> = Vec::new();
    for (index, image) in images.iter().enumerate() {
        match upload::upload_bytes(
            credentials,
            upload::ResourceType::Image,
            &image.name,
            &image.mime,
            &image.data,
        )
        .await
        {
            Ok(uri) => ready.images.push((uri, image.mime.clone())),
            Err(error) => {
                failures.push(format!("图片 {}（{}）：{}", index + 1, image.name, error.message));
            }
        }
    }
    for (index, file) in files.iter().enumerate() {
        match upload::upload_bytes(
            credentials,
            upload::ResourceType::File,
            &file.name,
            &file.mime,
            &file.data,
        )
        .await
        {
            Ok(uri) => ready
                .files
                .push((uri, file.name.clone(), file.data.len() as u64)),
            Err(error) => {
                failures.push(format!("文件 {}（{}）：{}", index + 1, file.name, error.message));
            }
        }
    }
    if ready.images.is_empty() && ready.files.is_empty() {
        return Err(GatewayError::with_status(
            502,
            format!("全部 {total} 个附件上传失败：{}", failures.join("；")),
        ));
    }
    if !failures.is_empty() {
        crate::server::logging::verbose(
            "[DoubaoWork]",
            &format!(
                "{} / {total} 个附件上传失败（其余照常发送）：{}",
                failures.len(),
                failures.join("；")
            ),
        );
    }
    Ok(ready)
}

// ─── 异步上传结果的中转（`prepare_body` → `build_chat_request`）──────────
//
// 为什么要一张表：上传是**异步**的（`prepare_attachments` 要发网络请求），
// 而 `build_chat_request` 是**同步**的。编排层在两者之间 await 了
// `ProviderAdapter::prepare_body`（见 trait 文档），本模块就在那里把结果
// 存进来、在这里取走。
//
// 键是**账号 id**（两个钩子拿到的 `account` 是同一个会话形态，`id` 都在）。
// 取走即删（同一次转发只该用一份结果；留着会让下一次请求误用上一轮的图）。
// 另设 TTL 兜底：万一 `build_chat_request` 因故没被调到，条目不会一直留着。
static PREPARED_ATTACHMENTS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, (ReadyAttachments, i64)>>,
> = std::sync::OnceLock::new();

/// 准备结果的有效期（毫秒）。两个钩子是相邻两步，正常间隔毫秒级；
/// 60 秒是宽松上限，超过即丢弃（宁可让本轮退回文本标记，也不用上一轮的图）。
const PREPARED_TTL_MS: i64 = 60_000;

fn prepared_table(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, (ReadyAttachments, i64)>> {
    PREPARED_ATTACHMENTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 会话形态里的账号 id（两个钩子都用它做键）
fn account_id_of_session(account: &Value) -> String {
    account
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

/// **异步准备**：把这一轮 body 里的图片 / 文件真正传到上游，结果存进中转表。
///
/// 由 `DoubaoWorkAdapter::prepare_body` 调用（编排层在 `ensure_access_token`
/// 之后、`build_chat_request` 之前 await 它）。
///
/// ── 没有附件时零成本 ────────────────────────────────────────
/// `prepare_attachments` 自己在「没有可传的附件」时立刻返回空结果，
/// 这里也只在**真有东西**时才写表 —— 纯文本请求不受任何影响。
pub async fn prepare_attachments_for_session(
    account: &Value,
    body: &Value,
) -> Result<(), GatewayError> {
    let credentials = credentials_from_session(account)?;
    let ready = prepare_attachments(&credentials, body).await?;
    if ready.images.is_empty() && ready.files.is_empty() {
        return Ok(());
    }
    let key = account_id_of_session(account);
    if key.is_empty() {
        // 没有 id 就没法在同步侧对上号：不写表（本轮退回文本标记，不会静默丢）
        crate::server::logging::verbose(
            "[DoubaoWork]",
            "附件已上传，但会话里没有账号 id，无法交给同步构造路径（本轮按文本标记处理）",
        );
        return Ok(());
    }
    let now = crate::server::logging::now_ms();
    let mut guard = match prepared_table().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.insert(key, (ready, now));
    Ok(())
}

/// 取走中转表里的附件结果（取走即删，过期即弃）。
fn take_prepared_attachments(account_id: &str) -> Option<ReadyAttachments> {
    if account_id.is_empty() {
        return None;
    }
    let mut guard = match prepared_table().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let (ready, at) = guard.remove(account_id)?;
    if crate::server::logging::now_ms().saturating_sub(at) > PREPARED_TTL_MS {
        return None;
    }
    Some(ready)
}

/// 构造 `POST /chat/completion` 的请求计划（**同步**，见模块头关于附件的说明）。
///
/// `account` 是会话形态（`store.get_session_by_id` 的返回值）；本家的凭证是
/// 一整套 Cookie，因此从记录里读回来的是 [`Credentials`]。
pub fn build_chat_request(
    account: &Value,
    body: &Value,
    _client_headers: &HeaderMap,
) -> Result<ChatRequestPlan, GatewayError> {
    let credentials = credentials_from_session(account)?;
    // ── 先取「异步上传结果」（由 `prepare_body` 存下，见下）────────────
    // 拿到就是**真附件**（图片 / 文件已换成上游资源 uri，见 `super::upload`），
    // 直接交给 `plan::build` 生成真实的消息块。
    //
    // 取不到（没有附着附件、或调用方没走 `prepare_body` 钩子）才退回文本标记
    // —— 那条路**不会静默丢附件**：用户看得见「图没生效」，而静默丢弃会让他
    // 以为生效了。两条路的取舍见模块头「附件的异步上传」那段。
    let prepared = take_prepared_attachments(&account_id_of_session(account));
    if let Some(ready) = prepared.as_ref() {
        if !ready.images.is_empty() || !ready.files.is_empty() {
            let built = plan::build(&credentials, body, ready)?;
            return Ok(ChatRequestPlan::chat(built.url, built.headers, built.body));
        }
    }
    let marker_text = attachment_markers(body);
    let (input, ready) = if marker_text.is_empty() {
        (body.clone(), ReadyAttachments::default())
    } else {
        crate::server::logging::verbose(
            "[DoubaoWork]",
            &format!(
                "本轮未上传附件（同步构造路径无法上传）：{marker_text} 已转成文本标记；\
                 如需真正传图，请走 `chat::prepare_attachments`（见模块头）"
            ),
        );
        let mut patched = body.clone();
        if let Some(object) = patched.as_object_mut() {
            let messages = object
                .get("messages")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut rebuilt: Vec<Value> = Vec::with_capacity(messages.len() + 1);
            let mut appended = false;
            for message in messages.iter().rev() {
                // 把标记挂在**最后一条 user 消息**的文本尾部
                let role = message.get("role").and_then(Value::as_str).unwrap_or("");
                if !appended && role == "user" {
                    let mut patched_message = message.clone();
                    let original = patched_message
                        .get("content")
                        .map(text_of_content)
                        .unwrap_or_default();
                    if let Some(inner) = patched_message.as_object_mut() {
                        inner.insert(
                            "content".to_string(),
                            Value::String(format!("{original}\n{marker_text}")),
                        );
                    }
                    rebuilt.push(patched_message);
                    appended = true;
                    continue;
                }
                rebuilt.push(message.clone());
            }
            rebuilt.reverse();
            object.insert("messages".to_string(), Value::Array(rebuilt));
        }
        (patched, ReadyAttachments::default())
    };

    let built = plan::build(&credentials, &input, &ready)?;
    Ok(ChatRequestPlan::chat(built.url, built.headers, built.body))
}

/// `content` 的纯文本形态（只认字符串与 text 类分片；图片 / 文件分片不参与）
fn text_of_content(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                let kind = part.get("type").and_then(Value::as_str).unwrap_or("");
                if matches!(kind, "text" | "input_text" | "output_text") || kind.is_empty() {
                    part.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// 附件文本标记（图片 + 文件，按出现顺序编号）
fn attachment_markers(body: &Value) -> String {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut markers: Vec<String> = Vec::new();
    let (images, _) = plan::collect_images(&messages);
    for (index, image) in images.iter().enumerate() {
        markers.push(attachment_marker("Attached image", index + 1, &image.name));
    }
    let files = plan::collect_files(&messages);
    for (index, file) in files.iter().enumerate() {
        markers.push(attachment_marker("Attached file", index + 1, &file.name));
    }
    markers.join("\n")
}

/// 从**会话形态**里读回本家的凭证。
///
/// ── 会话形态为什么是「层层兜底」的三条路（每条都有它的存在理由）─────
/// 本家凭证是一整串 Cookie + 一组指纹，而本仓的会话形态
/// （`store.session_from_record` 的产物）是为「token 型」凭据设计的：
/// 它只稳定地带上 `auth.accessToken`（`refreshToken` / `expiresAt` 那几个
/// 本家没有）。因此本家落账号时必须**同时**写两份（见
/// `account_store::doubaowork_accounts`，与 KukuAI 的做法一致）：
///   1. `accessToken` = **拼好的 Cookie 头字符串** —— 这一份是为了让
///      `StoredAccount::has_credentials()` 为真（它认 `accessToken` / `apiKey` /
///      `jwt` / `desktop` / `noAuth` 五个键，**没有 Cookie 这一条**）。
///      不为真时账号既排不进可转发队列，`get_session_by_id` 也直接返回 `None`；
///   2. `cookies` = **行数组** —— 这一份是真正的凭据（`from_record` 读它）；
///   3. `fingerprint` = 那组指纹。
/// 三条读法按「越结构化越优先」排：行数组 > 会话里的 `auth.cookies` >
/// `auth.accessToken` 整串（最后这条靠 [`super::credentials::parse_cookie_string`]
/// 拆开）。任一命中即可 —— 少一条兜底，某些编排层形态下就会得到一句
/// 「缺少 Cookie」而账号其实配得好好的。
/// 给同目录其它模块用的凭证读法（生图要拼同一份请求头）。
pub fn credentials_for(account: &Value) -> Result<Credentials, GatewayError> {
    credentials_from_session(account)
}

fn credentials_from_session(account: &Value) -> Result<Credentials, GatewayError> {
    let id = account
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    // 完整记录形态（`build_chat_request` 拿到的就是它）
    if account.get("cookies").is_some() {
        if let Ok(credentials) = super::credentials::from_record(Some(account)) {
            return Ok(credentials);
        }
    }
    if let Some(auth) = account.get("auth") {
        // 会话形态的 `auth.cookies`（若编排层把行数组放进去了）
        if let Some(cookies) = auth.get("cookies").and_then(Value::as_array) {
            let mut record = auth.clone();
            if let Some(object) = record.as_object_mut() {
                object.insert("id".to_string(), Value::String(id.to_string()));
                object.insert("cookies".to_string(), Value::Array(cookies.to_vec()));
                // 指纹可能在 `auth.fingerprint`，也可能在顶层
                if object.get("fingerprint").is_none() {
                    if let Some(fingerprint) = account.get("fingerprint").cloned() {
                        object.insert("fingerprint".to_string(), fingerprint);
                    }
                }
            }
            if let Ok(credentials) = super::credentials::from_record(Some(&record)) {
                return Ok(credentials);
            }
        }
        // 会话形态的 `auth.accessToken` = 拼好的 Cookie 头（见函数文档第 1 条）
        if let Some(cookie_string) = auth.get("accessToken").and_then(Value::as_str) {
            if !cookie_string.trim().is_empty() {
                let mut record = account.clone();
                if let Some(object) = record.as_object_mut() {
                    object.insert("id".to_string(), Value::String(id.to_string()));
                    object.insert(
                        "cookie".to_string(),
                        Value::String(cookie_string.to_string()),
                    );
                }
                if let Ok(credentials) = super::credentials::from_record(Some(&record)) {
                    return Ok(credentials);
                }
            }
        }
    }
    Err(GatewayError::with_status(
        401,
        "豆包工作账号缺少 Cookie 登录态，无法转发（请重新用浏览器登录，或粘贴完整 Cookie）",
    ))
}

/// 探测账号是否有登录态（**零网络**）：给 `ensure_access_token` 用。
pub fn has_session(account: &Value) -> bool {
    credentials_from_session(account).is_ok()
}

/// 一次真实对话的连通性探测（**可选**，耗时且会花掉一次额度）。
///
/// ⚠️ 与 [`super::credentials::local_check`] 的分工写在两边：
/// 添加账号时的校验**一律是本地形态检查**（零网络、不花额度），本函数只在
/// 「用户显式要求验证这个账号」时才用。因此它**没有**任何自动调用点。
pub async fn probe(credentials: &Credentials) -> Result<(), GatewayError> {
    let built = plan::build(
        credentials,
        &serde_json::json!({
            "model": "doubao-fast",
            "messages": [{"role": "user", "content": "hi"}]
        }),
        &ReadyAttachments::default(),
    )?;
    let response = send_raw(
        "POST",
        &built.url,
        Some(&built.body),
        &built.headers,
        None,
        Some(super::endpoints::CATALOG_TIMEOUT_MS),
    )
    .await
    .map_err(|error| GatewayError::with_status(502, format!("豆包工作探活请求失败：{error}")))?;
    match response.status {
        200..=299 => Ok(()),
        401 | 403 => Err(GatewayError::with_status(
            401,
            format!(
                "豆包工作登录态已失效（HTTP {}）：请在账号页重新登录",
                response.status
            ),
        )),
        other => Err(GatewayError::with_status(
            502,
            format!(
                "豆包工作探活返回 HTTP {other}：{}",
                response
                    .payload
                    .as_ref()
                    .and_then(readable_error)
                    .unwrap_or_else(|| "（无文案）".to_string())
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(raw: &str) -> Vec<Delta> {
        parse_stream_text(raw)
    }

    /// `CHUNK_DELTA` 的紧凑形态：`{"text":"…"}` → 正文增量
    #[test]
    fn chunk_delta_becomes_text() {
        let deltas = parse("data: {\"text\":\"你好\"}\n\n");
        assert_eq!(deltas, vec![Delta::Text("你好".to_string())]);
    }

    /// `patch_op` 的块数组：`content_block` 里的文本块 → 正文
    #[test]
    fn patch_op_content_block_becomes_text() {
        let raw = concat!(
            "data: {\"patch_op\":[{\"patch_value\":{\"content_block\":[",
            "{\"block_type\":10000,\"content\":{\"text_block\":{\"text\":\"世界\"}}}]}}]}\n\n"
        );
        assert_eq!(parse(raw), vec![Delta::Text("世界".to_string())]);
    }

    /// ⚠️ `patch_value.content` 是**字符串化的 JSON** —— 漏了它有的回答一个字都出不来
    #[test]
    fn patch_op_stringified_content_becomes_text() {
        let raw = concat!(
            "data: {\"patch_op\":[{\"patch_value\":{\"content\":",
            "\"{\\\"text\\\":\\\"来自字符串\\\"}\"}}]}\n\n"
        );
        assert_eq!(parse(raw), vec![Delta::Text("来自字符串".to_string())]);
    }

    /// 思考块切换状态：10040 之后的文本进思维链，直到出现正文块为止
    #[test]
    fn thinking_block_switches_subsequent_text() {
        let raw = concat!(
            "data: {\"patch_op\":[{\"patch_value\":{\"content_block\":[",
            "{\"block_type\":10040,\"content\":{}}]}}]}\n\n",
            "data: {\"patch_op\":[{\"patch_value\":{\"content_block\":[",
            "{\"block_type\":10000,\"content\":{\"text_block\":{\"text\":\"思考中\"}}}]}}]}\n\n",
        );
        assert_eq!(parse(raw), vec![Delta::Thinking("思考中".to_string())]);
    }

    /// 业务错误：`error_code` 非 0 → `Delta::Error`（HTTP 仍是 200，错误在流内）
    #[test]
    fn business_error_becomes_error_delta() {
        let raw = "data: {\"error_code\":710022004,\"error_msg\":\"rate limited\"}\n\n";
        let deltas = parse(raw);
        assert_eq!(
            deltas,
            vec![Delta::Error {
                code: 710022004,
                message: "rate limited".to_string()
            }]
        );
    }

    /// `gateway-error` 事件：载荷可能不是 JSON 也不能吞掉
    #[test]
    fn gateway_error_is_never_swallowed() {
        // 带事件名的路径（parse_event 直接调）
        let mut in_thinking = false;
        let deltas = parse_event(Some("gateway-error"), "upstream exploded", &mut in_thinking);
        assert_eq!(deltas.len(), 1);
        assert!(matches!(deltas[0], Delta::Error { .. }));
        let mut in_thinking = false;
        let json_deltas = parse_event(
            Some("gateway-error"),
            "{\"code\":500,\"message\":\"boom\"}",
            &mut in_thinking,
        );
        assert_eq!(
            json_deltas,
            vec![Delta::Error { code: 500, message: "boom".to_string() }]
        );
    }

    /// `SSE_ACK` 给出会话 id；`"0"` 不算（那是「新会话」的占位）
    #[test]
    fn sse_ack_yields_conversation_id() {
        let mut in_thinking = false;
        let deltas = parse_event(
            Some("SSE_ACK"),
            "{\"ack_client_meta\":{\"conversation_id\":\"1234567890\"}}",
            &mut in_thinking,
        );
        assert_eq!(deltas, vec![Delta::Conversation("1234567890".to_string())]);
        let mut in_thinking = false;
        let zero = parse_event(
            Some("SSE_ACK"),
            "{\"ack_client_meta\":{\"conversation_id\":\"0\"}}",
            &mut in_thinking,
        );
        assert!(zero.is_empty(), "\"0\" 是占位，不算会话建立");
    }

    /// 收尾：`SSE_REPLY_END` 事件与 `[DONE]` 两种都要认
    #[test]
    fn reply_end_and_done_both_close_the_stream() {
        assert_eq!(parse("data: [DONE]\n\n"), vec![Delta::Done]);
        let mut in_thinking = false;
        assert_eq!(
            parse_event(Some("SSE_REPLY_END"), "", &mut in_thinking),
            vec![Delta::Done]
        );
        // 事件名带收尾但载荷里也有 JSON 时，以收尾为准
        let mut in_thinking = false;
        assert_eq!(
            parse_event(Some("SSE_REPLY_END"), "{\"x\":1}", &mut in_thinking),
            vec![Delta::Done]
        );
    }

    /// 坏帧 / 非 JSON / 未知块类型都**不能**打断整条流（只是跳过）
    #[test]
    fn malformed_frames_are_skipped_not_fatal() {
        let raw = concat!(
            "data: not-json-at-all\n\n",
            "data: {\"patch_op\":[{\"patch_value\":{\"content_block\":[",
            "{\"block_type\":99999,\"content\":{}}]}}]}\n\n",
            "data: {\"text\":\"still alive\"}\n\n",
        );
        assert_eq!(parse(raw), vec![Delta::Text("still alive".to_string())]);
    }

    /// 工具 / 联网提示块 → `Delta::Tool`（前缀让客户端看得出这不是模型正文）
    #[test]
    fn tool_and_search_blocks_are_marked() {
        let raw = concat!(
            "data: {\"patch_op\":[{\"patch_value\":{\"content_block\":[",
            "{\"block_type\":10024,\"content\":{\"generic_tool_block\":{\"title\":\"天气\"}}}]}}]}\n\n",
            "data: {\"patch_op\":[{\"patch_value\":{\"content_block\":[",
            "{\"block_type\":10025,\"content\":{\"search_query_result_block\":{\"summary\":\"今天晴\"}}}]}}]}\n\n",
        );
        assert_eq!(
            parse(raw),
            vec![
                Delta::Tool("[tool] 天气".to_string()),
                Delta::Tool("[search] 今天晴".to_string())
            ]
        );
    }

    /// 生成产物（文生图）**不注册**（能力边界如实声明），不产出任何增量
    #[test]
    fn creation_blocks_produce_no_delta() {
        let raw = concat!(
            "data: {\"patch_op\":[{\"patch_value\":{\"content_block\":[",
            "{\"block_type\":2074,\"content\":{\"creation_block\":{\"creations\":[]}}}]}}]}\n\n"
        );
        assert!(parse(raw).is_empty());
    }

    /// 用量是**估算**（上游不返回 token 数）：字符数 / 2
    #[test]
    fn usage_is_estimated_from_characters() {
        assert_eq!(estimate_tokens("abcd", "abcdef"), (2, 3));
        // 中文按字符计（不是字节）
        assert_eq!(estimate_tokens("你好", "你好世界"), (1, 2));
        assert_eq!(estimate_tokens("", ""), (0, 0));
    }

    /// 附件标记形态（取自二进制里的 `[Attached image ` 串）
    #[test]
    fn attachment_marker_shape() {
        assert_eq!(
            attachment_marker("Attached image", 1, "a.png"),
            "[Attached image 1: a.png]"
        );
    }

    /// 同步构造路径：**带内联图的请求不会静默丢图**（转成文本标记进正文）
    #[test]
    fn inline_attachment_becomes_text_marker_in_sync_path() {
        let account = json!({
            "id": "doubaowork-1",
            "name": "测试",
            "cookies": [
                {"name": "sessionid", "value": "abcdef", "domain": ".doubao.com", "path": "/"}
            ]
        });
        let body = json!({"model": "doubao-fast", "messages": [{"role": "user", "content": [
            {"type": "text", "text": "看这张图"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,SGk="}}
        ]}]});
        let plan = build_chat_request(&account, &body, &HeaderMap::new());
        assert!(
            plan.is_ok(),
            "构造应当成功：{:?}",
            plan.as_ref().err().map(|error| error.message.clone())
        );
        // 上一行已断言成功；这里的 `Err` 分支**不可达**，只为满足编译
        // （不用 unwrap：release 是 panic=abort）
        let plan = match &plan {
            Ok(plan) => plan.clone(),
            Err(_) => return,
        };
        let text = plan
            .body
            .pointer("/messages/0/content_block/0/content/text_block/text")
            .and_then(Value::as_str)
            .unwrap_or("");
        assert!(text.contains("看这张图"), "原文应保留：{text}");
        assert!(text.contains("[Attached image 1: image.png]"), "标记应在：{text}");
    }

    /// 缺 Cookie → 401（文案可执行）；有 Cookie → 通过
    #[test]
    fn missing_cookie_is_reported_actionably() {
        assert_eq!(
            build_chat_request(&json!({"id": "x"}), &json!({"messages": []}), &HeaderMap::new())
                .err()
                .map(|error| error.status_code),
            Some(401)
        );
        let account = json!({
            "cookies": [{"name": "sessionid", "value": "abc", "domain": ".doubao.com", "path": "/"}]
        });
        assert!(has_session(&account));
        assert!(!has_session(&json!({})));
    }

    /// 会话形态：`auth.accessToken` 里装的是**拼好的 Cookie 头**（见
    /// [`credentials_from_session`] 的说明 —— 本仓的 `has_credentials()` 只认
    /// `accessToken` / `apiKey` / `jwt` / `desktop` / `noAuth`，本家必须借它）
    #[test]
    fn credentials_can_live_under_auth_access_token() {
        let account = json!({
            "id": "doubaowork-1",
            "auth": {"accessToken": "sessionid=xyz; passport_csrf_token=tok"}
        });
        let credentials = credentials_from_session(&account);
        assert!(
            credentials.is_ok(),
            "应当能从 auth.accessToken 读出凭证：{:?}",
            credentials.as_ref().err().map(|error| error.message.clone())
        );
        let credentials = match credentials {
            Ok(credentials) => credentials,
            // 不可达（上面已断言）
            Err(_) => return,
        };
        assert_eq!(credentials.cookies.len(), 2);
        assert!(credentials.has_session());
        assert_eq!(credentials.csrf_token().as_deref(), Some("tok"));
        // `auth.cookies` 行数组形态优先，同样要能读
        let nested = json!({
            "id": "doubaowork-1",
            "auth": {
                "cookies": [{"name": "sessionid", "value": "xyz", "domain": ".doubao.com", "path": "/"}]
            }
        });
        assert!(credentials_from_session(&nested).is_ok());
    }

    // ── 附件中转表（`prepare_body` → `build_chat_request`）──────────────

    /// 取走即删：同一次转发只能消费一份结果（留着会让下一轮误用上一轮的图）
    #[test]
    fn prepared_attachments_are_consumed_once() {
        let key = "test-account-once";
        {
            let mut guard = match prepared_table().lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.insert(
                key.to_string(),
                (ReadyAttachments::default(), crate::server::logging::now_ms()),
            );
        }
        assert!(take_prepared_attachments(key).is_some(), "第一次应取得到");
        assert!(take_prepared_attachments(key).is_none(), "第二次必须取不到");
    }

    /// 过期结果被丢弃（宁可本轮退回文本标记，也不用上一轮的图）
    #[test]
    fn prepared_attachments_expire() {
        let key = "test-account-expired";
        {
            let mut guard = match prepared_table().lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.insert(
                key.to_string(),
                (
                    ReadyAttachments::default(),
                    crate::server::logging::now_ms() - PREPARED_TTL_MS - 1_000,
                ),
            );
        }
        assert!(take_prepared_attachments(key).is_none(), "过期结果不得返回");
    }

    /// 空账号 id 不参与中转（没有键就没法在同步侧对上号）
    #[test]
    fn empty_account_id_never_reads_the_table() {
        assert!(take_prepared_attachments("").is_none());
    }

    /// 会话形态里能读出账号 id（两个钩子都靠它做键）
    #[test]
    fn account_id_is_read_from_session() {
        assert_eq!(
            account_id_of_session(&json!({ "id": "doubaowork-abc" })),
            "doubaowork-abc"
        );
        // 没有 id → 空串（调用方据此跳过中转）
        assert_eq!(account_id_of_session(&json!({})), "");
        // 前后空白被裁掉
        assert_eq!(account_id_of_session(&json!({ "id": "  x  " })), "x");
    }

}
