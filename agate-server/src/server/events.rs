//! `/api/events` SSE 事件总线（M3 · 方案 v4.0 §3.2 / O9）。
//!
//! 全局单条 tokio broadcast 通道：业务侧 `publish()` 把序列化好的 JSON 事件
//! 推给所有在线 SSE 订阅者；无订阅者时 send 落空（Err），静默忽略 ——
//! 总线常驻，不因「现在没人听」而记日志或报错。
//!
//! 容量 256：事件是「增量 Diff」语义（请求完成 / 签到完成 / 网关启动），
//! 消费者（面板 / Compose 侧 GatewayRepository）慢了就 Lagged，由客户端
//! 自行全量刷新兜底（Repository 的降级轮询那条路，O9），总线侧不做
//! 持久化补偿 —— 补偿会把「实时 Diff」悄悄退化成「延迟重放」。

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::sync::broadcast;

const CAPACITY: usize = 256;

static BUS: OnceLock<broadcast::Sender<String>> = OnceLock::new();

fn sender() -> broadcast::Sender<String> {
    BUS.get_or_init(|| broadcast::channel(CAPACITY).0).clone()
}

/// 毫秒时间戳（UTC epoch；面板按本地时区展示，与 request_stats 同口径）。
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 推一条事件。`kind` 是事件类型（客户端按它分流），`data` 是事件体；
/// 序列化在调用线程完成，订阅者拿到的就是可直接写进 SSE data 帧的
/// 完整对象（含 type / tsMs）。
pub fn publish(kind: &str, data: Value) {
    // F16：先看有没有订阅者，没有（无 SSE 连接，Core 态常态）直接返回，
    // 跳过 JSON 序列化 —— publish 在无人监听时才是零成本。
    let tx = sender();
    if tx.receiver_count() == 0 {
        return;
    }
    let payload = json!({
        "type": kind,
        "tsMs": now_ms(),
        "data": data,
    });
    // 无订阅者 = Err(SendError)：正常态（App 不在前台时没人听），忽略
    let _ = tx.send(payload.to_string());
}

/// 订阅事件流（每个 SSE 连接一份 Receiver）。
pub fn subscribe() -> broadcast::Receiver<String> {
    sender().subscribe()
}
