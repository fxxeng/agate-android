//! `raw` 底稿的原子变更助手。
//!
//! ── 这个文件解决什么问题 ────────────────────────────────────────
//! 配置的写盘底稿是 `RuntimeConfig::raw`（`Map<String, Value>`）。有一类保存
//! 不能简单地「读出来、改掉、写回去」，必须在**同一次变更内**先做条件校验
//! 再决定写不写 —— 典型就是「按修订号防覆盖」：客户端提交的是它读到的
//! 那一版，如果期间别处改过，这次保存必须拒绝而不是覆盖。
//!
//! 那种「先比较、再写入」如果拆成两个 API 调用，中间就有一个窗口。本文件
//! 把「取出该键的当前值 → 交给闭包判定并修改 → 值有变才落盘」收在
//! [`super::update`] 的一次变更里。
//!
//! ── 与上游的差异（本文件是裁剪实现）────────────────────────────
//! 上游同名函数除了写回 `raw`，还要同步刷新六七个 typed 快照字段
//! （`model_rules` / `routing_profiles` / `compact_routes` /
//! `client_identification` / `vision_compat`）并通知云同步。
//! Agate 不引入插件体系、也没有云同步，且本批的配置模块**刻意不新增
//! `RuntimeConfig` 的 typed 字段** —— 读取侧直接从 `raw()` 解析
//! （见 `title_routes::current`）。因此这里只保留核心语义：
//! 条件校验、值未变则不动、落盘失败返回调用方给的错误。

use serde_json::Value;

/// 在一个键上做「先校验后写」的原子变更。
///
/// - `key`：`raw` 里的顶层键。
/// - `mutate`：拿到该键的当前值。可以只读它做判定（比如比较修订号），
///   也可以就地修改。**返回 `Err` 表示拒绝本次保存**，`raw` 不会被改动。
/// - `storage_error`：落盘失败时要返回的错误。调用方自己构造，因为不同
///   模块的错误文案不同（上游也是这个约定）。
///
/// 语义细节：
/// 1. 键不存在时传入 `Value::Null`，与「读过一次、配置为空」一致；
/// 2. 闭包跑完后值与进来时相同 → 不落盘，直接返回成功（避免无谓的磁盘写
///    与修订号抖动）；
/// 3. 闭包返回 `Err` → 原样透传，内存与磁盘都不动。
pub(crate) fn try_update_raw_field<T, E>(
    key: &str,
    mutate: impl FnOnce(&mut Value) -> Result<T, E>,
    storage_error: impl FnOnce() -> E,
) -> Result<T, E> {
    let key = key.to_string();
    // `update` 内的闭包是 `FnOnce(&mut RuntimeConfig)`，不能把 `mutate` 与
    // `storage_error` 都 move 进去后还指望在外层再用 —— 所以结果先攒在这里。
    let mut outcome: Option<Result<T, E>> = None;
    let saved = super::update(move |config| {
        let before = config.raw.get(&key).cloned().unwrap_or(Value::Null);
        let mut value = before.clone();
        match mutate(&mut value) {
            Ok(result) => {
                if value != before {
                    config.raw.insert(key.clone(), value);
                }
                outcome = Some(Ok(result));
            }
            Err(error) => outcome = Some(Err(error)),
        }
    });
    match outcome {
        Some(Ok(result)) if saved => Ok(result),
        // 闭包通过但没写盘成功：内存已换新，磁盘没落 —— 必须让调用方知道。
        Some(Ok(_)) => Err(storage_error()),
        Some(Err(error)) => Err(error),
        // 理论上不可达：闭包总会写入 outcome。保守地按存储失败处理。
        None => Err(storage_error()),
    }
}
