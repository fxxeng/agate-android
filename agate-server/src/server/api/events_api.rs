//! GET /api/events —— SSE 增量事件流（M3）。
//!
//! 挂 protected 组：loopback 免鉴权形态直接可连；局域网形态走面板凭证
//! 或 API Key（与其它 /api/* 同一道闸门，见 http.rs 的 require_api_key）。
//!
//! 帧语义：
//!   · `hello`（具名事件）—— 连接建立即推一条，客户端用它确认链路活了
//!     （Repository 把它当「SSE 就绪」的信号，比对轮询更早）；
//!   · 默认 `message` 帧 —— data 是完整事件 JSON（type / tsMs / data）；
//!   · `ping`（具名）—— KeepAlive 每 15s，穿透中间层空闲断连；
//!   · `lagged`（具名）—— 消费者落后超过通道容量被跳帧，客户端应做一次
//!     全量刷新而不是继续拿残缺增量（见 events.rs 模块头）。

use std::convert::Infallible;
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::StreamExt;
use futures::future::ok;
use futures::stream::{self as fstream};
use serde_json::json;
use tokio_stream::wrappers::BroadcastStream;

use crate::server::events;

pub async fn handle() -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    // futures 0.3 的 filter_map 是异步签名（闭包须返回 Future<Output=Option<T>>），
    // 同步 match 用 async move 块包一层。
    let live = BroadcastStream::new(events::subscribe()).filter_map(|item| async move {
        match item {
            Ok(text) => Some(Ok(Event::default().data(text))),
            // Lagged：只告诉客户端「你落后了」，不带被丢的条数（拿到条数也没法
            // 补，客户端的动作只有一个 —— 全量刷新）
            Err(_) => Some(Ok(Event::default()
                .event("lagged")
                .data(r#"{"type":"lagged","data":{"hint":"refresh"}}"#))),
        }
    });
    let hello = fstream::once(ok(Event::default()
        .event("hello")
        .data(
            json!({
                "type": "hello",
                "tsMs": events::now_ms(),
                "data": { "version": env!("CARGO_PKG_VERSION") },
            })
            .to_string(),
        )));
    // axum 0.8 的 KeepAlive 只有 text()（发 `: ping` 注释帧，不带事件名）；
    // okhttp-sse 对注释帧不回调 onEvent，正好是纯心跳语义。
    Sse::new(hello.chain(live)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}
