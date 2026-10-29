//! 零宽空格脱敏词表：`GET/PUT /api/zwsp`。
//!
//! ── 这一页做什么 ────────────────────────────────────────────
//! 用户维护一份词表；转发层在每次出站前把每个词的所有出现位置**各插入一个
//! 零宽空格（U+200B）**，词本身一个字不改。逐字精确匹配的内容审核因此匹配
//! 不上，而人眼读到的仍是同一个词。
//!
//! ── 与 /api/sanitize 的关系：两层、互不替代 ─────────────────
//! `/api/sanitize` 管的那一层是**内置规则集**：只认上游实测拦过的那几句话
//! 与表头键值，是封闭的 —— 上游换模板就得改代码发版。本条管的是另一半：
//! 用户自己知道的、规则集里没有的词。两层在 `core::sanitize` 里先后执行，
//! 顺序不能换（理由见 `sanitize_text` 的文档）。
//!
//! 于是**两个开关可以独立开关**：关掉内置规则集（`/api/sanitize` 置 false）
//! 而保留词表，或反过来，都是合法组合。这正是「两层」与「一个开关加一个词表」
//! 的区别 —— 后者没有独立开关。
//!
//! ── 为什么是「整份替换」而不是逐词增删 ──────────────────────
//! 词表是**一个列表**，界面上的编辑动作（加一个、删一个、改一个、拖动排序）
//! 最终都落成「这份表现在长这样」。逐词增删要额外定义四种操作与它们各自的
//! 冲突语义（改词 = 删+增？删不存在的词算 404 还是 204？），而这些在真实
//! 使用里全是噪音：词表的规模是几条到几十条，整份 PUT 的代价可以忽略。
//! 与 `PUT /api/cline/headers`（整份覆盖表）同一取向。
//!
//! ── 写盘位置 ────────────────────────────────────────────────
//! `config` 的 `sanitizeZwspWords` 键（统一库 `kv` 表里的配置行，JSON 字符串
//! 数组）。转发层逐请求读快照，改完下一个请求立即生效，不重启进程。

use axum::body::Bytes;
use axum::extract::State;
use axum::response::Response;
use serde_json::{json, Value};

use crate::server::config::{
    self, KEY_SANITIZE_ZWSP_WORDS, SANITIZE_ZWSP_MAX_WORDS, SANITIZE_ZWSP_MAX_WORD_CHARS,
};
use crate::server::errors;
use crate::server::http::{ok_json, parse_body};
use crate::server::logging;
use crate::server::ServerState;

/// GET /api/zwsp —— 当前词表
pub async fn get_zwsp(State(_state): State<ServerState>) -> Response {
    ok_json(state_json())
}

/// PUT /api/zwsp —— body `{sanitizeZwspWords: ["词1", "词2", ...]}`
///
/// 整份替换（空数组 = 清空，这一层随即不生效）。写配置后下一个请求立即生效
/// （转发层逐请求读快照，不重启进程）。
///
/// 与 `/api/sanitize` 一样两个方向都记事件日志：加词的那一刻起，用户的正常
/// 文本会被静默插入不可见字符 —— 「这段回答里为什么多了个看不见的东西」这类
/// 问题，日志里必须查得到是哪个词、什么时候加的。
pub async fn put_zwsp(State(_state): State<ServerState>, body: Bytes) -> Response {
    let payload = match parse_body(&body) {
        Ok(value) => value,
        Err(error) => return errors::management_error(400, error.message),
    };
    let Some(object) = payload.as_object() else {
        return errors::management_error(400, "请求体必须是 JSON 对象");
    };
    let Some(value) = object.get(KEY_SANITIZE_ZWSP_WORDS) else {
        return errors::management_error(400, format!("缺少 {KEY_SANITIZE_ZWSP_WORDS}"));
    };
    let words = match validated_words(value) {
        Ok(words) => words,
        Err(message) => return errors::management_error(400, message),
    };
    if !config::set_sanitize_zwsp_words(words.clone()) {
        logging::log("[Config]", "⚠️  零宽空格词表写入失败，本次运行内仍生效");
    }
    logging::log("[Config]", &change_note(&words));
    ok_json(state_json())
}

/// POST /api/zwsp/preview —— body `{text, sanitizeZwspWords?}`
///
/// 返回「这句话经过词表层之后长什么样」。**不是**另写一套实现：直接调
/// `core::sanitize::preview_zwsp`，它内部走的是转发层同一个 `break_word`，
/// 所以预览与真实出站逐字一致（理由见那个函数的文档）。
///
/// `sanitizeZwspWords` 省略时用**当前已保存**的词表 —— 界面有两种用法：
///   · 编辑中实时预览没保存的候选词 → 带上候选；
///   · 只想确认「现在这套规则会怎么处理这句话」→ 不带。
///
/// 文本长度有上限（[`PREVIEW_MAX_CHARS`]）：这个端点只用于给人看的短样本，
/// 没有理由接受整篇文档；超限明确 400，不静默截断（截断后预览就骗人了）。
pub async fn preview_zwsp(_state: State<ServerState>, body: Bytes) -> Response {
    let payload = match parse_body(&body) {
        Ok(value) => value,
        Err(error) => return errors::management_error(400, error.message),
    };
    let Some(object) = payload.as_object() else {
        return errors::management_error(400, "请求体必须是 JSON 对象");
    };
    let Some(text) = object.get("text").and_then(Value::as_str) else {
        return errors::management_error(400, "缺少 text");
    };
    if text.chars().count() > PREVIEW_MAX_CHARS {
        return errors::management_error(
            400,
            format!("预览文本过长（上限 {PREVIEW_MAX_CHARS} 字符）"),
        );
    }
    // 候选词表要走与 PUT 同一套校验 —— 否则界面可以用一份「保存会失败」的
    // 词表看到漂亮的预览，然后保存时被 400 打回来。
    let words = match object.get(KEY_SANITIZE_ZWSP_WORDS) {
        Some(value) => match validated_words(value) {
            Ok(words) => words,
            Err(message) => return errors::management_error(400, message),
        },
        None => config::current().sanitize_zwsp_words().to_vec(),
    };
    let (transformed, inserted) = crate::server::core::sanitize::preview_zwsp(text, &words);
    ok_json(json!({
        "original": text,
        "transformed": transformed,
        "inserted": inserted,
        // 字数与字节数都给：零宽空格是 3 字节/1 字符，两个数字的变化量不同，
        // 「我以为只多了一个字符怎么大了 3 字节」这类疑问看一眼就明白。
        "charsBefore": text.chars().count(),
        "charsAfter": transformed.chars().count(),
        "bytesBefore": text.len(),
        "bytesAfter": transformed.len(),
        "words": words,
    }))
}

/// 预览文本的字符上限。够放下几段真实对话样本，又不至于让人拿它跑整篇文档。
const PREVIEW_MAX_CHARS: usize = 4000;

/// 词表的校验与规整（`Err` 的文本就是 400 的文案）。
///
/// ── 为什么抽成纯函数 ────────────────────────────────────────
/// 判据要能被单测钉住：`ServerState::bootstrap` 会开真库、写 `~/.agate`，
/// 用例驱动不了 handler 本身，而这几条判据恰恰是「写坏了静默改写每一条请求」
/// 的那一类（与 `api::auto_continue` 的 `patch_from` 同一手法）。
///
/// ── 逐条的判据与理由 ────────────────────────────────────────
///   - **必须是数组**：单个字符串听上去可以宽容地收下，但那会让「清空词表」
///     与「只加一个词」两种意图在类型上不可区分。
///   - **每项必须是字符串**：数字/布尔/对象都不该被猜成文本。
///   - **去首尾空白后非空**：空串会让 `str::find("")` 在**每个字节位置**命中，
///     等于把整段文本打成筛子 —— 这是必须挡住的一条。
///   - **不含零宽字符**：词条本身就是打断后的形态（用户从别处复制来的），
///     再加一个 ZWSP 会插在 ZWSP 后面，越积越长。
///   - **长度与条数有界**：见 `SANITIZE_ZWSP_MAX_WORD_CHARS` /
///     `SANITIZE_ZWSP_MAX_WORDS` 的说明。超限是 400 而不是截断：条数超限时
///     「截掉哪几条」不是网关该替用户决定的。
///   - **去重**：同一个词两次会让同一处命中被插两次，语义上是错的。保序
///     （用户看到的顺序就是他写的顺序，见 `parse::zwsp_words_field`）。
fn validated_words(value: &Value) -> Result<Vec<String>, String> {
    let Some(items) = value.as_array() else {
        return Err(format!("{KEY_SANITIZE_ZWSP_WORDS} 必须是字符串数组"));
    };
    if items.len() > SANITIZE_ZWSP_MAX_WORDS {
        return Err(format!(
            "{KEY_SANITIZE_ZWSP_WORDS} 最多 {SANITIZE_ZWSP_MAX_WORDS} 条，收到 {} 条",
            items.len()
        ));
    }
    let mut words: Vec<String> = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let Some(text) = item.as_str() else {
            return Err(format!("第 {} 项不是字符串", index + 1));
        };
        let text = text.trim();
        if text.is_empty() {
            return Err(format!("第 {} 项是空词条", index + 1));
        }
        if text.contains(config::ZWSP) {
            return Err(format!("第 {} 项已经含零宽空格，请填原始词", index + 1));
        }
        let chars = text.chars().count();
        if chars > SANITIZE_ZWSP_MAX_WORD_CHARS {
            return Err(format!(
                "第 {} 项有 {chars} 个字符，最多 {SANITIZE_ZWSP_MAX_WORD_CHARS} 个",
                index + 1
            ));
        }
        if words.iter().any(|existing| existing == text) {
            continue;
        }
        words.push(text.to_string());
    }
    Ok(words)
}

/// 这次改动的一句话记录（进事件日志）。
fn change_note(words: &[String]) -> String {
    if words.is_empty() {
        return "零宽空格词表已清空：这一层不再改写任何文本".to_string();
    }
    // 词条可能很长，日志里只列前几个，其余报个数
    let shown: Vec<&str> = words.iter().take(5).map(String::as_str).collect();
    let tail = if words.len() > shown.len() {
        format!(" 等 {} 条", words.len())
    } else {
        String::new()
    };
    format!(
        "零宽空格词表已更新为 {} 条：{}{}",
        words.len(),
        shown.join("、"),
        tail
    )
}

/// 词表（GET 与 PUT 共用，前端直接用响应刷新界面）
fn state_json() -> Value {
    json!({
        KEY_SANITIZE_ZWSP_WORDS: config::current().sanitize_zwsp_words(),
    })
}

#[cfg(test)]
mod tests {
    //! PUT 的几条判据：形状、空词条、零宽字符、长度、条数、去重。
    //!
    //! ── 为什么钉在这里而不是响应状态码上 ────────────────────
    //! handler 需要的 `ServerState` 只能由 `ServerState::bootstrap` 构造，而它会
    //! 开真库、写 `~/.agate` —— 用例驱动不了它。判据抽成 [`validated_words`]
    //! （返回 `Err(文案)`），handler 那侧只有一句 `Err(message) =>
    //! errors::management_error(400, message)`。与 `api::auto_continue` 同一取舍。
    //!
    //! ── 每条用例钉的是哪种「写歪」───────────────────────────
    //! 这些不是补覆盖率：每一条都对应一个**看起来合理**的实现 ——
    //! 收单个字符串、放过空词条、把超限截断、不去重。

    use super::*;

    /// 空串必须被拒：它会 `find` 在每个字节位置命中，等于毁掉整段文本。
    #[test]
    fn an_empty_word_is_rejected_rather_than_slicing_every_position() {
        for bad in [json!(""), json!("   "), json!("\t\r\n")] {
            assert!(
                validated_words(&json!([bad.clone()])).is_err(),
                "{bad} 是空词条，必须 400（否则 find(\"\") 会在每个字节位置命中）"
            );
        }
    }

    /// 已经是打断形态的词被拒：再加一个 ZWSP 会让它越积越长。
    #[test]
    fn an_already_broken_word_is_rejected() {
        let broken = format!("敏感{}{}", config::ZWSP, "词");
        let error = validated_words(&json!([broken])).expect_err("含零宽空格必须 400");
        assert!(error.contains("零宽空格"), "文案要说清是哪个问题: {error}");
    }

    /// 条数超限是 400，不是静默截断。
    ///
    /// 钉住的写歪：`truncate(MAX)` —— 用户会以为整份都生效了。
    #[test]
    fn too_many_words_is_an_error_not_a_silent_truncation() {
        let items: Vec<Value> = (0..=SANITIZE_ZWSP_MAX_WORDS)
            .map(|index| json!(format!("词{index}")))
            .collect();
        let error = validated_words(&Value::Array(items)).expect_err("超限必须 400");
        assert!(error.contains("最多"), "文案要带上限: {error}");
    }

    /// 恰好到上限是可以的（边界不多不少）。
    #[test]
    fn exactly_the_cap_is_accepted() {
        let items: Vec<Value> = (0..SANITIZE_ZWSP_MAX_WORDS)
            .map(|index| json!(format!("词{index}")))
            .collect();
        let words = validated_words(&Value::Array(items)).expect("正好到上限应当收下");
        assert_eq!(words.len(), SANITIZE_ZWSP_MAX_WORDS);
    }

    /// 去重且保序：重复项被丢掉，第一次出现的位置不变。
    ///
    /// 钉住的写歪：用 `HashSet` 去重（顺序丢了，用户看到的词表会自己重排）。
    #[test]
    fn duplicates_are_dropped_but_the_authors_order_is_kept() {
        let words = validated_words(&json!(["乙", "甲", "乙", "丙", "甲"]))
            .expect("重复不是错误，只是要收成一个");
        assert_eq!(words, vec!["乙".to_string(), "甲".to_string(), "丙".to_string()]);
    }

    /// 首尾空白被去掉，且去掉空白后若相同则视为重复。
    #[test]
    fn surrounding_whitespace_is_trimmed_before_dedup() {
        let words = validated_words(&json!(["  甲  ", "甲"])).expect("去空白后重复，收成一个");
        assert_eq!(words, vec!["甲".to_string()]);
    }

    /// 非字符串项与非数组体都要报错，且不是数组时文案要说清期望的形状。
    #[test]
    fn the_shape_is_the_shape() {
        for bad in [json!("甲"), json!(1), json!({"a": 1}), json!(null)] {
            let error = validated_words(&bad).expect_err("不是数组必须 400");
            assert!(error.contains("字符串数组"), "文案要说清期望: {error}");
        }
        let error = validated_words(&json!([1, 2])).expect_err("非字符串项必须 400");
        assert!(error.contains("不是字符串"), "文案要看得出是第几项: {error}");
    }
}
