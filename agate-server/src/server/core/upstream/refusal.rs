//! **流内拒绝**的门控：HTTP 200 也可能是一次拒绝。
//!
//! ── 为什么需要它 ────────────────────────────────────────────
//! 有些上游（实测 OfficeAce / 华为 AgentArts 网关）把「内容审核拒绝」包装成一次
//! 正常答复：`HTTP 200`、SSE 正常收尾，但正文是一句固定的拒绝话术（如
//! 「作为一个人工智能语言模型，我还没学习如何回答这个问题…」），并且终帧的
//! `choices[].finish_reason` 是 `content_filter`。
//!
//! 只看 HTTP 状态，这就成了一次**假成功**：编排层不会换家，客户端拿到的
//! 「答案」是一句套话 —— 用户以为网关坏了（真实现象：ZCode 的 agentic 请求打到
//! 本家条条如此）。
//!
//! ── 判据与代价都在 provider 那边声明，本模块只负责「等」 ──────
//! 「哪些 `finish_reason` 算拒绝」是**上游侧知识**（与 81004/11-128 同类），
//! 由适配器的 `stream_refusal_finishes` 回答；本模块只做一件事：把响应流
//! **拉到能判定为止**，把预读到的字节、剩余流、以及看到的 `finish_reason`
//! 一起交回。**没有 provider 声明时本模块一个字节都不碰** —— 调用点在
//! `provider_loop` 的账号循环里（响应头记账之后、Anthropic / chat 两条下行分支之前），
//! 声明为空时直接走原始响应，本模块不参与。
//!
//! ── 两种终止条件（都必要）────────────────────────────────────
//!   1. **看到 `finish_reason`**：终帧一到就能判定，不必再等。
//!   2. **缓冲到上限**：真答案可能很长，不能无限缓冲（否则等于把流式改成非流式）。
//! 拒绝话术只有几百字节，远在上限之内 ⇒ 这种「先缓冲再放行」对**拒绝**是必中的。
//!
//! ── 代价（审计指出原注释把它写轻了，这里按实际形状说）──────────
//! 正常流的 `finish_reason` 只出现在**末帧**，所以条件 1 对普通答复等价于
//! 「等整条流讲完」：不足上限的答案会被**整段缓冲**，下发形态从「逐段流式」退化成
//! 「完成才给」；超过上限的答案才由条件 2 提前放行，首字节延迟 ≈ 上游产出这么多
//! 字节的耗时。这不是实现取巧，是判据本身决定的 —— 拒绝也是一条**带正文**的 200，
//! 在它说完之前，没有任何字节能把「拒绝」与「正常答复」分开。
//!
//! ── 为什么上限取 32 KiB（2026-10-10 用户明确定：不压小）─────────
//! 实测上游的拒绝正文约 50 token（≈300 字节），32 KiB 对拒绝绰绰有余，同时给
//! 「拒绝前可能先有一段长前言」的形态留余量（那种形态本仓没观测过）。压小上限能
//! 换来更小的首字节延迟，代价是漏拦这种未观测形态 —— 用户选了不压，保持 32 KiB。
//!
//! ── 读不出来就当没看见（fail-open）────────────────────────────
//! 传输错误 / 空闲超时 / 流结束都没看到 `finish_reason` 时返回 `None`，调用点
//! 按「正常答案」放行 —— 门控判错的代价必须是「漏拦一次拒绝」，绝不能是
//! 「把一个正常答案判死」（那会把用户真能用的家挡在门外，比漏拦更坏）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::time::Duration;

use axum::http::HeaderMap;
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::Value;

use crate::server::config;
use crate::server::core::upstream::stall;

/// 门控在「看到 `finish_reason`」或「缓冲区达到上限」时结束（见模块头）。
///
/// 这是**拒答**那一支的上限：它要看的是终帧的 `finish_reason`，而终帧一般在流尾
/// ⇒ 对普通答复等价于「整段缓冲」（代价见模块头）。
const HOLD_CAP_BYTES: usize = 32 * 1024;

/// **只**嗅探流内错误帧时的上限 —— 比 [`HOLD_CAP_BYTES`] 小得多，因为两者的
/// 代价模型完全不同。
///
/// ── 为什么错误帧可以小得多（一条关键的性质）──────────────────────
/// 错误帧是**终态**：上游一旦决定不给答复，那条 `{"error":…}` 几乎总是**首帧**
/// （参考实现里它就是开头那一句）。所以用它判定不需要等流讲完 —— 循环一看到
/// 错误帧就 `break`（见 [`hold_head`]），上限只是「万一它出现在偏后位置」的兜底。
///
/// 对比之下，拒答必须读到 `finish_reason`（一般在流尾），上限压小就漏判。
/// 把两者的上限分开，是为了不让「只嗅探错误帧」的家为一次 32 KiB 的整段缓冲
/// 付出首字节延迟（那是本仓流式体验的实打实退化，不是纸面代价）。
const HOLD_CAP_ERROR_BYTES: usize = 8 * 1024;

/// 一次预读的产物。
pub struct HeldHead {
    /// 上游 HTTP 状态码（预读前取，之后 `Response` 已被消费）
    pub status: u16,
    /// 上游响应头（同上；调试采集要它）
    pub headers: HeaderMap,
    /// 预读到的字节（**原样**，一个字节不丢）
    pub buffered: Vec<u8>,
    /// 剩余的上游字节流（预读之后的那部分）
    pub rest: BoxStream<'static, Result<Bytes, std::io::Error>>,
    /// 预读窗口内看到的 `finish_reason`（`None` = 没看到 / 读不出来）
    pub finish_reason: Option<String>,
    /// 预读窗口内看到的**流内错误帧**文案（`None` = 没有；见 [`scan_stream_error`]）
    ///
    /// 与 `finish_reason` 并列而不是二选一：两者的判据与处置完全不同
    /// （拒答看 `finish_reason` 的取值、错误帧看正文），且一个窗口里**可能两者都有**
    /// —— 那时按各自的分支各判一次，谁先命中谁负责。
    pub stream_error: Option<String>,
}

/// `finish_reason` 是否在本家声明的「拒答」集合里（大小写不敏感）。
///
/// 集合来自适配器的 `stream_refusal_finishes`；空集恒为 `false`（默认不认）。
pub fn is_refusal(finishes: &[&str], finish: &str) -> bool {
    finishes
        .iter()
        .any(|marker| marker.eq_ignore_ascii_case(finish))
}

impl HeldHead {
    /// 把「预读到的字节 + 剩余流」拼成一条**完整**的下游字节流。
    ///
    /// 预读的字节原样在前（一个字节不丢），剩余流在后 —— 调用方拿到它就与
    /// 从未预读过视同上游流。`capture` 非空时逐片旁路给调试采集器（含预读
    /// 那一片）：预读发生在 `ForwardStream` 之前，采集得在这里补上，否则调试
    /// 模式会漏掉这一段的原始报文。
    pub fn into_stream(
        self,
        capture: Option<std::sync::Arc<crate::server::core::debug_traffic::TrafficCapture>>,
    ) -> BoxStream<'static, Result<Bytes, std::io::Error>> {
        let head = (!self.buffered.is_empty()).then(|| Bytes::from(self.buffered));
        if let (Some(bytes), Some(capture)) = (head.as_ref(), capture.as_ref()) {
            capture.push(bytes);
        }
        let rest = self.rest.map(move |item| {
            if let (Ok(bytes), Some(capture)) = (&item, &capture) {
                capture.push(bytes);
            }
            item
        });
        futures::stream::iter(head.into_iter().map(Ok)).chain(rest).boxed()
    }
}

/// 一段（可能不完整的）SSE 文本里的 `data:` 载荷，逐帧交出来（**不解析**，
/// 交出去的还是文本）。
///
/// 只认**成对换行结束**的块里的 `data:` 行，且载荷要 trim 后以 `{` 开头 ——
/// 尾部没结束的那半截不算一帧（当成一帧解析失败，会把「还没读到终帧」读成
/// 「没有终帧」）。`[DONE]` 哨兵与注释行（`: keepalive`）跳过。
///
/// 抽出来共用是有意的：本层的两条识别路径读的是**同一批字节** —— 「找
/// `finish_reason`」（这里）与「攒出这一轮的正文/终帧」（`continuation` 的
/// `TurnAccumulator`）。切帧口径写两份，就会出现一份算一帧、另一份算两帧的分歧，
/// 而这种分歧只会表现为「偶尔少判一次 / 多判一次」，查不出根因。
///
/// 它对 `core::upstream` 内部可见（`pub(super)`）而不是全开：切帧口径是这一层的
/// 私有约定，外层不该按它解析字节。
pub(super) fn frame_payloads(text: &str) -> impl Iterator<Item = &str> {
    text.split("\n\n")
        .flat_map(|block| block.lines())
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|data| !data.is_empty() && *data != "[DONE]" && data.starts_with('{'))
}

/// 从一段（可能不完整的）SSE 文本里找**第一个**非空 `choices[].finish_reason`。
///
/// 切帧口径见 [`frame_payloads`]（与本模块的预读窗口、`continuation` 的判定同源）。
///
/// 大小写保留原值（判定由适配器按大小写不敏感比较）；非 JSON 帧跳过。
pub fn scan_finish_reason(text: &str) -> Option<String> {
    for data in frame_payloads(text) {
        let Ok(frame) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        let Some(choices) = frame.get("choices").and_then(Value::as_array) else {
            continue;
        };
        for choice in choices {
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                if !reason.is_empty() {
                    return Some(reason.to_string());
                }
            }
        }
    }
    None
}

/// 预读窗口里出现的**第一个 `{"error": …}` 帧**的可读文案（`None` = 没有）。
///
/// ── 为什么需要它（与 `finish_reason` 是两件事）──────────────────
/// 有一类上游把「这次不给你答复」写成一条 **SSE 正文里的错误帧**：
/// `HTTP 200` + `data: {"error":{"message":"积分不足…"}}`，**终帧的
/// `finish_reason` 仍是正常的 `stop`** —— 于是它既不是拒答（`finish_reason`
/// 那条路认不出），也不是 HTTP 错误（状态码是 200）。编排层若不认它，客户端
/// 就会把一个「余额用完了」当成一次正常回答，且账号**不会**被换掉/冷却。
///
/// 这一形态是 LobsterAI / AtomCode 的实现明确记录的（两家都在自己的
/// `errors.rs` 里写着「额度耗尽可能以这个形态下发」，并把它做成了纯函数 —— 因为
/// 在本模块出现之前，逐帧出口没有「按内容嗅探」的挂点）。
///
/// ── 判据只管「有没有 error 帧」，**不管它是什么意思** ─────────────
/// 「这条错误属于哪一档」（余额耗尽 / 会话已死 / 别的）是**上游侧知识**，由适配器
/// 的 `stream_error_classifier` 回答（与本模块不碰 `finish_reason` 的含义同一分工）。
/// 本函数只负责从字节里把它挖出来，挖不到就是 `None`（fail-open，见模块头）。
///
/// 口径：`{"error":{"message":…}}` 与 `{"error":"…"}` 两种形态都认（宽容读取）；
/// 文案为空时给一句中性的兜底串（**不返回空串** —— 空串会让调用方以为没读到）。
pub fn scan_stream_error(text: &str) -> Option<String> {
    for data in frame_payloads(text) {
        let Ok(frame) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        let message = match frame.get("error") {
            Some(Value::Object(error)) => error
                .get("message")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("")
                .to_string(),
            Some(Value::String(text)) => text.trim().to_string(),
            _ => continue,
        };
        return Some(if message.is_empty() {
            "上游在流内返回了错误帧（未给出文案）".to_string()
        } else {
            message
        });
    }
    None
}

/// 把上游流拉到「能判定是否被拒 / 有没有错误帧」为止（见模块头）。
///
/// `watch_finishes` / `watch_error` 决定这次预读**要不要**收集哪一类判据：
///   · `watch_finishes` = 适配器声明了非空 `stream_refusal_finishes`；
///   · `watch_error` = 适配器的 `stream_error_classifier` 会给结论。
/// 两者都为假时**调用点根本不该进来**（构造期已短路）；真进来又都为假则立刻返回
/// 空预读（等价于不缓冲）—— 那是内部契约错误的自保，不是正常路径。
///
/// 上限按「要看什么」取（见 [`HOLD_CAP_BYTES`] / [`HOLD_CAP_ERROR_BYTES`]）：
/// 只看错误帧时用小的那个 —— 错误帧是终态、通常就是首帧，等它不需要读完流。
///
/// 入参是**未消费**的 `reqwest::Response`（调用点在本函数之后不再需要它）：
/// 状态码与响应头在这里取出，字节流被包上进空闲守卫（与 `ForwardStream::new`
/// 同一件事，只是提前到这里 —— 预读期间上游卡住也不能把请求挂死）。
pub async fn hold_head(
    response: reqwest::Response,
    watch_finishes: bool,
    watch_error: bool,
) -> HeldHead {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    // 与 `ForwardStream::new` / 聚合器同款：reqwest 错误就地描述成文案折进 io::Error
    let inner = response.bytes_stream().map(|item| {
        item.map_err(|error| {
            std::io::Error::other(crate::server::core::egress::describe_error_detail(&error))
        })
    });
    let mut source = stall::idle_guard(
        Box::pin(inner),
        Duration::from_millis(config::timeout_settings().stream_idle_ms()),
    );
    let mut buffered: Vec<u8> = Vec::new();
    let mut text = String::new();
    let mut finish_reason: Option<String> = None;
    let mut stream_error: Option<String> = None;
    // 上限按「要看什么」取（见两个常量的说明）：两者都看时取大的那个。
    let cap = if watch_finishes {
        HOLD_CAP_BYTES
    } else if watch_error {
        HOLD_CAP_ERROR_BYTES
    } else {
        0
    };
    loop {
        // 先看已缓冲的字节里有没有终帧（一次读可能带来好几帧）
        if watch_finishes {
            if let Some(reason) = scan_finish_reason(&text) {
                finish_reason = Some(reason);
                break;
            }
        }
        // 流内错误帧同样是「到此已可判定」：它一旦出现，后面再读到什么都不会改变
        // 「这次没有答复」这个事实（错误帧是终态），所以与终帧一样提前收手。
        if watch_error {
            if let Some(message) = scan_stream_error(&text) {
                stream_error = Some(message);
                break;
            }
        }
        if buffered.len() >= cap {
            break;
        }
        match source.next().await {
            // 流结束 / 传输错误 / 空闲超时：拿不到结论 ⇒ 放行（fail-open，见模块头）
            None | Some(Err(_)) => break,
            Some(Ok(chunk)) => {
                buffered.extend_from_slice(&chunk);
                text.push_str(&String::from_utf8_lossy(&chunk));
            }
        }
    }
    HeldHead {
        status,
        headers,
        buffered,
        rest: source,
        finish_reason,
        stream_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scans_the_first_non_empty_finish_reason() {
        // 普通流帧 finish_reason 为 null = 不算终结；终帧给出 stop
        let text = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        );
        assert_eq!(Some("stop".to_string()), scan_finish_reason(text));
        // content_filter 原样取出（判定在适配器侧）
        let text = "data: {\"choices\":[{\"delta\":{\"content\":\"x\"},\"finish_reason\":\"content_filter\"}]}\n\n";
        assert_eq!(Some("content_filter".to_string()), scan_finish_reason(text));
    }

    #[test]
    fn ignores_incomplete_trailing_frame_and_noise() {
        // 尾部没结束的那行不算一帧（半截 JSON 不能解析），这里因此看不到 finish
        let text = "data: {\"choices\":[{\"delta\":{\"content\":\"x\"},\"finish_reason\":\"content_fil";
        assert_eq!(None, scan_finish_reason(text));
        // 只有 null / 空串 / 非 data 行 / [DONE]：都不算终结
        let text = concat!(
            ": keepalive\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"},\"finish_reason\":\"\"}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"b\"},\"finish_reason\":null}]}\n\n",
            "data: [DONE]\n\n",
            "data: 不是一个 json\n\n",
        );
        assert_eq!(None, scan_finish_reason(text));
        // 多个 choice、终结在第二个时也能取到
        let text = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":null},{\"index\":1,\"delta\":{},\"finish_reason\":\"length\"}]}\n\n";
        assert_eq!(Some("length".to_string()), scan_finish_reason(text));
    }

    #[test]
    fn scan_tolerates_real_chunk_shape_with_usage_tail() {
        // 收尾 usage 帧（choices 空数组）不该打断扫描
        let text = concat!(
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"好\"},\"finish_reason\":\"content_filter\"}],\"usage\":null}\n\n",
            "data: {\"id\":\"c1\",\"choices\":[],\"usage\":{\"prompt_tokens\":711006,\"completion_tokens\":47}}\n\n",
            "data: [DONE]\n\n",
        );
        assert_eq!(Some("content_filter".to_string()), scan_finish_reason(text));
        // 顺带钉住 usage 帧的形状不会误报
        assert_eq!(
            None,
            scan_finish_reason("data: {\"choices\":[],\"usage\":{\"completion_tokens\":1}}\n\n")
        );
        let _ = json!({});
    }

    /// 流内错误帧：两种形态都认；普通帧与 usage 帧不能误判
    #[test]
    fn scan_stream_error_finds_the_error_frame() {
        // `{"error":{"message":…}}`（背后的真实形态）
        assert_eq!(
            Some("积分不足".to_string()),
            scan_stream_error("data: {\"error\":{\"message\":\"积分不足\"}}\n\n")
        );
        // `{"error":"…"}` 也认（宽容读取）
        assert_eq!(
            Some("token rejected".to_string()),
            scan_stream_error("data: {\"error\":\"token rejected\"}\n\n")
        );
        // **关键**：正常的答复帧里有 `error` 之外的键，一个都不该被当成错误帧
        let normal = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"completion_tokens\":3}}\n\n",
            "data: [DONE]\n\n",
        );
        assert_eq!(None, scan_stream_error(normal));
        // 错误帧里 `message` 为空时给中性兜底串，**不返回空串**
        assert_eq!(
            Some("上游在流内返回了错误帧（未给出文案）".to_string()),
            scan_stream_error("data: {\"error\":{\"message\":\"  \"}}\n\n")
        );
        // 半截帧 / 非 data 行 / 畸形 JSON：都不认
        assert_eq!(None, scan_stream_error("data: {\"error\":{\"message\":\"x\""));
        assert_eq!(None, scan_stream_error(": keepalive\n\n"));
        assert_eq!(None, scan_stream_error("data: 不是 json\n\n"));
        assert_eq!(None, scan_stream_error(""));
    }

    /// 两者能在**同一段字节**里各取各的（错误帧与终帧可能同时出现）
    #[test]
    fn finish_reason_and_stream_error_are_independent() {
        let text = concat!(
            "data: {\"error\":{\"message\":\"quota exhausted\"}}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        );
        // 两者都不为空 —— 判据是两件事，不互相遮蔽
        assert_eq!(Some("quota exhausted".to_string()), scan_stream_error(text));
        assert_eq!(Some("stop".to_string()), scan_finish_reason(text));
    }
}
