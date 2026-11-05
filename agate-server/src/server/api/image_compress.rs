//! 「会话图片压缩」的开关与三项参数：`GET/PUT /api/image-compress`。
//!
//! ── 这个端点做什么 ──────────────────────────────────────────
//! 转发前把对话请求体里内联的 base64 大图（解码后 ≥ 阈值）缩到长边上限、
//! 重编码成 JPEG。四条判据（只认 chat 形态 / 只认 jpeg·png·webp / 动图跳过 /
//! 压完更大就保留原图）、透明图填白与内容哈希缓存都在
//! [`crate::server::core::upstream::image_compress`]，这里只管四项设置：
//! 开不开（`imageCompressEnabled`）、长边上限（`imageCompressMaxEdge`，默认 2048）、
//! 质量（`imageCompressQuality`，默认 80）、触发阈值（`imageCompressThresholdKb`，
//! 默认 512）。
//!
//! ── 为什么一个端点装四项，而不是拆开 ────────────────────────
//! 与 `/api/auto-continue` 同一理由：这四个值**必须一起读才有意义**。
//! 「开关开了、长边还是 8192、阈值还是 0」这种中间态会在一瞬间把每一张图都
//! 重压一遍却几乎没变小 —— 那是同一个设置动作的四半，所以合成一条 PUT。
//!
//! ── 为什么默认关（与上游相反）──────────────────────────────
//! 上游 agent2api 这一项默认开：它是给自己的桌面客户端用的个人中转，默认压小
//! 就是默认省自己的流量。本项目是**网关**：默认改写用户要发出去的图片内容，
//! 属于「静默改变了客户端可观测的行为」，而且对「从截图里读小字」这类用法是
//! 净损失。所以默认关、开了才压 —— 与 `/api/keepalive-strip`、`/api/cors`
//! 同向（改可观测行为的开关默认关）。完整取舍见
//! [`crate::server::config::KEY_IMAGE_COMPRESS_ENABLED`]。
//!
//! ── 为什么四项能分开改（partial update）─────────────────────
//! 调用场景是分开的：先打开、看几条请求，再只想把长边收到 1280 —— 那一次 PUT
//! 不该顺手把其余三项写回旧值（一次多余的落盘与一条多余的日志），更不该要求
//! 调用方先 GET 才能安全地 PUT。所以**出现哪一项就改哪一项**，缺省项保持原值；
//! 同时全部校验通过才落盘（与 `put_auto_continue` / `put_retry` 同一顺序），
//! 避免出现「开关开了、参数没改」的半套设置。

use axum::body::Bytes;
use axum::extract::State;
use axum::response::Response;
use serde_json::{json, Map, Value};

use crate::server::config::parse::int_in_range;
use crate::server::config::{
    self, ImageCompressPatch, IMAGE_COMPRESS_MAX_MAX_EDGE, IMAGE_COMPRESS_MAX_QUALITY,
    IMAGE_COMPRESS_MAX_THRESHOLD_KB, IMAGE_COMPRESS_MIN_MAX_EDGE, IMAGE_COMPRESS_MIN_QUALITY,
    IMAGE_COMPRESS_MIN_THRESHOLD_KB, KEY_IMAGE_COMPRESS_ENABLED, KEY_IMAGE_COMPRESS_MAX_EDGE,
    KEY_IMAGE_COMPRESS_QUALITY, KEY_IMAGE_COMPRESS_THRESHOLD_KB,
};
use crate::server::errors;
use crate::server::http::{ok_json, parse_body};
use crate::server::logging;
use crate::server::ServerState;

/// GET /api/image-compress —— 当前开关与三项参数
pub async fn get_image_compress(State(_state): State<ServerState>) -> Response {
    ok_json(state_json())
}

/// PUT /api/image-compress —— body 可含其中任意几项
///
/// ```json
/// {"imageCompressEnabled": true, "imageCompressMaxEdge": 2048,
///  "imageCompressQuality": 80, "imageCompressThresholdKb": 512}
/// ```
///
/// 缺省项不动，全部校验通过才落盘。写配置后下一个请求立即生效（转发层逐请求
/// 读快照，不重启进程）。开与关都记一条事件日志：开的那一刻起，出站请求体里
/// 的图片会被改写（模型看到的是压缩后的像素），排查「为什么这张图糊了」时
/// 日志里必须查得到这条线。
pub async fn put_image_compress(State(_state): State<ServerState>, body: Bytes) -> Response {
    let payload = match parse_body(&body) {
        Ok(value) => value,
        Err(error) => return errors::management_error(400, error.message),
    };
    let Some(object) = payload.as_object() else {
        return errors::management_error(400, "请求体必须是 JSON 对象");
    };
    let patch = match patch_from(object) {
        Ok(patch) => patch,
        Err(message) => return errors::management_error(400, message),
    };
    if patch == ImageCompressPatch::default() {
        // 四项都没给：不写盘、不记日志（一条「设置已更新」的日志底下什么都没有，
        // 会让排障的人以为发生过一次改动），直接把当前状态原样回给调用方
        return ok_json(state_json());
    }
    if !config::set_image_compress(patch) {
        logging::log("[Config]", "⚠️  图片压缩设置写入配置失败，本次运行内仍生效");
    }
    logging::log("[Config]", &change_note(&patch));
    ok_json(state_json())
}

/// 请求体 → 部分更新入参（`Err` 的文本就是 400 的文案）。
///
/// ── 为什么抽成纯函数 ────────────────────────────────────────
/// 判据要能被单测钉住：`ServerState::bootstrap` 会开真库、写 `~/.agate`，
/// 用例驱动不了 handler 本身，而这几条判定恰恰是「写坏了静默伤到每一条请求」
/// 的那一类（与 `api::auto_continue::patch_from` 同一手法）。
///
/// ── 为什么不认 `null` ───────────────────────────────────────
/// 与四个键的读侧口径一致：缺失 = 用当前值（不改），出现了就必须是合法值。
/// `null` 出现在这里只能是值打错 —— 这条 PUT 没有「删掉这一项」的语义
/// （setter 只 insert，落库侧的删除只跟着底稿走），把它当成「这一项不改」
/// 放过，结果是调用方收到 200 而实际什么都没变。
fn patch_from(object: &Map<String, Value>) -> Result<ImageCompressPatch, String> {
    let mut patch = ImageCompressPatch::default();
    if let Some(value) = object.get(KEY_IMAGE_COMPRESS_ENABLED) {
        let Some(enabled) = value.as_bool() else {
            return Err(format!("{KEY_IMAGE_COMPRESS_ENABLED} 必须是 true 或 false"));
        };
        patch.enabled = Some(enabled);
    }
    // 三处范围校验共用读侧的常量与 `parse::int_in_range`：范围与「什么算一个
    // 整数」两侧各写一份，迟早出现接口拒绝而手改库接受
    if let Some(value) = object.get(KEY_IMAGE_COMPRESS_MAX_EDGE) {
        let Some(max_edge) = int_in_range(
            value,
            IMAGE_COMPRESS_MIN_MAX_EDGE,
            IMAGE_COMPRESS_MAX_MAX_EDGE,
        ) else {
            return Err(format!(
                "{KEY_IMAGE_COMPRESS_MAX_EDGE} 必须是 {IMAGE_COMPRESS_MIN_MAX_EDGE}~{IMAGE_COMPRESS_MAX_MAX_EDGE} 的整数"
            ));
        };
        patch.max_edge = Some(max_edge);
    }
    if let Some(value) = object.get(KEY_IMAGE_COMPRESS_QUALITY) {
        let Some(quality) = int_in_range(
            value,
            IMAGE_COMPRESS_MIN_QUALITY,
            IMAGE_COMPRESS_MAX_QUALITY,
        ) else {
            return Err(format!(
                "{KEY_IMAGE_COMPRESS_QUALITY} 必须是 {IMAGE_COMPRESS_MIN_QUALITY}~{IMAGE_COMPRESS_MAX_QUALITY} 的整数"
            ));
        };
        patch.quality = Some(quality);
    }
    if let Some(value) = object.get(KEY_IMAGE_COMPRESS_THRESHOLD_KB) {
        let Some(threshold_kb) = int_in_range(
            value,
            IMAGE_COMPRESS_MIN_THRESHOLD_KB,
            IMAGE_COMPRESS_MAX_THRESHOLD_KB,
        ) else {
            return Err(format!(
                "{KEY_IMAGE_COMPRESS_THRESHOLD_KB} 必须是 {IMAGE_COMPRESS_MIN_THRESHOLD_KB}~{IMAGE_COMPRESS_MAX_THRESHOLD_KB} 的整数"
            ));
        };
        patch.threshold_kb = Some(threshold_kb);
    }
    Ok(patch)
}

/// 这次改动的一句话记录（进事件日志）。
///
/// 逐项拼而不是只报「已更新」：日志里要能看出**这次 PUT 动了哪一项**，否则
/// 「谁把质量从 80 改到 40 的」这类问题只能靠翻请求体回答。
fn change_note(patch: &ImageCompressPatch) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(enabled) = patch.enabled {
        parts.push(if enabled {
            "会话图片压缩已开启（出站请求体里的内联大图会被缩小并重编码为 JPEG）".to_string()
        } else {
            "会话图片压缩已关闭：图片原样发出（与接入前逐字节一致）".to_string()
        });
    }
    if let Some(max_edge) = patch.max_edge {
        parts.push(format!("长边上限已设为 {max_edge}"));
    }
    if let Some(quality) = patch.quality {
        parts.push(format!("JPEG 质量已设为 {quality}"));
    }
    if let Some(threshold_kb) = patch.threshold_kb {
        parts.push(format!("触发阈值已设为 {threshold_kb}KB"));
    }
    parts.join(" / ")
}

/// 开关与三项参数（GET 与 PUT 共用，前端直接用响应刷新界面）
fn state_json() -> Value {
    let settings = config::image_compress_settings();
    json!({
        KEY_IMAGE_COMPRESS_ENABLED: settings.enabled,
        KEY_IMAGE_COMPRESS_MAX_EDGE: settings.max_edge,
        KEY_IMAGE_COMPRESS_QUALITY: settings.quality,
        KEY_IMAGE_COMPRESS_THRESHOLD_KB: settings.threshold_kb,
    })
}

#[cfg(test)]
mod tests {
    //! PUT 的两类判据：类型与范围。
    //!
    //! ── 为什么钉在这里而不是响应状态码上 ────────────────────
    //! handler 需要的 `ServerState` 只能由 `ServerState::bootstrap` 构造，而它会
    //! 开真库、写 `~/.agate` —— 用例驱动不了它。于是判据抽成 [`patch_from`]
    //! （返回 `Err(文案)`），handler 那侧只有一句
    //! `Err(message) => errors::management_error(400, message)`。与
    //! `api::auto_continue` 的同一取舍。

    use super::*;

    /// 一份只含给定几项的请求体。
    fn body(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    /// 开关只认字面布尔；`null` / 字符串 / 数字一律 400。
    ///
    /// 钉住的写歪：把 `null` 当成「没给」放过 —— 那样调用方收到 200 而开关
    /// 一动没动（这条 PUT 没有「清空」语义）。
    #[test]
    fn the_switch_accepts_only_a_literal_boolean() {
        let only_true = patch_from(&body(&[(KEY_IMAGE_COMPRESS_ENABLED, json!(true))]))
            .expect("字面 true 是合法值");
        assert_eq!(only_true.enabled, Some(true), "开启必须能被表达出来");

        for bad in [json!("true"), json!(1), json!(null), json!({})] {
            let error = match patch_from(&body(&[(KEY_IMAGE_COMPRESS_ENABLED, bad.clone())])) {
                Ok(patch) => panic!("{bad} 不该被接受，却拿到了 {patch:?}"),
                Err(error) => error,
            };
            assert!(
                error.contains("必须是 true 或 false"),
                "文案要与 /api/auto-continue 同形，拿到的是: {error}"
            );
        }
    }

    /// 三项参数都以读侧的边界为准：端点值收、越界一档就拒，`3.0` 这种整值浮点收。
    ///
    /// 钉住的写歪：判据改用 `value.as_i64()` —— 那样读侧接受 2048.0、写侧 400，
    /// 同一份配置两条口径（`parse::int_in_range` 的存在理由就是把它们钉成一条）。
    #[test]
    fn each_number_is_validated_against_the_read_side_bounds() {
        // (键, 下限, 上限)
        let cases = [
            (
                KEY_IMAGE_COMPRESS_MAX_EDGE,
                IMAGE_COMPRESS_MIN_MAX_EDGE,
                IMAGE_COMPRESS_MAX_MAX_EDGE,
            ),
            (
                KEY_IMAGE_COMPRESS_QUALITY,
                IMAGE_COMPRESS_MIN_QUALITY,
                IMAGE_COMPRESS_MAX_QUALITY,
            ),
            (
                KEY_IMAGE_COMPRESS_THRESHOLD_KB,
                IMAGE_COMPRESS_MIN_THRESHOLD_KB,
                IMAGE_COMPRESS_MAX_THRESHOLD_KB,
            ),
        ];
        for (key, min, max) in cases {
            for accepted in [json!(min), json!(max), json!(min as f64)] {
                assert!(
                    patch_from(&body(&[(key, accepted.clone())])).is_ok(),
                    "{key} 应当接受边界值 {accepted}"
                );
            }
            for rejected in [
                json!(min - 1),
                json!(max + 1),
                json!("80"),
                json!(null),
                json!(min as f64 + 0.5),
            ] {
                let error = match patch_from(&body(&[(key, rejected.clone())])) {
                    Ok(patch) => panic!("{key}={rejected} 不该被接受，却拿到了 {patch:?}"),
                    Err(error) => error,
                };
                assert!(
                    error.starts_with(key) && error.contains(&format!("{min}~{max}")),
                    "{key} 的 400 文案必须带上真正的边界，拿到的是: {error}"
                );
            }
        }
    }

    /// 四项独立：只出现一项时其余是 `None`；空对象 = 什么都没给。
    ///
    /// 钉住的写歪：缺省项填成默认值 —— 那会让「只想把长边收到 1280」这一次 PUT
    /// 顺手把开关写回关（用户刚打开的功能被自己下一次的调整抹掉），
    /// 且 `set_image_compress` 那边根本分不出「没给」与「给的是默认值」。
    #[test]
    fn an_absent_key_stays_absent_rather_than_becoming_the_default() {
        let only_edge = patch_from(&body(&[(KEY_IMAGE_COMPRESS_MAX_EDGE, json!(1280))]))
            .expect("合法");
        assert_eq!(
            (
                only_edge.enabled,
                only_edge.max_edge,
                only_edge.quality,
                only_edge.threshold_kb
            ),
            (None, Some(1280), None, None)
        );

        let nothing = patch_from(&body(&[])).expect("空对象不该报错");
        assert_eq!(
            nothing,
            ImageCompressPatch::default(),
            "什么都没给 = 默认补丁（handler 据此跳过落盘与日志）"
        );
    }
}
