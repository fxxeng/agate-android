//! Kiro 的会话式转发：发一次 `generateAssistantResponse`，把 AWS 事件流翻成
//! OpenAI chat 的流 / 完整响应。
//!
//! ── 为什么必须是会话式（`is_stateful() == true`）──────────────
//! 上游响应是 **AWS event stream**（二进制帧），`UpstreamKind` 是封闭枚举、
//! 没有这个变体。走 `forward_conversation` 之后，本模块自己读帧、自己推标准
//! chat 帧，网关的账号轮换 / 限额冷却 / 用量统计照旧工作。
//!
//! ── 先读第一帧再决定「算不算成功」──────────────────────────
//! 上游经常先回 200、再把错误塞进流里（`throttlingException`、
//! `INSUFFICIENT_MODEL_CAPACITY`）。直接推 200 的话，网关与客户端都会当成一次
//! 成功回答（用户看到的是一条空消息），而且这个账号不会被轮换。所以这里
//! **预读第一个事件**：是错误就整次上抛，编排层据此换账号。
//!
//! ── 401 之后强制续期一次 ────────────────────────────────────
//! 被拒的 token 时间上可能还很新（`fresh()` 为真），只看临期窗口会拿回同一个
//! 坏 token。所以 401/403 走 `refresh::force`（跳过临期判定），成功则重发一次；
//! 刷新也失败时**保留原始错误** —— 那比一句「续期失败」更贴近真实原因
//! （API key 与已吊销的登录态刷新本就救不回来）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic；持锁不跨 await。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use tokio_stream::wrappers::ReceiverStream;

use crate::server::core::account_store::{AccountStore, CredentialWrite};
use crate::server::core::proxies::ResolvedProxy;
use crate::server::core::upstream::usage::RequestTelemetry;
use crate::server::core::upstream::ForwardOutcome;
use crate::server::errors::GatewayError;

use super::credentials::KiroCredentials;
use super::events::{Decoder, Event, Usage};
use super::{endpoints, http, models, refresh, request};

/// 事件流的空闲兜底（编排层另有统一的 idle_guard；这里是兜底值）
const STREAM_IDLE_MS: u64 = 120_000;

/// 上游没给模型时的默认值（Kiro 自己的路由模型）
const DEFAULT_MODEL: &str = "auto";

/// 转发入口（`forward_conversation` 转调）
pub async fn run_chat(
    store: &AccountStore,
    account_id: &str,
    body: &Value,
    proxy: Option<ResolvedProxy>,
    stream: bool,
    telemetry: &Arc<RequestTelemetry>,
) -> Result<ForwardOutcome, GatewayError> {
    let record = store.kiro_account_record(account_id).ok_or_else(|| {
        GatewayError::with_status(
            404,
            "还没有 Kiro 账号：请粘贴 ksk_ 开头的 API key，或粘贴登录态 / 重新登录",
        )
    })?;
    let id = record
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(account_id)
        .to_string();
    let mut credentials = KiroCredentials::from_fields(&record)?;
    let mut changed = false;

    // 临期就先续期（`ensure_fresh` 在有效期内是零成本的原样返回）
    let next = refresh::ensure_fresh(&id, &credentials, proxy.as_ref()).await?;
    if next.access != credentials.access
        || next.refresh != credentials.refresh
        || next.expires != credentials.expires
    {
        credentials = next;
        changed = true;
    }
    // OAuth 首次使用要补 profile：额度、目录与对话都要它，缺了会拿到空清单或 400
    if !credentials.is_api_key() && credentials.profile.is_empty() {
        if let Ok(profile) = refresh::lookup_profile(&credentials, proxy.as_ref()).await {
            if !profile.is_empty() {
                credentials.profile = profile;
                changed = true;
            }
        }
    }
    if changed {
        write_back(store, &record, &credentials);
    }

    let model = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_MODEL)
        .to_string();
    let budget = request::thinking_budget(body, &model);
    let native = models::native_of(&id, &model);
    // API key 账号没有 profile，带上会 400
    let profile = if credentials.is_api_key() {
        String::new()
    } else {
        credentials.profile.clone()
    };
    let kiro_body = request::build(body, &model, &profile, budget, native.as_ref());

    let (response, credentials) =
        match http::ask(&credentials, &kiro_body, proxy.as_ref()).await {
            Ok(response) => (response, credentials),
            Err(error) if error.status_code == 401 || error.status_code == 403 => {
                let refreshed = refresh::force(&id, &credentials, proxy.as_ref()).await?;
                write_back(store, &record, &refreshed);
                let response = http::ask(&refreshed, &kiro_body, proxy.as_ref()).await?;
                (response, refreshed)
            }
            Err(error) => return Err(error),
        };

    let url = format!(
        "{}{}",
        endpoints::runtime(&credentials.region()),
        endpoints::CHAT_PATH
    );
    if let Some(capture) = telemetry.capture().as_deref() {
        capture.reset_request(&url, super::ID, &[], body);
        capture.attach_response(response.status().as_u16(), response.headers());
    }

    let names = request::tool_names(body);
    if !stream {
        let source = http::byte_stream(response, Duration::from_millis(STREAM_IDLE_MS));
        let mut decoder = Decoder::new(source, &model, &id, budget.is_some());
        return aggregate(&mut decoder, &model, &names).await;
    }

    let source = http::byte_stream(response, Duration::from_millis(STREAM_IDLE_MS));
    let mut decoder = Decoder::new(source, &model, &id, budget.is_some());
    // 预读一帧：错误必须在发 head 之前上抛（见模块头）
    let first = decoder.next().await;
    if let Some(Event::Error { status, text }) = &first {
        return Err(GatewayError::with_status(
            i32::from(status.unwrap_or(502)),
            text.clone(),
        ));
    }
    let mut forwarder = Forwarder::new(decoder, model, names);
    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);
    crate::spawn_task(async move {
        if let Err(error) = forwarder.drive(first, sender).await {
            crate::server::logging::verbose(
                "[Kiro]",
                &format!("对话流处理中断：{}", error.message),
            );
        }
    });
    Ok(ForwardOutcome::Stream {
        status: 200,
        stream: Box::new(ReceiverStream::new(receiver)),
    })
}

/// 续期 / 补 profile 之后写回账号（**比较-再写**：期间被换过就放弃）
fn write_back(store: &AccountStore, original: &Value, credentials: &KiroCredentials) {
    match store.update_kiro_credentials_if_current(original, credentials) {
        Ok(CredentialWrite::Written) => {}
        Ok(CredentialWrite::Stale) => {
            crate::server::logging::verbose(
                "[Kiro]",
                "续期结果未写回：账号在刷新期间被重新添加或替换过",
            );
        }
        Err(error) => {
            crate::server::logging::verbose(
                "[Kiro]",
                &format!("续期结果写回失败：{}", error.message),
            );
        }
    }
}

// ─── 帧与状态 ───────────────────────────────────────────────

/// 响应 id 与收尾状态
struct State {
    id: String,
    created: i64,
    model: String,
    started: bool,
    finished: bool,
}

impl State {
    fn new(model: &str) -> Self {
        Self {
            id: format!("chatcmpl-{}", http::message_id()),
            created: crate::server::logging::now_ms() / 1000,
            model: model.to_string(),
            started: false,
            finished: false,
        }
    }

    fn chunk(&self, delta: Value, finish: Option<&str>) -> Value {
        json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
        })
    }

    fn ensure_role(&mut self, out: &mut Vec<Value>) {
        if self.started {
            return;
        }
        self.started = true;
        out.push(self.chunk(json!({ "role": "assistant", "content": "" }), None));
    }

    /// 收尾帧（`finish_reason` + `usage`），幂等
    fn finish(&mut self, reason: &str, usage: Usage, out: &mut Vec<Value>) {
        if self.finished {
            return;
        }
        self.finished = true;
        let prompt = usage.input + usage.cache_read.max(0);
        let completion = usage.output;
        let mut chunk = self.chunk(json!({}), Some(reason));
        chunk["usage"] = json!({
            "prompt_tokens": prompt.max(0),
            "completion_tokens": completion.max(0),
            "total_tokens": (prompt + completion).max(0),
            "prompt_tokens_details": { "cached_tokens": usage.cache_read.max(0) },
        });
        if usage.cache_write > 0 {
            chunk["usage"]["cache_creation_input_tokens"] = json!(usage.cache_write);
        }
        out.push(chunk);
    }
}

/// SSE 帧
enum Frame {
    Chunk(Value),
    Done,
}

impl Frame {
    fn wire(&self) -> String {
        match self {
            Frame::Chunk(chunk) => format!("data: {chunk}\n\n"),
            Frame::Done => "data: [DONE]\n\n".to_string(),
        }
    }
}

/// 事件 → 帧
struct Forwarder {
    decoder: Decoder,
    state: State,
    /// 规整名 → 客户端原名（工具名不合规时上游回的是规整后的名字）
    names: HashMap<String, String>,
    tool_index: i64,
    tool_open: bool,
}

impl Forwarder {
    fn new(decoder: Decoder, model: String, names: HashMap<String, String>) -> Self {
        Self {
            decoder,
            state: State::new(&model),
            names,
            tool_index: -1,
            tool_open: false,
        }
    }

    /// 一条事件 → 若干帧
    fn frames(&mut self, event: Event) -> Vec<Frame> {
        let mut chunks: Vec<Value> = Vec::new();
        let mut done = false;
        match event {
            Event::Text(text) => {
                self.state.ensure_role(&mut chunks);
                chunks.push(self.state.chunk(json!({ "content": text }), None));
            }
            Event::Think(text) => {
                self.state.ensure_role(&mut chunks);
                chunks.push(self.state.chunk(json!({ "reasoning_content": text }), None));
            }
            Event::ToolStart { id, name } => {
                self.state.ensure_role(&mut chunks);
                self.tool_index += 1;
                self.tool_open = true;
                let name = self.names.get(&name).cloned().unwrap_or(name);
                chunks.push(self.state.chunk(
                    json!({
                        "tool_calls": [{
                            "index": self.tool_index,
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": "" },
                        }]
                    }),
                    None,
                ));
            }
            Event::ToolArgs(args) => {
                if !self.tool_open {
                    return Vec::new();
                }
                chunks.push(self.state.chunk(
                    json!({
                        "tool_calls": [{
                            "index": self.tool_index,
                            "function": { "arguments": args },
                        }]
                    }),
                    None,
                ));
            }
            Event::Stop { reason, usage } => {
                self.state.ensure_role(&mut chunks);
                self.state.finish(&reason, usage, &mut chunks);
                done = true;
            }
            Event::Error { status, text } => {
                let status = status.unwrap_or(502);
                crate::server::logging::verbose(
                    "[Kiro]",
                    &format!("流内错误（{status}）：{text}"),
                );
                // 流已经开始，状态码改不了了：按 OpenAI 的流内错误帧下发，
                // 客户端据此识别失败（而不是把半截回答当成完整回答）
                chunks.push(json!({
                    "error": {
                        "type": if status == 429 { "rate_limit_error" } else { "api_error" },
                        "message": text,
                    }
                }));
                done = true;
            }
        }
        let mut out: Vec<Frame> = chunks.into_iter().map(Frame::Chunk).collect();
        if done {
            out.push(Frame::Done);
        }
        out
    }

    /// 继续读流并下发（窥探用掉的那一帧一并带上）
    async fn drive(
        &mut self,
        first: Option<Event>,
        sender: tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    ) -> Result<(), GatewayError> {
        let mut pending = first;
        loop {
            let event = match pending.take() {
                Some(event) => Some(event),
                None => self.decoder.next().await,
            };
            let Some(event) = event else {
                // 事件流异常结束：补一个收尾帧，别让客户端一直等
                let mut chunks: Vec<Value> = Vec::new();
                self.state.ensure_role(&mut chunks);
                self.state.finish("stop", Usage::default(), &mut chunks);
                let mut out: Vec<Frame> = chunks.into_iter().map(Frame::Chunk).collect();
                out.push(Frame::Done);
                return send_frames(out, &sender).await;
            };
            let out = self.frames(event);
            let done = matches!(out.last(), Some(Frame::Done));
            send_frames(out, &sender).await?;
            if done {
                return Ok(());
            }
        }
    }
}

/// 下发一批帧。
///
/// 写成**自由函数**而不是 `&self` 的方法：`&Forwarder` 需要 `Send` 才能跨
/// `.await`，而 `Forwarder` 里装着 `Decoder`（内含 `dyn Stream + Send`，
/// 不含 `Sync`），所以 `&Forwarder` 不是 `Send` —— 方法版会让整个后台任务的
/// future 不 Send，直接编译失败。这里只需要帧本身，不需要任何状态。
async fn send_frames(
    frames: Vec<Frame>,
    sender: &tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
) -> Result<(), GatewayError> {
    for frame in frames {
        if sender.send(Ok(Bytes::from(frame.wire()))).await.is_err() {
            // 客户端断开：静默收工（这不是错误）
            return Ok(());
        }
    }
    Ok(())
}

// ─── 非流式聚合 ─────────────────────────────────────────────

/// 把事件流聚合成一份 `chat.completion`
async fn aggregate(
    decoder: &mut Decoder,
    model: &str,
    names: &HashMap<String, String>,
) -> Result<ForwardOutcome, GatewayError> {
    let mut state = State::new(model);
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut usage = Usage::default();
    let mut finish = "stop".to_string();
    while let Some(event) = decoder.next().await {
        match event {
            Event::Text(text) => {
                state.started = true;
                content.push_str(&text);
            }
            Event::Think(text) => {
                state.started = true;
                reasoning.push_str(&text);
            }
            Event::ToolStart { id, name } => {
                state.started = true;
                let name = names.get(&name).cloned().unwrap_or(name);
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": "" },
                }));
            }
            Event::ToolArgs(args) => {
                if let Some(call) = tool_calls.last_mut() {
                    let merged = format!(
                        "{}{}",
                        call.pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        args
                    );
                    call["function"]["arguments"] = json!(merged);
                }
            }
            Event::Stop {
                reason,
                usage: reported,
            } => {
                finish = reason;
                usage = reported;
            }
            Event::Error { status, text } => {
                return Err(GatewayError::with_status(
                    i32::from(status.unwrap_or(502)),
                    text,
                ));
            }
        }
    }
    let mut message = json!({ "role": "assistant", "content": content });
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
        finish = "tool_calls".to_string();
    }
    let prompt = usage.input + usage.cache_read.max(0);
    let completion = usage.output;
    let body = json!({
        "id": state.id,
        "object": "chat.completion",
        "created": state.created,
        "model": state.model,
        "choices": [{ "index": 0, "message": message, "finish_reason": finish }],
        "usage": {
            "prompt_tokens": prompt.max(0),
            "completion_tokens": completion.max(0),
            "total_tokens": (prompt + completion).max(0),
            "prompt_tokens_details": { "cached_tokens": usage.cache_read.max(0) },
        },
    });
    Ok(ForwardOutcome::Completion { body })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forwarder() -> Forwarder {
        Forwarder::new(
            Decoder::new(Box::pin(futures::stream::empty()), "auto", "kiro:test", false),
            "auto".to_string(),
            HashMap::new(),
        )
    }

    fn chunks(frames: &[Frame]) -> Vec<&Value> {
        frames
            .iter()
            .filter_map(|frame| match frame {
                Frame::Chunk(chunk) => Some(chunk),
                Frame::Done => None,
            })
            .collect()
    }

    /// 第一帧正文前先发 role（客户端按它开始渲染）
    #[test]
    fn the_first_content_frame_is_preceded_by_the_role() {
        let mut forwarder = forwarder();
        let frames = forwarder.frames(Event::Text("你好".to_string()));
        let chunks = chunks(&frames);
        assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(chunks[1]["choices"][0]["delta"]["content"], "你好");
    }

    /// 思考走 reasoning_content，不混进 content
    #[test]
    fn thinking_becomes_reasoning_content() {
        let mut forwarder = forwarder();
        let frames = forwarder.frames(Event::Think("想一想".to_string()));
        let payload = frames
            .iter()
            .filter_map(|frame| match frame {
                Frame::Chunk(chunk) => chunk.pointer("/choices/0/delta").cloned(),
                Frame::Done => None,
            })
            .find(|delta| delta.get("reasoning_content").is_some())
            .expect("有 reasoning_content");
        assert_eq!(payload["reasoning_content"], "想一想");
        assert!(payload.get("content").is_none());
    }

    /// 工具的 index 递增、第二个工具的 id 也带上
    #[test]
    fn tool_calls_are_indexed_and_spread_over_frames() {
        let mut forwarder = forwarder();
        let frames = forwarder.frames(Event::ToolStart {
            id: "call_1".to_string(),
            name: "get_weather".to_string(),
        });
        let first = chunks(&frames)
            .into_iter()
            .find(|chunk| chunk.pointer("/choices/0/delta/tool_calls").is_some())
            .expect("有 tool_calls")
            .clone();
        assert_eq!(first.pointer("/choices/0/delta/tool_calls/0/index"), Some(&json!(0)));
        assert_eq!(
            first.pointer("/choices/0/delta/tool_calls/0/function/name"),
            Some(&json!("get_weather"))
        );
        // 参数增量只带 arguments（index 指向同一个工具）
        let args = forwarder.frames(Event::ToolArgs("{\"a\":1}".to_string()));
        let delta = chunks(&args)[0]
            .pointer("/choices/0/delta/tool_calls/0")
            .cloned()
            .expect("有参数增量");
        assert_eq!(delta.get("index").cloned(), Some(json!(0)));
        assert_eq!(
            delta.pointer("/function/arguments").cloned(),
            Some(json!("{\"a\":1}"))
        );
    }

    /// 工具名要映射回客户端原名
    #[test]
    fn tool_names_are_mapped_back_for_the_client() {
        let mut names = HashMap::new();
        names.insert("weather_lookup_ab12".to_string(), "weather/lookup".to_string());
        let mut forwarder = Forwarder::new(
            Decoder::new(Box::pin(futures::stream::empty()), "auto", "kiro:test", false),
            "auto".to_string(),
            names,
        );
        let frames = forwarder.frames(Event::ToolStart {
            id: "call_1".to_string(),
            name: "weather_lookup_ab12".to_string(),
        });
        let name = chunks(&frames)
            .into_iter()
            .find_map(|chunk| {
                chunk
                    .pointer("/choices/0/delta/tool_calls/0/function/name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .expect("有工具名");
        assert_eq!(name, "weather/lookup");
    }

    /// 收尾：finish_reason 正确、用量换算到 OpenAI 口径、只收一次
    #[test]
    fn the_stop_frame_carries_finish_reason_and_usage() {
        let mut forwarder = forwarder();
        let frames = forwarder.frames(Event::Stop {
            reason: "stop".to_string(),
            usage: Usage {
                input: 100,
                output: 20,
                cache_read: 50,
                cache_write: 0,
                reported: true,
            },
        });
        assert!(matches!(frames.last(), Some(Frame::Done)));
        let last = chunks(&frames).into_iter().last().cloned().expect("有收尾帧");
        assert_eq!(last.pointer("/choices/0/finish_reason"), Some(&json!("stop")));
        // prompt = input + cache_read（缓存命中也要算进 prompt）
        assert_eq!(last.pointer("/usage/prompt_tokens"), Some(&json!(150)));
        assert_eq!(last.pointer("/usage/completion_tokens"), Some(&json!(20)));
        assert_eq!(
            last.pointer("/usage/prompt_tokens_details/cached_tokens"),
            Some(&json!(50))
        );
        // 幂等：再来一次不再发收尾帧
        let again = forwarder.frames(Event::Stop {
            reason: "stop".to_string(),
            usage: Usage::default(),
        });
        assert_eq!(chunks(&again).len(), 0);
    }

    /// 流内错误下发成错误帧 + [DONE]（让客户端知道这不是完整回答）
    #[test]
    fn an_in_stream_error_is_reported_inside_the_stream() {
        let mut forwarder = forwarder();
        let frames = forwarder.frames(Event::Error {
            status: Some(429),
            text: "usage limit reached".to_string(),
        });
        let error = chunks(&frames)
            .into_iter()
            .find_map(|chunk| chunk.get("error").cloned())
            .expect("有 error 帧");
        assert_eq!(error["type"], "rate_limit_error");
        assert!(matches!(frames.last(), Some(Frame::Done)));
    }

    /// 没发过 ToolStart 的 ToolArgs 丢弃（否则客户端会收到无主的参数）
    #[test]
    fn orphan_tool_arguments_are_dropped() {
        let mut forwarder = forwarder();
        let frames = forwarder.frames(Event::ToolArgs("{}".to_string()));
        assert!(chunks(&frames).is_empty());
    }
}
