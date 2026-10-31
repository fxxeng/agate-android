//! 签到历史台账 —— 签到中心「最近签到记录」的数据源。
//!
//! ── 为什么需要它 ────────────────────────────────────────────
//! 定时签到只有 `autoCheckin.lastResult` 一条（被下一轮覆盖），账号页只能看到
//! 「最后一次」的结果；签到中心的时间线要回看多轮（定时 / 手动 / 启动补签），
//! 所以把每轮批量签到的汇总追加进 config.json 的 `checkinHistory`。
//!
//! ── 只记批量轮次（id=None），单账号签到不记 ─────────────────
//! 时间线记录的是「一轮」的成败总览；单账号签到是账号页 / 明细行上的点操作，
//! 它的效果已经通过 `checkinAt` 反映在每日签到的账号状态里，再进时间线只会
//! 把「今天 09:12 签了浣熊-C」这种碎行混进轮次记录里。
//!
//! ── 为什么放 core 而不是 api ────────────────────────────────
//! 记账点是 `billing::checkin::run_checkin`（定时签到与手动批量共用那一段执行体），
//! 它在 core 里；`core::auto_checkin` 直接读写 config 已是先例（见它的模块头
//! 「配置读写」），本模块沿用同一模式：`config::update_raw_field` 合并写回，
//! config.json 里的其它字段原样保留。
//!
//! ── 容量 ────────────────────────────────────────────────────
//! 保留最近 [`MAX_ENTRIES`]（20）条：一天最多定时 1 轮 + 手动若干轮，20 条
//! 足够回看两三天；再大只会把 config.json 撑大，没有查询价值。

use serde_json::{json, Value};

use crate::server::config;
use crate::server::core::auto_checkin;
use crate::server::logging;

/// config.json 里承载签到历史的键
const CONFIG_KEY: &str = "checkinHistory";
/// 台账保留条数（新条目在前，超出裁掉最老的）
pub const MAX_ENTRIES: usize = 20;

/// 读台账（新在前）。非数组 / 缺失都给空表 —— 与 auto_checkin 的 raw_object
/// 同一兜底口径：旧 config.json 里没有这个字段，读出来必须是合法默认值。
pub fn list() -> Vec<Value> {
    match config::current().raw().get(CONFIG_KEY) {
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    }
}

/// 追加一轮签到汇总（兼容旧调用）。写入失败只返回 false，不改变签到结果。
pub fn record(summary: &Value, reason: &str) -> bool {
    record_inner(summary, reason, None)
}

/// 按稳定 run_id 幂等投影签到轮次。恢复时重复调用不会重复插入时间线。
pub fn record_for_run(summary: &Value, run_id: &str, reason: &str) -> Result<(), String> {
    if run_id.trim().is_empty() {
        return Err("签到时间线缺少 run_id".to_string());
    }
    if record_inner(summary, reason, Some(run_id)) {
        Ok(())
    } else {
        Err("签到时间线写入失败；批次汇总仍保留在执行台账".to_string())
    }
}

fn record_inner(summary: &Value, reason: &str, run_id: Option<&str>) -> bool {
    let number = |key: &str| summary.get(key).and_then(Value::as_u64).unwrap_or(0);
    let failures: Vec<String> = summary
        .get("failed")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_else(|| {
            summary
                .get("results")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            let error = item.get("error").and_then(Value::as_str)?;
                            let name = item
                                .get("name")
                                .and_then(Value::as_str)
                                .filter(|text| !text.is_empty())
                                .or_else(|| item.get("id").and_then(Value::as_str))
                                .unwrap_or("未知账号");
                            Some(format!("{name}（{error}）"))
                        })
                        .collect()
                })
                .unwrap_or_default()
        });
    let now_ms = summary
        .get("at")
        .and_then(Value::as_i64)
        .unwrap_or_else(logging::now_ms);
    let mut entry = json!({
        "at": now_ms,
        "date": summary.get("date").and_then(Value::as_str)
            .map(str::to_string).unwrap_or_else(auto_checkin::today_key),
        "reason": reason,
        "succeeded": number("succeeded"),
        "active": number("active"),
        "total": number("total"),
        "skipped": number("skipped"),
        "failed": failures.iter().take(5).cloned().collect::<Vec<_>>(),
        "failedCount": summary
            .get("failedCount")
            .and_then(Value::as_u64)
            .unwrap_or(failures.len() as u64),
    });
    if let (Some(run_id), Some(object)) = (run_id, entry.as_object_mut()) {
        object.insert("runId".to_string(), Value::String(run_id.to_string()));
    }
    let mut entries = list();
    if let Some(run_id) = run_id {
        // 以稳定身份替换而非盲目 prepend：同一批次恢复多次只保留一条。
        entries.retain(|item| item.get("runId").and_then(Value::as_str) != Some(run_id));
    }
    entries.insert(0, entry);
    entries.truncate(MAX_ENTRIES);
    write(entries)
}

/// 合并写回 `checkinHistory`（与 auto_checkin::write_state 同一模式：
/// `update_raw_field` 按键直写，config.json 里的其它字段原样保留）
fn write(entries: Vec<Value>) -> bool {
    config::update_raw_field(CONFIG_KEY, Value::Array(entries))
}
