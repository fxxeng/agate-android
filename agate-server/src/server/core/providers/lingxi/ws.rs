//! cowork WebSocket 通道（P3）+ 群聊任务的前置动作（P2）。
//!
//! ── 两条 WS 不是同一条 ──────────────────────────────────────
//! ```text
//!   对话主通道  wss /api/aioffice/v1/cowork/sessions/{id}/sandbox/completions/ws
//!   群聊前置    wss /api/aioffice/v1/sessions/{id}/completions/ws
//! ```
//! 会话都先 `POST /api/aioffice/v1/cowork/sessions` 建；host 是
//! **`lingxi.kdocs.cn`**，握手 `Origin` 也必须是它（`endpoints::ws_origin`）。
//!
//! ── 为什么值得单独一条通道 ──────────────────────────────────
//! 私有 SSE（`stream.rs`）只能拿到正文与思考；cowork WS 这条能拿到**真工具
//! 执行**：`c_execute_code` 跑在云端沙箱里，`full_content.output` 是真实
//! stdout。本仓是网关，没法把沙箱执行渲染成工具调用交给下游，所以按插件
//! 作者的做法把它**折成 Markdown 正文**（代码 + 输出），至少让用户看到
//! 「真的跑了」。
//!
//! ── 帧协议（`xfyang728/dsh-router-wpslingxi` README 的真机实测）──────
//! 服务端回 JSON 文本帧 `{"event":"…","data":{…}}`，**语义靠 `data.type`**
//! （这是与 SSE 通道最大的差别）：
//! ```text
//!   run.start         {id, group_id}
//!   component.start   {type, id, parent_id, group_id, payload?}
//!   component.content {type, id, delta}          增量
//!   component.end     {type, id, full_content}   完整内容
//!   turn.start/end    {type, id, agent_time}
//!   run.end           {id, credit_consumed, session_credit_consumed}
//! ```
//! 组件类型：`c_reasoning` → 思考增量；`c_text` → 正文；`c_execute_code` 等
//! 工具 → 渲染成正文；`c_recommend` → 忽略。
//!
//! ── 与 SSE 通道的三处**有意差异**（都写在界面与文档里）─────────────
//!   1. 该协议只收**一段** `agent_input.text`，没有 messages 数组 ⇒ 多轮
//!      历史压平成带角色前缀的一段文本（`flatten_prompt`）；
//!   2. 没有 token 用量（`run.end` 给的是**智点**消耗）⇒ 不下发 `usage`，
//!      请求日志里这一条的 token 会是 0，不拿估算值冒充；
//!   3. 账号级出网代理只作用于建会话的 HTTP 调用，WS 直连（与 MonkeyCode
//!      任务流同一条取舍）。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic，不持锁跨 await。

use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Map, Value};

use crate::server::core::upstream::ForwardOutcome;
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::{client, endpoints};

/// 单帧读取上限（上游一帧最多几 KB，给 8MB 是防御性上限）。
const MAX_FRAME_BYTES: usize = 8 << 20;
/// 整轮对话的总时长上限（与 MonkeyCode 的 1 小时同量级；cowork 带沙箱执行）。
const TOTAL_TIMEOUT: Duration = Duration::from_secs(1800);
/// 空闲上限：上游正常每几秒就有帧，长时间无帧即视为断线（避免挂到总超时）。
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

type Socket = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

fn header_pair(name: &str, value: &str) -> Result<(String, String), GatewayError> {
    if value.contains('\n') || value.contains('\r') {
        return Err(GatewayError::with_status(500, format!("灵犀握手头 {name} 含换行")));
    }
    Ok((name.to_string(), value.to_string()))
}

/// 建立 WS 连接（Cookie 走握手头，`Origin` 固定 kdocs 域）。
async fn connect(url: &str, cookie: Option<&str>, sid: &str) -> Result<Socket, GatewayError> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};

    let mut request = url
        .into_client_request()
        .map_err(|error| GatewayError::with_status(502, format!("灵犀通道地址无效：{error}")))?;
    let cookie_value = match cookie {
        Some(full) if !full.trim().is_empty() => full.trim().to_string(),
        _ => format!("wps_sid={sid}"),
    };
    let headers = [
        header_pair("Cookie", &cookie_value)?,
        header_pair("User-Agent", endpoints::USER_AGENT)?,
        header_pair("Origin", endpoints::ws_origin())?,
    ];
    for (name, value) in headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| GatewayError::with_status(500, format!("灵犀握手头名非法：{name}")))?;
        let header_value = HeaderValue::from_str(&value)
            .map_err(|_| GatewayError::with_status(500, format!("灵犀握手头 {name} 含非法字符")))?;
        request.headers_mut().insert(header_name, header_value);
    }
    let wait = Duration::from_secs(20);
    let (socket, _response) = tokio::time::timeout(
        wait,
        tokio_tungstenite::connect_async_tls_with_config(request, None, false, None),
    )
    .await
    .map_err(|_| GatewayError::with_status(504, "灵犀通道连接超时（WebSocket 握手未完成）"))?
    .map_err(connect_error)?;
    Ok(socket)
}

fn connect_error(error: tokio_tungstenite::tungstenite::Error) -> GatewayError {
    use tokio_tungstenite::tungstenite::Error as WsError;
    match error {
        WsError::Http(response) => {
            let status = response.status().as_u16();
            if status == 401 || status == 403 {
                GatewayError::with_status(
                    401,
                    format!("灵犀通道鉴权失败（HTTP {status}）：请重新登录或粘贴 Cookie"),
                )
            } else {
                GatewayError::with_status(502, format!("灵犀通道握手被拒（HTTP {status}）"))
            }
        }
        other => GatewayError::with_status(502, format!("灵犀通道连接失败：{other}")),
    }
}

/// 建 cowork 会话（WS 通道专用：body 是 `{}`，模型在 `agent_input.expect_model`）。
async fn create_session(
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
    body: &Value,
) -> Result<String, GatewayError> {
    let reply = client::request(
        "POST",
        endpoints::sessions_path(),
        sid,
        device_id,
        cookie,
        Some(body),
    )
    .await?;
    if !(200..300).contains(&reply.status) || !reply.result_ok {
        return Err(GatewayError::with_status(
            i32::from(reply.status),
            format!("灵犀建会话失败（HTTP {}）", reply.status),
        ));
    }
    let session_id = reply
        .body
        .pointer("/data/session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if session_id.is_empty() {
        return Err(GatewayError::with_status(502, "灵犀建会话响应里没有 session_id"));
    }
    Ok(session_id)
}

// ─── 帧翻译（纯函数，可单测）────────────────────────────────

/// 一帧翻译出来的增量。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Delta {
    pub content: Option<String>,
    pub reasoning: Option<String>,
}

/// 帧的语义类别。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// 有意外的增量（正文 / 思考 / 工具渲染）。
    Delta(Delta),
    /// 轮次结束（`run.end` / `stream.end` / `done`）。
    Finish,
    /// 控制帧，什么也不下发。
    Ignore,
}

fn event_of(frame: &Value) -> &str {
    frame.get("event").and_then(Value::as_str).unwrap_or("")
}

fn component_type(frame: &Value) -> &str {
    frame
        .pointer("/data/type")
        .and_then(Value::as_str)
        .or_else(|| frame.get("type").and_then(Value::as_str))
        .unwrap_or("")
}

fn delta_text(frame: &Value) -> &str {
    frame
        .pointer("/data/delta")
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn full_content(frame: &Value) -> &Value {
    frame.pointer("/data/full_content").unwrap_or(&Value::Null)
}

/// 工具组件的正文渲染（`c_execute_code` 等的 `full_content`）。
///
/// 形状（实测）：`{"arguments":{"code":"…","brief":"…"},"output":"真实 stdout"}`。
/// 只认这几个键，其余工具（`c_websearch` / `c_read_file` …）按同样的
/// `brief` + `output` 渲染 —— 猜具体参数名就是编协议。
pub fn render_tool(full: &Value) -> Option<String> {
    let arguments = full.get("arguments").unwrap_or(&Value::Null);
    let brief = arguments
        .get("brief")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let code = arguments
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let output = full.get("output").and_then(Value::as_str).unwrap_or("").trim();
    if brief.is_empty() && code.is_empty() && output.is_empty() {
        return None;
    }
    let mut text = String::new();
    text.push_str("\n\n> 🔧 **");
    text.push_str(if brief.is_empty() { "工具执行" } else { brief });
    text.push_str("**\n\n");
    if !code.is_empty() {
        text.push_str("```\n");
        text.push_str(code);
        text.push_str("\n```\n");
    }
    if !output.is_empty() {
        text.push_str("\n**输出：**\n\n```\n");
        text.push_str(output);
        text.push_str("\n```\n");
    }
    Some(text)
}

/// 翻译一帧（纯函数）。判据见模块头的帧协议表。
pub fn translate_frame(frame: &Value) -> Frame {
    let event = event_of(frame);
    match event {
        "run.end" | "stream.end" | "done" => return Frame::Finish,
        _ => {}
    }
    if !event.starts_with("component.") {
        // run.start / turn.start / turn.end / ping 等控制帧：不下发
        return Frame::Ignore;
    }
    let kind = component_type(frame);
    if kind.is_empty() || kind == "c_recommend" {
        return Frame::Ignore;
    }
    if event == "component.content" {
        let text = delta_text(frame);
        if text.is_empty() {
            return Frame::Ignore;
        }
        return match kind {
            "c_reasoning" => Frame::Delta(Delta {
                reasoning: Some(text.to_string()),
                content: None,
            }),
            "c_text" => Frame::Delta(Delta {
                content: Some(text.to_string()),
                reasoning: None,
            }),
            // 工具的代码正文不逐帧推（半截代码没有意义，README 明确点名这个坑）
            _ => Frame::Ignore,
        };
    }
    if event == "component.end" {
        if kind == "c_text" || kind == "c_reasoning" {
            // 正文/思考的全文在 component.content 已经流过了，这里不重复
            return Frame::Ignore;
        }
        return match render_tool(full_content(frame)) {
            Some(text) => Frame::Delta(Delta { content: Some(text), reasoning: None }),
            None => Frame::Ignore,
        };
    }
    Frame::Ignore
}

/// 多轮 messages → 一段 `agent_input.text`（该协议只收一段文本）。
///
/// 单条用户消息原样透出（不污染提示词）；多轮时按角色加前缀压平，
/// 让上游至少能看到「谁说了什么」。
pub fn flatten_prompt(messages: &Value) -> String {
    let Some(items) = messages.as_array() else {
        return String::new();
    };
    let texts: Vec<(String, String)> = items
        .iter()
        .filter_map(|item| {
            let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
            let content = item.get("content")?;
            let text = match content {
                Value::String(text) => text.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };
            let text = text.trim().to_string();
            if text.is_empty() {
                None
            } else {
                Some((role.to_string(), text))
            }
        })
        .collect();
    if texts.is_empty() {
        return String::new();
    }
    if texts.len() == 1 && texts[0].0 == "user" {
        return texts[0].1.clone();
    }
    let mut out = String::new();
    for (role, text) in texts {
        let label = match role.as_str() {
            "assistant" => "助手",
            "system" | "developer" => "系统",
            "tool" | "function" => "工具结果",
            _ => "用户",
        };
        out.push_str("【");
        out.push_str(label);
        out.push_str("】");
        out.push_str(&text);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// 上游帧 → OpenAI chat chunk（纯函数）。
pub fn chunk_of(model: &str, delta: &Delta, finish: bool) -> Value {
    let mut body = Map::new();
    body.insert("object".to_string(), Value::String("chat.completion.chunk".to_string()));
    body.insert("model".to_string(), Value::String(model.to_string()));
    let mut payload = Map::new();
    if let Some(reasoning) = delta.reasoning.as_ref() {
        payload.insert("reasoning_content".to_string(), Value::String(reasoning.clone()));
    }
    if let Some(content) = delta.content.as_ref() {
        payload.insert("content".to_string(), Value::String(content.clone()));
    }
    if finish {
        payload.insert("finish_reason".to_string(), Value::String("stop".to_string()));
    }
    body.insert(
        "choices".to_string(),
        json!([{ "index": 0, "delta": Value::Object(payload) }]),
    );
    Value::Object(body)
}

fn sse_bytes(value: &Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}

fn done_bytes() -> Bytes {
    Bytes::from_static(b"data: [DONE]\n\n")
}

/// WS 通道的一次对话：建会话 → 连 WS → 发 `user.input` → 读帧 → 出 SSE。
///
/// 与 SSO 通道的差异见模块头「三处有意差异」。`stream` 决定出口形态：
/// 流式返回 `ForwardOutcome::Stream`（后台任务喂 mpsc），非流式聚合成
/// 一份 `chat.completion`。
#[allow(clippy::too_many_arguments)]
pub async fn forward(
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
    model: &str,
    body: &Value,
    stream_requested: bool,
) -> Result<ForwardOutcome, GatewayError> {
    let prompt = flatten_prompt(body.get("messages").unwrap_or(&Value::Null));
    if prompt.trim().is_empty() {
        return Err(GatewayError::with_status(400, "灵犀通道需要至少一条非空的用户消息"));
    }
    let session_id = create_session(sid, device_id, cookie, &json!({})).await?;
    let url = endpoints::cowork_ws_url(&session_id);
    let mut socket = connect(&url, cookie, sid).await?;
    send_user_input(&mut socket, model, &prompt, body).await?;

    let translator = Translator::new(model);
    if stream_requested {
        let (sender, receiver) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);
        crate::spawn_task(async move {
            drive_stream(socket, translator, sender).await;
        });
        Ok(ForwardOutcome::Stream {
            status: 200,
            stream: Box::new(tokio_stream::wrappers::ReceiverStream::new(receiver)),
        })
    } else {
        let body = drive_aggregate(socket, translator).await?;
        Ok(ForwardOutcome::Completion { body })
    }
}

/// 起始交互：`user.input`（控制面字段见插件 README）。
async fn send_user_input(
    socket: &mut Socket,
    model: &str,
    prompt: &str,
    request: &Value,
) -> Result<(), GatewayError> {
    let effort = request
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .unwrap_or("low");
    let payload = json!({
        "event": "user.input",
        "data": {
            "agent_input": {
                "text": prompt,
                "files": [],
                "refs": [],
                "file_ids": [],
                "expect_model": model,
                "reasoning_effort": effort,
                "current_mode": "agent",
                "permission_mode": "default",
                "browser_plugin_enabled": false,
            },
            "headers": {
                "client_type": endpoints::CLIENT_TYPE,
                "open_position": "clawapp",
            },
        },
    });
    socket
        .send(tokio_tungstenite::tungstenite::Message::text(payload.to_string()))
        .await
        .map_err(|error| GatewayError::with_status(502, format!("灵犀通道发送失败：{error}")))
}

/// 帧 → SSE 状态机（流式）。
pub struct Translator {
    model: String,
    finished: bool,
}

impl Translator {
    pub fn new(model: &str) -> Self {
        Self { model: model.to_string(), finished: false }
    }

    /// 处理一帧，产出要下发的字节（可能为空）。
    pub fn on_frame(&mut self, frame: &Value) -> Vec<Bytes> {
        if self.finished {
            return Vec::new();
        }
        match translate_frame(frame) {
            Frame::Delta(delta) => vec![sse_bytes(&chunk_of(&self.model, &delta, false))],
            Frame::Finish => {
                self.finished = true;
                vec![
                    sse_bytes(&chunk_of(&self.model, &Delta::default(), true)),
                    done_bytes(),
                ]
            }
            Frame::Ignore => Vec::new(),
        }
    }

    pub fn finished(&self) -> bool {
        self.finished
    }
}

async fn next_frame(socket: &mut Socket) -> Result<Value, GatewayError> {
    loop {
        let message = tokio::time::timeout(IDLE_TIMEOUT, socket.next())
            .await
            .map_err(|_| GatewayError::with_status(504, "灵犀通道空闲超时（长时间没有帧）"))?;
        let Some(message) = message else {
            return Err(GatewayError::with_status(502, "灵犀通道已关闭"));
        };
        let message = message.map_err(|error| {
            GatewayError::with_status(502, format!("灵犀通道读取失败：{error}"))
        })?;
        match message {
            tokio_tungstenite::tungstenite::Message::Text(text) => {
                if text.len() > MAX_FRAME_BYTES {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(text.as_str()) {
                    return Ok(value);
                }
            }
            tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
                if bytes.len() > MAX_FRAME_BYTES {
                    continue;
                }
                if let Ok(value) = serde_json::from_slice::<Value>(bytes.as_ref()) {
                    return Ok(value);
                }
            }
            tokio_tungstenite::tungstenite::Message::Close(_) => {
                return Err(GatewayError::with_status(502, "灵犀通道已关闭"));
            }
            // ping/pong 由 tungstenite 自动回应
            _ => continue,
        }
    }
}

/// 流式驱动：一直读到 `run.end`（或出错），收尾必发 `[DONE]`。
async fn drive_stream(
    mut socket: Socket,
    mut translator: Translator,
    sender: tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
) {
    let deadline = tokio::time::Instant::now() + TOTAL_TIMEOUT;
    loop {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        match next_frame(&mut socket).await {
            Ok(frame) => {
                for bytes in translator.on_frame(&frame) {
                    if sender.send(Ok(bytes)).await.is_err() {
                        // 客户端断开：停止消费并关连接
                        let _ = socket.close(None).await;
                        return;
                    }
                }
                if translator.finished() {
                    break;
                }
            }
            Err(error) => {
                // 已经是流式响应头之后：只能把错误写进流里，客户端才看得见
                let payload = json!({ "error": { "message": error.message, "type": "upstream_error" } });
                let _ = sender.send(Ok(sse_bytes(&payload))).await;
                break;
            }
        }
    }
    if !translator.finished() {
        let _ = sender.send(Ok(done_bytes())).await;
    }
    let _ = socket.close(None).await;
}

/// 聚合驱动：非流式出口（拼一份 `chat.completion`）。
async fn drive_aggregate(mut socket: Socket, mut translator: Translator) -> Result<Value, GatewayError> {
    let mut content = String::new();
    let mut reasoning = String::new();
    let deadline = tokio::time::Instant::now() + TOTAL_TIMEOUT;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(GatewayError::with_status(504, "灵犀通道超时"));
        }
        let frame = next_frame(&mut socket).await?;
        match translate_frame(&frame) {
            Frame::Delta(delta) => {
                if let Some(text) = delta.content {
                    content.push_str(&text);
                }
                if let Some(text) = delta.reasoning {
                    reasoning.push_str(&text);
                }
            }
            Frame::Finish => {
                translator.finished = true;
                break;
            }
            Frame::Ignore => continue,
        }
    }
    let _ = socket.close(None).await;
    let mut message = Map::new();
    message.insert("role".to_string(), Value::String("assistant".to_string()));
    message.insert("content".to_string(), Value::String(content));
    if !reasoning.is_empty() {
        message.insert("reasoning_content".to_string(), Value::String(reasoning));
    }
    Ok(json!({
        "object": "chat.completion",
        "model": translator.model.clone(),
        "choices": [{ "index": 0, "message": Value::Object(message), "finish_reason": "stop" }],
    }))
}

// ─── 群聊任务（P2）──────────────────────────────────────────

/// 群聊体验任务：`cowork_web` 会话 → WS 发一次 `user.input` → 建群 → 解散。
///
/// 关键判据（面板源码实测）：建群前必须让该会话在 **completions WS** 上收到过
/// 一次 `user.input`（网页端建群前的固定动作），只走 REST 对话再建群会直接
/// 400「参数错误」；`expect_model` 固定 `plan_pro`（网页端固定值，不是目录模型）。
pub async fn complete_group_task(
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
) -> Result<String, GatewayError> {
    let session_id = create_session(
        sid,
        device_id,
        cookie,
        &json!({ "session_type": endpoints::SESSION_TYPE_GROUP }),
    )
    .await?;
    let url = endpoints::group_ws_url(&session_id);
    let mut socket = connect(&url, cookie, sid).await?;
    send_user_input(&mut socket, super::tasks::GROUP_PLAN_MODEL, super::tasks::TASK_PROMPT, &Value::Null)
        .await?;
    // 等到 run 结束（最多 90s，面板同值）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        if tokio::time::Instant::now() >= deadline {
            let _ = socket.close(None).await;
            return Err(GatewayError::with_status(504, "群聊任务：通道未在 90 秒内结束"));
        }
        match next_frame(&mut socket).await {
            Ok(frame) => {
                if event_of(&frame) == "run.end" {
                    break;
                }
            }
            Err(error) => {
                let _ = socket.close(None).await;
                return Err(error);
            }
        }
    }
    let _ = socket.close(None).await;

    // 建群
    let created = client::request(
        "POST",
        endpoints::imchat_channels_path(),
        sid,
        device_id,
        cookie,
        Some(&json!({
            "session_id": session_id,
            "expect_model": super::tasks::GROUP_PLAN_MODEL,
        })),
    )
    .await?;
    if !(200..300).contains(&created.status) || !created.result_ok {
        return Err(GatewayError::with_status(
            502,
            format!("群聊任务：建群失败（HTTP {}）", created.status),
        ));
    }
    let channel_id = created
        .body
        .pointer("/data/channel_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if channel_id.is_empty() {
        // 上游没给 channel_id 时任务可能已经点亮，如实说「没拿到 id」而不是谎报成功
        return Ok("对话已完成，但建群未返回 channel_id（任务可能已点亮）".to_string());
    }
    // 解散（失败不影响任务判定，如实记日志）
    let path = endpoints::imchat_channel_path(&channel_id);
    match client::request("DELETE", &path, sid, device_id, cookie, None).await {
        Ok(reply) if (200..300).contains(&reply.status) => Ok("已建群并解散（判定完成）".to_string()),
        Ok(reply) => {
            logging::log(
                "[Models]",
                &format!("⚠️ 灵犀群聊任务：解散群 {channel_id} 失败（HTTP {}）", reply.status),
            );
            Ok("已建群（解散失败，已记日志）".to_string())
        }
        Err(error) => {
            logging::log(
                "[Models]",
                &format!("⚠️ 灵犀群聊任务：解散群 {channel_id} 失败：{}", error.message),
            );
            Ok("已建群（解散失败，已记日志）".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_reasoning_components_map_to_the_right_delta_fields() {
        let text = translate_frame(&json!({
            "event": "component.content",
            "data": { "type": "c_text", "id": "1", "delta": "你好" }
        }));
        assert_eq!(
            text,
            Frame::Delta(Delta { content: Some("你好".to_string()), reasoning: None })
        );
        let reasoning = translate_frame(&json!({
            "event": "component.content",
            "data": { "type": "c_reasoning", "id": "2", "delta": "想想" }
        }));
        assert_eq!(
            reasoning,
            Frame::Delta(Delta { content: None, reasoning: Some("想想".to_string()) })
        );
    }

    #[test]
    fn control_frames_never_emit_an_empty_chunk() {
        for frame in [
            json!({ "event": "run.start", "data": { "id": "r1", "group_id": "g1" } }),
            json!({ "event": "component.start", "data": { "type": "c_text", "id": "1" } }),
            json!({ "event": "turn.end", "data": { "type": "t", "agent_time": 3 } }),
            json!({ "event": "component.content", "data": { "type": "c_recommend", "delta": "x" } }),
        ] {
            assert_eq!(translate_frame(&frame), Frame::Ignore, "{frame}");
        }
    }

    #[test]
    fn run_end_finishes_the_stream() {
        assert_eq!(
            translate_frame(&json!({ "event": "run.end", "data": { "credit_consumed": 1 } })),
            Frame::Finish
        );
    }

    #[test]
    fn tool_code_is_rendered_once_at_component_end_never_per_delta() {
        let during = json!({
            "event": "component.content",
            "data": { "type": "c_execute_code", "id": "3", "delta": "print(" }
        });
        assert_eq!(translate_frame(&during), Frame::Ignore, "半截代码不能推给下游");

        let end = json!({
            "event": "component.end",
            "data": {
                "type": "c_execute_code",
                "id": "3",
                "full_content": {
                    "arguments": { "code": "print(1)", "brief": "跑一次打印" },
                    "output": "1\n"
                }
            }
        });
        match translate_frame(&end) {
            Frame::Delta(delta) => {
                let text = delta.content.unwrap_or_default();
                assert!(text.contains("跑一次打印"), "{text}");
                assert!(text.contains("print(1)"), "{text}");
                assert!(text.contains("输出"), "{text}");
            }
            other => panic!("期望渲染正文，实际 {other:?}"),
        }
    }

    #[test]
    fn translator_emits_finish_chunk_and_done_exactly_once() {
        let mut translator = Translator::new("glm-5.3");
        let first = translator.on_frame(&json!({
            "event": "component.content",
            "data": { "type": "c_text", "delta": "A" }
        }));
        assert_eq!(first.len(), 1);
        let last = translator.on_frame(&json!({ "event": "run.end", "data": {} }));
        assert_eq!(last.len(), 2, "finish chunk + [DONE]");
        assert!(translator.finished());
        assert!(translator.on_frame(&json!({ "event": "run.end", "data": {} })).is_empty());
    }

    #[test]
    fn single_user_message_is_not_decorated_but_multi_turn_is_flattened() {
        let single = json!([{ "role": "user", "content": "解释一下三次握手" }]);
        assert_eq!(flatten_prompt(&single), "解释一下三次握手");
        let multi = json!([
            { "role": "system", "content": "你是助手" },
            { "role": "user", "content": "第一问" },
            { "role": "assistant", "content": "第一答" },
            { "role": "user", "content": "第二问" }
        ]);
        let flat = flatten_prompt(&multi);
        assert!(flat.contains("【系统】你是助手"));
        assert!(flat.contains("【助手】第一答"));
        assert!(flat.trim_end().ends_with("【用户】第二问"));
    }

    #[test]
    fn chunk_shape_is_openai_compatible() {
        let chunk = chunk_of(
            "glm-5.3",
            &Delta { content: Some("x".to_string()), reasoning: None },
            false,
        );
        assert_eq!(chunk["object"], json!("chat.completion.chunk"));
        assert_eq!(chunk["choices"][0]["delta"]["content"], json!("x"));
        assert!(chunk.get("usage").is_none(), "WS 通道没有 token 用量，不许编");
    }

    #[test]
    fn tool_render_declines_frames_without_any_payload() {
        assert!(render_tool(&json!({})).is_none());
        assert!(render_tool(&json!({ "arguments": {} })).is_none());
        assert!(render_tool(&json!({ "output": "   " })).is_none());
    }

    #[test]
    fn ws_urls_use_the_kdocs_host_and_the_two_different_paths() {
        let chat = endpoints::cowork_ws_url("s1");
        assert!(chat.starts_with("wss://lingxi.kdocs.cn/"), "{chat}");
        assert!(chat.contains("/cowork/sessions/s1/sandbox/completions/ws"), "{chat}");
        let group = endpoints::group_ws_url("s1");
        assert!(group.starts_with("wss://lingxi.kdocs.cn/"), "{group}");
        assert!(group.contains("/sessions/s1/completions/ws"), "{group}");
        assert!(!group.contains("sandbox"));
        assert_eq!(endpoints::ws_origin(), "https://lingxi.kdocs.cn");
    }
}
